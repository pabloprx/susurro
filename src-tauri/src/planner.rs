//! Voice control v2 planner (`.lo/VOICE_CONTROL_V2.md` 1.4, bench `.lo/planner-bench.md`): one OpenRouter chat call
//! per new chunk of speech, strict JSON schema out. The planner writes the actions (text, keys, target words); Jev
//! grounds the targets on fresh marks right before each action (command.rs), so a queued action never holds a mark id.

use crate::ax::Field;
use crate::stt::{OPENROUTER_BASE, http, send_json, url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The bench v2 prompt with click_mark{mark} -> click{target}, plus rest / answer / hints (vc-feedback 10, 18).
pub const PROMPT: &str = r#"You are the planner for susurro voice control on macOS. The user speaks orders; you turn them into UI actions.
Input (JSON): utterance (live speech so far, may be cut mid-order), final (true = the user stopped talking), memory (what susurro itself did earlier: typed text, copies, pastes, clicks), app (frontmost app), focused (role, name and current value of the focused field, read just now), marks (actionable elements on screen now, "name (role, region)"), hints (shortcuts of the frontmost app), pending (a question susurro asked the user, or null).
Output: strict JSON {"complete": bool, "rest": string, "answer": string|null, "say": string|null, "actions": [...]}.
- complete=false when the utterance ends in an unfinished order ("open", "type", "create a new tab and"); then actions holds only the finished leading orders and rest = the words of the unfinished order, verbatim. complete=true: rest="".
- final=true: the user stopped; plan what you can, complete=true, rest="".
- Ops (unused fields = null):
  open_app{app}: launch/focus app by its real name (fix STT spelling: "Ghosty" -> "Ghostty").
  key{keys}: one shortcut, lowercase, "+"-joined, modifiers cmd/opt/ctrl/shift: "cmd+t", "return", "cmd+a", "delete", "cmd+1", "opt+shift+left". Any combo is allowed.
  type{text, target}: insert text. text = ONLY the content to write, never the target words ("type hello onto the search input" -> text "hello", target = the search field). target=null types into the focused field. In terminals and code, commands are lowercase ("type LS" -> "ls"). Drop the period STT adds at the end.
  click{target}: press or focus an element.
  select_typed{}: select the last text susurro typed (from memory).
  delete_typed{}: delete the last text susurro typed (from memory), in one go.
  copy{target}: copy a link or the value of a field (target = that element); target=null copies the current selection.
  paste{target}: paste the clipboard; target = the field to paste into first, or null for the focused field.
  scroll{dir, target}: dir up|down. wait_user{}: do nothing, wait for more speech.
- target: copy the mark text exactly when the element is in marks ("Share (button, toolbar)"); else a few words naming it ("Public radio button"): it may appear only after an earlier action.
- type never presses return. Add key return only when the user says "run", "execute", "enter" or "X command" ("type ls" -> type only; "run ls" -> type + return).
- "delete all the writing" = key cmd+a then key delete. "remove what you typed" = one delete_typed, never several key delete.
- memory is only what susurro did. The screen (focused value) is the truth: if memory says susurro typed something that is no longer in the focused value, do not delete; return actions=[] and say why.
- "search X" in a browser = key cmd+l, type X, key return. Use the app hints.
- pending: if the utterance answers it ("yes", "sí", "no", "the second one", "7"), set answer to "yes", "no" or the number and actions=[].
- risky=true when the action itself sends a message, posts, deletes data or spends money.
- Noise or filler ("voila", "um", "thank you") = complete=true, actions=[].
- Prefer the fewest actions. No prose outside JSON."#;

/// next-queue 4. Appended only when the order sounds like a spelling fix: the extra op text moved 4 of 23 unrelated
/// planner cases (2 runs each, gpt-oss-120b, 2026-10-17), so normal orders keep the bench prompt byte for byte.
const CORRECT_WORD: &str = "\n- One more op, correct_word{target, text}: fix a misheard written word (\"corrige: Jeff es Jev, J E V\", \"the last thing you wrote, GAB must be Jev\"). target = the wrong word as written, text = the right word; spelled letters win (\"J E V\", \"jota e ve\" -> \"Jev\"; unsure: copy the letters as said).";

/// Words of a spoken spelling fix ("corrige: Jeff es Jev", "GAB must be Jev", "se escribe J E V").
/// ponytail: a keyword list; a miss means the planner never sees correct_word, so add the phrase here.
const CORRECTION_WORDS: &[&str] = &[
    "corrige", "corregir", "corrigelo", "correct", "correction", "fix", "must be", "should be", "is spelled", "spelled",
    "spell", "debe ser", "deberia ser", "se escribe", "escribe con", "deletrea", "misheard", "mal escrito", "no es", "it's not",
    "is not", "isn't", "es con", "wrong word", "mal",
];

pub fn system_for(utterance: &str) -> String {
    let u = format!(" {} ", crate::text::norm(utterance).replace(|c: char| !c.is_alphanumeric() && c != '\'', " "));
    let u = u.split_whitespace().collect::<Vec<_>>().join(" ");
    let fix = CORRECTION_WORDS.iter().any(|w| format!(" {u} ").contains(&format!(" {w} ")));
    if fix { format!("{PROMPT}{CORRECT_WORD}") } else { PROMPT.to_string() }
}

/// One planner action. Flat on purpose: it is the strict schema 1:1 (unused fields null).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Action {
    pub op: String,
    pub app: Option<String>,
    pub keys: Option<String>,
    pub text: Option<String>,
    pub target: Option<String>,
    pub dir: Option<String>,
    pub risky: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub complete: bool,
    pub rest: String,
    pub answer: Option<String>,
    pub say: Option<String>,
    pub actions: Vec<Action>,
    /// Set by live.rs from the screen, not by the model: no other text input to name, so an untargeted `type` goes
    /// to the focus at once (live::hold).
    #[serde(skip)]
    pub type_here: bool,
}

pub const OPS: [&str; 11] = ["open_app", "key", "type", "click", "select_typed", "delete_typed", "copy", "paste", "scroll", "wait_user", "correct_word"];

pub fn schema() -> Value {
    let s = json!({"type": ["string", "null"]});
    let action = json!({
        "type": "object", "additionalProperties": false,
        "required": ["op", "app", "keys", "text", "target", "dir", "risky"],
        "properties": {"op": {"type": "string", "enum": OPS}, "app": s, "keys": s, "text": s, "target": s, "dir": s, "risky": {"type": "boolean"}},
    });
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["complete", "rest", "answer", "say", "actions"],
        "properties": {"complete": {"type": "boolean"}, "rest": {"type": "string"}, "answer": s, "say": s, "actions": {"type": "array", "items": action}},
    })
}

/// Per-model request extras (bench: these settings were measured). ponytail: a fixed table; add a settings field
/// if Pablo picks models outside it.
pub fn extra_for(model: &str) -> Value {
    if model.starts_with("openai/gpt-oss") {
        json!({"provider": {"only": ["cerebras"], "allow_fallbacks": false}, "reasoning": {"effort": "low"}})
    } else if model.starts_with("openai/gpt-6") || model.starts_with("google/gemini-3") {
        json!({"reasoning": {"effort": "minimal"}})
    } else if model.starts_with("google/gemini-2.5-flash") && !model.contains("lite") {
        json!({"reasoning": {"enabled": false}})
    } else {
        json!({})
    }
}

/// Light per-app shortcuts for the planner (vc-feedback 18): data, not code branches.
const APP_HINTS: [(&[&str], &str); 7] = [
    (&["Google Chrome", "Safari", "Arc", "Firefox", "Brave Browser", "Microsoft Edge", "Dia"], "cmd+l address bar (type a URL or a search, then return), cmd+f find in page, cmd+t new tab, cmd+w close tab, cmd+1..9 tab N, cmd+[ back"),
    (&["Slack"], "cmd+k jump to a channel or person, cmd+f search, cmd+n new message, return sends"),
    (&["Ghostty", "Terminal", "iTerm2", "Warp", "WezTerm", "kitty", "Alacritty"], "cmd+t new tab, cmd+1..9 tab N, commands are typed lowercase, ctrl+c stops a command, no text selection"),
    (&["Finder"], "cmd+shift+g go to folder, cmd+f search, cmd+n new window"),
    (&["Cursor", "Code", "Visual Studio Code", "Zed"], "cmd+p open a file, cmd+shift+p command palette, cmd+f find"),
    (&["WhatsApp", "Messages", "Telegram"], "cmd+f search chats, return sends"),
    (&["Notes", "TextEdit", "Notion", "Obsidian"], "cmd+a select all, cmd+z undo"),
];

pub fn hints_for(app: &str) -> Option<&'static str> {
    APP_HINTS.iter().find(|(apps, _)| apps.iter().any(|a| a.eq_ignore_ascii_case(app))).map(|h| h.1)
}

/// What the planner sees, read right before the call.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub utterance: String,
    pub last: bool,
    pub app: String,
    pub focused: Field,
    pub memory: Vec<String>,
    /// `ax::describe` of each mark, ids dropped.
    pub marks: Vec<String>,
    pub pending: Option<String>,
}

/// The user message. A secure field is `{"role": "secure"}`: no value, ever.
pub fn user_message(i: &Input) -> Value {
    let f = &i.focused;
    let focused = if f.secure {
        json!({"role": "secure"})
    } else {
        json!({"role": f.role.trim_start_matches("AX"), "name": f.name, "value": f.tail(), "selected": f.selected.as_deref().unwrap_or("")})
    };
    json!({
        "utterance": i.utterance, "final": i.last, "memory": i.memory, "app": i.app, "focused": focused,
        "marks": i.marks, "hints": hints_for(&i.app), "pending": i.pending,
    })
}

pub fn request(model: &str, user: &Value) -> Value {
    request_with(model, PROMPT, user)
}

pub fn request_with(model: &str, system: &str, user: &Value) -> Value {
    let mut b = json!({
        "model": model, "temperature": 0, "max_tokens": 1500,
        "response_format": {"type": "json_schema", "json_schema": {"name": "plan", "strict": true, "schema": schema()}},
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user.to_string()}],
    });
    if let (Some(b), Some(x)) = (b.as_object_mut(), extra_for(model).as_object()) {
        b.extend(x.clone());
    }
    b
}

/// The first `{...}` of the model text (some models wrap it in a code fence).
pub fn parse(content: &str) -> anyhow::Result<Plan> {
    let (a, b) = (content.find('{'), content.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else { anyhow::bail!("planner: no JSON in {content:?}") };
    let mut p: Plan = serde_json::from_str(&content[a..=b]).map_err(|e| anyhow::anyhow!("planner: bad JSON ({e}): {content:?}"))?;
    p.actions.retain(|a| OPS.contains(&a.op.as_str()) && a.op != "wait_user");
    if p.complete {
        p.rest.clear();
    }
    Ok(p)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub latency_ms: u32,
    pub cost: f64,
}

pub const TIMEOUT: Duration = Duration::from_secs(6);

/// One planner call. `base` = OPENROUTER_BASE (tests: the mock server).
pub fn plan(base: &str, key: &str, model: &str, input: &Input) -> anyhow::Result<(Plan, Meta)> {
    let t = Instant::now();
    let rb = http()
        .post(url(base, "chat/completions"))
        .bearer_auth(key)
        .header("HTTP-Referer", "https://susurro.app")
        .header("X-Title", "susurro")
        .timeout(TIMEOUT)
        .json(&request_with(model, &system_for(&input.utterance), &user_message(input)));
    let v = send_json(rb, "Planner")?;
    if let Some(e) = v.get("error") {
        anyhow::bail!("Planner: {}", e["message"].as_str().unwrap_or("error"));
    }
    let content = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow::anyhow!("Planner: no message in the response"))?;
    let p = parse(content)?;
    Ok((p, Meta { latency_ms: t.elapsed().as_millis() as u32, cost: v["usage"]["cost"].as_f64().unwrap_or(0.0) }))
}

/// The production call.
pub fn plan_openrouter(key: &str, model: &str, input: &Input) -> anyhow::Result<(Plan, Meta)> {
    plan(OPENROUTER_BASE, key, model, input)
}

/// Short label for the overlay queue: "⌘T", "type “ls”", "⏎".
pub fn label(a: &Action) -> String {
    let q = |s: &Option<String>| format!("“{}”", s.as_deref().unwrap_or(""));
    match a.op.as_str() {
        "key" => {
            let k = a.keys.as_deref().unwrap_or("").to_lowercase();
            let parts: Vec<String> = k
                .split('+')
                .map(|p| match p.trim() {
                    "cmd" | "command" => "⌘".into(),
                    "shift" => "⇧".into(),
                    "opt" | "option" | "alt" => "⌥".into(),
                    "ctrl" | "control" => "⌃".into(),
                    "return" | "enter" => "⏎".into(),
                    "delete" | "backspace" => "⌫".into(),
                    "escape" | "esc" => "⎋".into(),
                    "tab" => "⇥".into(),
                    p => p.to_uppercase(),
                })
                .collect();
            parts.concat()
        }
        "type" => format!("type {}", q(&a.text)),
        "click" => format!("click {}", q(&a.target)),
        "open_app" => format!("open {}", a.app.as_deref().unwrap_or("")),
        "select_typed" => "select typed".into(),
        "delete_typed" => "delete typed".into(),
        "copy" => a.target.as_ref().map_or("copy".into(), |_| format!("copy {}", q(&a.target))),
        "paste" => a.target.as_ref().map_or("paste".into(), |_| format!("paste into {}", q(&a.target))),
        "scroll" => format!("scroll {}", a.dir.as_deref().unwrap_or("down")),
        "correct_word" => format!("fix {} → {}", q(&a.target), q(&a.text)),
        op => op.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn input() -> Input {
        Input {
            utterance: "create a new tab and run ls".into(),
            app: "Ghostty".into(),
            focused: Field { role: "AXTextArea".into(), value: Some("pablo@mac ~ % ".into()), ..Default::default() },
            memory: vec!["21:10:25 typed 'hello' into WhatsApp 'Type a message'".into()],
            marks: vec!["New Tab (button, toolbar)".into()],
            ..Default::default()
        }
    }

    #[test]
    fn correct_word_prompt_only_for_spelling_fixes() {
        for u in ["Corrige: Jeff es Jev, J E V.", "The last thing you wrote, GAB must be Jev.", "No es Jeff, es jota e ve.", "fix the word type safe"] {
            assert!(system_for(u).contains("correct_word{target, text}"), "{u}");
        }
        for u in ["Type hello there.", "LS command.", "Remove what you typed.", "Onto the top header, click the share button."] {
            assert_eq!(system_for(u), PROMPT, "{u}");
        }
    }

    #[test]
    fn planner_request_uses_the_setting_and_the_schema() {
        let mut srv = mockito::Server::new();
        let reply = json!({"choices": [{"message": {"content": "```json\n{\"complete\":true,\"rest\":\"\",\"answer\":null,\"say\":null,\"actions\":[{\"op\":\"key\",\"app\":null,\"keys\":\"cmd+t\",\"text\":null,\"target\":null,\"dir\":null,\"risky\":false},{\"op\":\"type\",\"app\":null,\"keys\":null,\"text\":\"ls\",\"target\":null,\"dir\":null,\"risky\":false},{\"op\":\"key\",\"app\":null,\"keys\":\"return\",\"text\":null,\"target\":null,\"dir\":null,\"risky\":false}]}\n```"}}], "usage": {"cost": 0.00045}});
        let m = srv
            .mock("POST", "/api/v1/chat/completions")
            .match_header("authorization", "Bearer sk-or")
            .match_body(Matcher::PartialJson(json!({
                "model": "openai/gpt-oss-120b",
                "temperature": 0,
                "provider": {"only": ["cerebras"]},
                "reasoning": {"effort": "low"},
                "response_format": {"type": "json_schema", "json_schema": {"strict": true}},
            })))
            .with_body(reply.to_string())
            .create();
        let (p, meta) = plan(&format!("{}/api/v1", srv.url()), "sk-or", "openai/gpt-oss-120b", &input()).unwrap();
        m.assert();
        assert!(p.complete);
        assert_eq!(p.actions.iter().map(|a| a.op.as_str()).collect::<Vec<_>>(), ["key", "type", "key"]);
        assert_eq!(p.actions[1].text.as_deref(), Some("ls"));
        assert_eq!(meta.cost, 0.00045);
    }

    #[test]
    fn user_message_shape_and_secure_fields() {
        let u = user_message(&input());
        assert_eq!(u["focused"]["role"], "TextArea");
        assert_eq!(u["focused"]["value"], "pablo@mac ~ % ");
        assert_eq!(u["hints"].as_str().unwrap().contains("cmd+t new tab"), true);
        assert_eq!(u["final"], false);
        let mut i = input();
        i.focused = Field { role: "AXTextField".into(), secure: true, value: Some("hunter2".into()), ..Default::default() };
        let u = user_message(&i);
        assert_eq!(u["focused"], json!({"role": "secure"}));
        assert!(!u.to_string().contains("hunter2"));
        // the value tail is 200 chars
        i.focused = Field { value: Some("x".repeat(500) + "END"), ..Default::default() };
        let v = user_message(&i)["focused"]["value"].as_str().unwrap().to_string();
        assert_eq!(v.chars().count(), 200);
        assert!(v.ends_with("END"));
        assert_eq!(hints_for("Some App"), None);
    }

    #[test]
    fn parse_partial_noise_and_bad_ops() {
        let p = parse(r#"{"complete":false,"rest":"create a new tab and","answer":null,"say":null,"actions":[]}"#).unwrap();
        assert!(!p.complete && p.rest == "create a new tab and" && p.actions.is_empty());
        let p = parse(r#"{"complete":true,"rest":"left over","answer":"yes","say":null,"actions":[{"op":"wait_user"},{"op":"rm_rf"},{"op":"paste"}]}"#).unwrap();
        assert_eq!(p.rest, "", "complete = nothing left");
        assert_eq!(p.answer.as_deref(), Some("yes"));
        assert_eq!(p.actions.len(), 1, "wait_user and unknown ops are dropped");
        assert!(parse("Sure! I will open the tab.").is_err());
        assert!(parse("{not json}").is_err());
    }

    #[test]
    fn extras_and_labels() {
        assert_eq!(extra_for("openai/gpt-6-luna")["reasoning"]["effort"], "minimal");
        assert_eq!(extra_for("anthropic/claude-haiku-5.5"), json!({}));
        let r = request("anthropic/claude-haiku-5.5", &json!({}));
        assert!(r.get("provider").is_none());
        let a = |op: &str, keys: Option<&str>, text: Option<&str>| Action { op: op.into(), keys: keys.map(Into::into), text: text.map(Into::into), ..Default::default() };
        assert_eq!(label(&a("key", Some("cmd+t"), None)), "⌘T");
        assert_eq!(label(&a("key", Some("return"), None)), "⏎");
        assert_eq!(label(&a("key", Some("cmd+shift+left"), None)), "⌘⇧LEFT");
        assert_eq!(label(&a("type", None, Some("ls"))), "type “ls”");
        assert_eq!(label(&a("delete_typed", None, None)), "delete typed");
    }

    #[test]
    fn schema_is_strict_and_lists_every_op() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["actions"]["items"]["properties"]["op"]["enum"].as_array().unwrap().len(), OPS.len());
        assert_eq!(s["properties"]["actions"]["items"]["required"].as_array().unwrap().len(), 7);
    }
}
