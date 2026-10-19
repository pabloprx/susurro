//! WP2: whisper-rs 0.16 (metal), WhisperContext cached in AppState.whisper by model file.

use super::{SttOutput, SttRequest, Transcriber};
use crate::AppState;
use crate::contract::SttProvider;
use std::path::{Path, PathBuf};
use crate::dictionary::Hints;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
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

/// The model to load: `file` if it is downloaded, else the best downloaded one (catalog order) and a note
/// for the user. Nothing downloaded: `file`, so the load fails with "not downloaded".
pub fn usable(data_dir: &Path, file: &str) -> (String, Option<String>) {
    if model_path(data_dir, file).exists() {
        return (file.into(), None);
    }
    match crate::models::CATALOG.iter().map(|(f, ..)| *f).find(|f| model_path(data_dir, f).exists()) {
        Some(f) => (f.into(), Some(format!("Whisper model {file} is not downloaded - used {f}"))),
        None => (file.into(), None),
    }
}

/// Load `settings.stt.whisper_model` (or the `usable` stand-in) on a background thread into AppState.whisper.
/// Called at startup and by save_settings when the model changes. Never at the first press.
/// Skipped when nothing uses whisper (a cloud provider without local fallback).
pub fn load_in_background(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let all = state.settings.read().unwrap().clone();
        let s = all.stt.clone();
        // the command STT uses the same instance (auto = local Whisper)
        let commands = all.command.enabled && super::command_stt(&all) == SttProvider::WhisperLocal;
        if s.provider != SttProvider::WhisperLocal && !s.local_fallback && !commands {
            return;
        }
        let (file, _) = usable(&state.data_dir, &s.whisper_model);
        if !model_path(&state.data_dir, &file).exists() {
            log::info!("whisper preload: {file} is not downloaded");
            return;
        }
        if let Err(e) = context(&state, &file) {
            log::warn!("whisper preload failed: {e:#}");
        }
    });
}

pub struct Whisper {
    ctx: Arc<WhisperContext>,
    /// Shown to the user with the text (a stand-in model was used, see `usable`).
    pub note: Option<String>,
}

impl Whisper {
    pub fn new(ctx: Arc<WhisperContext>) -> Self {
        Self { ctx, note: None }
    }
}

impl Transcriber for Whisper {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        self.run(req, "")
    }
}

impl Whisper {
    /// `prev`: the text of the chunk before this one (streaming), the last words go into the initial prompt.
    fn run(&self, req: &SttRequest, prev: &str) -> anyhow::Result<SttOutput> {
        let lang = req.lang();
        // An English-only model turns other speech into invented English: fail (the WAV is kept for Retry).
        anyhow::ensure!(self.ctx.is_multilingual() || lang == Some("en"), "{}", ENGLISH_ONLY);
        let count = |s: &str| self.ctx.tokenize(s, 1024).map(|t| t.len()).unwrap_or(usize::MAX);
        // Half the budget for the previous chunk's last words, the rest for the dictionary terms.
        let mut tail: Vec<&str> = prev.split_whitespace().collect();
        tail.drain(..tail.len().saturating_sub(60));
        while !tail.is_empty() && count(&tail.join(" ")) > PROMPT_TOKENS / 2 {
            tail.remove(0);
        }
        let tail = tail.join(" ");
        let budget = PROMPT_TOKENS - if tail.is_empty() { 0 } else { count(&tail) + 1 };
        let terms = if req.hints.terms.is_empty() { String::new() } else { crate::dictionary::whisper_prompt(req.hints, lang, budget, &count) };
        let prompt = [terms.as_str(), tail.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ").replace('\0', "");
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some(lang.unwrap_or("auto")));
        p.set_no_context(true);
        p.set_suppress_nst(true);
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        // SUSURRO_WHISPER_THREADS: a cap for benchmarks on a shared Mac.
        let threads = std::env::var("SUSURRO_WHISPER_THREADS").ok().and_then(|v| v.parse().ok());
        p.set_n_threads(threads.unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get().saturating_sub(2).max(1)).unwrap_or(4)) as i32);
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
        Ok(SttOutput { text: text.trim().to_string(), language, note: self.note.clone(), fell_back: false })
    }
}

/// Streaming: one worker thread transcribes the chunks in order while the user still speaks (one context,
/// one chunk at a time, the previous chunk's text as the prompt). `finish` adds the tail and joins the texts.
pub struct Stream {
    tx: Option<mpsc::Sender<Vec<f32>>>,
    done: Option<std::thread::JoinHandle<anyhow::Result<SttOutput>>>,
    abort: Arc<AtomicBool>,
}

impl Stream {
    pub fn spawn(w: Whisper, language: Option<String>, hints: Hints) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let abort = Arc::new(AtomicBool::new(false));
        let stop = abort.clone();
        let done = std::thread::Builder::new()
            .name("whisper-stream".into())
            .spawn(move || {
                let (mut texts, mut lang) = (Vec::<String>::new(), None);
                for audio in rx {
                    if stop.load(Ordering::SeqCst) {
                        anyhow::bail!("aborted");
                    }
                    let t = Instant::now();
                    // "auto": the first chunk's language holds for the rest, like single-shot (whisper detects once).
                    let auto = language.as_deref().is_none_or(|l| l.is_empty() || l == "auto");
                    let req = SttRequest { audio: &audio, language: if auto { lang.as_deref().or(language.as_deref()) } else { language.as_deref() }, hints: &hints };
                    let o = w.run(&req, texts.last().map_or("", String::as_str))?;
                    log::info!("whisper stream: chunk {} ({} ms audio) in {} ms", texts.len() + 1, audio.len() / 16, t.elapsed().as_millis());
                    if !o.text.is_empty() {
                        lang = lang.or(o.language); // a noise-only chunk's language is a guess
                        texts.push(o.text);
                    }
                }
                Ok(SttOutput { text: texts.join(" "), language: lang, note: w.note.clone(), fell_back: false })
            })
            .expect("spawn whisper-stream");
        Self { tx: Some(tx), done: Some(done), abort }
    }

    pub fn push(&self, chunk: Vec<f32>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(chunk);
        }
    }

    /// Blocks until every chunk (and `tail`) is transcribed. Err = some chunk failed: run the clip single-shot.
    pub fn finish(mut self, tail: Option<Vec<f32>>) -> anyhow::Result<SttOutput> {
        if let Some(t) = tail {
            self.push(t);
        }
        self.tx = None;
        self.done.take().unwrap().join().map_err(|_| anyhow::anyhow!("whisper stream panicked"))?
    }
}

impl Drop for Stream {
    /// Cancel: the worker skips the queued chunks (the one on the GPU finishes).
    fn drop(&mut self) {
        self.abort.store(true, Ordering::SeqCst);
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
    fn a_missing_model_falls_back_to_the_best_downloaded_one() {
        let d = std::env::temp_dir().join(format!("susurro-usable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("models")).unwrap();
        let turbo = "ggml-large-v3-turbo-q5_0.bin";
        assert_eq!(super::usable(&d, turbo), (turbo.to_string(), None), "nothing downloaded: the load reports it");
        for f in ["ggml-base-q5_1.bin", "ggml-tiny-q5_1.bin"] {
            std::fs::write(d.join("models").join(f), b"x").unwrap();
        }
        let (f, note) = super::usable(&d, turbo);
        assert_eq!(f, "ggml-base-q5_1.bin", "best downloaded in catalog order");
        assert!(note.unwrap().contains("not downloaded - used ggml-base-q5_1.bin"));
        assert_eq!(super::usable(&d, "ggml-tiny-q5_1.bin"), ("ggml-tiny-q5_1.bin".to_string(), None), "a downloaded pick is kept");
        let _ = std::fs::remove_dir_all(d);
    }

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
