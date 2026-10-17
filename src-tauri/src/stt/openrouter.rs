//! OpenRouter transcription, JSON body (`.lo/openrouter-stt.md`): `POST {base}/audio/transcriptions` with
//! `input_audio` base64. Multipart has no `provider` field and OpenRouter ignores `prompt` there, so the
//! dictionary words reach the model only through `provider.options`, keyed by the endpoint tag.

use super::{SttOutput, SttRequest, Transcriber, http, send_json, timeout_for, url, wav_bytes};
use base64::Engine;
use serde_json::{Value, json};

/// Azure phrase list cap: 500 phrases (Microsoft Q&A answer,
/// https://learn.microsoft.com/en-us/answers/questions/2117653/few-questions-on-the-limitations-of-phrase-list-fe).
const MAX_PHRASES: usize = 500;

pub struct OpenRouter {
    /// Includes `/api/v1`.
    pub base: String,
    pub key: String,
    pub model: String,
    /// Cleanup is on: MAI drops fillers itself (transcribeStyle "clean").
    pub clean: bool,
}

/// `provider.options` for this model, or None. Unknown models get nothing: Azure-style passthrough turns
/// an unknown field into a provider error.
pub fn options_for(model: &str, terms: &[String], clean: bool) -> Option<Value> {
    let h = crate::dictionary::Hints { terms: terms.to_vec(), ..Default::default() };
    if model.starts_with("microsoft/mai-transcribe") {
        let mut azure = serde_json::Map::new();
        let phrases = crate::dictionary::keyterms(&h, MAX_PHRASES);
        if !phrases.is_empty() {
            azure.insert("phraseList".into(), json!({ "phrases": phrases }));
        }
        if clean {
            azure.insert("enhancedMode".into(), json!({ "modelOptions": { "transcribeStyle": "clean" } }));
        }
        return (!azure.is_empty()).then(|| json!({ "azure": azure }));
    }
    // ponytail: no ElevenLabs branch. `provider.options.elevenlabs.keyterms` gave no gain through OpenRouter
    // (2026-10-17, scribe-v2, en_rare.wav, 3 runs: 0/4 rare words, like no keyterms). Add it back when
    // a real run shows a gain; the direct ElevenLabs client keeps its keyterms.
    None
}

/// The JSON body. `language` is omitted for auto-detect.
pub fn body(model: &str, wav: &[u8], lang: Option<&str>, terms: &[String], clean: bool) -> Value {
    let mut b = json!({
        "model": model,
        "input_audio": { "data": base64::engine::general_purpose::STANDARD.encode(wav), "format": "wav" },
        "response_format": "json",
        "temperature": 0,
    });
    if let Some(l) = lang {
        b["language"] = json!(l);
    }
    if let Some(o) = options_for(model, terms, clean) {
        b["provider"] = json!({ "options": o });
    }
    b
}

/// JSON base64 past 25 MB works only on OpenAI and Groq models (.lo/openrouter-stt.md); about 586 s of WAV.
/// ponytail: 25,000,000 is the safe reading of "25 MB"; whether the real limit is MiB is not measured.
pub const MAX_B64: usize = 25_000_000;

/// True if the base64 of `wav_len` WAV bytes fits the JSON body for every model.
pub fn json_fits(wav_len: usize) -> bool {
    wav_len.div_ceil(3) * 4 <= MAX_B64
}

impl Transcriber for OpenRouter {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        let wav = wav_bytes(req.audio)?;
        let rb = http()
            .post(url(&self.base, "audio/transcriptions"))
            .bearer_auth(&self.key)
            .header("HTTP-Referer", "https://susurro.app")
            .header("X-Title", "susurro")
            .timeout(timeout_for(req.audio.len()));
        let rb = if json_fits(wav.len()) {
            rb.json(&body(&self.model, &wav, req.lang(), &req.hints.terms, self.clean))
        } else {
            // Multipart (25 MB, the 600 s max is 19.2 MB) has no `provider` field: this clip loses the phrase list.
            use reqwest::blocking::multipart::{Form, Part};
            let mut form = Form::new()
                .part("file", Part::bytes(wav).file_name("audio.wav").mime_str("audio/wav")?)
                .text("model", self.model.clone())
                .text("response_format", "json")
                .text("temperature", "0");
            if let Some(l) = req.lang() {
                form = form.text("language", l.to_string());
            }
            rb.multipart(form)
        };
        let v = send_json(rb, "OpenRouter")?;
        let text = v["text"].as_str().ok_or_else(|| anyhow::anyhow!("OpenRouter: no text in the response"))?;
        Ok(SttOutput { text: text.trim().to_string(), language: v["language"].as_str().map(str::to_string), note: None, fell_back: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::Hints;
    use std::sync::{Arc, Mutex};

    fn run(model: &str, clean: bool, terms: &[&str], lang: Option<&str>) -> Value {
        let mut srv = mockito::Server::new();
        let seen = Arc::new(Mutex::new(String::new()));
        let s = seen.clone();
        let m = srv
            .mock("POST", "/api/v1/audio/transcriptions")
            .match_header("authorization", "Bearer sk-or")
            .match_header("content-type", "application/json")
            .with_body_from_request(move |r| {
                *s.lock().unwrap() = r.utf8_lossy_body().unwrap().into_owned();
                br#"{"text":" Contaflow rocks ","usage":{"seconds":1,"cost":0.00003}}"#.to_vec()
            })
            .create();
        let c = OpenRouter { base: format!("{}/api/v1", srv.url()), key: "sk-or".into(), model: model.into(), clean };
        let hints = Hints { terms: terms.iter().map(|t| t.to_string()).collect(), ..Default::default() };
        let out = c.transcribe(&SttRequest { audio: &[0.0; 1600], language: lang, hints: &hints }).unwrap();
        m.assert();
        assert_eq!(out.text, "Contaflow rocks");
        let b: Value = serde_json::from_str(&seen.lock().unwrap()).unwrap();
        b
    }

    #[test]
    fn mai_gets_the_phrase_list_and_clean_style() {
        let b = run("microsoft/mai-transcribe-2", true, &["Contaflow", "Tauri"], Some("es"));
        assert_eq!(b["model"], "microsoft/mai-transcribe-2");
        assert_eq!(b["input_audio"]["format"], "wav");
        let wav = base64::engine::general_purpose::STANDARD.decode(b["input_audio"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(&wav[..4], b"RIFF", "base64 of the WAV itself, not a data URI");
        assert_eq!(b["language"], "es");
        assert_eq!(b["response_format"], "json");
        let azure = &b["provider"]["options"]["azure"];
        assert_eq!(azure["phraseList"]["phrases"], json!(["Contaflow", "Tauri"]));
        assert_eq!(azure["enhancedMode"]["modelOptions"]["transcribeStyle"], "clean");
    }

    #[test]
    fn mai_without_cleanup_or_words() {
        let b = run("microsoft/mai-transcribe-1.5", false, &["Contaflow"], None);
        assert!(b.get("language").is_none(), "auto: no language field");
        assert!(b["provider"]["options"]["azure"].get("enhancedMode").is_none(), "cleanup off keeps verbatim");
        let b = run("microsoft/mai-transcribe-2", false, &[], None);
        assert!(b.get("provider").is_none(), "nothing to send");
        let b = run("microsoft/mai-transcribe-2", true, &[], None);
        assert!(b["provider"]["options"]["azure"].get("phraseList").is_none());
        assert_eq!(b["provider"]["options"]["azure"]["enhancedMode"]["modelOptions"]["transcribeStyle"], "clean");
    }

    #[test]
    fn other_models_get_no_provider_options() {
        for m in ["elevenlabs/scribe-v2", "qwen/qwen3-asr-1.7b", "openai/whisper-large-v3", "x-ai/grok-stt-1.0", "mistralai/voxtral-mini-transcribe"] {
            let b = run(m, true, &["Contaflow"], Some("en"));
            assert!(b.get("provider").is_none(), "{m}");
            assert_eq!(b["language"], "en");
        }
    }

    #[test]
    fn phrase_cap() {
        let terms: Vec<String> = (0..700).map(|i| format!("word{i}")).collect();
        let o = options_for("microsoft/mai-transcribe-2", &terms, false).unwrap();
        assert_eq!(o["azure"]["phraseList"]["phrases"].as_array().unwrap().len(), MAX_PHRASES);
    }

    #[test]
    fn errors_map() {
        let mut srv = mockito::Server::new();
        let _m = srv.mock("POST", "/api/v1/audio/transcriptions").with_status(402).with_body(r#"{"error":{"code":402,"message":"Insufficient credits"}}"#).create();
        let c = OpenRouter { base: format!("{}/api/v1", srv.url()), key: "k".into(), model: "microsoft/mai-transcribe-2".into(), clean: false };
        let e = c.transcribe(&SttRequest { audio: &[0.0; 160], language: None, hints: &Hints::default() }).unwrap_err().to_string();
        assert!(e.contains("OpenRouter: out of credits"), "{e}");
    }
}
