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
        // English-only models: auto-detect is wasted work and reports a wrong language.
        let lang = if self.ctx.is_multilingual() { req.lang() } else { Some("en") };
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
        let text: String = st
            .as_iter()
            .filter(|s| s.no_speech_probability() <= NO_SPEECH_MAX)
            .map(|s| s.to_str_lossy().map(|c| c.into_owned()).unwrap_or_default())
            .collect();
        let language = whisper_rs::get_lang_str(st.full_lang_id_from_state()).map(str::to_string);
        Ok(SttOutput { text: text.trim().to_string(), language, note: None })
    }
}
