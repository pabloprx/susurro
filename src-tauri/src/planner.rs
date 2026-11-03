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
Input (JSON): utterance (live speech so far, may be cut mid-order), final (true = the user stopped talking), memory (what susurro itself did earlier: typed text, copies, pastes, clicks), app (frontmost app), focused (role, name and current value of the focused field, read just now), marks (actionable elements on screen now, "[N] name (role, region)"), hints (shortcuts of the frontmost app), pending (a question susurro asked the user, or null).
Output: strict JSON {"complete": bool, "rest": string, "answer": string|null, "say": string|null, "actions": [...]}.
- complete=false when the utterance ends in an unfinished order ("open", "type", "create a new tab and"); then actions holds only the finished leading orders and rest = the words of the unfinished order, verbatim. complete=true: rest="".
- final=true: the user stopped; plan what you can, complete=true, rest="".
- Ops (unused fields = null):
  open_app{app}: launch/focus app by its real name (fix STT spelling: "Ghosty" -> "Ghostty").
  key{keys}: one shortcut, lowercase, "+"-joined, modifiers cmd/opt/ctrl/shift: "cmd+t", "return", "cmd+a", "delete", "cmd+1", "opt+shift+left". Any combo is allowed.
  type{text, target}: insert text. text = ONLY the content to write, never the target words ("type hello onto the search input" -> text "hello", target = the search field). target=null types into the focused field. In terminals and code, commands are lowercase ("type LS" -> "ls"). Drop the period STT adds at the end. Drop fillers and cut-off words ("eh", "este,", "um", "you-"): "type eh, hel- hello world" -> text "hello world".
  click{target}: press or focus an element.
  select_typed{}: select the last text susurro typed (from memory).
  delete_typed{}: delete the last text susurro typed (from memory), in one go.
  copy{target}: copy a link or the value of a field (target = that element); target=null copies the current selection.
  paste{target}: paste the clipboard; target = the field to paste into first, or null for the focused field.
  scroll{dir, target}: dir up|down. wait_user{}: do nothing, wait for more speech.
- target: copy the mark text exactly when the element is in marks ("[12] Share (button, toolbar)"); else a few words naming it ("Public radio button"): it may appear only after an earlier action.
- type never presses return. Add key return only when the user says "run", "execute", "enter" or "X command" ("type ls" -> type only; "run ls" -> type + return).
- "delete all the writing" = key cmd+a then key delete. "remove what you typed" = one delete_typed, never several key delete.
- memory is only what susurro did. The screen (focused value) is the truth: if memory says susurro typed something that is no longer in the focused value, do not delete; return actions=[] and say why.
- "search X" / "busca X": when marks show the page's or app's own search box (search field or combo box, "Search", "Buscar"), click it, type X, key return, all in this one order ("busca en YouTube Ultra Miami" on YouTube = click the YouTube search box, type "Ultra Miami", key return). With no such box, in a browser = key cmd+l, type X, key return. Outside a browser, "search X" uses the app's own search, unless X is a website ("YouTube", "Google", "Amazon"): then open_app a browser first. Use the app hints.
- A search with no query yet ("busca en YouTube", "search on Google") and final=false: complete=false, actions=[], rest = the utterance: the query comes in the next words.
- pending: if the utterance answers it ("yes", "sí", "no", "the second one", "7"), set answer to "yes", "no" or the number in digits ("the third one" -> "3") and actions=[].
- risky=true when the action itself sends a message, posts, deletes data or spends money.
- Noise or filler ("voila", "um", "thank you") = complete=true, actions=[].
- Each mark starts with [N], the number drawn on screen next to it. A spoken number ("41", "the 7") names that mark: target = that mark's text.
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

/// Case 27: appended only with a `previous` and an order that sounds like "no, not that": the same text on every
/// order moved 2 of 25 unrelated cases ("LS command." lost its Return; gpt-oss-120b, 2026-10-18, 2 runs each).
const RETRY: &str = "\n- previous (in the input): the user's last order, the actions susurro ran for it and what each did.\n- One more op, retry{target, text}: the last click hit the wrong element; click again. target = the words inside previous's click “...” (what the user wanted), never the element it hit; text = the user's constraint (\"bottom\", \"left\", \"the other one\"). susurro never hits the same element twice.\n- A correction is about previous, never a new order: \"no, on the bottom, not the top\", \"not that one\", \"the other one\", \"no, la de abajo\", \"I said X\" = one retry (previous \"click “red”: clicked “Colors”\" + \"no, on the bottom\" -> retry{target \"red\", text \"bottom, not the top\"}). Never click a mark the correction does not name (\"New Tab\", \"Back\").\n- \"no\" / \"no, bro\" / \"stop\" alone = complete=true, actions=[] (susurro stops), never a click.";

/// Case 26: appended only when the order names a color (a paint palette has no marks for its swatches). Same reason
/// as RETRY: global text moves unrelated cases.
const OFF_MARKS: &str = "\n- A color, a swatch or a tool icon that is not in marks is drawn on the page (a paint palette): still one click on its words, with the place the user said (\"change the color to red on the bottom\" -> click{target \"red\"}). Never a click on a menu with a similar name (\"Colors\") and never type the color name.";

/// Color words. ponytail: a list; a color missing here only means the planner sees the base prompt.
const COLOR_WORDS: &[&str] = &[
    "color", "colour", "colors", "colours", "red", "blue", "green", "yellow", "black", "white", "orange", "purple", "pink", "gray",
    "grey", "brown", "rojo", "roja", "azul", "verde", "amarillo", "negro", "blanco", "naranja", "morado", "rosado", "gris", "cafe",
];

/// Words of a "no, not that" correction. ponytail: a keyword list, like CORRECTION_WORDS.
const RETRY_WORDS: &[&str] = &["no", "not", "other", "another", "wrong", "didn't", "didnt", "otra", "otro", "esa no", "eso no", "mal", "stop", "nope"];

fn has_word(utterance: &str, list: &[&str]) -> bool {
    let u = format!(" {} ", crate::text::norm(utterance).replace(|c: char| !c.is_alphanumeric() && c != '\'', " "));
    let u = u.split_whitespace().collect::<Vec<_>>().join(" ");
    list.iter().any(|w| format!(" {u} ").contains(&format!(" {w} ")))
}

/// The prompt for one call: PROMPT, + correct_word for a spelling fix, + the off-marks click for a color, + retry for
/// a correction of `previous`.
pub fn system_for(utterance: &str, previous: bool) -> String {
    let mut s = PROMPT.to_string();
    if has_word(utterance, CORRECTION_WORDS) {
        s.push_str(CORRECT_WORD);
    }
    if has_word(utterance, COLOR_WORDS) {
        s.push_str(OFF_MARKS);
    }
    if previous && has_word(utterance, RETRY_WORDS) {
        s.push_str(RETRY);
    }
    s
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
    /// The words of the order this action came from (live.rs sets it; pick mode reads it). Not in the schema.
    #[serde(skip)]
    pub said: String,
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

pub const OPS: [&str; 12] = ["open_app", "key", "type", "click", "select_typed", "delete_typed", "copy", "paste", "scroll", "wait_user", "correct_word", "retry"];

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
        // Cerebras first (fastest), then the other fast hosts: Cerebras alone timed out for Pablo (2026-10-19,
        // "the planner timed out"). require_parameters keeps the strict JSON schema on every host.
        json!({"provider": {"order": ["cerebras", "groq", "sambanova"], "allow_fallbacks": true, "require_parameters": true}, "reasoning": {"effort": "low"}})
    } else if model.starts_with("openai/gpt-6") || model.starts_with("google/gemini-3") {
        json!({"reasoning": {"effort": "minimal"}})
    } else if model.starts_with("google/gemini-2.5-flash") && !model.contains("lite") {
        json!({"reasoning": {"enabled": false}})
    } else {
        json!({})
    }
}

/// Light per-app shortcuts for the planner (vc-feedback 18): data, not code branches.
/// Electron apps: keyboard first (next-build 6): a palette + the name + return beats a click on a row.
const APP_HINTS: [(&[&str], &str); 11] = [
    (&["Google Chrome", "Safari", "Arc", "Firefox", "Brave Browser", "Microsoft Edge", "Dia"], "cmd+l address bar (type a URL or a search, then return), cmd+f find in page, cmd+t new tab, cmd+w close tab, cmd+1..9 tab N, cmd+[ back"),
    (&["Slack"], "open a channel or DM: key cmd+k, type its name, key return (better than a click); cmd+f search, cmd+n new message, return in the message box sends"),
    (&["Discord"], "open a channel or DM: key cmd+k, type its name, key return (better than a click); return in the message box sends"),
    (&["Claude"], "cmd+k search chats (type the title, key return), cmd+n new chat, return in the prompt box sends"),
    (&["Notion"], "cmd+p find a page (type its title, key return), cmd+n new page"),
    (&["Linear"], "cmd+k command menu (type the command or issue, key return), c new issue"),
    (&["Ghostty", "Terminal", "iTerm2", "Warp", "WezTerm", "kitty", "Alacritty"], "cmd+t new tab, cmd+1..9 tab N, commands are typed lowercase, ctrl+c stops a command, no text selection"),
    (&["Finder"], "cmd+shift+g go to folder, cmd+f search, cmd+n new window"),
    (&["Cursor", "Code", "Visual Studio Code", "Zed"], "cmd+p open a file (type its name, key return), cmd+shift+p command palette, cmd+f find"),
    (&["WhatsApp", "Messages", "Telegram"], "cmd+f search chats, return sends"),
    (&["Notes", "TextEdit", "Obsidian"], "cmd+a select all, cmd+z undo"),
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
    /// "[N] " + `ax::describe` of each mark: N = the number drawn on screen.
    pub marks: Vec<String>,
    pub pending: Option<String>,
    /// The last order, its actions and their results (live.rs `previous_line`): corrections refer to it (case 27).
    pub previous: Option<String>,
}

/// `previous`, unless a question waits: then "no, bro" answers it ("Press Return?"), never retries the last click.
fn previous(i: &Input) -> Option<&String> {
    i.previous.as_ref().filter(|_| i.pending.is_none())
}

/// The user message. A secure field is `{"role": "secure"}`: no value, ever.
pub fn user_message(i: &Input) -> Value {
    let f = &i.focused;
    let focused = if f.secure {
        json!({"role": "secure"})
    } else {
        json!({"role": f.role.trim_start_matches("AX"), "name": f.name, "value": f.tail(), "selected": f.selected.as_deref().unwrap_or("")})
    };
    let mut u = json!({
        "utterance": i.utterance, "final": i.last, "memory": i.memory, "app": i.app, "focused": focused,
        "marks": i.marks, "hints": hints_for(&i.app), "pending": i.pending,
    });
    // only when there is one: the bench input stays byte for byte otherwise
    if let Some(p) = previous(i) {
        u["previous"] = json!(p);
    }
    u
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
    for a in p.actions.iter_mut().filter(|a| a.op == "type") {
        a.text = a.text.as_deref().map(clean_spoken);
    }
    // a key with no keys or a type with nothing left to type is a no-op; run, it would fail and drop the rest of
    // the order (replay 2026-10-18: Luna sent {"op":"key","keys":null} before cmd+l)
    let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
    p.actions.retain(|a| !((a.op == "key" && blank(&a.keys)) || (a.op == "type" && blank(&a.text))));
    // gpt-oss-120b on Cerebras (2026-10-18, 3 runs out of 3) answers "Run LS." with [return, type ls]: the Return
    // would hit an empty prompt. A plan that is only those two runs the text first.
    // ponytail: only this exact two-action shape; a real "press enter, then type X" order is then swapped too.
    if let [k, t] = p.actions.as_slice() {
        if k.op == "key" && k.keys.as_deref().is_some_and(|s| matches!(s.trim(), "return" | "enter")) && t.op == "type" {
            p.actions.swap(0, 1);
        }
    }
    if p.complete {
        p.rest.clear();
    }
    Ok(p)
}

/// "LS command." = type + Return (vc-feedback 2): gpt-oss-120b dropped the Return 3 of 3 runs on 2026-10-18 with
/// the prompt rule in place, and a prompt example moved 3 other cases. A plan that ends in a type for an order
/// ending in "command" / "comando" gets the Return (the executor still asks before Return where it must).
/// ponytail: only the last word; "the ls command, please" keeps the planner's answer.
pub fn command_return(utterance: &str, p: &mut Plan) {
    let last = crate::text::norm(utterance).trim_end_matches(|c: char| !c.is_alphanumeric()).rsplit(' ').next().unwrap_or("").to_string();
    if matches!(last.as_str(), "command" | "comando") && p.actions.last().is_some_and(|a| a.op == "type") {
        p.actions.push(Action { op: "key".into(), keys: Some("return".into()), ..Default::default() });
    }
}

/// Spoken fillers that never belong in typed text.
const FILLERS: [&str; 13] = ["eh", "ehm", "em", "emm", "um", "umm", "uh", "uhm", "uhh", "mm", "mmm", "hmm", "erm"];

/// The text to type without fillers ("eh", "um", "este,") and cut-off words ("you-"): live log 2026-10-18,
/// "search for you- YouTube" typed "you- YouTube". A word is cut off when it is letters + one trailing "-";
/// "-la", "i--" and a lone "-" stay. "este" goes only as a punctuated filler ("este,"): it is also a real word.
pub fn clean_spoken(text: &str) -> String {
    let keep: Vec<&str> = text
        .split(' ')
        .filter(|tok| {
            let core = tok.trim_end_matches([',', '.', ';', ':', '!', '?', '…']);
            let cut = core.strip_suffix('-').is_some_and(|w| !w.is_empty() && w.chars().all(char::is_alphabetic));
            let n = crate::text::norm(core);
            let filler = FILLERS.contains(&n.as_str()) || (n == "este" && core.len() < tok.len());
            !(cut || filler)
        })
        .collect();
    keep.join(" ").trim().trim_start_matches([',', ';', ' ']).trim().to_string()
}

/// Verbs whose number is content, not a mark: "type 41", "search 41", "press 4".
const NOT_A_MARK: [&str; 13] = ["type", "write", "escribe", "escribi", "tipea", "search", "busca", "buscar", "press", "presiona", "pulsa", "scroll", "set"];
/// Words right before a number that make it a mark: "the 41", "la 41", "mark 41".
const MARK_WORDS: [&str; 14] = ["mark", "marca", "the", "la", "el", "number", "numero", "número", "link", "button", "boton", "botón", "en", "on"];
/// Click verbs: "click 41", "abre 41".
const CLICK_VERBS: [&str; 12] = ["click", "clic", "clickea", "cliquea", "clica", "open", "abre", "abrir", "select", "selecciona", "tap", "toca"];

/// A spoken mark number with nothing else to plan: "41", "la 41", "mark 41", "click the 41", "open the first link, 41"
/// (live log 2026-10-18: the planner returned no actions). The number must be the last word and a mark on screen.
/// ponytail: word lists; an order that ends in a number for another reason ("set the volume to 50") is excluded by verb.
pub fn mark_ref(text: &str, ids: &[u32]) -> Option<u32> {
    let raw = text.trim().trim_end_matches(['.', '!', '?', ',']).trim();
    let n = crate::text::norm(raw);
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let (last, before) = words.split_last()?;
    let k: u32 = last.parse().ok()?;
    if !ids.contains(&k) || words.len() > 8 || words.first().is_some_and(|w| NOT_A_MARK.contains(w)) {
        return None;
    }
    let comma = raw.strip_suffix(*last).is_some_and(|b| b.trim_end().ends_with(','));
    let ok = before.is_empty() || comma || before.last().is_some_and(|w| MARK_WORDS.contains(w)) || words.first().is_some_and(|w| CLICK_VERBS.contains(w));
    ok.then_some(k)
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
        .json(&request_with(model, &system_for(&input.utterance, previous(input).is_some()), &user_message(input)));
    let v = send_json(rb, "Planner")?;
    if let Some(e) = v.get("error") {
        anyhow::bail!("Planner: {}", e["message"].as_str().unwrap_or("error"));
    }
    let content = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow::anyhow!("Planner: no message in the response"))?;
    let mut p = parse(content)?;
    command_return(&input.utterance, &mut p);
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
        "retry" => format!("retry {} ({})", q(&a.target), a.text.as_deref().unwrap_or("")),
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
            assert!(system_for(u, false).contains("correct_word{target, text}"), "{u}");
        }
        for u in ["Type hello there.", "LS command.", "Remove what you typed.", "Onto the top header, click the share button."] {
            assert_eq!(system_for(u, false), PROMPT, "{u}");
            assert_eq!(system_for(u, true), PROMPT, "{u}: no correction, no retry text");
        }
        for u in ["No, on the bottom, not the top.", "No.", "The other one.", "I didn't say new tab, I say bottom bar.", "No, la otra."] {
            assert!(system_for(u, true).contains("retry{target, text}"), "{u}");
            assert!(!system_for(u, false).contains("retry{target, text}"), "{u}: no previous, no retry");
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
                "provider": {"order": ["cerebras", "groq", "sambanova"], "allow_fallbacks": true},
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
    fn command_return_only_after_a_final_command_word() {
        let t = |u: &str, ops: &[(&str, Option<&str>)]| {
            let mut p = Plan { actions: ops.iter().map(|(op, k)| Action { op: (*op).into(), keys: k.map(String::from), ..Default::default() }).collect(), ..Default::default() };
            command_return(u, &mut p);
            p.actions.iter().map(|a| a.op.clone()).collect::<Vec<_>>()
        };
        assert_eq!(t("LS command.", &[("type", None)]), ["type", "key"]);
        assert_eq!(t("git status comando", &[("type", None)]), ["type", "key"]);
        assert_eq!(t("LS command.", &[("type", None), ("key", Some("return"))]), ["type", "key"], "no second Return");
        assert_eq!(t("Type LS.", &[("type", None)]), ["type"]);
        assert_eq!(t("command", &[]), Vec::<String>::new());
    }

    #[test]
    fn previous_goes_out_only_when_no_question_waits() {
        let i = Input { utterance: "No, bro.".into(), previous: Some("“click red” -> click “red”: clicked “Colors”".into()), ..Default::default() };
        assert!(user_message(&i).get("previous").is_some());
        let i = Input { pending: Some("Press Return in Ghostty?".into()), ..i };
        assert!(user_message(&i).get("previous").is_none(), "a waiting confirm: no retry context");
        assert!(!system_for(&i.utterance, previous(&i).is_some()).contains("retry{target, text}"));
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
    fn return_before_the_only_type_runs_after_it() {
        let ops = |s: &str| parse(s).unwrap().actions.iter().map(|a| a.op.clone()).collect::<Vec<_>>();
        let a = r#"{"complete":true,"rest":"","answer":null,"say":null,"actions":["#;
        assert_eq!(ops(&format!(r#"{a}{{"op":"key","keys":"return"}},{{"op":"type","text":"ls"}}]}}"#)), ["type", "key"]);
        assert_eq!(ops(&format!(r#"{a}{{"op":"type","text":"ls"}},{{"op":"key","keys":"return"}}]}}"#)), ["type", "key"]);
        // longer plans keep their order
        assert_eq!(ops(&format!(r#"{a}{{"op":"key","keys":"cmd+t"}},{{"op":"key","keys":"return"}},{{"op":"type","text":"ls"}}]}}"#)), ["key", "key", "type"]);
    }

    #[test]
    fn typed_text_drops_fillers_and_cut_off_words() {
        // live log 2026-10-18: "search for you- YouTube" typed "you- YouTube"
        let rows = [
            ("you- YouTube", "YouTube"),
            ("eh, hola", "hola"),
            ("um hello there", "hello there"),
            ("este, mañana a las 5", "mañana a las 5"),
            ("este mensaje es para ti", "este mensaje es para ti"),
            ("hel- hello world", "hello world"),
            ("ls -la", "ls -la"),
            ("git commit -m fix", "git commit -m fix"),
            ("cd -", "cd -"),
            ("i-- in a loop", "i-- in a loop"),
            ("e-mail me", "e-mail me"),
            ("Danny Rep videos", "Danny Rep videos"),
            ("eh", ""),
        ];
        for (raw, want) in rows {
            assert_eq!(clean_spoken(raw), want, "{raw:?}");
        }
        let p = parse(r#"{"complete":true,"rest":"","answer":null,"say":null,"actions":[{"op":"type","text":"you- YouTube","target":null},{"op":"click","target":"you- tube"}]}"#).unwrap();
        assert_eq!(p.actions[0].text.as_deref(), Some("YouTube"));
        assert_eq!(p.actions[1].target.as_deref(), Some("you- tube"), "only typed text is cleaned");
        let p = parse(r#"{"complete":true,"rest":"","actions":[{"op":"key","keys":null},{"op":"key","keys":""},{"op":"type","text":"eh,"},{"op":"key","keys":"cmd+l"}]}"#).unwrap();
        assert_eq!(p.actions.iter().map(label).collect::<Vec<_>>(), ["⌘L"], "blank keys and empty text are dropped");
    }

    #[test]
    fn a_spoken_mark_number_is_a_click() {
        let ids: Vec<u32> = (1..=75).collect();
        let rows = [
            ("41", Some(41)),
            ("41.", Some(41)),
            ("la 41", Some(41)),
            ("the 41", Some(41)),
            ("mark 41", Some(41)),
            ("número 7", Some(7)),
            ("click 12", Some(12)),
            ("abre la 3", Some(3)),
            ("Open the first link, 41.", Some(41)),
            ("Open the first link 41", Some(41)),
            ("send it to ana in 5", None),
            ("type 41", None),
            ("search for 41", None),
            ("set the volume to 50", None),
            ("press cmd 1", None),
            ("la 99", None),
            ("open slack", None),
            ("", None),
        ];
        for (t, want) in rows {
            assert_eq!(mark_ref(t, &ids), want, "{t:?}");
        }
        assert!(PROMPT.contains("Each mark starts with [N]"));
    }

    /// Real API: the orders of Pablo's live session (susurro.log 2026-10-18 00:28-00:30 UTC) through the production
    /// planner call (prompt, "[N]" marks, extras, parse + clean) on DEFAULT_PLANNER, plus the mark-number pass live.rs
    /// runs first. Screens from `.lo/planner-bench/planner_bench3.py`. 1 call per order, ~$0.001.
    /// `OPENROUTER_API_KEY=... cargo test replay_the_live_log -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn replay_the_live_log_through_the_planner() {
        let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
        let model = std::env::var("PLANNER").unwrap_or_else(|_| crate::contract::DEFAULT_PLANNER.into());
        let numbered = |first: u32, m: &[&str]| m.iter().enumerate().map(|(i, x)| format!("[{}] {x}", first + i as u32)).collect::<Vec<_>>();
        let claude = numbered(1, &["New chat (button, sidebar)", "Search (button, sidebar)", "Chats (button, sidebar)", "Projects (button, sidebar)", "Artifacts (button, sidebar)",
            "susurro voice control (link, sidebar)", "Planner bench (link, sidebar)", "Write your prompt to Claude (text area, main)", "Add files (button, main)",
            "Opus 5.5 (pop up button, main)", "Send message (button, main, disabled)", "Settings (button, sidebar)"]);
        let edge_new = numbered(1, &["New Tab (tab, top)", "New Tab (button, top)", "Back (button, toolbar, disabled)", "Refresh (button, toolbar)", "Address and search bar (text field, toolbar)",
            "Favorites (button, toolbar)", "Copilot (button, toolbar)", "Settings and more (button, toolbar)", "Search the web (search field, main)",
            "Search with your voice (button, main)", "Outlook (link, main)", "YouTube (link, main)", "LinkedIn (link, main)", "Amazon (link, main)", "Weather (link, main)"]);
        // mark 41 = "YouTube Music", the 2nd result: the number must win over "the first link"
        let edge_res: Vec<String> = [34, 35, 36, 37, 38, 39, 40, 42, 41, 43, 44, 45, 46].iter().zip(["YouTube - Search (tab, top)", "Back (button, toolbar)", "Address and search bar (text field, toolbar)",
            "Search the web (search field, top)", "All (link, top)", "Videos (link, top)", "Images (link, top)", "YouTube YouTube https://www.youtube.com (link, main)", "YouTube Music (link, main)",
            "YouTube - Apps on Google Play (link, main)", "YouTube on the App Store (link, main)", "YouTube - Wikipedia (link, main)", "Next page (link, bottom)"]).map(|(i, m)| format!("[{i}] {m}")).collect();
        let yt = numbered(1, &["YouTube (tab, top)", "Address and search bar (text field, toolbar)", "Guide (button, top)", "YouTube Home (link, top)", "Search (search field, top)",
            "Search (button, top)", "Search with your voice (button, top)", "Sign in (link, top)", "Home (link, left)", "Shorts (link, left)", "Subscriptions (link, left)"]);
        // the YouTube page of the 01:51 session (Chrome in Spanish): the address bar and the "Buscar" combo box
        let yt_es = numbered(1, &["YouTube (tab, top)", "Barra de direcciones y de búsqueda (text field, toolbar)", "Menú de guía (button, top)", "Página principal de YouTube (link, top)",
            "Buscar (combo box, main)", "Buscar (button, main)", "Buscar con la voz (button, main)", "Crear (button, top)", "Notificaciones (button, top)", "Principal (link, left)", "Shorts (link, left)",
            "Suscripciones (link, left)", "Ultra Music Festival 2015 - Aftermovie (link, main)"]);
        fn yt_search(p: &Plan) -> bool {
            let l: Vec<String> = p.actions.iter().map(label).collect();
            l.len() == 3 && l[0].starts_with("click") && l[0].contains("Buscar") && l[0].contains("combo box") && l[1].to_lowercase() == "type “ultra miami 2015”" && l[2] == "⏎"
        }
        let web = Field { role: "AXWebArea".into(), ..Default::default() };
        let chat = Field { role: "AXTextArea".into(), name: "Write your prompt to Claude".into(), ..Default::default() };
        type Check = fn(&Plan) -> bool;
        let cases: Vec<(&str, &str, Vec<String>, Field, Option<&str>, Check)> = vec![
            ("search for you- YouTube.", "Microsoft Edge", edge_new.clone(), web.clone(), None, |p| {
                p.actions.iter().any(|a| a.op == "type" && a.text.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("youtube")))
                    && p.actions.last().is_some_and(|a| a.op == "key" && a.keys.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("return")))
            }),
            ("Open the first link, 41.", "Microsoft Edge", edge_res.clone(), web.clone(), None, |p| {
                p.actions.len() == 1 && p.actions[0].op == "click" && p.actions[0].target.as_deref().is_some_and(|t| t.starts_with("[41]") || t.to_lowercase().contains("youtube music"))
            }),
            ("Open, Microsoft Edge.", "Claude", claude.clone(), chat.clone(), None, |p| {
                p.actions.len() == 1 && p.actions[0].op == "open_app" && p.actions[0].app.as_deref().is_some_and(|a| a.to_lowercase().contains("edge"))
            }),
            ("There, search for Danny Rep videos.", "Microsoft Edge", yt.clone(), web.clone(), None, |p| {
                p.actions.iter().any(|a| a.op == "type" && a.text.as_deref().is_some_and(|t| t.to_lowercase().trim_end_matches('.') == "danny rep videos"))
            }),
            ("Open it and search for YouTube.", "Claude", claude.clone(), chat.clone(), None, |p| {
                !p.actions.iter().any(|a| a.op == "click" && a.target.as_deref().is_some_and(|t| t.contains("Search (button, sidebar)")))
            }),
            ("Yes.", "Microsoft Edge", yt.clone(), web.clone(), Some("Click “Send”? Say yes · F11 · Esc"), |p| p.answer.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("yes")) && p.actions.is_empty()),
            ("la dos", "Microsoft Edge", yt.clone(), web.clone(), Some("Which one? 12 Danny Rep - YouTube · 15 Danny Rep Shorts · 18 Danny Rep Live"), |p| {
                matches!(p.answer.as_deref(), Some("2" | "15")) && p.actions.is_empty()
            }),
            ("Okay.", "Microsoft Edge", edge_res.clone(), web.clone(), None, |p| p.actions.is_empty()),
            // susurro.log 2026-10-18 01:51-01:52 (Scribe heard Spanish as Portuguese): no query yet = wait for it
            ("Boa- Busca em YouTube.", "Google Chrome", yt_es.clone(), web.clone(), None, |p| !p.complete && p.actions.is_empty()),
            ("Boa- Busca em YouTube. Ultra Miami 2015.", "Google Chrome", yt_es.clone(), web.clone(), None, yt_search),
            ("Busca en YouTube Ultra Miami 2015.", "Google Chrome", yt_es.clone(), web.clone(), None, yt_search),
        ];
        let (mut ok, mut cost) = (0, 0.0);
        for (utt, app, marks, focused, pending, check) in &cases {
            // live.rs runs the mark-number pass before the planner
            let ids: Vec<u32> = marks.iter().filter_map(|m| m.strip_prefix('[')?.split(']').next()?.parse().ok()).collect();
            if pending.is_none() && let Some(n) = mark_ref(utt, &ids) {
                let target = marks.iter().find(|m| m.starts_with(&format!("[{n}]"))).cloned();
                let p = Plan { complete: true, actions: vec![Action { op: "click".into(), target, ..Default::default() }], ..Default::default() };
                let hit = check(&p);
                ok += hit as u32;
                println!("{:5} {utt:40} mark pass -> {:?}", if hit { "OK" } else { "MISS" }, p.actions.iter().map(label).collect::<Vec<_>>());
            }
            let input = Input { utterance: utt.to_string(), app: app.to_string(), focused: focused.clone(), marks: marks.clone(), pending: pending.map(String::from), ..Default::default() };
            let (p, m) = plan_openrouter(&key, &model, &input).unwrap_or_else(|e| panic!("{utt}: {e:#}"));
            cost += m.cost;
            let hit = check(&p);
            ok += hit as u32;
            println!("{:5} {utt:40} {model} {} ms -> {:?} answer={:?}", if hit { "OK" } else { "MISS" }, m.latency_ms, p.actions.iter().map(label).collect::<Vec<_>>(), p.answer);
        }
        println!("{ok}/{} right, ${cost:.5}", cases.len() + 1);
        assert!(ok as usize >= cases.len(), "{ok}/{}", cases.len() + 1);
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
