//! WP2: whisper-rs 0.16 (metal), WhisperContext cached in AppState.whisper by model file.

use super::{SttOutput, SttRequest, Transcriber};
use crate::AppState;
use crate::contract::SttProvider;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// PLAN 2.6. A guess: needs to be measured (PLAN 12).
const NO_SPEECH_MAX: f32 = 0.6;
/// A segment this far below the clip's peak is over silence or room noise, not speech.
/// Speech segments in the fixtures sit within 4 dB of the clip peak; 30 dB is a wide margin.
const QUIET_BELOW_PEAK_DB: f32 = 30.0;
pub const ENGLISH_ONLY: &str = "This whisper model understands only English. Set the language to English or pick a multilingual model";
/// PLAN 2.4: whisper prompt budget, counted with the real tokenizer.
const PROMPT_TOKENS: usize = 200;

pub fn model_path(data_dir: &Path, file: &str) -> PathBuf {
    data_dir.join("models").join(file)
}

pub fn load(path: &Path) -> anyhow::Result<WhisperContext> {
    let mut p = WhisperContextParameters::default();
    p.use_gpu(true).flash_attn(true);
    let path = path.to_str().ok_or_else(|| anyhow::anyhow!("model path is not UTF-8"))?;
    Ok(WhisperContext::new_with_params(path, p)?)
}

/// The cached context for `file`, loaded now if needed. The lock is held during the load, so a
/// caller that arrives while the preload runs waits for it instead of loading the model twice.
pub fn context(state: &AppState, file: &str) -> anyhow::Result<Arc<WhisperContext>> {
    let mut g = state.whisper.lock().unwrap();
    if let Some((f, ctx)) = g.as_ref() {
        if f == file {
            return Ok(ctx.clone());
        }
    }
    let path = model_path(&state.data_dir, file);
    anyhow::ensure!(path.exists(), "Whisper model {file} is not downloaded");
    let t = Instant::now();
    *g = None; // free the old model before loading the new one
    let ctx = Arc::new(load(&path)?);
    log::info!("whisper: loaded {file} in {} ms", t.elapsed().as_millis());
    *g = Some((file.to_string(), ctx.clone()));
    Ok(ctx)
}

/// Load `settings.stt.whisper_model` on a background thread into AppState.whisper.
/// Called at startup and by save_settings when the model changes. Never at the first press.
/// Skipped when nothing uses whisper (a cloud provider without local fallback).
pub fn load_in_background(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let s = state.settings.read().unwrap().stt.clone();
        if s.provider != SttProvider::WhisperLocal && !s.local_fallback {
            return;
        }
        if !model_path(&state.data_dir, &s.whisper_model).exists() {
            log::info!("whisper preload: {} is not downloaded", s.whisper_model);
            return;
        }
        if let Err(e) = context(&state, &s.whisper_model) {
            log::warn!("whisper preload failed: {e:#}");
        }
    });
}

pub struct Whisper {
    ctx: Arc<WhisperContext>,
}

impl Whisper {
    pub fn new(ctx: Arc<WhisperContext>) -> Self {
        Self { ctx }
    }
}

impl Transcriber for Whisper {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        let lang = req.lang();
        // An English-only model turns other speech into invented English: fail (the WAV is kept for Retry).
        anyhow::ensure!(self.ctx.is_multilingual() || lang == Some("en"), "{}", ENGLISH_ONLY);
        let prompt = if req.hints.terms.is_empty() {
            String::new()
        } else {
            let count = |s: &str| self.ctx.tokenize(s, 1024).map(|t| t.len()).unwrap_or(usize::MAX);
            crate::dictionary::whisper_prompt(req.hints, lang, PROMPT_TOKENS, &count).replace('\0', "")
        };
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some(lang.unwrap_or("auto")));
        p.set_no_context(true);
        p.set_suppress_nst(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        p.set_n_threads(std::thread::available_parallelism().map(|n| n.get().saturating_sub(2).max(1)).unwrap_or(4) as i32);
        if !prompt.is_empty() {
            p.set_initial_prompt(&prompt);
        }
        let mut st = self.ctx.create_state()?;
        st.full(p, req.audio)?;
        if std::env::var("SEGDBG").is_ok() { for s in st.as_iter() { eprintln!("SEG p={:.3} t={}-{} peak={:.1} {:?}", s.no_speech_probability(), s.start_timestamp(), s.end_timestamp(), peak_db(req.audio, s.start_timestamp(), s.end_timestamp()), s.to_str_lossy().unwrap_or_default()); } }
        let clip_peak = peak_db(req.audio, 0, i64::MAX);
        let text: String = st
            .as_iter()
            .filter(|s| s.no_speech_probability() <= NO_SPEECH_MAX)
            .map(|s| (peak_db(req.audio, s.start_timestamp(), s.end_timestamp()), s.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default()))
            // A "you" / "Thank you." over the trailing silence: no_speech_probability is often 0 there
            // (measured on long.wav, ggml-base), so check the audio under the segment instead.
            .filter(|(peak, t)| !(*peak < clip_peak - QUIET_BELOW_PEAK_DB && crate::text::is_hallucination(t)))
            .map(|(_, t)| t)
            .collect();
        let language = whisper_rs::get_lang_str(st.full_lang_id_from_state()).map(str::to_string);
        Ok(SttOutput { text: text.trim().to_string(), language, note: None, fell_back: false })
    }
}

/// Peak dB of 16 kHz `a` between whisper timestamps t0..t1 (centiseconds = 160 samples).
fn peak_db(a: &[f32], t0: i64, t1: i64) -> f32 {
    let at = |t: i64| (t.max(0) as usize).saturating_mul(160).min(a.len());
    let (i, j) = (at(t0), at(t1).max(at(t0)));
    let p = a[i..j].iter().fold(0f32, |m, x| m.max(x.abs()));
    20.0 * p.max(1e-9).log10()
}

#[cfg(test)]
mod tests {
    #[test]
    fn peak_db_spans_are_clamped() {
        let mut a = vec![0.0f32; 1600];
        a[200] = 0.5; // inside 1..2 cs
        assert!((super::peak_db(&a, 1, 2) + 6.02).abs() < 0.1);
        assert!(super::peak_db(&a, 5, 9) < -150.0);
        assert!(super::peak_db(&a, 0, i64::MAX) > -7.0);
        assert!(super::peak_db(&a, 50, 3) < -150.0); // reversed or past the end: empty span
    }
}
