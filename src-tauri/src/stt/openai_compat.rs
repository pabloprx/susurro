//! WP2: `POST {base}/audio/transcriptions` multipart. whisper-server, speaches and OpenRouter.

use super::{SttOutput, SttRequest, Transcriber, http, send_json, timeout_for, url, wav_bytes};
use reqwest::blocking::multipart::{Form, Part};

/// PLAN 2.4: the whisper prompt budget, counted as chars/4 (no tokenizer here).
const PROMPT_TOKENS: usize = 200;

pub struct OpenAiCompat {
    /// Includes `/v1` (PLAN 7).
    pub base: String,
    pub model: String,
    pub key: Option<String>,
    /// speaches: also send `hotwords`.
    pub hotwords: bool,
    /// For error messages: "local STT server" | "OpenRouter".
    pub label: &'static str,
}

impl Transcriber for OpenAiCompat {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        let lang = req.lang();
        let mut form = Form::new()
            .part("file", Part::bytes(wav_bytes(req.audio)?).file_name("audio.wav").mime_str("audio/wav")?)
            .text("model", self.model.clone())
            .text("response_format", "json")
            .text("temperature", "0");
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }
        if !req.hints.terms.is_empty() {
            let count = |s: &str| s.chars().count().div_ceil(4);
            form = form.text("prompt", crate::dictionary::whisper_prompt(req.hints, lang, PROMPT_TOKENS, &count));
            if self.hotwords {
                form = form.text("hotwords", req.hints.terms.join(", "));
            }
        }
        let mut rb = http().post(url(&self.base, "audio/transcriptions")).timeout(timeout_for(req.audio.len())).multipart(form);
        if let Some(k) = self.key.as_deref().filter(|k| !k.is_empty()) {
            rb = rb.bearer_auth(k);
        }
        let v = send_json(rb, self.label)?;
        let text = v["text"].as_str().ok_or_else(|| anyhow::anyhow!("{}: no text in the response", self.label))?;
        Ok(SttOutput { text: text.trim().to_string(), language: v["language"].as_str().map(str::to_string), note: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::Hints;
    use std::sync::{Arc, Mutex};

    fn client(base: String, key: Option<&str>, hotwords: bool) -> OpenAiCompat {
        OpenAiCompat { base, model: "whisper-1".into(), key: key.map(Into::into), hotwords, label: "OpenRouter" }
    }

    /// Mock that records the request body and returns `body`.
    fn capture(srv: &mut mockito::Server, path: &str, status: usize, body: &'static str) -> (mockito::Mock, Arc<Mutex<String>>) {
        let seen = Arc::new(Mutex::new(String::new()));
        let s = seen.clone();
        let m = srv
            .mock("POST", path)
            .with_status(status)
            .with_body_from_request(move |r| {
                *s.lock().unwrap() = r.utf8_lossy_body().unwrap().into_owned();
                body.as_bytes().to_vec()
            })
            .create();
        (m, seen)
    }

    fn field(name: &str) -> String {
        format!("name=\"{name}\"")
    }

    #[test]
    fn auto_language_no_hints_no_key() {
        let mut srv = mockito::Server::new();
        let (m, seen) = capture(&mut srv, "/v1/audio/transcriptions", 200, r#"{"text":" hello there "}"#);
        let m = m.match_header("authorization", mockito::Matcher::Missing).match_header("content-type", mockito::Matcher::Regex("^multipart/form-data".into()));
        let out = client(format!("{}/v1", srv.url()), None, false)
            .transcribe(&SttRequest { audio: &[0.0; 1600], language: Some("auto"), hints: &Hints::default() })
            .unwrap();
        m.assert();
        assert_eq!(out.text, "hello there");
        let b = seen.lock().unwrap().clone();
        for f in ["file", "model", "response_format", "temperature"] {
            assert!(b.contains(&field(f)), "missing {f}");
        }
        assert!(b.contains("filename=\"audio.wav\"") && b.contains("RIFF"));
        assert!(!b.contains(&field("language")), "language must be omitted for auto");
        assert!(!b.contains(&field("prompt")) && !b.contains(&field("hotwords")));
    }

    #[test]
    fn openrouter_path_key_language_prompt_hotwords() {
        let mut srv = mockito::Server::new();
        let (m, seen) = capture(&mut srv, "/api/v1/audio/transcriptions", 200, r#"{"text":"Contaflow rocks","usage":{"seconds":1}}"#);
        let m = m.match_header("authorization", "Bearer sk-or");
        let hints = Hints { terms: vec!["Contaflow".into(), "Tauri".into()], ..Default::default() };
        let out = client(format!("{}/api/v1/", srv.url()), Some("sk-or"), true)
            .transcribe(&SttRequest { audio: &[0.0; 1600], language: Some("es"), hints: &hints })
            .unwrap();
        m.assert();
        assert_eq!(out.text, "Contaflow rocks");
        let b = seen.lock().unwrap().clone();
        assert!(b.contains(&field("language")) && b.contains("\r\n\r\nes\r\n"));
        assert!(b.contains(&field("prompt")) && b.contains("Contaflow"));
        assert!(b.contains(&field("hotwords")) && b.contains("Contaflow, Tauri"));
    }

    #[test]
    fn http_errors_map() {
        let mut srv = mockito::Server::new();
        let req = SttRequest { audio: &[0.0; 160], language: None, hints: &Hints::default() };
        let (m, _) = capture(&mut srv, "/v1/audio/transcriptions", 401, r#"{"error":{"message":"No auth"}}"#);
        let e = client(format!("{}/v1", srv.url()), Some("bad"), false).transcribe(&req).unwrap_err().to_string();
        assert!(e.contains("OpenRouter: invalid or missing API key (HTTP 401)"), "{e}");
        m.remove();
        let (_m, _) = capture(&mut srv, "/v1/audio/transcriptions", 500, "upstream down");
        let e = client(format!("{}/v1", srv.url()), None, false).transcribe(&req).unwrap_err().to_string();
        assert!(e.contains("server error (HTTP 500): upstream down"), "{e}");
    }

    #[test]
    fn missing_text_is_an_error() {
        let mut srv = mockito::Server::new();
        let (_m, _) = capture(&mut srv, "/v1/audio/transcriptions", 200, r#"{"foo":1}"#);
        let req = SttRequest { audio: &[0.0; 160], language: None, hints: &Hints::default() };
        assert!(client(format!("{}/v1", srv.url()), None, false).transcribe(&req).is_err());
    }
}
