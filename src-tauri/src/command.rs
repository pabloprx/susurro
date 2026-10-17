//! Voice control executor (`.lo/VOICE_CONTROL_V2.md` section 2). The planner (planner.rs) writes the actions; the
//! live session (live.rs) queues them; `exec` runs one: re-read the screen -> ground the target words on fresh marks
//! (exact name first, else the measured Jev path: text first, the marked screenshot when spatial or unsure,
//! `.lo/som-experiment/RESULTS.md`) -> gates (pick / confirm / secure) -> act -> read the field again -> memory.
//! Side effects go through `Desk` (one real impl, one test fake), so every gate and the abort checks are unit-tested.
//! ABORT (Esc) is checked right before every event susurro posts.

use crate::ax::{self, El, Field};
use crate::contract::{CmdStatus, CommandStep, Mark, MemKind, MemoryEntry, PtRect};
use crate::jev::{self, Bbox, Choice, Reply};
use crate::paste::{CMD, OPT, SHIFT};
use crate::planner::Action;
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Below this target confidence: retry with the screenshot, then ask (pick). RESULTS.md: every miss was < 0.8.
pub const CONF_MIN: f32 = 0.8;
/// Below this nothing is worth a pick list: "Not sure where".
pub const UNSURE_MIN: f32 = 0.2;
/// risky noul at or above this + confirm_risky = wait for a yes. VOICE_CONTROL.md: low on purpose,
/// a false alarm costs one "yes". Not measured yet.
pub const RISKY_MIN: f32 = 0.3;
pub const VISIBLE_MIN: f32 = 0.5;
/// Fewer AX elements than this = the box fallback.
pub const MIN_AX: usize = 5;
/// ponytail: fixed wait after a click / key / app switch so the next action reads the new screen; not measured,
/// an AX-change wait would be faster.
pub const SETTLE_MS: u64 = 250;
/// The marks window draws before the screenshot is taken.
const MARKS_DRAW_MS: u64 = 250;
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
pub const PICK_TIMEOUT: Duration = Duration::from_secs(20);
/// Jev's image limit per side.
const IMG_MAX: f64 = 1568.0;
/// A box center this close to an AX element snaps to it.
const SNAP_PT: f64 = 24.0;
/// Between the focus and the paste of a type step.
const FOCUS_MS: u64 = 150;
/// Cmd+C needs the app to write the pasteboard.
const COPY_WAIT_MS: u64 = 150;
/// Memory entries kept (RAM only).
pub const MEMORY_MAX: usize = 50;

// ---------- the log + the command_step event ----------

static LOG: Mutex<Vec<CommandStep>> = Mutex::new(Vec::new());
const LOG_MAX: usize = 200;

/// Sends one step to every window and keeps it in the in-memory log (one row per step number).
pub fn publish(app: &AppHandle, s: CommandStep) {
    record(&mut LOG.lock().unwrap(), s.clone());
    let _ = app.emit("command_step", s);
}

/// A new live session starts a new log.
pub fn clear_log() {
    LOG.lock().unwrap().clear();
}

/// One row per step: a later status of the same step replaces the earlier one.
fn record(log: &mut Vec<CommandStep>, s: CommandStep) {
    match log.iter().position(|l| l.step == s.step && l.order == s.order) {
        Some(i) => log[i] = s,
        None => log.push(s),
    }
    if log.len() > LOG_MAX {
        log.remove(0);
    }
}

/// The voice-control steps of the current (or last) live session, oldest first.
#[tauri::command]
pub fn command_log() -> Vec<CommandStep> {
    LOG.lock().unwrap().clone()
}

// ---------- abort / confirm / pick (session.rs and live.rs set these) ----------

static ABORT: AtomicBool = AtomicBool::new(false);
/// The live session number. A Real desk of an older session (Esc left its thread in a Jev call) is aborted for
/// good once a new session starts, even though `reset` clears ABORT.
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CONFIRMED: AtomicBool = AtomicBool::new(false);
static DENIED: AtomicBool = AtomicBool::new(false);
/// 0 = not waiting, 1 = confirm, 2 = pick.
static WAITING: AtomicU8 = AtomicU8::new(0);
static ANSWER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    Confirm,
    Pick,
}

/// What the executor waits for right now (a voice answer, or the command key = yes).
pub fn waiting() -> Option<Wait> {
    match WAITING.load(Ordering::SeqCst) {
        1 => Some(Wait::Confirm),
        2 => Some(Wait::Pick),
        _ => None,
    }
}

pub fn abort() {
    ABORT.store(true, Ordering::SeqCst);
}

pub fn aborted() -> bool {
    ABORT.load(Ordering::SeqCst)
}

/// A new live session: no abort, no answer pending.
pub fn reset() {
    GEN.fetch_add(1, Ordering::SeqCst);
    for f in [&ABORT, &CONFIRMED, &DENIED] {
        f.store(false, Ordering::SeqCst);
    }
    WAITING.store(0, Ordering::SeqCst);
}

pub fn confirm() {
    CONFIRMED.store(true, Ordering::SeqCst);
}

/// A spoken "no" to a confirm: skip this action (the rest of its order goes too).
pub fn deny() {
    DENIED.store(true, Ordering::SeqCst);
}

/// The spoken answer to "Which one?".
pub fn answer(text: String) {
    *ANSWER.lock().unwrap() = Some(text);
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Yes,
    Text(String),
    /// Esc, a spoken no, or the timeout.
    No,
}

/// The local yes / no words (VOICE_CONTROL_V2 2.2): 0 ms, $0. None = not a plain answer (ask the planner).
pub fn yes_no(text: &str) -> Option<bool> {
    const YES: [&str; 12] = ["sí", "si", "yes", "yeah", "yep", "confirm", "dale", "ok", "okay", "hazlo", "do it", "go"];
    const NO: [&str; 7] = ["no", "cancel", "stop", "para", "nope", "don't", "cancela"];
    let t = text.trim().trim_end_matches(['.', '!', ',', '?']).trim().to_lowercase();
    let t = t.trim_start_matches(['¡', '¿']);
    if YES.contains(&t) {
        Some(true)
    } else if NO.contains(&t) {
        Some(false)
    } else {
        None
    }
}

// ---------- pure decisions ----------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gate {
    Act,
    /// Wait for the command key (risky step).
    Confirm,
    /// Show the top 3, wait for the spoken number.
    Pick,
    Unsure,
}

/// The gate table: confidence < 0.2 -> unsure, < 0.8 -> pick, risky >= 0.3 with confirm_risky -> confirm, else act.
pub fn gate(confidence: f32, risky: f32, confirm_risky: bool) -> Gate {
    if confidence < UNSURE_MIN {
        Gate::Unsure
    } else if confidence < CONF_MIN {
        Gate::Pick
    } else if confirm_risky && risky >= RISKY_MIN {
        Gate::Confirm
    } else {
        Gate::Act
    }
}

/// Words the element names alone cannot answer (RESULTS.md: "next page" needed the image).
pub fn spatial(instr: &str) -> bool {
    const W: [&str; 34] = [
        "first", "second", "third", "fourth", "fifth", "last", "next", "previous", "left", "right", "top", "bottom", "above",
        "below", "under", "beside", "corner", "middle", "red", "green", "blue", "yellow", "icon", "arrow", "primer", "segund",
        "tercer", "último", "siguiente", "anterior", "izquierd", "derech", "arriba", "abajo",
    ];
    let l = instr.to_lowercase();
    l.split(|c: char| !c.is_alphanumeric()).any(|w| W.iter().any(|s| w.starts_with(s)))
}

const NUMBERS: [(&str, u32); 42] = [
    ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6), ("seven", 7), ("eight", 8), ("nine", 9),
    ("ten", 10), ("eleven", 11), ("twelve", 12), ("thirteen", 13), ("fourteen", 14), ("fifteen", 15), ("sixteen", 16),
    ("seventeen", 17), ("eighteen", 18), ("nineteen", 19), ("twenty", 20), ("uno", 1), ("una", 1), ("dos", 2), ("tres", 3),
    ("cuatro", 4), ("cinco", 5), ("seis", 6), ("siete", 7), ("ocho", 8), ("nueve", 9), ("diez", 10), ("once", 11),
    ("doce", 12), ("trece", 13), ("catorce", 14), ("quince", 15), ("dieciséis", 16), ("diecisiete", 17), ("dieciocho", 18),
    ("diecinueve", 19), ("veinte", 20), ("un", 1),
];

/// The spoken answer to "Which one?" -> a mark id. A number names a mark ("20", "twenty"); an ordinal names a
/// place in the pick list ("the second one").
/// ponytail: number words up to 20 (EN/ES); STT writes digits for larger ones.
pub fn parse_pick(text: &str, choices: &[(u32, f32)], ids: &[u32]) -> Option<u32> {
    let l = text.to_lowercase();
    let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let nth = |k: usize| choices.get(k).map(|c| c.0);
    for w in &words {
        if let Ok(n) = w.parse::<u32>() {
            return ids.contains(&n).then_some(n);
        }
        let ord = [("first", 0), ("primer", 0), ("second", 1), ("segund", 1), ("third", 2), ("tercer", 2)];
        if let Some((_, k)) = ord.iter().find(|(o, _)| w.starts_with(o)) {
            return nth(*k);
        }
    }
    let n = words.iter().find_map(|w| NUMBERS.iter().find(|(s, _)| s == w).map(|p| p.1))?;
    ids.contains(&n).then_some(n)
}

/// The pick list of a "Which one? 4 #general · 7 #design" question: (mark id, name).
pub fn pick_options(question: &str) -> Vec<(u32, String)> {
    let list = question.split_once('?').map_or(question, |(_, r)| r);
    list.split('·')
        .filter_map(|o| {
            let (id, name) = o.trim().split_once(' ').unwrap_or((o.trim(), ""));
            Some((id.parse().ok()?, name.trim().to_string()))
        })
        .collect()
}

/// Words that carry no choice: "the second one", "la del medio", "number 4".
const PICK_FILLER: [&str; 16] = ["the", "one", "el", "la", "lo", "los", "las", "de", "del", "number", "número", "numero", "please", "por", "favor", "that"];

/// A name of a shown option in `text` (or `text` inside one name), when exactly one option fits.
pub fn pick_by_name(text: &str, options: &[(u32, String)]) -> Option<u32> {
    let n = crate::text::norm(text);
    let said: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !PICK_FILLER.contains(w)).collect();
    if said.is_empty() {
        return None;
    }
    let said = said.join(" ");
    let fits: Vec<u32> = options
        .iter()
        .filter(|(_, name)| {
            let name = crate::text::norm(name).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
            name.chars().count() >= 3 && (said.contains(&name) || (said.chars().count() >= 3 && name.contains(&said)))
        })
        .map(|o| o.0)
        .collect();
    (fits.len() == 1).then(|| fits[0])
}

/// The spoken text answers "Which one?" (open risk c): at most 5 words and a number, an ordinal or a shown name.
/// A full new order ("open a new tab and type hello") is not a pick: the planner gets it.
/// ponytail: a short order with a number ("type 5") still counts as a pick; add a verb check if that bites.
pub fn looks_like_pick(text: &str, question: &str) -> bool {
    let l = text.to_lowercase();
    let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() || words.len() > 5 {
        return false;
    }
    let ord = ["first", "primer", "second", "segund", "third", "tercer", "last", "últim", "ultim"];
    let number = |w: &str| w.parse::<u32>().is_ok() || NUMBERS.iter().any(|(s, _)| *s == w);
    words.iter().any(|w| number(w) || ord.iter().any(|o| w.starts_with(o))) || pick_by_name(text, &pick_options(question)).is_some()
}

/// The text of "type hello in the search field", "write 'meet in the lobby'", "busca gatos".
/// ponytail: verb + cut at "in the / into / en el"; an LLM would read odd phrasings (quotes always work).

/// Tiles of `d` (global points) that are each at most 1568 px a side at native resolution, row by row.
pub fn tiles(d: PtRect, scale: f64) -> Vec<PtRect> {
    let cols = (d.w * scale / IMG_MAX).ceil().max(1.0);
    let rows = (d.h * scale / IMG_MAX).ceil().max(1.0);
    let (w, h) = (d.w / cols, d.h / rows);
    let mut out = vec![];
    for r in 0..rows as usize {
        for c in 0..cols as usize {
            out.push(PtRect { x: d.x + c as f64 * w, y: d.y + r as f64 * h, w, h });
        }
    }
    out
}

/// Box fractions of an image that shows `img` (global points) -> the center, in global points. The pixel size of
/// the image never enters: Retina or not, downscaled or not.
pub fn box_point(b: Bbox, img: PtRect) -> (f64, f64) {
    (img.x + (b.left + b.right) / 2.0 * img.w, img.y + (b.top + b.bottom) / 2.0 * img.h)
}

/// Reject inverted, empty or huge boxes (research call 4: a full-height strip).
pub fn sane(b: Bbox) -> bool {
    let inside = [b.top, b.bottom, b.left, b.right].iter().all(|v| (0.0..=1.0).contains(v));
    inside && b.right > b.left && b.bottom > b.top && b.right - b.left <= 0.6 && b.bottom - b.top <= 0.6
}

/// The element under or nearest to `p`, within SNAP_PT.
pub fn snap(p: (f64, f64), els: &[El]) -> Option<&El> {
    let dist = |r: PtRect| {
        let dx = (r.x - p.0).max(p.0 - (r.x + r.w)).max(0.0);
        let dy = (r.y - p.1).max(p.1 - (r.y + r.h)).max(0.0);
        (dx * dx + dy * dy).sqrt()
    };
    els.iter().map(|e| (dist(e.mark.rect), e)).filter(|(d, _)| *d <= SNAP_PT).min_by(|a, b| a.0.total_cmp(&b.0)).map(|p| p.1)
}

/// The display (global points, scale) that holds `p`, else the first one.
pub fn display_for(p: (f64, f64), displays: &[(PtRect, f64)]) -> Option<(PtRect, f64)> {
    let inside = |r: &PtRect| p.0 >= r.x && p.0 < r.x + r.w && p.1 >= r.y && p.1 < r.y + r.h;
    displays.iter().find(|(r, _)| inside(r)).or(displays.first()).copied()
}

pub fn center(r: PtRect) -> (f64, f64) {
    ((r.x + r.w / 2.0).round(), (r.y + r.h / 2.0).round())
}


// ---------- Jev grounding requests (the production builders; the real-API test uses them too) ----------

fn risky_q(instr: &str) -> Value {
    json!({ "type": "noul", "instructions": format!("The user said: \"{instr}\". Would the next step itself send a message, email or post to another person, delete data, or spend money?") })
}

/// One grounding call: the target over the mark ids + risky. `image` = the marked screenshot (the marks window is up).
pub fn marks_request(instr: &str, app: &str, els: &[El], image: Option<&Value>) -> (Value, Value) {
    let mut state = vec![json!(match image {
        None => format!("Frontmost app: {app}. The options are the clickable elements on screen, from the accessibility tree."),
        Some(_) => format!(
            "Screenshot of the frontmost app ({app}). Every clickable element has an outline and a numbered mark at its top-left corner. The mark numbers are the option ids."
        ),
    })];
    state.extend(image.cloned());
    let mut targets: Map<String, Value> = els.iter().map(|e| (e.mark.id.to_string(), json!(ax::describe(e)))).collect();
    targets.insert("none".into(), json!("no element on screen fits"));
    let q = json!({
        "target": { "type": "choice", "instructions": format!("Which element must the user click or type into to do this: \"{instr}\""), "criteria": targets },
        "risky": risky_q(instr),
    });
    (Value::Array(state), q)
}

/// Box fallback: the tiles in `state` by name, a tile choice, one box and one visible noul per tile.
pub fn box_request(instr: &str, app: &str, imgs: &[Value]) -> (Value, Value) {
    let mut state = Map::new();
    state.insert("app".into(), json!(app));
    let mut tile_c = Map::new();
    let mut q = json!({ "risky": risky_q(instr) });
    for (i, img) in imgs.iter().enumerate() {
        let t = format!("tile_{i}");
        state.insert(t.clone(), img.clone());
        tile_c.insert(t.clone(), json!(format!("`{t}`, part {} of the screen (row by row)", i + 1)));
        q[format!("box_{i}")] = json!({ "type": "box", "instructions": format!("The element to click to do this: \"{instr}\", in `{t}`.") });
        q[format!("visible_{i}")] = json!({ "type": "noul", "instructions": format!("Is the element to click for \"{instr}\" visible in `{t}`?") });
    }
    q["tile"] = json!({ "type": "choice", "instructions": format!("Which tile shows the element to click to do this: \"{instr}\"?"), "criteria": tile_c });
    (Value::Object(state), q)
}

/// The answers of one grounding.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub target: Choice,
    pub risky: f32,
    pub cost: f64,
    pub latency_ms: u32,
    pub used_image: bool,
    /// The text answer was weak and no screenshot could be taken (no Screen Recording).
    pub need_screen: bool,
}

fn parse(r: &Reply) -> Decision {
    let a = &r.answers;
    Decision {
        target: jev::choice(a, "target").unwrap_or(Choice { choice: "none".into(), confidence: 0.0, ranked: vec![] }),
        risky: jev::noul(a, "risky").unwrap_or(1.0), // missing = treat as risky
        cost: r.cost,
        latency_ms: r.latency_ms,
        used_image: false,
        need_screen: false,
    }
}

pub type Ask<'a> = dyn FnMut(&Value, &Value) -> anyhow::Result<Reply> + 'a;

/// Text first; the marked screenshot for spatial words or a weak target; the more confident answer wins.
/// `shot` = the marked screenshot as a Jev image part, None = not available.
pub fn decide(instr: &str, app: &str, els: &[El], ask: &mut Ask, shot: &mut dyn FnMut() -> Option<Value>) -> anyhow::Result<Decision> {
    let (s, q) = marks_request(instr, app, els, None);
    let mut d = parse(&ask(&s, &q)?);
    if spatial(instr) || d.target.confidence < CONF_MIN {
        match shot() {
            Some(img) => {
                let (s, q) = marks_request(instr, app, els, Some(&img));
                let mut d2 = parse(&ask(&s, &q)?);
                d2.cost += d.cost;
                d2.latency_ms += d.latency_ms;
                d2.risky = d2.risky.max(d.risky);
                d2.used_image = true;
                if d2.target.confidence >= d.target.confidence {
                    d = d2;
                } else {
                    (d.cost, d.latency_ms, d.risky, d.used_image) = (d2.cost, d2.latency_ms, d2.risky, true);
                }
            }
            None => d.need_screen = d.target.confidence < CONF_MIN,
        }
    }
    Ok(d)
}

/// The planner's target words name one mark exactly (it copies "Share (button, toolbar)" from the marks it saw):
/// no Jev call. Tried in order: the full label, "name (role)", the name. Two matches = ambiguous = None.
pub fn local_match<'a>(words: &str, els: &'a [El]) -> Option<&'a El> {
    let norm = |s: &str| s.trim().trim_matches(['"', '“', '”', '\'']).trim().to_lowercase();
    let w = norm(words);
    if w.is_empty() {
        return None;
    }
    let forms: [&dyn Fn(&El) -> String; 3] = [&|e| ax::describe(e), &|e| format!("{} ({})", e.mark.name, e.mark.role), &|e| e.mark.name.clone()];
    forms.iter().find_map(|f| {
        let mut m = els.iter().filter(|e| norm(&f(e)) == w);
        match (m.next(), m.next()) {
            (Some(e), None) => Some(e),
            _ => None,
        }
    })
}

/// A name that sends, deletes or pays: a local exact match skips Jev's risky noul, so this stands in for it.
pub fn risky_name(name: &str) -> bool {
    const W: [&str; 14] = ["send", "post", "delete", "remove", "buy", "pay", "purchase", "publish", "submit", "enviar", "eliminar", "borrar", "comprar", "pagar"];
    let l = name.to_lowercase();
    l.split(|c: char| !c.is_alphanumeric()).any(|w| W.contains(&w))
}

// ---------- the executor ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Mark(u32),
    /// Box fallback with no AX element near: global points.
    Point(f64, f64),
}

/// Every event the executor sends. Typing goes through `Desk::type_text` (it returns what went in).
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Click(Target),
    Focus(Target),
    /// keycode + CGEventFlags bits (paste::parse_combo).
    Combo(u16, u64),
    /// The same combo n times, 4 ms apart (no settle, no Jev call between them).
    Burst(u16, u64, u32),
    Scroll { down: bool, at: Option<Target> },
    OpenApp(String),
}

const DELETE: u16 = 51;
const KEY_C: u16 = 8;
const KEY_V: u16 = 9;
const KEY_Z: u16 = 6;
const LEFT: u16 = 123;

/// What the executor sees: the frontmost app, its window's display, the marks.
#[derive(Debug, Clone, Default)]
pub struct Screen {
    pub app: String,
    pub display: PtRect,
    pub scale: f64,
    pub els: Vec<El>,
}

/// The executor's side effects. Real: `mac::Real` (AX, CGEvent, screencapture). Tests: a fake desk.
pub trait Desk {
    /// Err("accessibility") = no AX access; any other Err = nothing to act on.
    fn read(&mut self) -> Result<Screen, String>;
    /// The focused element right now (re-read before every action).
    fn focused(&mut self) -> Field;
    /// AXSelectedTextRange on the focused element (UTF-16 units). false = refused.
    fn set_selection(&mut self, start: usize, len: usize) -> bool;
    /// Clipboard text and the pasteboard change count.
    fn clipboard(&mut self) -> (Option<String>, isize);
    /// Leave `s` on the clipboard (no restore).
    fn set_clipboard(&mut self, s: &str);
    /// Paste `s` into the focused field (the clipboard is restored after); returns what went in (smart spacing).
    fn type_text(&mut self, s: &str) -> anyhow::Result<String>;
    fn screen_ok(&mut self) -> bool;
    /// JPEG of `r` (global points). `native` = full resolution (tiles), else at most 1568 px a side.
    fn capture(&mut self, r: PtRect, native: bool) -> anyhow::Result<Vec<u8>>;
    fn act(&mut self, a: &Act) -> anyhow::Result<()>;
    /// macOS secure input is on (a password field has the focus somewhere): no keys, no typing.
    fn secure_input(&mut self) -> bool;
    /// The element at this global point is a password field (box fallback clicks: no AX mark to check).
    fn secure_at(&mut self, p: (f64, f64)) -> bool;
    fn emit(&mut self, s: &CommandStep);
    fn sleep(&mut self, ms: u64);
    fn wait(&mut self, w: Wait, timeout: Duration) -> Answer;
    fn aborted(&self) -> bool;
    /// correct_word: add `heard` as a correction of the dictionary word `word`. Err = not saved (the reason).
    fn learn_word(&mut self, _word: &str, _heard: &str) -> Result<(), String> {
        Ok(())
    }
}

pub struct Cfg {
    pub confirm_risky: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub status: CmdStatus,
    /// What susurro did, for the History row ("typed “ls”").
    pub summary: String,
    pub message: Option<String>,
    pub steps: u32,
}

const NOT_SURE: &str = "Not sure where. Say it again.";
const BLOCKED: &str = "Password field. susurro won't touch it.";
pub const GONE: &str = "The text I typed is not there any more.";

/// Terminals have no text selection: delete by backspace burst (VOICE_CONTROL_V2 2.3).
/// ponytail: a fixed list; add AX role checks if an unknown terminal shows up.
const TERMINALS: [&str; 14] = [
    "Ghostty", "Terminal", "iTerm2", "Warp", "WezTerm", "kitty", "Alacritty", "com.mitchellh.ghostty", "com.apple.Terminal",
    "com.googlecode.iterm2", "dev.warp.Warp-Stable", "com.github.wez.wezterm", "net.kovidgoyal.kitty", "org.alacritty",
];

pub fn terminal(f: &Field) -> bool {
    TERMINALS.iter().any(|t| t.eq_ignore_ascii_case(&f.app) || t.eq_ignore_ascii_case(&f.bundle))
}

/// How many Opt+Left presses cover `text` from its end: one per run of letters/digits (punctuation and spaces are
/// skipped by the word jump). At least 1.
pub fn words(text: &str) -> u32 {
    let mut n = 0;
    let mut in_word = false;
    for c in text.chars() {
        let w = c.is_alphanumeric() || c == '_';
        if w && !in_word {
            n += 1;
        }
        in_word = w;
    }
    n.max(1)
}

/// UTF-16 (start, len) of `needle` in `value`: the last match that ends at or before the caret, else the last one.
pub fn find_range(value: &str, needle: &str, caret: Option<usize>) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let len = needle.encode_utf16().count();
    let starts: Vec<usize> = value.match_indices(needle).map(|(i, _)| value[..i].encode_utf16().count()).collect();
    let before = caret.and_then(|c| starts.iter().rev().find(|s| *s + len <= c));
    before.or(starts.last()).map(|s| (*s, len))
}

/// One memory entry plus what the delete / select strategies need.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub e: MemoryEntry,
    pub pid: i32,
    pub role: String,
    /// The field value right after susurro's paste (the undo strategy checks nothing changed since).
    pub value_after: Option<String>,
}

pub type Memory = VecDeque<Entry>;

pub fn remember(mem: &mut Memory, kind: MemKind, text: &str, f: &Field, value_after: Option<String>) {
    let field = if f.name.is_empty() { f.role.trim_start_matches("AX").to_string() } else { f.name.clone() };
    mem.push_back(Entry {
        e: MemoryEntry { at: crate::history::now_ms(), kind, text: text.into(), app: f.app.clone(), field },
        pid: f.pid,
        role: f.role.clone(),
        value_after,
    });
    while mem.len() > MEMORY_MAX {
        mem.pop_front();
    }
}

fn quote(s: &str) -> String {
    format!("“{}”", if s.is_empty() { "(no name)" } else { s })
}

/// One action in flight: its step row and the end helpers.
struct Run {
    st: CommandStep,
}

impl Run {
    fn end(&mut self, desk: &mut dyn Desk, status: CmdStatus, message: Option<&str>, summary: String) -> End {
        self.st.status = status.clone();
        self.st.message = message.map(String::from);
        desk.emit(&self.st);
        End { status, summary, message: self.st.message.clone(), steps: self.st.step }
    }

    fn done(&mut self, desk: &mut dyn Desk, summary: String) -> End {
        self.end(desk, CmdStatus::Done, None, summary)
    }

    fn fail(&mut self, desk: &mut dyn Desk, status: CmdStatus, message: &str) -> End {
        self.end(desk, status, Some(message), String::new())
    }

    /// Wait for a yes. false = no / timeout / Esc (the run ends Stopped).
    fn confirm(&mut self, desk: &mut dyn Desk, question: &str) -> bool {
        self.st.status = CmdStatus::Confirm;
        self.st.message = Some(question.into());
        desk.emit(&self.st);
        let ok = desk.wait(Wait::Confirm, CONFIRM_TIMEOUT) == Answer::Yes;
        self.st.message = None;
        ok
    }
}

/// A grounded target.
struct Hit {
    target: Target,
    mark: Mark,
    url: Option<String>,
    value: Option<String>,
    risky: f32,
}

/// Read the screen and find `words` on it. Err = the run ended (unsure, setup, pick refused...).
fn ground(r: &mut Run, verb: &str, words: &str, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    let scr = match desk.read() {
        Ok(s) => s,
        Err(e) if e == "accessibility" => return Err(r.fail(desk, CmdStatus::Setup, "accessibility")),
        Err(e) => return Err(r.fail(desk, CmdStatus::Unsure, &e)),
    };
    r.st.display = scr.display;
    r.st.marks = scr.els.iter().map(|e| e.mark.clone()).collect();
    r.st.status = CmdStatus::Marks;
    desk.emit(&r.st);
    if let Some(e) = local_match(words, &scr.els) {
        let risky = if risky_name(&e.mark.name) { 1.0 } else { 0.0 };
        return Ok(Hit { target: Target::Mark(e.mark.id), mark: e.mark.clone(), url: e.url.clone(), value: e.value.clone(), risky });
    }
    let instr = format!("{verb} {words}");
    if scr.els.len() < MIN_AX {
        return box_ground(r, &instr, &scr, ask, desk);
    }
    let display = scr.display;
    let d = {
        let mut shot = || {
            if desk.aborted() || !desk.screen_ok() {
                return None;
            }
            desk.sleep(MARKS_DRAW_MS);
            desk.capture(display, false).map_err(|e| log::warn!("screenshot: {e:#}")).ok().map(|b| jev::image(&b, "image/jpeg"))
        };
        decide(&instr, &scr.app, &scr.els, ask, &mut shot)
    };
    let d = d.map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    (r.st.latency_ms, r.st.cost_usd) = (r.st.latency_ms + d.latency_ms, r.st.cost_usd + d.cost);
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    let conf = if d.target.choice == "none" { 0.0 } else { d.target.confidence };
    if d.need_screen && conf < CONF_MIN {
        return Err(r.fail(desk, CmdStatus::Setup, "screen_recording"));
    }
    let mut id = d.target.choice.parse::<u32>().ok();
    match gate(conf, 0.0, false) {
        Gate::Unsure => return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)),
        Gate::Pick => {
            r.st.choices = d.target.ranked.iter().filter_map(|(k, p)| k.parse::<u32>().ok().map(|k| (k, *p))).take(3).collect();
            r.st.status = CmdStatus::Pick;
            let names: Vec<String> = r.st.choices.iter().filter_map(|(id, _)| scr.els.iter().find(|e| e.mark.id == *id).map(|e| format!("{id} {}", e.mark.name))).collect();
            r.st.message = Some(format!("Which one? {}", names.join(" · ")));
            desk.emit(&r.st);
            let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
            id = match desk.wait(Wait::Pick, PICK_TIMEOUT) {
                Answer::Text(t) => match parse_pick(&t, &r.st.choices, &ids).or_else(|| pick_by_name(&t, &pick_options(r.st.message.as_deref().unwrap_or("")))) {
                    Some(id) => Some(id),
                    None => return Err(r.fail(desk, CmdStatus::Unsure, "Didn't catch a number. Say it again.")),
                },
                _ => return Err(r.end(desk, CmdStatus::Stopped, None, String::new())),
            };
            r.st.choices.clear();
            r.st.message = None;
        }
        _ => {}
    }
    let Some(e) = id.and_then(|id| scr.els.iter().find(|e| e.mark.id == id)) else { return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)) };
    Ok(Hit { target: Target::Mark(e.mark.id), mark: e.mark.clone(), url: e.url.clone(), value: e.value.clone(), risky: d.risky })
}

/// The box fallback (an app with < 5 AX elements): full-resolution tiles, Jev box per tile, the visible noul.
fn box_ground(r: &mut Run, instr: &str, scr: &Screen, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    if !desk.screen_ok() {
        return Err(r.fail(desk, CmdStatus::Setup, "screen_recording"));
    }
    let ts = tiles(scr.display, scr.scale.max(1.0));
    let mut imgs = vec![];
    for t in &ts {
        match desk.capture(*t, true) {
            Ok(b) => imgs.push(jev::image(&b, "image/jpeg")),
            Err(e) => return Err(r.fail(desk, CmdStatus::Error, &format!("screenshot: {e:#}"))),
        }
    }
    let (s, q) = box_request(instr, &scr.app, &imgs);
    let rep = ask(&s, &q).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    (r.st.latency_ms, r.st.cost_usd) = (r.st.latency_ms + rep.latency_ms, r.st.cost_usd + rep.cost);
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    let a = &rep.answers;
    let risky = jev::noul(a, "risky").unwrap_or(1.0);
    // The tile, its box, and the visible gate: never click on a weak or broken box.
    let found = (|| {
        let i: usize = jev::choice(a, "tile")?.choice.strip_prefix("tile_")?.parse().ok()?;
        let b = jev::bbox(a, &format!("box_{i}")).filter(|b| sane(*b))?;
        (jev::noul(a, &format!("visible_{i}")).unwrap_or(0.0) >= VISIBLE_MIN).then_some(())?;
        Some((b, *ts.get(i)?))
    })();
    let Some((b, tile)) = found else { return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)) };
    let p = box_point(b, tile);
    let rect = PtRect { x: tile.x + b.left * tile.w, y: tile.y + b.top * tile.h, w: (b.right - b.left) * tile.w, h: (b.bottom - b.top) * tile.h };
    Ok(match snap(p, &scr.els) {
        Some(e) => Hit { target: Target::Mark(e.mark.id), mark: e.mark.clone(), url: e.url.clone(), value: e.value.clone(), risky },
        None => {
            let mark = Mark { id: 1, rect, name: String::new(), role: "box".into(), region: "screen".into() };
            r.st.marks = vec![mark.clone()];
            Hit { target: Target::Point(p.0.round(), p.1.round()), mark, url: None, value: None, risky }
        }
    })
}

/// Shows the target (marks window brackets) right before the event.
fn aim(r: &mut Run, desk: &mut dyn Desk, h: &Hit) {
    r.st.target = Some(h.mark.id);
    r.st.status = CmdStatus::Aim;
    desk.emit(&r.st);
}

/// Ground + aim + secure checks + focus, for type / paste with a target. Err = the run ended.
fn focus_target(r: &mut Run, words: &str, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Option<Hit>, End> {
    let h = ground(r, "type into", words, ask, desk)?;
    aim(r, desk, &h);
    if let Target::Point(x, y) = h.target {
        if desk.secure_at((x, y)) {
            return Err(r.fail(desk, CmdStatus::Blocked, BLOCKED));
        }
    }
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    desk.act(&Act::Focus(h.target.clone())).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    desk.sleep(FOCUS_MS);
    Ok(Some(h))
}

/// Runs one planner action. `step` numbers the action in the session, `order` the planner reply it came from.
pub fn exec(a: &Action, step: u32, order: u32, cfg: &Cfg, ask: &mut Ask, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    let st = CommandStep { instruction: crate::planner::label(a), step, max: 0, status: CmdStatus::Thinking, op: Some(a.op.clone()), order, ..Default::default() };
    let mut r = Run { st };
    if desk.aborted() {
        return r.end(desk, CmdStatus::Stopped, None, String::new());
    }
    desk.emit(&r.st);
    let target = a.target.as_deref().map(str::trim).filter(|t| !t.is_empty());
    // A trailing newline in a paste runs a terminal command: Return must go through the key op and its confirm.
    let text = a.text.as_deref().unwrap_or_default().trim_end_matches(['\n', '\r']).to_string();
    match a.op.as_str() {
        "key" => {
            let keys = a.keys.as_deref().unwrap_or("");
            let (code, flags) = match crate::paste::parse_combo(keys) {
                Ok(k) => k,
                Err(e) => return r.fail(desk, CmdStatus::Unsure, &e),
            };
            r.st.action = Some("press_key".into());
            r.st.detail = Some(keys.into());
            if crate::paste::blocked_combo(code, flags) {
                return r.fail(desk, CmdStatus::Blocked, &format!("{keys} is blocked for voice control"));
            }
            // Return sends in every chat app, whatever the planner says (v1 rule, any app).
            let enter = code == 36 || code == 76;
            if enter || (a.risky && cfg.confirm_risky) {
                let q = if enter { "Press Return? Say yes · F11 · Esc" } else { "Do it? Say yes · F11 · Esc" };
                if !r.confirm(desk, q) {
                    return r.end(desk, CmdStatus::Stopped, None, String::new());
                }
            }
            if desk.secure_input() {
                return r.fail(desk, CmdStatus::Blocked, BLOCKED);
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            let f = desk.focused();
            if let Err(e) = desk.act(&Act::Combo(code, flags)) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            remember(mem, MemKind::Key, keys, &f, None);
            desk.sleep(SETTLE_MS);
            r.done(desk, format!("pressed {keys}"))
        }
        "type" => {
            if text.is_empty() {
                return r.fail(desk, CmdStatus::Unsure, "Nothing to type. Say it again.");
            }
            r.st.action = Some("focus_and_type".into());
            r.st.detail = Some(text.clone());
            let hit = match target {
                Some(t) => match focus_target(&mut r, t, ask, desk) {
                    Ok(h) => h,
                    Err(end) => return end,
                },
                None => None,
            };
            // Check after the focus: the focus itself can land in a password field.
            if desk.secure_input() || desk.focused().secure {
                return r.fail(desk, CmdStatus::Blocked, BLOCKED);
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            let typed = match desk.type_text(&text) {
                Ok(t) => t,
                Err(e) => return r.fail(desk, CmdStatus::Error, &format!("{e:#}")),
            };
            let after = desk.focused();
            remember(mem, MemKind::Typed, &typed, &after, after.value.clone());
            let into = hit.map(|h| format!(" into {}", quote(&h.mark.name))).unwrap_or_default();
            r.done(desk, format!("typed {}{into}", quote(&text)))
        }
        "click" => {
            let Some(t) = target else { return r.fail(desk, CmdStatus::Unsure, NOT_SURE) };
            r.st.action = Some("click".into());
            let h = match ground(&mut r, "click", t, ask, desk) {
                Ok(h) => h,
                Err(end) => return end,
            };
            let risky = a.risky || h.risky >= RISKY_MIN;
            if gate(1.0, if risky { 1.0 } else { 0.0 }, cfg.confirm_risky) == Gate::Confirm {
                r.st.target = Some(h.mark.id);
                if !r.confirm(desk, &format!("Click {}? Say yes · F11 · Esc", quote(&h.mark.name))) {
                    return r.end(desk, CmdStatus::Stopped, None, String::new());
                }
            }
            aim(&mut r, desk, &h);
            if let Target::Point(x, y) = h.target {
                if desk.secure_at((x, y)) {
                    return r.fail(desk, CmdStatus::Blocked, BLOCKED);
                }
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            let f = desk.focused();
            if let Err(e) = desk.act(&Act::Click(h.target.clone())) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            remember(mem, MemKind::Clicked, &h.mark.name, &f, None);
            desk.sleep(SETTLE_MS);
            r.done(desk, format!("clicked {} ({})", quote(&h.mark.name), h.mark.role))
        }
        "open_app" => {
            let app = a.app.clone().unwrap_or_default();
            if app.trim().is_empty() {
                return r.fail(desk, CmdStatus::Unsure, "Which app? Say it again.");
            }
            r.st.action = Some("open_app".into());
            r.st.detail = Some(app.clone());
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            if let Err(e) = desk.act(&Act::OpenApp(app.clone())) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            remember(mem, MemKind::Opened, &app, &Field { app: app.clone(), ..Default::default() }, None);
            r.done(desk, format!("opened {app}"))
        }
        "select_typed" | "delete_typed" => typed_span(&mut r, a.op == "delete_typed", desk, mem),
        "copy" => copy(&mut r, target, ask, desk, mem),
        "paste" => {
            r.st.action = Some("paste".into());
            if let Some(t) = target {
                if let Err(end) = focus_target(&mut r, t, ask, desk) {
                    return end;
                }
            }
            let f = desk.focused();
            if desk.secure_input() || f.secure {
                return r.fail(desk, CmdStatus::Blocked, BLOCKED);
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            let clip = desk.clipboard().0.unwrap_or_default();
            if let Err(e) = desk.act(&Act::Combo(KEY_V, CMD)) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            desk.sleep(FOCUS_MS);
            let after = desk.focused();
            remember(mem, MemKind::Pasted, &clip, &after, after.value.clone());
            r.done(desk, format!("pasted {}", quote(&clip.chars().take(40).collect::<String>())))
        }
        "scroll" => {
            let down = a.dir.as_deref() != Some("up");
            r.st.action = Some(if down { "scroll_down" } else { "scroll_up" }.into());
            let at = match target {
                Some(t) => match ground(&mut r, "scroll", t, ask, desk) {
                    Ok(h) => Some(h.target),
                    Err(end) => return end,
                },
                None => None,
            };
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            if let Err(e) = desk.act(&Act::Scroll { down, at }) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            r.done(desk, format!("scrolled {}", if down { "down" } else { "up" }))
        }
        "correct_word" => correct_word(&mut r, target, &text, desk, mem),
        op => r.fail(desk, CmdStatus::Unsure, &format!("Unknown action {op}")),
    }
}

/// Spoken letter names, ES and EN ("jota e ve", "jay ee vee"). Single letters ("e", "a") pass as themselves.
const LETTER_NAMES: &[(&str, &str)] = &[
    ("be bee", "b"), ("ce cee see", "c"), ("de dee", "d"), ("efe ef", "f"), ("ge gee", "g"), ("hache aitch", "h"),
    ("jota jay", "j"), ("ka kay", "k"), ("ele el", "l"), ("eme em", "m"), ("ene en", "n"), ("pe pee", "p"),
    ("cu cue", "q"), ("erre ar", "r"), ("ese ess", "s"), ("te tee", "t"), ("uve ve vee", "v"), ("equis ex", "x"),
    ("ye why", "y"), ("zeta zed zee", "z"), ("ee", "e"),
];

/// Spelled letters win (next-queue 4): "J E V" / "J-E-V" / "j. e. v." -> "Jev". Anything else stays as written.
pub fn spelled(said: &str) -> String {
    let parts: Vec<&str> = said.split(|c: char| c.is_whitespace() || c == '-' || c == '.' || c == ',').filter(|p| !p.is_empty()).collect();
    let letter = |p: &str| -> Option<String> {
        if p.chars().count() == 1 && p.chars().all(char::is_alphabetic) {
            return Some(p.to_string());
        }
        let n = crate::text::norm(p);
        LETTER_NAMES.iter().find(|(names, _)| names.split(' ').any(|x| x == n)).map(|(_, l)| l.to_string())
    };
    let letters: Option<Vec<String>> = parts.iter().map(|p| letter(p)).collect();
    let Some(letters) = letters.filter(|l| l.len() >= 2) else {
        return said.trim().to_string();
    };
    let w: String = letters.concat();
    // All one case = a name: "Jev". Mixed case was spelled on purpose: "T y p e S a f e" -> "TypeSafe".
    if w.chars().all(char::is_uppercase) || w.chars().all(char::is_lowercase) {
        let mut cs = w.chars();
        cs.next().map(|c| c.to_uppercase().chain(cs.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
    } else {
        w
    }
}

/// UTF-16 (start, len) of the last whole-word `word` in `value` (case and accents ignored) that ends at or before
/// the caret, else the last one.
pub fn find_word(value: &str, word: &str, caret: Option<usize>) -> Option<(usize, usize)> {
    let want = crate::text::norm(word.trim());
    let n = word.trim().chars().count();
    if want.is_empty() {
        return None;
    }
    let idx: Vec<(usize, char)> = value.char_indices().collect();
    let mut hits = vec![];
    for k in 0..idx.len() {
        let (start, _) = idx[k];
        let end = idx.get(k + n).map_or(value.len(), |e| e.0);
        if k + n > idx.len() || crate::text::norm(&value[start..end]) != want {
            continue;
        }
        let wordc = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        if wordc(value[..start].chars().next_back()) || wordc(value[end..].chars().next()) {
            continue;
        }
        let u = |b: usize| value[..b].encode_utf16().count();
        hits.push((u(start), u(end) - u(start)));
    }
    let before = caret.and_then(|c| hits.iter().rev().find(|(s, l)| s + l <= c));
    before.or(hits.last()).copied()
}

/// correct_word (next-queue 4): "corrige: Jeff es Jev, J E V". The dictionary learns it first, so it sticks even when
/// the field cannot be read; then the last "Jeff" before the caret in the focused field becomes "Jev".
fn correct_word(r: &mut Run, heard: Option<&str>, said: &str, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    r.st.action = Some("correct_word".into());
    let word = spelled(said);
    let Some(heard) = heard.filter(|h| !word.is_empty() && crate::text::norm(h) != crate::text::norm(&word)) else {
        return r.fail(desk, CmdStatus::Unsure, "Which word, and how is it spelled? Say it again.");
    };
    r.st.detail = Some(format!("{heard} → {word}"));
    let learned = desk.learn_word(&word, heard);
    if let Err(e) = &learned {
        log::warn!("correct_word: dictionary not updated: {e}");
    }
    let f = desk.focused();
    if f.secure || desk.secure_input() {
        return r.fail(desk, CmdStatus::Blocked, BLOCKED);
    }
    if desk.aborted() {
        return r.end(desk, CmdStatus::Stopped, None, String::new());
    }
    let summary = format!("fixed {} → {}", quote(heard), quote(&word));
    let span = f.value.as_deref().and_then(|v| find_word(v, heard, f.sel.map(|s| s.0 + s.1)));
    if span.is_some_and(|(s, l)| desk.set_selection(s, l)) {
        let typed = match desk.type_text(&word) {
            Ok(t) => t,
            Err(e) => return r.fail(desk, CmdStatus::Error, &format!("{e:#}")),
        };
        let after = desk.focused();
        remember(mem, MemKind::Typed, &typed, &after, after.value.clone());
        return match learned {
            Ok(()) => r.done(desk, summary),
            Err(e) => r.end(desk, CmdStatus::Done, Some(&format!("Fixed here, but not saved to the dictionary: {e}")), summary),
        };
    }
    match learned {
        Ok(()) => r.end(desk, CmdStatus::Done, Some(&format!("Can't fix {} in this field. {word} is in your dictionary now.", quote(heard))), summary),
        Err(e) => r.fail(desk, CmdStatus::Unsure, &format!("Can't find {} here, and the dictionary said: {e}", quote(heard))),
    }
}

/// select_typed / delete_typed (VOICE_CONTROL_V2 2.3): the first strategy that holds wins.
/// 1. AX range: the field shows the typed text -> select exactly it (+ one Delete).
/// 2. Undo: the range was refused and nothing changed since susurro's paste -> one Cmd+Z.
/// 3. Word select: the value is not readable -> Opt+Shift+Left per word (+ one Delete).
/// 4. Terminal: one burst of backspaces, one per char.
///    The value is readable and the text is gone (Pablo changed it by hand) = do nothing (case 8).
fn typed_span(r: &mut Run, delete: bool, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    r.st.action = Some(if delete { "delete_typed" } else { "select_typed" }.into());
    let Some(i) = mem.iter().rposition(|e| e.e.kind == MemKind::Typed) else {
        return r.fail(desk, CmdStatus::Unsure, "I have not typed anything yet.");
    };
    let typed = mem[i].e.text.clone();
    r.st.detail = Some(typed.clone());
    let f = desk.focused();
    if f.secure || desk.secure_input() {
        return r.fail(desk, CmdStatus::Blocked, BLOCKED);
    }
    if desk.aborted() {
        return r.end(desk, CmdStatus::Stopped, None, String::new());
    }
    let what = if delete { "deleted" } else { "selected" };
    let ok = |r: &mut Run, desk: &mut dyn Desk, mem: &mut Memory, how: &str| {
        log::info!("live: {} {typed:?} by {how}", r.st.action.as_deref().unwrap_or(""));
        if delete {
            mem[i].e.kind = MemKind::Deleted;
        }
        r.done(desk, format!("{what} {}", quote(&typed)))
    };
    let act = |r: &mut Run, desk: &mut dyn Desk, a: Act| desk.act(&a).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")));
    let value = f.value.clone().filter(|_| !f.role.is_empty());
    // Another app has the focus now: the same words there are Pablo's, not susurro's.
    if f.pid != 0 && mem[i].pid != 0 && f.pid != mem[i].pid {
        return r.fail(desk, CmdStatus::Unsure, GONE);
    }
    // The blind strategies (backspaces, word select) cannot see the text: they are safe only while the caret is still
    // right after it. A key, click, paste or app switch since then (Return ran the command, a new tab, another
    // field) means the caret moved: they would delete Pablo's text.
    let field = if f.name.is_empty() { f.role.trim_start_matches("AX").to_string() } else { f.name.clone() };
    let moved = field != mem[i].e.field || mem.iter().skip(i + 1).any(|e| matches!(e.e.kind, MemKind::Key | MemKind::Clicked | MemKind::Pasted | MemKind::Opened));
    if terminal(&f) {
        if !delete {
            return r.fail(desk, CmdStatus::Unsure, "Terminals have no selection.");
        }
        if moved || value.as_ref().is_some_and(|v| !v.contains(typed.trim())) {
            return r.fail(desk, CmdStatus::Unsure, GONE);
        }
        if let Err(end) = act(r, desk, Act::Burst(DELETE, 0, typed.chars().count() as u32)) {
            return end;
        }
        return ok(r, desk, mem, "terminal backspaces");
    }
    match value {
        Some(v) => {
            let needle = if v.contains(&typed) { typed.as_str() } else { typed.trim() };
            let Some((start, len)) = find_range(&v, needle, f.sel.map(|s| s.0 + s.1)) else {
                return r.fail(desk, CmdStatus::Unsure, GONE);
            };
            if desk.set_selection(start, len) {
                if delete {
                    if let Err(end) = act(r, desk, Act::Combo(DELETE, 0)) {
                        return end;
                    }
                }
                return ok(r, desk, mem, "AX range");
            }
            if delete && mem[i].value_after.as_deref() == Some(v.as_str()) {
                if let Err(end) = act(r, desk, Act::Combo(KEY_Z, CMD)) {
                    return end;
                }
                return ok(r, desk, mem, "undo");
            }
            // The range was refused: select by words, but only when the caret sits right after the text.
            if f.sel.is_some_and(|(c, l)| c + l != start + len) {
                return r.fail(desk, CmdStatus::Unsure, "I can't select that text here.");
            }
        }
        None if moved => return r.fail(desk, CmdStatus::Unsure, GONE),
        None => {}
    }
    if let Err(end) = act(r, desk, Act::Burst(LEFT, OPT | SHIFT, words(&typed))) {
        return end;
    }
    if delete {
        if let Err(end) = act(r, desk, Act::Combo(DELETE, 0)) {
            return end;
        }
    }
    ok(r, desk, mem, "word select")
}

/// copy (VOICE_CONTROL_V2 2.4): a target's AXURL, else its AXValue, else a "Copy link" button click checked by the
/// pasteboard change count; no target = the selection (AXSelectedText, else Cmd+C).
fn copy(r: &mut Run, target: Option<&str>, ask: &mut Ask, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    r.st.action = Some("copy".into());
    let f = desk.focused();
    if f.secure {
        return r.fail(desk, CmdStatus::Blocked, BLOCKED);
    }
    let got = match target {
        Some(t) => {
            let h = match ground(r, "copy", t, ask, desk) {
                Ok(h) => h,
                Err(end) => return end,
            };
            aim(r, desk, &h);
            if let Some(s) = h.url.clone().or(h.value.clone()).filter(|s| !s.trim().is_empty()) {
                desk.set_clipboard(&s);
                Some(s)
            } else if h.mark.role == "button" || h.mark.name.to_lowercase().contains("cop") {
                // "Copy link" buttons: click, then the clipboard must have changed.
                let before = desk.clipboard().1;
                if desk.aborted() {
                    return r.end(desk, CmdStatus::Stopped, None, String::new());
                }
                if let Err(e) = desk.act(&Act::Click(h.target.clone())) {
                    return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
                }
                desk.sleep(SETTLE_MS);
                let (t, n) = desk.clipboard();
                t.filter(|_| n != before)
            } else {
                None
            }
        }
        None => match f.selected.clone().filter(|s| !s.is_empty()).or(f.url.clone()) {
            Some(s) => {
                desk.set_clipboard(&s);
                Some(s)
            }
            None => {
                let before = desk.clipboard().1;
                if desk.aborted() {
                    return r.end(desk, CmdStatus::Stopped, None, String::new());
                }
                if let Err(e) = desk.act(&Act::Combo(KEY_C, CMD)) {
                    return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
                }
                desk.sleep(COPY_WAIT_MS);
                let (t, n) = desk.clipboard();
                t.filter(|_| n != before)
            }
        },
    };
    match got {
        Some(s) => {
            remember(mem, MemKind::Copied, &s, &f, None);
            r.done(desk, format!("copied {}", quote(&s.chars().take(40).collect::<String>())))
        }
        None => r.fail(desk, CmdStatus::Unsure, "Nothing to copy there."),
    }
}

/// Polls the flags (50 ms): the command key or a spoken yes / no (confirm), the spoken answer (pick), Esc, the timeout.
pub fn wait_flags(w: Wait, timeout: Duration) -> Answer {
    CONFIRMED.store(false, Ordering::SeqCst);
    DENIED.store(false, Ordering::SeqCst);
    *ANSWER.lock().unwrap() = None;
    WAITING.store(if w == Wait::Confirm { 1 } else { 2 }, Ordering::SeqCst);
    let until = std::time::Instant::now() + timeout;
    let out = loop {
        if ABORT.load(Ordering::SeqCst) || DENIED.swap(false, Ordering::SeqCst) {
            break Answer::No;
        }
        if w == Wait::Confirm && CONFIRMED.swap(false, Ordering::SeqCst) {
            break Answer::Yes;
        }
        if let Some(t) = ANSWER.lock().unwrap().take() {
            break Answer::Text(t);
        }
        if std::time::Instant::now() >= until {
            break Answer::No;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    WAITING.store(0, Ordering::SeqCst);
    out
}

/// The end of a live session, for session.rs (overlay idle + the History row).
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// Everything heard, joined.
    pub instruction: String,
    pub end: End,
    pub latency_ms: u32,
}

#[cfg(target_os = "macos")]
pub mod mac {
    use super::*;
    use core_foundation::base::CFType;
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField, ScrollEventUnit};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use tauri::Manager;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn IsSecureEventInputEnabled() -> u8;
    }

    pub struct Real {
        app: AppHandle,
        handles: Vec<CFType>,
        els: Vec<El>,
        /// The focused element of the last `focused()` (set_selection acts on it).
        focus: Option<CFType>,
        /// The live session this desk belongs to.
        generation: u64,
    }

    impl Real {
        pub fn new(app: AppHandle) -> Self {
            Self { app, handles: vec![], els: vec![], focus: None, generation: GEN.load(Ordering::SeqCst) }
        }

        fn point(&self, t: &Target) -> Option<(f64, f64)> {
            match t {
                Target::Point(x, y) => Some((*x, *y)),
                Target::Mark(id) => self.els.iter().find(|e| e.mark.id == *id).map(|e| center(e.mark.rect)),
            }
        }

        fn handle(&self, t: &Target) -> Option<&CFType> {
            let Target::Mark(id) = t else { return None };
            self.els.iter().find(|e| e.mark.id == *id).and_then(|e| self.handles.get(e.h))
        }
    }

    fn displays() -> Vec<(PtRect, f64)> {
        CGDisplay::active_displays()
            .unwrap_or_default()
            .into_iter()
            .map(|id| {
                let d = CGDisplay::new(id);
                let b = d.bounds();
                let px = d.display_mode().map(|m| m.pixel_width() as f64).unwrap_or(b.size.width);
                (PtRect { x: b.origin.x, y: b.origin.y, w: b.size.width, h: b.size.height }, px / b.size.width.max(1.0))
            })
            .collect()
    }

    fn click_at(p: (f64, f64)) -> anyhow::Result<()> {
        let src = || CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| anyhow::anyhow!("no event source"));
        let pt = CGPoint::new(p.0, p.1);
        for (ty, click) in [(CGEventType::MouseMoved, false), (CGEventType::LeftMouseDown, true), (CGEventType::LeftMouseUp, true)] {
            let e = CGEvent::new_mouse_event(src()?, ty, pt, CGMouseButton::Left).map_err(|_| anyhow::anyhow!("mouse event"))?;
            if click {
                e.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
            }
            e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, crate::paste::SUSURRO_TAG);
            e.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// ponytail: 5 lines at the target (or the window under the mouse); whether a set location routes the wheel
    /// to the window there is not verified on a real app.
    fn scroll(down: bool, at: Option<(f64, f64)>) -> anyhow::Result<()> {
        let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| anyhow::anyhow!("no event source"))?;
        let e = CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 1, if down { -5 } else { 5 }, 0, 0).map_err(|_| anyhow::anyhow!("scroll event"))?;
        if let Some(p) = at {
            e.set_location(CGPoint::new(p.0, p.1));
        }
        e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, crate::paste::SUSURRO_TAG);
        e.post(CGEventTapLocation::HID);
        Ok(())
    }

    impl Desk for Real {
        fn learn_word(&mut self, word: &str, heard: &str) -> Result<(), String> {
            crate::learn::add_correction(&self.app, word, heard)
        }

        fn read(&mut self) -> Result<Screen, String> {
            if !crate::permissions::accessibility_trusted() {
                return Err("accessibility".into());
            }
            let front = crate::paste::target_app().ok_or("No app in front")?;
            let tree = ax::mac::read(front.pid).ok_or_else(|| format!("{} has no window to act on", front.name.clone().unwrap_or_default()))?;
            let els = ax::collect(&tree.root, tree.window);
            let (display, scale) = display_for(center(tree.window), &displays()).unwrap_or((tree.window, 1.0));
            self.handles = tree.handles;
            self.els = els.clone();
            Ok(Screen { app: front.name.unwrap_or_default(), display, scale, els })
        }

        fn focused(&mut self) -> Field {
            let front = crate::paste::target_app();
            let (mut f, h) = ax::mac::focused().map_or((Field::default(), None), |(f, h)| (f, Some(h)));
            self.focus = h;
            if let Some(a) = front {
                (f.app, f.bundle) = (a.name.unwrap_or_default(), a.bundle_id.unwrap_or_default());
                if f.pid == 0 {
                    f.pid = a.pid;
                }
            }
            f
        }

        fn set_selection(&mut self, start: usize, len: usize) -> bool {
            self.focus.as_ref().is_some_and(|h| ax::mac::set_selection(h, start, len))
        }

        fn clipboard(&mut self) -> (Option<String>, isize) {
            crate::paste::clipboard()
        }

        fn set_clipboard(&mut self, s: &str) {
            crate::paste::copy(s);
        }

        fn type_text(&mut self, s: &str) -> anyhow::Result<String> {
            let opts = self.app.state::<crate::AppState>().settings.read().unwrap().paste.clone();
            match crate::paste::paste_ex(s, &opts, None) {
                (crate::paste::PasteOutcome::Pasted, typed) => Ok(typed),
                (o, _) => anyhow::bail!("could not type: {o:?}"),
            }
        }

        fn screen_ok(&mut self) -> bool {
            objc2_core_graphics::CGPreflightScreenCaptureAccess()
        }

        fn capture(&mut self, r: PtRect, native: bool) -> anyhow::Result<Vec<u8>> {
            let path = std::env::temp_dir().join(format!("susurro-cmd-{}-{}.jpg", std::process::id(), crate::history::now_ms()));
            let rect = format!("-R{},{},{},{}", r.x.round(), r.y.round(), r.w.round(), r.h.round());
            let ok = std::process::Command::new("/usr/sbin/screencapture").args(["-x", "-t", "jpg", &rect]).arg(&path).status()?.success();
            anyhow::ensure!(ok, "screencapture failed");
            if !native {
                std::process::Command::new("/usr/bin/sips").args(["-Z", "1568"]).arg(&path).output()?;
            }
            let b = std::fs::read(&path);
            let _ = std::fs::remove_file(&path);
            Ok(b?)
        }

        fn act(&mut self, a: &Act) -> anyhow::Result<()> {
            #[cfg(debug_assertions)]
            if let Some(why) = crate::test_hooks::paste_blocked() {
                anyhow::bail!("test guard: {why}");
            }
            crate::live::own_input();
            match a {
                Act::Click(t) => {
                    if !self.handle(t).is_some_and(ax::mac::press) {
                        click_at(self.point(t).ok_or_else(|| anyhow::anyhow!("the element is gone"))?)?;
                    }
                }
                Act::Focus(t) => {
                    if !self.handle(t).is_some_and(ax::mac::focus) {
                        click_at(self.point(t).ok_or_else(|| anyhow::anyhow!("the element is gone"))?)?;
                    }
                }
                Act::Combo(code, flags) => crate::paste::combo(*code, *flags),
                Act::Burst(code, flags, n) => crate::paste::burst(*code, *flags, *n),
                Act::Scroll { down, at } => scroll(*down, at.as_ref().and_then(|t| self.point(t)))?,
                Act::OpenApp(name) => {
                    let ok = std::process::Command::new("/usr/bin/open").args(["-a", name]).status()?.success();
                    anyhow::ensure!(ok, "could not open {name}");
                    // Wait up to 2 s for it to come to the front, so the next action reads its window.
                    for _ in 0..40 {
                        if crate::paste::frontmost_app().and_then(|f| f.name).is_some_and(|n| n.eq_ignore_ascii_case(name)) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            Ok(())
        }

        fn secure_input(&mut self) -> bool {
            unsafe { IsSecureEventInputEnabled() != 0 }
        }

        fn secure_at(&mut self, p: (f64, f64)) -> bool {
            ax::mac::secure_at(p)
        }

        /// Rust owns the marks window: it shows on every step with marks; live.rs hides it (session end, took over).
        fn emit(&mut self, s: &CommandStep) {
            if !s.marks.is_empty() && !crate::live::took_over() {
                crate::platform::macos::show_marks(&self.app, Some(s.display));
            }
            crate::live::step_seen(&self.app, s);
            publish(&self.app, s.clone());
        }

        fn sleep(&mut self, ms: u64) {
            std::thread::sleep(Duration::from_millis(ms));
        }

        fn wait(&mut self, w: Wait, timeout: Duration) -> Answer {
            wait_flags(w, timeout)
        }

        fn aborted(&self) -> bool {
            ABORT.load(Ordering::SeqCst) || GEN.load(Ordering::SeqCst) != self.generation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::tests::n;
    use std::sync::{Arc, Mutex as M};

    #[test]
    fn gate_table() {
        use Gate::*;
        // (confidence, risky, confirm_risky) -> gate
        let rows: [(f32, f32, bool, Gate); 10] = [
            (0.95, 0.04, true, Act),
            (0.95, 0.04, false, Act),
            (0.95, 0.90, true, Confirm),
            (0.95, 0.90, false, Act),
            (0.95, 0.29, true, Act), // "risky >= 0.3" (VOICE_CONTROL.md)
            (0.95, 0.30, true, Confirm),
            (0.80, 0.0, true, Act), // the boundary acts
            (0.79, 0.0, true, Pick),
            (0.56, 0.99, true, Pick), // ask which one first; the confirm comes after the pick
            (0.10, 0.0, true, Unsure),
        ];
        for (c, r, cr, want) in rows {
            assert_eq!(gate(c, r, cr), want, "conf {c}, risky {r}, confirm {cr}");
        }
    }

    #[test]
    fn spatial_words() {
        for s in ["Open the second channel", "Go to the next page of emails", "Click the gear icon at the bottom", "abre el siguiente"] {
            assert!(spatial(s), "{s}");
        }
        for s in ["Send the message", "Open my DM with Ana", "Turn on dark mode"] {
            assert!(!spatial(s), "{s}");
        }
    }

    #[test]
    fn pick_answers() {
        let choices = [(20, 0.56), (19, 0.31), (18, 0.08)];
        let ids: Vec<u32> = (1..=43).collect();
        assert_eq!(parse_pick("19", &choices, &ids), Some(19));
        assert_eq!(parse_pick("Number 20.", &choices, &ids), Some(20));
        assert_eq!(parse_pick("twenty", &choices, &ids), Some(20));
        assert_eq!(parse_pick("the second one", &choices, &ids), Some(19), "an ordinal is a place in the list, not 'one'");
        assert_eq!(parse_pick("el tercero", &choices, &ids), Some(18));
        assert_eq!(parse_pick("dos", &choices, &ids), Some(2));
        assert_eq!(parse_pick("99", &choices, &ids), None, "not a mark");
        // names of the shown options (open risk c)
        let q = "Which one? 4 #general · 7 #design · 9 #random-stuff";
        let opts = pick_options(q);
        assert_eq!(opts, [(4, "#general".into()), (7, "#design".into()), (9, "#random-stuff".into())]);
        assert_eq!(pick_by_name("the design one", &opts), Some(7));
        assert_eq!(pick_by_name("Random stuff.", &opts), Some(9));
        assert_eq!(pick_by_name("ra", &opts), None, "too short to name one");
        assert_eq!(pick_by_name("type hello", &opts), None);
        assert!(looks_like_pick("el tercero", q) && looks_like_pick("General.", q) && looks_like_pick("20", q));
        assert!(!looks_like_pick("open slack and search for design reviews", q), "a full order");
        assert!(!looks_like_pick("type hello", q));
        assert_eq!(parse_pick("never mind", &choices, &ids), None);
    }

    const RETINA: PtRect = PtRect { x: 0.0, y: 0.0, w: 1512.0, h: 982.0 };

    #[test]
    fn tiles_and_box_points() {
        // Retina 1512x982 pt @2 = 3024x1964 px -> 2x2 tiles of 756x491 pt (1512x982 px, under 1568)
        let t = tiles(RETINA, 2.0);
        assert_eq!(t.len(), 4);
        assert_eq!(t[3], PtRect { x: 756.0, y: 491.0, w: 756.0, h: 491.0 });
        let mid = Bbox { top: 0.5, bottom: 0.5, left: 0.5, right: 0.5 };
        assert_eq!(box_point(mid, t[3]), (1134.0, 736.5));
        assert_eq!(box_point(mid, RETINA), (756.0, 491.0), "full display, any image size");
        // non-Retina 1920x1080 left of the main display: 2x1 tiles, negative x
        let left = PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let t = tiles(left, 1.0);
        assert_eq!(t.len(), 2);
        let p = box_point(Bbox { top: 0.1, bottom: 0.2, left: 0.1, right: 0.2 }, t[0]);
        assert!(p.0 < 0.0 && (p.0 - (-1920.0 + 0.15 * 960.0)).abs() < 1e-9, "{p:?}");
        // a display above the main one: negative y
        let above = PtRect { x: 0.0, y: -1080.0, w: 1920.0, h: 1080.0 };
        assert!(box_point(mid, above).1 < 0.0);
        // the research ultrawide 5092x2132 px @1: every tile <= 1568 px and >= 280 px a side
        for t in tiles(PtRect { x: 0.0, y: 0.0, w: 5092.0, h: 2132.0 }, 1.0) {
            assert!(t.w <= 1568.0 && t.h <= 1568.0 && t.w >= 280.0 && t.h >= 280.0, "{t:?}");
        }
    }

    #[test]
    fn display_for_a_point() {
        let ds = [(RETINA, 2.0), (PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 }, 1.0)];
        assert_eq!(display_for((-100.0, 500.0), &ds).unwrap().1, 1.0);
        assert_eq!(display_for((100.0, 500.0), &ds).unwrap().1, 2.0);
        assert_eq!(display_for((9999.0, 0.0), &ds).unwrap().1, 2.0, "off every display: the first");
    }

    #[test]
    fn box_shapes() {
        assert!(!sane(Bbox { top: 0.03, bottom: 0.97, left: 0.91, right: 0.97 }), "research call 4 clock strip");
        assert!(!sane(Bbox { top: 0.5, bottom: 0.4, left: 0.1, right: 0.2 }));
        assert!(!sane(Bbox { top: 0.5, bottom: 0.5, left: 0.1, right: 0.2 }), "zero area");
        assert!(sane(Bbox { top: 0.66, bottom: 0.87, left: 0.43, right: 0.46 }), "research call 3 Slack dock");
        assert!(sane(Bbox { top: 0.07, bottom: 0.14, left: 0.82, right: 0.96 }));
    }

    fn el(id: u32, name: &str, role: &str, r: (f64, f64, f64, f64)) -> El {
        El { mark: Mark { id, rect: PtRect { x: r.0, y: r.1, w: r.2, h: r.3 }, name: name.into(), role: role.into(), region: "main".into() }, state: None, h: id as usize, url: None, value: None }
    }

    #[test]
    fn snap_to_the_nearest_element() {
        let els = [el(1, "a", "button", (100.0, 100.0, 20.0, 20.0)), el(2, "b", "button", (200.0, 100.0, 20.0, 20.0))];
        assert_eq!(snap((110.0, 110.0), &els).unwrap().mark.id, 1, "inside");
        assert_eq!(snap((135.0, 110.0), &els).unwrap().mark.id, 1, "15 pt right of it");
        assert!(snap((160.0, 110.0), &els).is_none(), "40 pt from both");
    }


    // ---------- the executor on a fake desk (every vc-feedback.md case, VOICE_CONTROL_V2 section 8) ----------

    /// One text field of the fake desk. ASCII in the tests, so UTF-16 units = bytes.
    #[derive(Clone, Default)]
    struct FField {
        role: String,
        name: String,
        value: String,
        caret: usize,
        sel: usize,
        /// AXSelectedTextRange is settable.
        range_ok: bool,
        /// AXValue is readable (Electron: no).
        readable: bool,
        secure: bool,
        undo: Vec<String>,
        /// The mark that focuses this field.
        mark: Option<u32>,
    }

    impl FField {
        fn new(role: &str, name: &str, value: &str, mark: Option<u32>) -> Self {
            Self { role: role.into(), name: name.into(), value: value.into(), caret: value.len(), range_ok: true, readable: true, mark, ..Default::default() }
        }

        fn insert(&mut self, s: &str) {
            self.undo.push(self.value.clone());
            self.value.replace_range(self.caret..self.caret + self.sel, s);
            (self.caret, self.sel) = (self.caret + s.len(), 0);
        }

        fn backspace(&mut self, n: usize) {
            if self.sel > 0 {
                self.value.replace_range(self.caret..self.caret + self.sel, "");
                self.sel = 0;
            } else {
                let from = self.caret.saturating_sub(n);
                self.value.replace_range(from..self.caret, "");
                self.caret = from;
            }
        }
    }

    #[derive(Default)]
    struct FakeDesk {
        app: String,
        fields: Vec<FField>,
        focus: usize,
        clip: (Option<String>, isize),
        /// Clicking this mark copies this text (a "Copy link" button).
        copy_button: Option<(u32, String)>,
        screens: Vec<Screen>,
        reads: usize,
        acts: Vec<Act>,
        typed: Vec<String>,
        selections: Vec<(usize, usize)>,
        sleeps: Vec<u64>,
        tabs: u32,
        steps: Vec<CommandStep>,
        answers: Vec<Answer>,
        waits: Vec<Wait>,
        screen_ok: bool,
        abort_after_acts: Option<usize>,
    }

    impl FakeDesk {
        fn f(&mut self) -> &mut FField {
            &mut self.fields[self.focus]
        }

        fn value(&self, i: usize) -> &str {
            &self.fields[i].value
        }

        fn focus_mark(&mut self, id: u32) {
            if let Some(i) = self.fields.iter().position(|f| f.mark == Some(id)) {
                self.focus = i;
            }
        }
    }

    impl Desk for FakeDesk {
        fn read(&mut self) -> Result<Screen, String> {
            self.reads += 1;
            let i = (self.reads - 1).min(self.screens.len().saturating_sub(1));
            self.screens.get(i).cloned().ok_or_else(|| "no window".into())
        }
        fn focused(&mut self) -> Field {
            let app = self.app.clone();
            let f = &self.fields[self.focus];
            let sel = f.value.get(f.caret..f.caret + f.sel).map(String::from).filter(|s| !s.is_empty());
            Field {
                app,
                pid: 1,
                role: f.role.clone(),
                name: f.name.clone(),
                value: (f.readable && !f.secure).then(|| f.value.clone()),
                sel: f.readable.then_some((f.caret, f.sel)),
                selected: sel.filter(|_| f.readable),
                secure: f.secure,
                ..Default::default()
            }
        }
        fn set_selection(&mut self, start: usize, len: usize) -> bool {
            self.selections.push((start, len));
            let f = self.f();
            if f.range_ok {
                (f.caret, f.sel) = (start, len);
            }
            f.range_ok
        }
        fn clipboard(&mut self) -> (Option<String>, isize) {
            self.clip.clone()
        }
        fn set_clipboard(&mut self, s: &str) {
            self.clip = (Some(s.into()), self.clip.1 + 1);
        }
        fn type_text(&mut self, s: &str) -> anyhow::Result<String> {
            assert!(!self.aborted(), "typed after the abort");
            self.typed.push(s.into());
            self.f().insert(s);
            Ok(s.into())
        }
        fn screen_ok(&mut self) -> bool {
            self.screen_ok
        }
        fn capture(&mut self, _: PtRect, _: bool) -> anyhow::Result<Vec<u8>> {
            Ok(b"\xff\xd8jpeg".to_vec())
        }
        fn act(&mut self, a: &Act) -> anyhow::Result<()> {
            assert!(!self.aborted(), "an action ran after the abort");
            self.acts.push(a.clone());
            match a.clone() {
                Act::Focus(Target::Mark(id)) => self.focus_mark(id),
                Act::Click(Target::Mark(id)) => {
                    self.focus_mark(id);
                    if let Some((_, t)) = self.copy_button.clone().filter(|b| b.0 == id) {
                        self.set_clipboard(&t);
                    }
                }
                Act::Combo(DELETE, 0) => self.f().backspace(1),
                Act::Burst(DELETE, 0, n) => self.f().backspace(n as usize),
                Act::Combo(KEY_Z, CMD) => {
                    let f = self.f();
                    if let Some(v) = f.undo.pop() {
                        (f.caret, f.sel) = (v.len(), 0);
                        f.value = v;
                    }
                }
                Act::Combo(0, CMD) => {
                    let f = self.f();
                    (f.caret, f.sel) = (0, f.value.len());
                }
                Act::Combo(KEY_C, CMD) => {
                    let f = &self.fields[self.focus];
                    if f.sel > 0 {
                        let t = f.value[f.caret..f.caret + f.sel].to_string();
                        self.set_clipboard(&t);
                    }
                }
                Act::Combo(KEY_V, CMD) => {
                    let t = self.clip.0.clone().unwrap_or_default();
                    self.f().insert(&t);
                }
                Act::Combo(17, CMD) => self.tabs += 1,
                Act::Burst(LEFT, f, n) if f == OPT | SHIFT => {
                    // select n words left of the caret
                    let fl = self.f();
                    let mut start = fl.caret;
                    for _ in 0..n {
                        let b = fl.value.as_bytes();
                        while start > 0 && !b[start - 1].is_ascii_alphanumeric() {
                            start -= 1;
                        }
                        while start > 0 && b[start - 1].is_ascii_alphanumeric() {
                            start -= 1;
                        }
                    }
                    (fl.sel, fl.caret) = (fl.caret - start, start);
                }
                Act::OpenApp(a) => self.app = a,
                _ => {}
            }
            Ok(())
        }
        fn secure_input(&mut self) -> bool {
            self.fields[self.focus].secure
        }
        fn secure_at(&mut self, _: (f64, f64)) -> bool {
            false
        }
        fn emit(&mut self, s: &CommandStep) {
            self.steps.push(s.clone());
        }
        fn sleep(&mut self, ms: u64) {
            self.sleeps.push(ms);
        }
        fn wait(&mut self, w: Wait, _: Duration) -> Answer {
            self.waits.push(w);
            if self.answers.is_empty() { Answer::No } else { self.answers.remove(0) }
        }
        fn aborted(&self) -> bool {
            self.abort_after_acts.is_some_and(|n| self.acts.len() >= n)
        }
    }

    fn mk(id: u32, name: &str, role: &str, region: &str, url: Option<&str>, value: Option<&str>) -> El {
        let r = PtRect { x: 100.0 * id as f64, y: 50.0, w: 80.0, h: 24.0 };
        El { mark: Mark { id, rect: r, name: name.into(), role: role.into(), region: region.into() }, state: None, h: id as usize, url: url.map(Into::into), value: value.map(Into::into) }
    }

    const LINK: &str = "https://chatgpt.com/share/6a1f";

    /// The bench's Chrome screen (`.lo/planner-bench/planner_bench.py`): two Share buttons, a link field.
    fn chrome() -> Screen {
        let els = vec![
            mk(1, "Back", "button", "toolbar", None, None),
            mk(2, "Share", "button", "toolbar", None, None),
            mk(3, "Search", "search field", "toolbar", None, None),
            mk(4, LINK, "text field", "main", None, Some(LINK)),
            mk(5, "Copy link", "button", "main", None, None),
            mk(6, "Public", "radio button", "main", None, None),
            mk(7, "Message", "text area", "bottom", None, None),
            mk(8, "Share", "button", "main", None, None),
            mk(9, "Send", "button", "bottom", None, None),
        ];
        Screen { app: "Google Chrome".into(), display: PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }, scale: 2.0, els }
    }

    fn chrome_desk() -> FakeDesk {
        FakeDesk {
            app: "Google Chrome".into(),
            fields: vec![FField::new("AXTextArea", "Notes", "meet at 5", None), FField::new("AXSearchField", "Search", "", Some(3)), FField::new("AXTextArea", "Message", "", Some(7))],
            screens: vec![chrome()],
            ..Default::default()
        }
    }

    fn ghostty() -> FakeDesk {
        let mut f = FField::new("AXTextArea", "", "pablo@mac ~ % ", None);
        f.range_ok = false; // shells have no selection
        FakeDesk { app: "Ghostty".into(), fields: vec![f], ..Default::default() }
    }

    fn act(op: &str) -> Action {
        Action { op: op.into(), ..Default::default() }
    }
    fn key(k: &str) -> Action {
        Action { op: "key".into(), keys: Some(k.into()), ..Default::default() }
    }
    fn typ(t: &str, target: Option<&str>) -> Action {
        Action { op: "type".into(), text: Some(t.into()), target: target.map(Into::into), ..Default::default() }
    }
    fn on(op: &str, target: &str) -> Action {
        Action { op: op.into(), target: Some(target.into()), ..Default::default() }
    }

    const CFG: Cfg = Cfg { confirm_risky: true };

    /// No Jev call expected (local matches only).
    fn no_jev(_: &Value, _: &Value) -> anyhow::Result<Reply> {
        panic!("Jev was called")
    }

    /// Runs the actions in order like the live executor: a stopped action drops the rest.
    fn run_all(acts: &[Action], d: &mut FakeDesk, mem: &mut Memory) -> Vec<End> {
        let mut out = vec![];
        for (i, a) in acts.iter().enumerate() {
            let e = exec(a, i as u32 + 1, 1, &CFG, &mut no_jev, d, mem);
            let stop = e.status != CmdStatus::Done;
            out.push(e);
            if stop {
                break;
            }
        }
        out
    }

    fn statuses(e: &[End]) -> Vec<CmdStatus> {
        e.iter().map(|e| e.status.clone()).collect()
    }

    #[test]
    fn type_pastes_planner_text_verbatim() {
        // case 1: the executor never re-cases; the planner wrote "ls"
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[typ("ls", None)], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.typed, ["ls"]);
        assert_eq!(d.value(0), "pablo@mac ~ % ls");
        assert!(d.acts.is_empty(), "no key events for a plain type");
        assert_eq!(mem.back().unwrap().e.kind, MemKind::Typed);
    }

    #[test]
    fn return_waits_for_confirm_then_voice_si_resolves() {
        // case 2: "run ls" = type + return, Return always confirms
        let (mut d, mut mem) = (ghostty(), Memory::new());
        d.answers = vec![Answer::Yes];
        let e = run_all(&[typ("ls", None), key("return")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Done]);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert_eq!(d.acts, [Act::Combo(36, 0)]);
        assert!(d.steps.iter().any(|s| s.status == CmdStatus::Confirm && s.message.as_deref().is_some_and(|m| m.contains("Return"))));
        assert_eq!(yes_no("Sí."), Some(true));
        assert_eq!(yes_no("dale"), Some(true));
        assert_eq!(yes_no("No."), Some(false));
        assert_eq!(yes_no("open slack"), None);
    }

    #[test]
    fn a_typed_newline_never_bypasses_the_return_confirm() {
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[typ("ls\n", None), typ("pwd\r\n", None)], &mut d, &mut mem);
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 2]);
        assert_eq!(d.typed, ["ls", "pwd"]);
        assert!(d.waits.is_empty() && d.acts.is_empty());
    }

    #[test]
    fn return_no_answer_in_10s_stops() {
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[key("enter")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Stopped]);
        assert!(d.acts.is_empty(), "nothing pressed without a yes");
    }

    #[test]
    fn one_order_runs_its_actions_in_order() {
        // case 3: "create a new tab and run the command ls"
        let (mut d, mut mem) = (ghostty(), Memory::new());
        d.answers = vec![Answer::Yes];
        let e = run_all(&[key("cmd+t"), typ("ls", None), key("return")], &mut d, &mut mem);
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 3]);
        assert_eq!(d.acts, [Act::Combo(17, CMD), Act::Combo(36, 0)]);
        assert_eq!((d.tabs, d.typed.as_slice()), (1, &["ls".to_string()][..]));
        let orders: Vec<(u32, Option<String>)> = d.steps.iter().filter(|s| s.status == CmdStatus::Done).map(|s| (s.step, s.op.clone())).collect();
        assert_eq!(orders, [(1, Some("key".into())), (2, Some("type".into())), (3, Some("key".into()))]);
    }

    fn fix(heard: &str, text: &str) -> Action {
        Action { op: "correct_word".into(), target: Some(heard.into()), text: Some(text.into()), ..Default::default() }
    }

    #[test]
    fn correct_word_fixes_the_last_heard_word_and_spelled_letters_win() {
        assert_eq!(spelled("J E V"), "Jev");
        assert_eq!(spelled("j-e-v"), "Jev");
        assert_eq!(spelled("J. E. V."), "Jev");
        assert_eq!(spelled("T y p e S a f e"), "TypeSafe");
        assert_eq!(spelled("Jev"), "Jev");
        assert_eq!(spelled("jota e ve"), "Jev");
        assert_eq!(spelled("jay, ee, vee"), "Jev");
        assert_eq!(spelled("de a ve"), "Dav");
        assert_eq!(spelled("el modelo"), "el modelo", "not all letter names");
        assert_eq!(spelled("Eleven Labs"), "Eleven Labs");
        assert_eq!(find_word("Jeff dijo que jeff, no Jeffrey", "Jeff", None), Some((14, 4)));
        assert_eq!(find_word("Jeff dijo que jeff", "jeff", Some(4)), Some((0, 4)), "the one before the caret");
        assert_eq!(find_word("Jeffrey", "Jeff", None), None, "whole words only");
        assert_eq!(find_word("Pérez y perez", "PEREZ", None), Some((8, 5)));
        // "corrige: Jeff es Jev, J E V" after a dictation "Le pregunte a Jeff por el modelo."
        let mut d = FakeDesk { app: "Slack".into(), fields: vec![FField::new("AXTextArea", "Message", "Le pregunte a Jeff por el modelo.", None)], ..Default::default() };
        d.fields[0].caret = d.fields[0].value.len();
        let mut mem = Memory::new();
        let e = run_all(&[fix("Jeff", "J E V")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.value(0), "Le pregunte a Jev por el modelo.");
        assert_eq!(d.typed, ["Jev"]);
        assert_eq!(e[0].summary, "fixed “Jeff” → “Jev”");
        // not in the field: the dictionary still learns it, the step says so
        let mut d = FakeDesk { app: "Slack".into(), fields: vec![FField::new("AXTextArea", "Message", "nothing here", None)], ..Default::default() };
        let e = run_all(&[fix("GAB", "Jev")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert!(e[0].message.as_deref().is_some_and(|m| m.contains("Jev is in your dictionary")), "{:?}", e[0].message);
        assert!(d.typed.is_empty() && d.value(0) == "nothing here");
        // the same word twice, or no word: ask again
        let e = run_all(&[fix("Jev", "jev")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
        // a password field is never touched
        let mut d = FakeDesk { app: "Safari".into(), fields: vec![FField { secure: true, ..FField::new("AXSecureTextField", "Password", "Jeff", None) }], ..Default::default() };
        assert_eq!(statuses(&run_all(&[fix("Jeff", "Jev")], &mut d, &mut mem)), [CmdStatus::Blocked]);
        assert!(d.typed.is_empty());
    }

    #[test]
    fn delete_typed_uses_memory_ax_range() {
        // case 4: field "hi ", susurro typed "ls -" -> "hi ls -"; delete_typed selects exactly it, one Delete
        let mut d = FakeDesk { app: "TextEdit".into(), fields: vec![FField::new("AXTextArea", "", "hi ", None)], ..Default::default() };
        let mut mem = Memory::new();
        run_all(&[typ("ls -", None)], &mut d, &mut mem);
        assert_eq!(d.value(0), "hi ls -");
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.value(0), "hi ");
        assert_eq!(d.selections, [(3, 4)]);
        assert_eq!(d.acts, [Act::Combo(DELETE, 0)], "one AX call + one key, never N deletes");
        assert_eq!(mem.back().unwrap().e.kind, MemKind::Deleted);
    }

    #[test]
    fn second_delete_takes_the_entry_before() {
        let mut d = FakeDesk { app: "Notes".into(), fields: vec![FField::new("AXTextArea", "", "", None)], ..Default::default() };
        let mut mem = Memory::new();
        run_all(&[typ("hello", None), typ(" world", None)], &mut d, &mut mem);
        assert_eq!(d.value(0), "hello world");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(d.value(0), "hello");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(d.value(0), "");
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Unsure], "nothing typed is left");
    }

    #[test]
    fn correct_ls_deletes_then_types() {
        // log 20:59: "type ls minus, remove what you inputted and write ls"
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[typ("LS", None), act("delete_typed"), typ("ls", None)], &mut d, &mut mem);
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 3]);
        assert_eq!(d.value(0), "pablo@mac ~ % ls");
        assert_eq!(d.acts, [Act::Burst(DELETE, 0, 2)]);
    }

    #[test]
    fn type_with_target_focuses_target_then_types_content_only() {
        // case 5: "type hello onto the search input": text "hello", target the search field
        let (mut d, mut mem) = (chrome_desk(), Memory::new());
        let e = run_all(&[typ("hello", Some("Search (search field, toolbar)"))], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.acts, [Act::Focus(Target::Mark(3))]);
        assert_eq!((d.value(1), d.value(0)), ("hello", "meet at 5"));
        assert!(e[0].summary.contains("into “Search”"), "{}", e[0].summary);
        // "name (role)" and the bare name match too
        for t in ["Search (search field)", "search", "“Search”"] {
            assert_eq!(local_match(t, &chrome().els).map(|e| e.mark.id), Some(3), "{t}");
        }
        assert_eq!(local_match("Share", &chrome().els), None, "two Share buttons: Jev decides");
        assert_eq!(local_match("Share (button, toolbar)", &chrome().els).map(|e| e.mark.id), Some(2));
    }

    #[test]
    fn cmd_a_then_delete_empties_the_field() {
        // case 6
        let mut d = FakeDesk { app: "Google Chrome".into(), fields: vec![FField::new("AXSearchField", "Search", "math coding hello", None)], ..Default::default() };
        let e = run_all(&[key("cmd+a"), key("delete")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 2]);
        assert_eq!(d.value(0), "");
        assert_eq!(d.acts, [Act::Combo(0, CMD), Act::Combo(DELETE, 0)]);
    }

    #[test]
    fn select_copy_focus_paste_moves_the_text() {
        // case 7: "select the text that is there and paste it in the search box"
        let (mut d, mut mem) = (chrome_desk(), Memory::new());
        let e = run_all(&[key("cmd+a"), act("copy"), on("paste", "Search (search field, toolbar)")], &mut d, &mut mem);
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 3]);
        assert_eq!(d.clip.0.as_deref(), Some("meet at 5"));
        assert_eq!(d.value(1), "meet at 5");
        assert_eq!(d.acts, [Act::Combo(0, CMD), Act::Focus(Target::Mark(3)), Act::Combo(KEY_V, CMD)], "AXSelectedText: no Cmd+C needed");
        let kinds: Vec<MemKind> = mem.iter().map(|e| e.e.kind.clone()).collect();
        assert_eq!(kinds, [MemKind::Key, MemKind::Copied, MemKind::Pasted]);
    }

    #[test]
    fn copy_without_ax_selection_uses_cmd_c_and_checks_the_change_count() {
        let mut d = chrome_desk();
        d.fields[0].readable = false; // Electron: no AXSelectedText
        d.fields[0].sel = 9;
        d.fields[0].caret = 0;
        let e = run_all(&[act("copy")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.acts, [Act::Combo(KEY_C, CMD)]);
        assert_eq!(d.clip.0.as_deref(), Some("meet at 5"));
        // nothing selected: the count does not move = unsure, the old clipboard is not reported as copied
        let mut d = chrome_desk();
        d.fields[0].readable = false;
        d.clip = (Some("old".into()), 7);
        let e = run_all(&[act("copy")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
    }

    #[test]
    fn reread_before_every_action() {
        // case 8: susurro typed "hello", Pablo cleared the field by hand -> delete_typed does nothing
        let mut d = FakeDesk { app: "WhatsApp".into(), fields: vec![FField::new("AXTextArea", "Type a message", "", None)], ..Default::default() };
        let mut mem = Memory::new();
        run_all(&[typ("hello", None)], &mut d, &mut mem);
        d.fields[0].value.clear();
        d.fields[0].caret = 0;
        let before = d.acts.len();
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
        assert_eq!(e[0].message.as_deref(), Some(GONE));
        assert_eq!(d.acts.len(), before, "zero events");
        assert!(d.selections.is_empty());
    }

    #[test]
    fn marks_ids_never_cross_actions() {
        // "click Share, then Public": Public exists only after the Share click, with an id from that later read
        let mut after = chrome();
        after.els.insert(0, mk(10, "Dialog title", "button", "main", None, None));
        for (i, e) in after.els.iter_mut().enumerate() {
            e.mark.id = i as u32 + 1;
        }
        let mut d = chrome_desk();
        d.screens = vec![chrome(), after];
        let e = run_all(&[on("click", "Share (button, toolbar)"), on("click", "Public (radio button, main)")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 2]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2)), Act::Click(Target::Mark(7))], "Public is 7 on the 2nd read");
        assert_eq!(d.reads, 2, "one read per action");
    }

    #[test]
    fn copy_link_reads_axurl_without_keys() {
        // case 14
        let mut s = chrome();
        s.els[0].url = Some("https://example.com/back".into());
        let mut d = chrome_desk();
        d.screens = vec![s];
        let e = run_all(&[on("copy", "Back (button, toolbar)")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.clip.0.as_deref(), Some("https://example.com/back"));
        assert!(d.acts.is_empty(), "0 key or click events");
        // a link field: its value
        let mut d = chrome_desk();
        run_all(&[on("copy", &format!("{LINK} (text field, main)"))], &mut d, &mut Memory::new());
        assert_eq!(d.clip.0.as_deref(), Some(LINK));
    }

    #[test]
    fn copy_link_button_click_checks_change_count() {
        let mut d = chrome_desk();
        d.copy_button = Some((5, LINK.into()));
        let e = run_all(&[on("copy", "Copy link (button, main)")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
        assert_eq!(d.clip.0.as_deref(), Some(LINK));
        // the button did nothing: unsure
        let mut d = chrome_desk();
        let e = run_all(&[on("copy", "Copy link (button, main)")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
    }

    #[test]
    fn type_between_copy_and_paste_keeps_the_link() {
        let (mut d, mut mem) = (chrome_desk(), Memory::new());
        d.answers = vec![];
        let e = run_all(
            &[on("copy", &format!("{LINK} (text field, main)")), typ("hi", Some("Search (search field, toolbar)")), on("paste", "Message (text area, bottom)")],
            &mut d,
            &mut mem,
        );
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 3]);
        assert_eq!((d.value(1), d.value(2)), ("hi", LINK));
        assert_eq!(mem.back().unwrap().e.text, LINK);
    }

    #[test]
    fn delete_typed_strategy_table() {
        // case 15: (desk setup, typed, expected events, expected value after)
        let typed = |d: &mut FakeDesk, mem: &mut Memory, t: &str| {
            run_all(&[typ(t, None)], d, mem);
            (d.acts.clear(), d.selections.clear(), d.sleeps.clear());
        };
        // 1. AX range: 2 events (the selection + one Delete)
        let mut d = FakeDesk { app: "TextEdit".into(), fields: vec![FField::new("AXTextArea", "", "a ", None)], ..Default::default() };
        let mut mem = Memory::new();
        typed(&mut d, &mut mem, "hola mundo");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!((d.selections.len(), d.acts.as_slice(), d.value(0)), (1, &[Act::Combo(DELETE, 0)][..], "a "));
        // 2. undo: the range is refused, nothing changed since the paste -> one Cmd+Z
        let mut d = FakeDesk { app: "TextEdit".into(), fields: vec![FField { range_ok: false, ..FField::new("AXTextArea", "", "a ", None) }], ..Default::default() };
        let mut mem = Memory::new();
        typed(&mut d, &mut mem, "hola mundo");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!((d.acts.as_slice(), d.value(0)), (&[Act::Combo(KEY_Z, CMD)][..], "a "));
        // 3. word select: the value is not readable -> Opt+Shift+Left per word, one Delete
        let mut d = FakeDesk { app: "Slack".into(), fields: vec![FField { readable: false, ..FField::new("AXTextArea", "", "a ", None) }], ..Default::default() };
        let mut mem = Memory::new();
        typed(&mut d, &mut mem, "hola mundo");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(d.acts, [Act::Burst(LEFT, OPT | SHIFT, 2), Act::Combo(DELETE, 0)]);
        assert_eq!(d.fields[0].value, "a ");
        // 4. terminal: one burst of backspaces, no settle sleep around it
        let (mut d, mut mem) = (ghostty(), Memory::new());
        typed(&mut d, &mut mem, "ls -la");
        run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(d.acts, [Act::Burst(DELETE, 0, 6)]);
        assert!(d.sleeps.iter().all(|ms| *ms <= 5), "{:?}", d.sleeps);
        assert_eq!(d.value(0), "pablo@mac ~ % ");
        // select_typed in a terminal: unsure, no events
        typed(&mut d, &mut mem, "ls");
        let e = run_all(&[act("select_typed")], &mut d, &mut mem);
        assert_eq!((statuses(&e), d.acts.len()), (vec![CmdStatus::Unsure], 0));
        // select_typed in a field: the selection, no Delete
        let mut d = FakeDesk { app: "TextEdit".into(), fields: vec![FField::new("AXTextArea", "", "x ", None)], ..Default::default() };
        let mut mem = Memory::new();
        typed(&mut d, &mut mem, "abc");
        run_all(&[act("select_typed")], &mut d, &mut mem);
        assert_eq!((d.selections.as_slice(), d.acts.len(), d.fields[0].sel), (&[(2, 3)][..], 0, 3));
    }

    #[test]
    fn delete_typed_never_backspaces_past_what_susurro_typed() {
        // "run ls" then "remove what you typed": Return ran the command, the new prompt holds Pablo's "git"
        let (mut d, mut mem) = (ghostty(), Memory::new());
        d.answers = vec![Answer::Yes];
        run_all(&[typ("ls", None), key("return")], &mut d, &mut mem);
        d.fields[0].insert("\npablo@mac ~ % git");
        let n = d.acts.len();
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!((statuses(&e), e[0].message.as_deref()), (vec![CmdStatus::Unsure], Some(GONE)));
        assert_eq!(d.acts.len(), n, "no backspace");
        assert!(d.value(0).ends_with("% git"));
        // Slack (no readable value), then a click on another chat: the word select would eat Pablo's words
        let mut d = chrome_desk();
        d.fields[2].readable = false;
        d.focus = 2;
        let mut mem = Memory::new();
        run_all(&[typ("hola mundo", None), on("click", "Search (search field, toolbar)")], &mut d, &mut mem);
        d.focus = 2;
        let n = d.acts.len();
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
        assert_eq!(d.acts.len(), n);
        // another app in front with the same words: not susurro's
        let mut d = FakeDesk { app: "TextEdit".into(), fields: vec![FField::new("AXTextArea", "", "", None)], ..Default::default() };
        let mut mem = Memory::new();
        run_all(&[typ("hello", None)], &mut d, &mut mem);
        mem.back_mut().unwrap().pid = 2;
        let e = run_all(&[act("delete_typed")], &mut d, &mut mem);
        assert_eq!((statuses(&e), d.value(0)), (vec![CmdStatus::Unsure], "hello"));
    }

    #[test]
    fn words_count_table() {
        for (t, n) in [("ls", 1), ("ls -la", 2), ("hola mundo", 2), ("https://chatgpt.com/share/6a1f", 5), ("", 1), ("  hi.", 1), ("a_b c", 2), ("über straße", 2)] {
            assert_eq!(words(t), n, "{t:?}");
        }
        assert_eq!(find_range("ls ls", "ls", Some(2)), Some((0, 2)), "the match before the caret");
        assert_eq!(find_range("ls ls", "ls", None), Some((3, 2)), "else the last");
        assert_eq!(find_range("é ls", "ls", None), Some((2, 2)), "UTF-16 units, not bytes");
        assert_eq!(find_range("abc", "x", None), None);
    }

    #[test]
    fn blocked_combos_and_secure_fields_never_act() {
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[key("cmd+shift+q")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Blocked]);
        let e = run_all(&[key("hyper+x")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Unsure]);
        let mut d = chrome_desk();
        d.fields[1].secure = true;
        let e = run_all(&[typ("hunter2", Some("Search (search field, toolbar)"))], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Blocked]);
        assert!(d.typed.is_empty());
        assert_eq!(d.acts, [Act::Focus(Target::Mark(3))], "the focus ran, the text did not");
    }

    #[test]
    fn abort_is_checked_before_every_event() {
        let (mut d, mut mem) = (chrome_desk(), Memory::new());
        d.abort_after_acts = Some(1);
        let e = run_all(&[key("cmd+a"), key("delete"), typ("x", None)], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Stopped]);
        assert_eq!(d.acts.len(), 1);
    }

    #[test]
    fn risky_click_waits_for_a_yes() {
        let (mut d, mut mem) = (chrome_desk(), Memory::new());
        let e = run_all(&[on("click", "Send (button, bottom)")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Stopped], "Send = risky by name, no answer = no click");
        assert!(d.acts.is_empty());
        let mut d = chrome_desk();
        d.answers = vec![Answer::Yes];
        let e = run_all(&[on("click", "Send (button, bottom)")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(9))]);
        let mut d = chrome_desk();
        let e = run_all(&[Action { risky: true, ..key("cmd+backspace") }], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Stopped], "the planner's risky flag confirms too");
    }

    fn jev_target(choice: &str, conf: f32, risky: f32) -> Value {
        json!({ "target": { "choice": choice, "probabilities": { choice: conf, "6": 0.2, "1": 0.05 }, "confidence": conf }, "risky": { "noul": risky } })
    }

    /// An `ask` that plays the replies in order and records each request.
    fn script(replies: Vec<Value>) -> (impl FnMut(&Value, &Value) -> anyhow::Result<Reply>, Arc<M<Vec<(Value, Value)>>>) {
        let seen = Arc::new(M::new(vec![]));
        let s2 = seen.clone();
        let mut it = replies.into_iter();
        (
            move |s: &Value, q: &Value| {
                s2.lock().unwrap().push((s.clone(), q.clone()));
                it.next().map(|a| Reply { answers: a, cost: 0.0001, latency_ms: 700 }).ok_or_else(|| anyhow::anyhow!("no more scripted replies"))
            },
            seen,
        )
    }

    #[test]
    fn words_off_screen_go_to_jev_and_a_weak_answer_asks_which_one() {
        // "the share button in the top header": not an exact label -> Jev, text only
        let (mut ask, seen) = script(vec![jev_target("2", 0.93, 0.05)]);
        let mut d = chrome_desk();
        let e = exec(&on("click", "share button in the top header"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        let q = &seen.lock().unwrap()[0].1;
        assert_eq!(q["target"]["criteria"]["8"], "Share (button, main)");
        assert!(q.get("action").is_none(), "grounding asks only target + risky");
        // weak: the screenshot retry (screen ok), still weak -> pick by voice "la dos" -> mark 2
        let (mut ask, seen) = script(vec![jev_target("8", 0.5, 0.0), jev_target("8", 0.55, 0.0)]);
        let mut d = chrome_desk();
        d.screen_ok = true;
        d.answers = vec![Answer::Text("el segundo".into())];
        let e = exec(&on("click", "the share one"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(d.waits, [Wait::Pick]);
        assert_eq!(e.status, CmdStatus::Done);
        assert!(d.steps.iter().any(|s| s.status == CmdStatus::Pick && s.choices.len() == 3));
        assert_eq!(d.acts, [Act::Click(Target::Mark(6))], "the 2nd of the ranked list (8, 6, 1)");
    }

    #[test]
    fn ax_tree_to_marks_request() {
        let tree = n("AXWindow", "", (0.0, 0.0, 1440.0, 900.0), (0..6).map(|i| n("AXButton", &format!("B{i}"), (100.0 * i as f64 + 400.0, 400.0, 50.0, 20.0), vec![])).collect());
        let els = ax::collect(&tree, PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 });
        let (s, q) = marks_request("click B3", "Notes", &els, None);
        assert_eq!(q["target"]["criteria"]["4"], "B3 (button, main)");
        assert_eq!(s[0], "Frontmost app: Notes. The options are the clickable elements on screen, from the accessibility tree.");
    }

    /// Real API, the 24 synthetic targets of `.lo/som-experiment` through the production builder (`decide`).
    /// `OPENROUTER_API_KEY=... cargo test jev_set_of_mark -- --ignored --nocapture`. Max 30 calls.
    #[test]
    #[ignore]
    fn jev_set_of_mark() {
        let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/e2e/fixtures/som");
        let tasks: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tasks.json")).unwrap()).unwrap();
        let calls = std::cell::Cell::new(0);
        let (mut hits, mut total, mut cost) = (0, 0, 0.0);
        for page in ["slack", "gmail", "settings", "ide"] {
            let raw: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{page}.json"))).unwrap()).unwrap();
            let els: Vec<El> = raw
                .iter()
                .map(|e| El {
                    mark: Mark {
                        id: e["id"].as_u64().unwrap() as u32,
                        rect: PtRect { x: e["x"].as_f64().unwrap(), y: e["y"].as_f64().unwrap(), w: e["w"].as_f64().unwrap(), h: e["h"].as_f64().unwrap() },
                        name: e["name"].as_str().unwrap().into(),
                        role: e["role"].as_str().unwrap().into(),
                        region: e["region"].as_str().unwrap().into(),
                    },
                    state: e["state"].as_str().map(String::from),
                    h: 0,
                    url: None,
                    value: None,
                })
                .collect();
            let png = std::fs::read(dir.join(format!("{page}-marks.png"))).unwrap();
            for t in tasks[page].as_array().unwrap() {
                let (instr, want) = (t[0].as_str().unwrap(), t[1].as_u64().unwrap() as u32);
                let mut ask = |s: &Value, q: &Value| {
                    anyhow::ensure!(calls.get() < 30, "call cap");
                    calls.set(calls.get() + 1);
                    jev::ask(jev::DECISIONS_URL, &key, crate::contract::JEV_MODEL, s, q)
                };
                // Over the cap the image retry is skipped (the text answer stands).
                let mut shot = || (calls.get() < 30).then(|| jev::image(&png, "image/png"));
                let d = decide(instr, page, &els, &mut ask, &mut shot).unwrap();
                let hit = d.target.choice == want.to_string();
                (hits, total, cost) = (hits + hit as u32, total + 1, cost + d.cost);
                println!("{page:9} {instr:50} want {want:3} got {:5} conf {:.2} image {} {}", d.target.choice, d.target.confidence, d.used_image, if hit { "HIT" } else { "MISS" });
            }
        }
        println!("{hits}/{total} hits, {} calls, ${cost:.5}", calls.get());
        assert!(hits >= 22, "{hits}/24");
    }
}
