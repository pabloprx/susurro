//! WP2: one Transcriber trait, three impls, four providers (PLAN 2.4, 7).
//! Shared here: the HTTP client, error mapping, the scaled timeout and the audio encoders.

pub mod elevenlabs;
pub mod openai_compat;
pub mod openrouter;
pub mod scribe_rt;
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
            let (file, note) = whisper_local::usable(&state.data_dir, &st.whisper_model);
            anyhow::ensure!(
                whisper_local::model_path(&state.data_dir, &file).exists(),
                "No Whisper model is downloaded yet ({file}). Wait for the download to finish, or download one in Providers"
            );
            let mut w = whisper_local::Whisper::new(whisper_local::context(state, &file)?);
            w.note = note;
            return Ok(Box::new(w));
        }
        SttProvider::OpenAiCompat => Box::new(openai_compat::OpenAiCompat {
            base: st.openai_compat.base_url.clone(),
            model: st.openai_compat.model.clone(),
            key: secrets::get(KeyName::LocalStt),
            hotwords: st.openai_compat.send_hotwords,
        }),
        SttProvider::OpenRouter => {
            anyhow::ensure!(!st.openrouter_model.is_empty(), "Pick an OpenRouter transcription model first");
            Box::new(openrouter::OpenRouter {
                base: OPENROUTER_BASE.into(),
                model: st.openrouter_model.clone(),
                key: secrets::get(KeyName::Openrouter).ok_or_else(|| anyhow::anyhow!("Add an OpenRouter API key first"))?,
                clean: st.mai_clean,
            })
        }
        SttProvider::ElevenLabs => Box::new(elevenlabs::ElevenLabs {
            base: ELEVENLABS_BASE.into(),
            key: secrets::get(KeyName::Elevenlabs).ok_or_else(|| anyhow::anyhow!("Add an ElevenLabs API key first"))?,
            opts: st.elevenlabs.clone(),
        }),
        // The live provider's batch path (Retry, the fallback after a failed socket): MAI, else Scribe v2 batch.
        // ponytail: OpenAI Realtime falls back like Scribe Realtime (MAI, else Scribe batch); no OpenAI batch path.
        SttProvider::ElevenLabsRealtime | SttProvider::OpenAiRealtime => {
            let mut b = s.clone();
            b.stt.provider = batch_for_realtime(secrets::get(KeyName::Openrouter).is_some() && !st.openrouter_model.is_empty());
            return build(&b, state);
        }
    };
    if !st.local_fallback {
        return Ok(primary);
    }
    // ponytail: only an already-loaded context is used (try_lock, no wait on a running preload).
    let (file, _) = whisper_local::usable(&state.data_dir, &st.whisper_model);
    let local = state.whisper.try_lock().ok().and_then(|g| g.as_ref().filter(|(f, _)| *f == file).map(|(_, c)| c.clone()));
    Ok(match local {
        Some(ctx) => Box::new(WithFallback { primary, local: whisper_local::Whisper::new(ctx) }),
        None => primary,
    })
}

/// The batch STT behind Scribe Realtime: MAI when the OpenRouter key exists, else Scribe v2 batch (same key).
pub fn batch_for_realtime(openrouter: bool) -> SttProvider {
    if openrouter { SttProvider::OpenRouter } else { SttProvider::ElevenLabs }
}

/// The STT of voice-control commands: the explicit choice, else (auto) Scribe Realtime when the ElevenLabs key is
/// saved, else the dictation provider.
pub fn command_provider(s: &Settings, elevenlabs_key: bool) -> SttProvider {
    match &s.command.stt {
        Some(p) => p.clone(),
        None if elevenlabs_key => SttProvider::ElevenLabsRealtime,
        None => s.stt.provider.clone(),
    }
}

/// The realtime session options of provider `p`, or None when `p` is not a realtime provider or its key is
/// missing. `keyterms`: dictionary words (commands always send them, vc-feedback 18a; dictation follows the Send
/// keyterms toggle).
pub fn realtime_opts(p: &SttProvider, s: &Settings, rate: u32, hints: &Hints, keyterms: bool) -> Option<scribe_rt::Opts> {
    let (proto, url, k) = match p {
        SttProvider::ElevenLabsRealtime => (scribe_rt::Proto::Scribe, scribe_rt::URL, KeyName::Elevenlabs),
        SttProvider::OpenAiRealtime => (scribe_rt::Proto::OpenAi, scribe_rt::OPENAI_URL, KeyName::Openai),
        _ => return None,
    };
    let key = secrets::get(k)?;
    let keyterms = if keyterms { crate::dictionary::keyterms(hints, 50) } else { vec![] };
    Some(scribe_rt::Opts { proto, url: url.into(), key, rate, language: Some(s.language.clone()), keyterms })
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

/// Waits before each retry of a 429 / 5xx (vc-feedback 16: never show the first rate limit). A Retry-After header wins.
#[cfg(not(test))]
const BACKOFF_MS: [u64; 3] = [300, 800, 2000];
#[cfg(test)]
const BACKOFF_MS: [u64; 3] = [1, 1, 1];
/// The longest Retry-After we honor; a longer one fails now (the user would wait for nothing).
const RETRY_AFTER_MAX_MS: u64 = 5000;

/// Sends the request and returns the JSON body. Maps transport and HTTP errors to clear messages.
/// A 429 or 5xx is retried silently (3 times, 300 ms / 800 ms / 2 s, or Retry-After). Streamed bodies (multipart)
/// cannot be cloned and get no retry.
pub(crate) fn send_json(mut rb: reqwest::blocking::RequestBuilder, what: &str) -> anyhow::Result<serde_json::Value> {
    for (i, wait) in BACKOFF_MS.iter().enumerate() {
        let Some(again) = rb.try_clone() else { break };
        match send_once(rb, what) {
            Err((e, Some(after))) => {
                let ms = after.unwrap_or(*wait);
                if ms > RETRY_AFTER_MAX_MS {
                    return Err(e);
                }
                log::info!("{what}: {e:#}; retry {} in {ms} ms", i + 1);
                std::thread::sleep(Duration::from_millis(ms));
                rb = again;
            }
            r => return r.map_err(|e| e.0),
        }
    }
    send_once(rb, what).map_err(|e| e.0)
}

/// Err.1 = Some(Retry-After ms) when the error is worth a retry (429, 5xx).
fn send_once(rb: reqwest::blocking::RequestBuilder, what: &str) -> Result<serde_json::Value, (anyhow::Error, Option<Option<u64>>)> {
    let resp = rb.send().map_err(|e| {
        if e.is_timeout() {
            anyhow::anyhow!("{what} timed out")
        } else if e.is_connect() || refused(&e) {
            anyhow::anyhow!("Cannot connect to {what}. Is it running and reachable?")
        } else {
            anyhow::anyhow!("{what} request failed: {e}")
        }
    }).map_err(|e| (e, None))?;
    let code = resp.status().as_u16();
    let after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()?.trim().parse::<f64>().ok()).map(|s| (s * 1000.0) as u64);
    let retry = (code == 429 || code >= 500).then_some(after);
    let body = resp.text().map_err(|e| (anyhow::anyhow!("{what}: reading the response failed: {e}"), retry))?;
    http_result(code, &body, what).map_err(|e| (e, retry))
}

fn http_result(code: u16, body: &str, what: &str) -> anyhow::Result<serde_json::Value> {
    let snippet: String = body.chars().take(300).collect();
    // OpenRouter 402/403-budget and ElevenLabs 401-quota_exceeded: the key is fine, the account is not.
    let out_of_credits = code == 402 || {
        let l = body.to_lowercase();
        ["quota", "budget", "credit"].iter().any(|w| l.contains(w))
    };
    match code {
        200..=299 => serde_json::from_str(body).map_err(|_| anyhow::anyhow!("{what} returned invalid JSON: {snippet}")),
        401..=403 if out_of_credits => anyhow::bail!("{what}: out of credits or over quota (HTTP {code}): {snippet}"),
        401 | 403 => match server_reason(body) {
            // "API key expired" vs a typo: pass the server's reason on.
            Some(r) => anyhow::bail!("{what}: invalid or missing API key (HTTP {code}): {r}"),
            None => anyhow::bail!("{what}: invalid or missing API key (HTTP {code})"),
        },
        429 => anyhow::bail!("{what}: rate limited or out of credits (HTTP 429)"),
        500.. => anyhow::bail!("{what} server error (HTTP {code}): {snippet}"),
        _ => anyhow::bail!("{what} rejected the request (HTTP {code}): {snippet}"),
    }
}

/// A streamed (multipart) body to a closed port fails as a body error: reqwest's blocking body
/// channel reports "Disconnected" and drops the connect error. Probe the socket to tell.
fn refused(e: &reqwest::Error) -> bool {
    use std::net::TcpStream;
    let Some(u) = e.url().filter(|_| e.is_body() || e.is_request()) else { return false };
    let Some(addr) = u.socket_addrs(|| None).ok().and_then(|a| a.into_iter().next()) else { return false };
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err()
}

/// `{"error":{"message":..}}` (OpenAI/OpenRouter), `{"detail":{"message":..}}` / `{"detail":".."}` (ElevenLabs).
fn server_reason(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let m = [&v["error"]["message"], &v["detail"]["message"], &v["detail"], &v["message"], &v["error"]]
        .into_iter()
        .find_map(|m| m.as_str())?;
    Some(m.chars().take(200).collect())
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
        "stt" if s.stt.provider.realtime() => {
            let o = realtime_opts(&s.stt.provider, &s, 16000, &Hints::default(), false).ok_or_else(|| anyhow::anyhow!("Add the provider's API key first"))?;
            Ok(scribe_rt::transcribe(o, &jfk(), false, Duration::from_secs(20))?.0)
        }
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
    let stt = kind == "stt";
    let mut out: Vec<OrModel> = data
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let name = m["name"].as_str().unwrap_or(&id).to_string();
            let p = |k: &str| m["pricing"][k].as_str().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let rank = RANKED.iter().position(|r| *r == id).map(|i| i as u32 + 1).filter(|_| stt);
            let price_hour = if stt { price_hour(&id, p("prompt"), p("completion")) } else { None };
            let keywords = stt && openrouter::options_for(&id, &["x".into()], false).is_some();
            let rec = if stt { crate::contract::DEFAULT_OPENROUTER_STT } else { crate::contract::DEFAULT_OPENROUTER_CLEANUP };
            Some(OrModel { recommended: id == rec, rank, price_hour, keywords, id, name })
        })
        .collect();
    // Ranked first in ranking order, then the rest by name.
    out.sort_by(|a, b| (a.rank.unwrap_or(u32::MAX), a.name.to_lowercase()).cmp(&(b.rank.unwrap_or(u32::MAX), b.name.to_lowercase())));
    Ok(out)
}

/// ponytail: hardcoded 2026-10-17 pick from the OpenRouter transcription ranking (Pablo's order, MAI first);
/// no JSON source exists (.lo/openrouter-stt.md). An id gone from the live list simply does not show.
pub const RANKED: [&str; 10] = [
    "microsoft/mai-transcribe-2",
    "openai/whisper-large-v3",
    "openai/whisper-large-v3-turbo",
    "elevenlabs/scribe-v2",
    "openai/gpt-4o-mini-transcribe",
    "openai/whisper-1",
    "openai/gpt-transcribe",
    "x-ai/grok-stt-1.0",
    "qwen/qwen3-asr-1.7b",
    "mistralai/voxtral-mini-transcribe",
];

/// USD per hour of audio from the models API pricing. `pricing.prompt` is per second for audio-priced
/// models, but MAI-Transcribe's is already per hour ("0.1" = $0.10/h, the real 11 s call cost $0.000306).
/// A completion price means per-token pricing: None.
fn price_hour(id: &str, prompt: f64, completion: f64) -> Option<f64> {
    if completion > 0.0 || prompt <= 0.0 {
        return None;
    }
    Some(if id.starts_with("microsoft/mai-transcribe") { prompt } else { prompt * 3600.0 })
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
        // A multipart POST to a closed port is a body error in reqwest, not a connect error.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let form = reqwest::blocking::multipart::Form::new()
            .part("file", reqwest::blocking::multipart::Part::bytes(vec![0u8; 64 * 1024]).file_name("a.wav"));
        let e = send_json(http().post(format!("http://127.0.0.1:{port}/x")).multipart(form), "Prov").unwrap_err().to_string();
        assert!(e.contains("Cannot connect to Prov"), "{e}");
        // The server's reason for a 401 is kept: an expired key is not a typo.
        let m = srv.mock("GET", "/x").with_status(401).with_body(r#"{"error":{"message":"API key expired","code":401}}"#).create();
        let e = send_json(http().get(format!("{}/x", srv.url())), "Prov").unwrap_err().to_string();
        assert_eq!(e, "Prov: invalid or missing API key (HTTP 401): API key expired");
        m.remove();
    }

    #[test]
    fn rate_limits_and_5xx_retry_silently_then_fail() {
        // vc-feedback 16: 429 / 5xx never reach the user on the first hit.
        let mut srv = mockito::Server::new();
        let busy = srv.mock("POST", "/x").with_status(429).with_header("retry-after", "0").expect(2).create();
        let ok = srv.mock("POST", "/x").with_body(r#"{"ok":1}"#).expect(1).create();
        let v = send_json(http().post(format!("{}/x", srv.url())).json(&serde_json::json!({"a": 1})), "Prov").unwrap();
        assert_eq!(v["ok"], 1);
        busy.assert();
        ok.assert();
        busy.remove();
        ok.remove();
        let down = srv.mock("POST", "/x").with_status(503).with_body("down").expect(4).create();
        let e = send_json(http().post(format!("{}/x", srv.url())).json(&serde_json::json!({})), "Prov").unwrap_err().to_string();
        down.assert();
        assert!(e.contains("server error (HTTP 503)"), "{e}");
        down.remove();
        // a 400 is the request's fault: no retry; a long Retry-After fails now
        let bad = srv.mock("POST", "/x").with_status(400).expect(1).create();
        assert!(send_json(http().post(format!("{}/x", srv.url())).json(&serde_json::json!({})), "Prov").is_err());
        bad.assert();
        bad.remove();
        let long = srv.mock("POST", "/x").with_status(429).with_header("retry-after", "60").expect(1).create();
        assert!(send_json(http().post(format!("{}/x", srv.url())).json(&serde_json::json!({})), "Prov").is_err());
        long.assert();
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
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["elevenlabs/scribe-v2", "openai/whisper-1"], "ranked order");

        let chat = srv.mock("GET", "/api/v1/models").match_query(mockito::Matcher::Missing).with_body(r#"{"data":[{"id":"a/b"}]}"#).create();
        let m = list_models(&base, "chat", None).unwrap();
        chat.assert();
        assert_eq!(m, vec![OrModel { id: "a/b".into(), name: "a/b".into(), rank: None, price_hour: None, keywords: false, recommended: false }]);
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

#[cfg(test)]
mod ranking_tests {
    use super::*;

    #[test]
    fn ranked_first_then_by_name_with_prices() {
        let mut srv = mockito::Server::new();
        let _m = srv
            .mock("GET", "/api/v1/models")
            .match_query(mockito::Matcher::Any)
            .with_body(
                r#"{"data":[
                {"id":"zeta/asr","name":"Zeta ASR","pricing":{"prompt":"0.0001","completion":"0"}},
                {"id":"assemblyai/universal-3-6-pro","name":"AssemblyAI: Universal-3.6 Pro","pricing":{"prompt":"0.0000833333","completion":"0"}},
                {"id":"openai/whisper-large-v3","name":"OpenAI: Whisper Large V3","pricing":{"prompt":"0.0000075","completion":"0"}},
                {"id":"openai/gpt-4o-mini-transcribe","name":"OpenAI: GPT-4o Mini Transcribe","pricing":{"prompt":"0.00000125","completion":"0.000005"}},
                {"id":"elevenlabs/scribe-v2","name":"ElevenLabs: Scribe v2","pricing":{"prompt":"0.00003055555555555","completion":"0"}},
                {"id":"microsoft/mai-transcribe-2","name":"Microsoft AI: MAI-Transcribe 2","pricing":{"prompt":"0.1","completion":"0"}}
            ]}"#,
            )
            .create();
        let m = list_models(&format!("{}/api/v1", srv.url()), "stt", None).unwrap();
        let ids: Vec<&str> = m.iter().map(|m| m.id.as_str()).collect();
        // whisper-large-v3-turbo etc. are missing from the live list: dropped, the order still holds.
        assert_eq!(ids, ["microsoft/mai-transcribe-2", "openai/whisper-large-v3", "elevenlabs/scribe-v2", "openai/gpt-4o-mini-transcribe", "assemblyai/universal-3-6-pro", "zeta/asr"]);
        assert_eq!(m.iter().map(|m| m.rank).collect::<Vec<_>>(), [Some(1), Some(2), Some(4), Some(5), None, None]);
        let ph = |i: usize| m[i].price_hour.map(|p| (p * 1000.0).round() / 1000.0);
        assert_eq!(ph(0), Some(0.1), "MAI prompt is already per hour");
        assert_eq!(ph(1), Some(0.027));
        assert_eq!(ph(2), Some(0.11));
        assert_eq!(ph(3), None, "token-priced");
        assert_eq!(ph(4), Some(0.3));
        assert!(m[0].recommended && !m[1].recommended);
        assert_eq!(m.iter().map(|m| m.keywords).collect::<Vec<_>>(), [true, false, false, false, false, false]);
    }
}
