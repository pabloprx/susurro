//! WP2: ElevenLabs Scribe v2, `POST {base}/speech-to-text` multipart (PLAN 7).

use super::{SttOutput, SttRequest, Transcriber, http, pcm_s16le, send_json, timeout_for, url};
use crate::contract::ElevenOpts;
use reqwest::blocking::multipart::{Form, Part};

pub struct ElevenLabs {
    /// Includes `/v1`.
    pub base: String,
    pub key: String,
    pub opts: ElevenOpts,
}

impl Transcriber for ElevenLabs {
    fn transcribe(&self, req: &SttRequest) -> anyhow::Result<SttOutput> {
        let mut form = Form::new()
            .text("model_id", self.opts.model_id.clone())
            .part("file", Part::bytes(pcm_s16le(req.audio)).file_name("audio.pcm").mime_str("application/octet-stream")?)
            .text("file_format", "pcm_s16le_16")
            .text("tag_audio_events", "false")
            .text("timestamps_granularity", "none");
        if let Some(l) = req.lang() {
            form = form.text("language_code", l.to_string());
        }
        // Keyterms cost +20% (the UI says so next to the toggle). Sanitized and capped by the dictionary.
        if self.opts.send_keyterms && !req.hints.terms.is_empty() {
            for k in crate::dictionary::keyterms(req.hints, self.opts.max_keyterms as usize) {
                form = form.text("keyterms", k);
            }
        }
        let rb = http()
            .post(url(&self.base, "speech-to-text"))
            .header("xi-api-key", &self.key)
            .timeout(timeout_for(req.audio.len()))
            .multipart(form);
        let v = send_json(rb, "ElevenLabs")?;
        let text = v["text"].as_str().ok_or_else(|| anyhow::anyhow!("ElevenLabs: no text in the response"))?;
        Ok(SttOutput { text: text.trim().to_string(), language: v["language_code"].as_str().map(str::to_string), note: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::Hints;
    use std::sync::{Arc, Mutex};

    fn run(srv: &mut mockito::Server, status: usize, resp: &'static str, send_keyterms: bool, hints: &Hints, lang: Option<&str>) -> (anyhow::Result<SttOutput>, String) {
        let seen = Arc::new(Mutex::new(String::new()));
        let s = seen.clone();
        let m = srv
            .mock("POST", "/v1/speech-to-text")
            .match_header("xi-api-key", "xi-key")
            .match_header("authorization", mockito::Matcher::Missing)
            .with_status(status)
            .with_body_from_request(move |r| {
                *s.lock().unwrap() = r.utf8_lossy_body().unwrap().into_owned();
                resp.as_bytes().to_vec()
            })
            .create();
        let c = ElevenLabs {
            base: format!("{}/v1", srv.url()),
            key: "xi-key".into(),
            opts: ElevenOpts { model_id: "scribe_v2".into(), send_keyterms, max_keyterms: 100 },
        };
        let r = c.transcribe(&SttRequest { audio: &[0.5; 1600], language: lang, hints });
        m.assert();
        m.remove();
        let b = seen.lock().unwrap().clone();
        (r, b)
    }

    #[test]
    fn request_fields_and_parse() {
        let mut srv = mockito::Server::new();
        let (r, b) = run(&mut srv, 200, r#"{"language_code":"eng","language_probability":0.98,"text":"Hello world."}"#, false, &Hints::default(), None);
        let out = r.unwrap();
        assert_eq!(out.text, "Hello world.");
        assert_eq!(out.language.as_deref(), Some("eng"));
        for (f, v) in [("model_id", "scribe_v2"), ("file_format", "pcm_s16le_16"), ("tag_audio_events", "false"), ("timestamps_granularity", "none")] {
            assert!(b.contains(&format!("name=\"{f}\"\r\n\r\n{v}\r\n")), "missing {f}={v}");
        }
        assert!(b.contains("name=\"file\"; filename=\"audio.pcm\""));
        assert!(!b.contains("name=\"language_code\""), "language_code must be omitted for auto");
        assert!(!b.contains("name=\"keyterms\""));
    }

    #[test]
    fn keyterms_repeated_and_language() {
        let mut srv = mockito::Server::new();
        let hints = Hints { terms: vec!["Contaflow".into(), "Tauri".into(), "Pablo Pérez".into()], ..Default::default() };
        let (r, b) = run(&mut srv, 200, r#"{"text":"ok"}"#, true, &hints, Some("es"));
        r.unwrap();
        assert_eq!(b.matches("name=\"keyterms\"").count(), 3);
        assert!(b.contains("name=\"language_code\"\r\n\r\nes\r\n"));
        // Off -> none sent even with terms.
        let (_, b) = run(&mut srv, 200, r#"{"text":"ok"}"#, false, &hints, None);
        assert_eq!(b.matches("name=\"keyterms\"").count(), 0);
    }

    #[test]
    fn errors_map() {
        let mut srv = mockito::Server::new();
        let (r, _) = run(&mut srv, 401, r#"{"detail":{"status":"invalid_api_key"}}"#, false, &Hints::default(), None);
        assert!(r.unwrap_err().to_string().contains("ElevenLabs: invalid or missing API key (HTTP 401)"));
        let (r, _) = run(&mut srv, 503, "busy", false, &Hints::default(), None);
        assert!(r.unwrap_err().to_string().contains("ElevenLabs server error (HTTP 503): busy"));
    }
}
