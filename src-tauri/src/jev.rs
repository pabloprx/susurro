//! TypeSafe Jev through OpenRouter's Decisions API (`.lo/jev-research.md`): typed questions in, typed answers out.
//! `POST https://openrouter.ai/api/alpha/decisions`, Bearer = the OpenRouter key. Four question types: noul,
//! choice, score, box. Jev returns no text. One retry on 429 / 529 (the docs say: back off and retry).

use crate::stt::{http, send_json};
use base64::Engine;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
const TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub answers: Value,
    /// usage.cost, USD.
    pub cost: f64,
    pub latency_ms: u32,
}

pub fn ask(url: &str, key: &str, model: &str, state: &Value, questions: &Value) -> anyhow::Result<Reply> {
    let body = json!({ "model": model, "state": state, "questions": questions });
    let t = Instant::now();
    let send = || {
        let rb = http()
            .post(url)
            .bearer_auth(key)
            .header("HTTP-Referer", "https://susurro.app")
            .header("X-Title", "susurro")
            .timeout(TIMEOUT)
            .json(&body);
        send_json(rb, "Jev")
    };
    let v = match send() {
        Err(e) if retryable(&e.to_string()) => {
            std::thread::sleep(BACKOFF);
            send()?
        }
        r => r?,
    };
    anyhow::ensure!(v["answers"].is_object(), "Jev: no answers in the response");
    Ok(Reply { cost: v["usage"]["cost"].as_f64().unwrap_or(0.0), answers: v["answers"].clone(), latency_ms: t.elapsed().as_millis() as u32 })
}

/// send_json's text for 429 and 529 (overloaded).
fn retryable(e: &str) -> bool {
    e.contains("(HTTP 429)") || e.contains("(HTTP 529)")
}

/// A Jev image part: only PNG and JPEG data URLs are accepted.
pub fn image(bytes: &[u8], mime: &str) -> Value {
    json!({ "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)) } })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub choice: String,
    pub confidence: f32,
    /// Every option with its probability, highest first.
    pub ranked: Vec<(String, f32)>,
}

pub fn choice(answers: &Value, id: &str) -> Option<Choice> {
    let a = &answers[id];
    let choice = a["choice"].as_str()?.to_string();
    let mut ranked: Vec<(String, f32)> =
        a["probabilities"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0) as f32)).collect()).unwrap_or_default();
    ranked.sort_by(|x, y| y.1.total_cmp(&x.1));
    let confidence = a["confidence"].as_f64().map(|c| c as f32).or_else(|| ranked.iter().find(|r| r.0 == choice).map(|r| r.1)).unwrap_or(0.0);
    Some(Choice { choice, confidence, ranked })
}

pub fn noul(answers: &Value, id: &str) -> Option<f32> {
    answers[id]["noul"].as_f64().map(|v| v as f32)
}

/// Box edges, fractions 0..1 of the image from its top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

pub fn bbox(answers: &Value, id: &str) -> Option<Bbox> {
    let b = &answers[id]["box"];
    Some(Bbox { top: b["top"].as_f64()?, bottom: b["bottom"].as_f64()?, left: b["left"].as_f64()?, right: b["right"].as_f64()? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const OK: &str = r#"{"model":"typesafe/jev-1.14-20261014","answers":{
        "target":{"type":"choice","choice":"12","probabilities":{"3":0.03,"12":0.95,"7":0.02},"confidence":0.93},
        "risky":{"type":"noul","noul":0.04},
        "where":{"type":"box","box":{"top":0.66,"bottom":0.87,"left":0.43,"right":0.46},"confidence":0.48}},
        "usage":{"input_tokens":3105,"output_tokens":91,"cost":0.00013041},"id":"gen-dec-1","provider":"TypeSafe"}"#;

    #[test]
    fn request_shape_and_answers() {
        let mut srv = mockito::Server::new();
        let seen = Arc::new(Mutex::new(Value::Null));
        let s = seen.clone();
        let m = srv
            .mock("POST", "/api/alpha/decisions")
            .match_header("authorization", "Bearer sk-or")
            .match_header("content-type", "application/json")
            .with_body_from_request(move |r| {
                *s.lock().unwrap() = serde_json::from_slice(r.body().unwrap()).unwrap();
                OK.as_bytes().to_vec()
            })
            .create();
        let state = json!(["Frontmost app: Slack.", image(b"\x89PNG", "image/png")]);
        let q = json!({ "risky": { "type": "noul", "instructions": "Would it send a message?" } });
        let r = ask(&format!("{}/api/alpha/decisions", srv.url()), "sk-or", "typesafe/jev-1.14", &state, &q).unwrap();
        m.assert();
        let b = seen.lock().unwrap().clone();
        assert_eq!(b["model"], "typesafe/jev-1.14", "pinned, never ~latest");
        assert_eq!(b["questions"]["risky"]["type"], "noul");
        assert!(b["state"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,iVBORw=="));
        assert_eq!(r.cost, 0.00013041);
        let c = choice(&r.answers, "target").unwrap();
        assert_eq!((c.choice.as_str(), c.confidence), ("12", 0.93));
        assert_eq!(c.ranked.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["12", "3", "7"]);
        assert_eq!(noul(&r.answers, "risky"), Some(0.04));
        assert_eq!(bbox(&r.answers, "where"), Some(Bbox { top: 0.66, bottom: 0.87, left: 0.43, right: 0.46 }));
        assert_eq!(choice(&r.answers, "missing"), None);
    }

    #[test]
    fn retries_once_on_429_and_529() {
        let mut srv = mockito::Server::new();
        let url = format!("{}/d", srv.url());
        let busy = srv.mock("POST", "/d").with_status(429).with_body(r#"{"error":{"code":429,"message":"Rate limit exceeded"}}"#).expect(1).create();
        let ok = srv.mock("POST", "/d").with_body(OK).expect(1).create();
        assert!(ask(&url, "k", "m", &json!("s"), &json!({})).is_ok());
        busy.assert();
        ok.assert();
        busy.remove();
        ok.remove();
        let over = srv.mock("POST", "/d").with_status(529).with_body("overloaded").expect(2).create();
        let e = ask(&url, "k", "m", &json!("s"), &json!({})).unwrap_err().to_string();
        over.assert();
        assert!(e.contains("Jev server error (HTTP 529)"), "{e}");
    }

    #[test]
    fn errors_are_clear_and_not_retried() {
        let mut srv = mockito::Server::new();
        let url = format!("{}/d", srv.url());
        for (code, want) in [(401, "Jev: invalid or missing API key"), (402, "Jev: out of credits"), (413, "Jev rejected the request (HTTP 413)")] {
            let m = srv.mock("POST", "/d").with_status(code).with_body(r#"{"error":{"message":"x"}}"#).expect(1).create();
            let e = ask(&url, "k", "m", &json!("s"), &json!({})).unwrap_err().to_string();
            m.assert();
            assert!(e.contains(want), "{code}: {e}");
            m.remove();
        }
        let m = srv.mock("POST", "/d").with_body(r#"{"usage":{}}"#).create();
        assert!(ask(&url, "k", "m", &json!("s"), &json!({})).unwrap_err().to_string().contains("no answers"));
        m.remove();
    }
}
