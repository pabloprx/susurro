//! The screen-context card (Pablo 2026-10-19: "when I press the button, show a quick overlay on the right side: which
//! application, all the context he has of what's on the screen, so I can say 'send this message to Juanito on
//! Telegram'"). F11 down in agent mode: the front app, its window, the focused field and the main items (AX, at once,
//! live.rs `show_glance`), then one or two sentences on what the screen shows (a vision model over OpenRouter, only
//! with a key). Marks.tsx draws it on the right; it goes as soon as an order runs, so the agent's screenshots never
//! show it.

use crate::ax::{El, Field};
use crate::contract::Glance;
use serde_json::json;
use std::time::Duration;

/// Items on the card.
pub const ITEMS: usize = 6;
/// The summary request's timeout: later than this, Pablo has already spoken.
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(8);
/// The screenshot for the summary: the long side in pixels.
pub const SUMMARY_PX: f64 = 1024.0;

/// What AX tells at once.
pub fn local(app: &str, title: &str, f: &Field, els: &[El]) -> Glance {
    let texty = crate::ax::takes_text(&f.role) || ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"].contains(&f.role.as_str());
    let focus = match (f.name.trim(), texty) {
        ("", false) => String::new(),
        ("", true) => "a text field".into(),
        (n, true) => format!("{n} (text field)"),
        (n, false) => n.to_string(),
    };
    let draft = if f.secure { String::new() } else { f.value.as_deref().map(tail).unwrap_or_default() };
    Glance { app: app.into(), window: title.trim().into(), focus, draft, items: items(els, title), summary: None }
}

/// The last 80 characters of a draft.
fn tail(v: &str) -> String {
    let v = v.trim();
    let n = v.chars().count();
    if n <= 80 { v.into() } else { format!("…{}", v.chars().skip(n - 80).collect::<String>()) }
}

/// The names worth a line: the main region first, then the rest; named, 2-40 characters, no repeats, not the title.
pub fn items(els: &[El], title: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let main = els.iter().filter(|e| e.mark.region == "main");
    let rest = els.iter().filter(|e| e.mark.region != "main");
    for e in main.chain(rest) {
        let n = e.mark.name.split_whitespace().collect::<Vec<_>>().join(" ");
        let len = n.chars().count();
        if !(2..=40).contains(&len) || n.eq_ignore_ascii_case(title.trim()) || out.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            continue;
        }
        out.push(n);
        if out.len() == ITEMS {
            break;
        }
    }
    out
}

/// The request for the summary: the screenshot and what AX said, one or two short sentences back.
pub fn summary_request(model: &str, jpeg: &[u8], g: &Glance, spanish: bool) -> serde_json::Value {
    use base64::Engine;
    let lang = if spanish { "Spanish" } else { "English" };
    let ask = format!(
        "This is the user's screen; the front app is {} (window: \"{}\"). In one or two short sentences, at most 30 words, \
         in {lang}, say what it shows so the user can give a voice order about it: what is open (a chat and with whom, a \
         page and about what, a file), and anything waiting (a draft, an unread message, a dialog). No preamble.",
        g.app, g.window
    );
    let img = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg));
    json!({
        "model": model,
        "max_tokens": 120,
        "messages": [{ "role": "user", "content": [
            { "type": "text", "text": ask },
            { "type": "image_url", "image_url": { "url": img } }
        ] }]
    })
}

/// The summary from a vision model on OpenRouter. Err = no answer (the card keeps the AX lines).
pub fn summarize(base: &str, key: &str, model: &str, jpeg: &[u8], g: &Glance, spanish: bool) -> anyhow::Result<String> {
    use crate::stt::{http, send_json, url};
    let rb = http()
        .post(url(base, "chat/completions"))
        .bearer_auth(key)
        .header("HTTP-Referer", "https://susurro.app")
        .header("X-Title", "susurro")
        .timeout(SUMMARY_TIMEOUT)
        .json(&summary_request(model, jpeg, g, spanish));
    let v = send_json(rb, "Glance")?;
    let t = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
    anyhow::ensure!(!t.is_empty(), "Glance: an empty answer");
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Mark, PtRect};

    fn el(id: u32, name: &str, region: &str) -> El {
        let rect = PtRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        El { mark: Mark { id, rect, name: name.into(), role: "button".into(), region: region.into() }, state: None, h: 0, url: None, value: None }
    }

    #[test]
    fn telegram_card() {
        let els = [el(1, "Search", "toolbar"), el(2, "Juanito", "main"), el(3, "Mamá", "main"), el(4, "juanito", "sidebar"), el(5, "x", "main"), el(6, "Telegram", "main")];
        let f = Field { app: "Telegram".into(), role: "AXTextArea".into(), name: "Message".into(), value: Some("  nos vemos a las 8  ".into()), ..Default::default() };
        let g = local("Telegram", "Telegram", &f, &els);
        assert_eq!(g.items, ["Juanito", "Mamá", "Search"], "main first, no repeats, no one-letter names, not the title");
        assert_eq!((g.focus.as_str(), g.draft.as_str()), ("Message (text field)", "nos vemos a las 8"));
        let secret = Field { secure: true, value: Some("hunter2".into()), ..f.clone() };
        assert_eq!(local("Telegram", "", &secret, &els).draft, "");
        let long = Field { value: Some("a".repeat(100) + "fin"), ..f };
        let d = local("Telegram", "", &long, &els).draft;
        assert!(d.starts_with('…') && d.ends_with("fin") && d.chars().count() == 81, "{d}");
    }

    #[test]
    fn items_cap() {
        let els: Vec<El> = (0..20).map(|i| el(i, &format!("Chat {i}"), "main")).collect();
        assert_eq!(items(&els, "").len(), ITEMS);
    }

    #[test]
    fn summary_request_carries_the_image_and_the_language() {
        let g = Glance { app: "Telegram".into(), window: "Juanito".into(), ..Default::default() };
        let v = summary_request("google/gemini-3.5-flash-lite", b"\xff\xd8", &g, true);
        let c = &v["messages"][0]["content"];
        assert!(c[0]["text"].as_str().unwrap().contains("in Spanish") && c[0]["text"].as_str().unwrap().contains("Juanito"));
        assert!(c[1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn summarize_reads_the_answer() {
        let mut srv = mockito::Server::new();
        let m = srv.mock("POST", "/chat/completions").with_body(r#"{"choices":[{"message":{"content":" Chat con Juanito abierto. "}}]}"#).create();
        let g = Glance { app: "Telegram".into(), ..Default::default() };
        assert_eq!(summarize(&srv.url(), "k", "m", b"x", &g, true).unwrap(), "Chat con Juanito abierto.");
        m.assert();
    }
}
