//! WP2: one Transcriber trait, three impls, four providers (PLAN 2.4, 7).
//! Shared here: the HTTP client, error mapping, the scaled timeout and the audio encoders.

pub mod elevenlabs;
pub mod openai_compat;
pub mod whisper_local;

use crate::AppState;
use crate::contract::{KeyName, OrModel, ProviderTest, Settings, SttProvider};
use crate::dictionary::Hints;
use crate::secrets;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
pub const ELEVENLABS_BASE: &str = "https://api.elevenlabs.io/v1";
const JFK_WAV: &[u8] = include_bytes!("../../resources/jfk.wav");

pub struct SttRequest<'a> {
    pub audio: &'a [f32],
    /// None (or "auto") = auto-detect. Otherwise ISO-639-1.
    pub language: Option<&'a str>,
    pub hints: &'a Hints,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SttOutput {
    pub text: String,
    pub language: Option<String>,
    /// Set when the cloud STT failed and local whisper produced the text. The toast shows it.
    pub note: Option<String>,
    /// Local whisper produced the text after the cloud STT failed (the History provider label).
    pub fell_back: bool,
}

pub trait Transcriber {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput>;
}

impl SttRequest<'_> {
    pub(crate) fn lang(&self) -> Option<&str> {
        self.language.filter(|l| !l.is_empty() && *l != "auto")
    }
}

/// Builds the configured provider. Cloud/server providers get a local whisper fallback when
/// `local_fallback` is on and the whisper context is already loaded (the preload keeps it warm).
pub fn build(s: &Settings, state: &AppState) -> anyhow::Result<Box<dyn Transcriber>> {
    let st = &s.stt;
    let primary: Box<dyn Transcriber> = match st.provider {
        SttProvider::WhisperLocal => {
            return Ok(Box::new(whisper_local::Whisper::new(whisper_local::context(state, &st.whisper_model)?)));
        }
        SttProvider::OpenAiCompat => Box::new(openai_compat::OpenAiCompat {
            base: st.openai_compat.base_url.clone(),
            model: st.openai_compat.model.clone(),
            key: secrets::get(KeyName::LocalStt),
            hotwords: st.openai_compat.send_hotwords,
            label: "local STT server",
        }),
        SttProvider::OpenRouter => {
            anyhow::ensure!(!st.openrouter_model.is_empty(), "Pick an OpenRouter transcription model first");
            Box::new(openai_compat::OpenAiCompat {
                base: OPENROUTER_BASE.into(),
                model: st.openrouter_model.clone(),
                key: Some(secrets::get(KeyName::Openrouter).ok_or_else(|| anyhow::anyhow!("Add an OpenRouter API key first"))?),
                hotwords: false,
                label: "OpenRouter",
            })
        }
        SttProvider::ElevenLabs => Box::new(elevenlabs::ElevenLabs {
            base: ELEVENLABS_BASE.into(),
            key: secrets::get(KeyName::Elevenlabs).ok_or_else(|| anyhow::anyhow!("Add an ElevenLabs API key first"))?,
            opts: st.elevenlabs.clone(),
        }),
    };
    if !st.local_fallback {
        return Ok(primary);
    }
    // ponytail: only an already-loaded context is used (try_lock, no wait on a running preload).
    let local = state.whisper.try_lock().ok().and_then(|g| g.as_ref().filter(|(f, _)| *f == st.whisper_model).map(|(_, c)| c.clone()));
    Ok(match local {
        Some(ctx) => Box::new(WithFallback { primary, local: whisper_local::Whisper::new(ctx) }),
        None => primary,
    })
}

struct WithFallback {
    primary: Box<dyn Transcriber>,
    local: whisper_local::Whisper,
}

impl Transcriber for WithFallback {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        match self.primary.transcribe(req) {
            Ok(o) => Ok(o),
            Err(e) => {
                log::warn!("cloud STT failed, trying local whisper: {e:#}");
                let mut o = self.local.transcribe(req).map_err(|le| anyhow::anyhow!("{e:#}; local fallback also failed: {le:#}"))?;
                o.note = Some(format!("Used local whisper: {e:#}"));
                o.fell_back = true;
                Ok(o)
            }
        }
    }
}

/// PLAN 7: clamp(30 s + clip_s / 20, 30 s, 60 s). 60 s is OpenRouter's upstream limit.
pub fn timeout_for(samples: usize) -> Duration {
    let clip_s = samples as f64 / 16000.0;
    Duration::from_secs_f64((30.0 + clip_s / 20.0).clamp(30.0, 60.0))
}

pub(crate) fn http() -> &'static reqwest::blocking::Client {
    static C: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    C.get_or_init(reqwest::blocking::Client::new)
}

/// Sends the request and returns the JSON body. Maps transport and HTTP errors to clear messages.
pub(crate) fn send_json(rb: reqwest::blocking::RequestBuilder, what: &str) -> anyhow::Result<serde_json::Value> {
    let resp = rb.send().map_err(|e| {
        if e.is_timeout() {
            anyhow::anyhow!("{what} timed out")
        } else if e.is_connect() {
            anyhow::anyhow!("Cannot connect to {what}. Is it running and reachable?")
        } else {
            anyhow::anyhow!("{what} request failed: {e}")
        }
    })?;
    let code = resp.status().as_u16();
    let body = resp.text().map_err(|e| anyhow::anyhow!("{what}: reading the response failed: {e}"))?;
    let snippet: String = body.chars().take(300).collect();
    // OpenRouter 402/403-budget and ElevenLabs 401-quota_exceeded: the key is fine, the account is not.
    let out_of_credits = code == 402 || {
        let l = body.to_lowercase();
        ["quota", "budget", "credit"].iter().any(|w| l.contains(w))
    };
    match code {
        200..=299 => serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("{what} returned invalid JSON: {snippet}")),
        401..=403 if out_of_credits => anyhow::bail!("{what}: out of credits or over quota (HTTP {code}): {snippet}"),
        401 | 403 => anyhow::bail!("{what}: invalid or missing API key (HTTP {code})"),
        429 => anyhow::bail!("{what}: rate limited or out of credits (HTTP 429)"),
        500.. => anyhow::bail!("{what} server error (HTTP {code}): {snippet}"),
        _ => anyhow::bail!("{what} rejected the request (HTTP {code}): {snippet}"),
    }
}

pub(crate) fn url(base: &str, path: &str) -> String {
    format!("{}/{path}", base.trim_end_matches('/'))
}

fn to_i16(audio: &[f32]) -> impl Iterator<Item = i16> + '_ {
    audio.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
}

/// Raw s16le, the ElevenLabs `pcm_s16le_16` format.
pub(crate) fn pcm_s16le(audio: &[f32]) -> Vec<u8> {
    to_i16(audio).flat_map(i16::to_le_bytes).collect()
}

/// 16 kHz mono s16 WAV in memory.
pub(crate) fn wav_bytes(audio: &[f32]) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut cur = std::io::Cursor::new(Vec::with_capacity(44 + audio.len() * 2));
    let mut w = hound::WavWriter::new(&mut cur, spec)?;
    for s in to_i16(audio) {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(cur.into_inner())
}

/// The bundled jfk.wav (11 s of English speech), for the provider test and the whisper tests.
pub fn jfk() -> Vec<f32> {
    crate::history::decode_wav(JFK_WAV).expect("bundled jfk.wav")
}

fn run_test(app: &AppHandle, which: &str) -> anyhow::Result<String> {
    let state = app.state::<AppState>();
    let mut s = state.settings.read().unwrap().clone();
    match which {
        "stt" => {
            s.stt.local_fallback = false; // test the provider itself
            let t = build(&s, &state)?;
            let lang = Some(s.language.as_str());
            Ok(t.transcribe(&SttRequest { audio: &jfk(), language: lang, hints: &Hints::default() })?.text)
        }
        "cleanup" => {
            let key = secrets::get(crate::cleanup::key_name(&s));
            anyhow::ensure!(crate::cleanup::configured(&s, key.as_deref()), "{}", crate::cleanup::NOT_CONFIGURED);
            if s.cleanup.level == crate::contract::CleanupLevel::None {
                s.cleanup.level = crate::contract::CleanupLevel::Light;
            }
            crate::cleanup::cleanup(&s, key.as_deref(), &s.cleanup.level, "um so I think we should uh meet tomorrow at at three", &Hints::default())
        }
        _ => anyhow::bail!("unknown provider kind {which:?}"),
    }
}

/// which: "stt" | "cleanup". STT runs the bundled jfk.wav.
#[tauri::command]
pub async fn test_provider(app: AppHandle, which: String) -> Result<ProviderTest, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let t = Instant::now();
        let r = run_test(&app, &which);
        let latency_ms = t.elapsed().as_millis() as u32;
        match r {
            Ok(text) => ProviderTest { ok: true, latency_ms, text, error: None },
            Err(e) => ProviderTest { ok: false, latency_ms, text: String::new(), error: Some(format!("{e:#}")) },
        }
    })
    .await
    .map_err(|e| e.to_string())
}

pub(crate) fn list_models(base: &str, kind: &str, key: Option<&str>) -> anyhow::Result<Vec<OrModel>> {
    let path = match kind {
        "stt" => "models?output_modalities=transcription",
        "chat" => "models",
        _ => anyhow::bail!("unknown model kind {kind:?}"),
    };
    let mut rb = http().get(url(base, path)).timeout(Duration::from_secs(20));
    if let Some(k) = key {
        rb = rb.bearer_auth(k);
    }
    let v = send_json(rb, "OpenRouter")?;
    let data = v["data"].as_array().ok_or_else(|| anyhow::anyhow!("OpenRouter: no model list in the response"))?;
    let mut out: Vec<OrModel> = data
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let name = m["name"].as_str().unwrap_or(&id).to_string();
            Some(OrModel { id, name })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// kind: "stt" | "chat". The model list is public; the key is sent when present.
#[tauri::command]
pub async fn openrouter_models(kind: String) -> Result<Vec<OrModel>, String> {
    tauri::async_runtime::spawn_blocking(move || list_models(OPENROUTER_BASE, &kind, secrets::get(KeyName::Openrouter).as_deref()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_scales_with_clip() {
        assert_eq!(timeout_for(0), Duration::from_secs(30));
        assert_eq!(timeout_for(16000 * 200), Duration::from_secs(40));
        assert_eq!(timeout_for(16000 * 600), Duration::from_secs(60));
        assert_eq!(timeout_for(16000 * 3600), Duration::from_secs(60));
    }

    #[test]
    fn encoders() {
        let a = [0.0f32, 1.0, -1.0, 2.0];
        assert_eq!(pcm_s16le(&a), vec![0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x7f]);
        let w = wav_bytes(&a).unwrap();
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(jfk().len(), 176000); // 11 s at 16 kHz
    }

    #[test]
    fn url_join_has_no_double_slash() {
        assert_eq!(url("http://x/v1/", "audio/transcriptions"), "http://x/v1/audio/transcriptions");
        assert_eq!(url("http://x/api/v1", "chat/completions"), "http://x/api/v1/chat/completions");
    }

    #[test]
    fn http_errors_are_clear() {
        let mut srv = mockito::Server::new();
        for (code, want) in [(401, "invalid or missing API key (HTTP 401)"), (500, "server error (HTTP 500): boom"), (429, "rate limited"), (400, "rejected the request (HTTP 400)")] {
            let m = srv.mock("GET", "/x").with_status(code).with_body("boom").create();
            let e = send_json(http().get(format!("{}/x", srv.url())), "Prov").unwrap_err().to_string();
            assert!(e.contains(want), "{code}: {e}");
            m.remove();
        }
        let e = send_json(http().get("http://127.0.0.1:9/x"), "Prov").unwrap_err().to_string();
        assert!(e.contains("Cannot connect to Prov"), "{e}");
    }

    #[test]
    fn openrouter_model_lists() {
        let mut srv = mockito::Server::new();
        let stt = srv
            .mock("GET", "/api/v1/models")
            .match_query(mockito::Matcher::UrlEncoded("output_modalities".into(), "transcription".into()))
            .match_header("authorization", "Bearer k")
            .with_body(r#"{"data":[{"id":"openai/whisper-1","name":"Whisper"},{"id":"elevenlabs/scribe-v2","name":"ElevenLabs: Scribe v2"}]}"#)
            .create();
        let base = format!("{}/api/v1", srv.url());
        let m = list_models(&base, "stt", Some("k")).unwrap();
        stt.assert();
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["elevenlabs/scribe-v2", "openai/whisper-1"]);

        let chat = srv.mock("GET", "/api/v1/models").match_query(mockito::Matcher::Missing).with_body(r#"{"data":[{"id":"a/b"}]}"#).create();
        let m = list_models(&base, "chat", None).unwrap();
        chat.assert();
        assert_eq!(m, vec![OrModel { id: "a/b".into(), name: "a/b".into() }]);
        assert!(list_models(&base, "tts", None).is_err());
    }

    // ---------- adversarial (2026-10-17 review) ----------

    #[test]
    fn credit_and_quota_errors_are_not_called_bad_keys() {
        let mut srv = mockito::Server::new();
        // OpenRouter: 402 = insufficient credits; 403 with a budget body = guardrail budget.
        // ElevenLabs: 401 with detail.status = quota_exceeded.
        let cases = [
            (402, r#"{"error":{"code":402,"message":"Insufficient credits"}}"#),
            (403, r#"{"error":{"code":403,"message":"Budget limit exceeded (weekly limit)"}}"#),
            (401, r#"{"detail":{"status":"quota_exceeded","message":"This request exceeds your quota"}}"#),
        ];
        for (code, body) in cases {
            let m = srv.mock("GET", "/x").with_status(code).with_body(body).create();
            let e = send_json(http().get(format!("{}/x", srv.url())), "Prov").unwrap_err().to_string();
            assert!(!e.contains("invalid or missing API key"), "{code}: {e}");
            assert!(e.to_lowercase().contains("credit") || e.contains("quota") || e.contains("Budget"), "{code}: {e}");
            m.remove();
        }
    }
}
