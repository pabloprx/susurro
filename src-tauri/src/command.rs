//! Voice control runner (`.lo/VOICE_CONTROL.md`, measured approach: `.lo/som-experiment/RESULTS.md`).
//! F2 hold -> STT (session.rs) -> `run`:
//! 1. fast path: one text-only Jev call over a fixed intent list (open_app, paste_last, new_tab, none);
//! 2. else up to 6 steps: read the AX tree -> numbered marks -> Jev choice over the mark ids, text only; the marked
//!    screenshot only for spatial words or a confidence < 0.8 -> gates (pick / confirm) -> 300 ms aim -> act -> re-read.
//!    An app with < 5 AX elements gets the box fallback: full-resolution tiles, Jev box, a visible noul, snap.
//! Side effects go through `Desk` (one real impl, one test fake), so every gate and the abort checks are unit-tested.
//! ABORT (Esc, F2 while acting) is checked between steps, after every Jev call and right before every action.

use crate::ax::{self, El};
use crate::contract::{CmdStatus, CommandStep, Mark, PtRect};
use crate::jev::{self, Bbox, Choice, Reply};
use serde_json::{Map, Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const MAX_STEPS: u32 = 6;
/// Below this target confidence: retry with the screenshot, then ask (pick). RESULTS.md: every miss was < 0.8.
pub const CONF_MIN: f32 = 0.8;
/// Below this nothing is worth a pick list: "Not sure where".
pub const UNSURE_MIN: f32 = 0.2;
/// risky noul at or above this + confirm_risky = wait for the command key. VOICE_CONTROL.md: low on purpose,
/// a false alarm costs one key press. Not measured yet.
pub const RISKY_MIN: f32 = 0.3;
pub const INTENT_MIN: f32 = 0.6;
/// A non-target action (scroll, key, done) weaker than this stops as unsure.
pub const ACTION_MIN: f32 = 0.5;
pub const VISIBLE_MIN: f32 = 0.5;
pub const DONE_MIN: f32 = 0.6;
/// Fewer AX elements than this = the box fallback.
pub const MIN_AX: usize = 5;
pub const AIM_MS: u64 = 300;
/// ponytail: fixed settle time after an action, not measured; an AX-change wait would be faster.
pub const SETTLE_MS: u64 = 400;
/// The marks window draws before the screenshot is taken.
const MARKS_DRAW_MS: u64 = 250;
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
pub const PICK_TIMEOUT: Duration = Duration::from_secs(20);
/// Jev's image limit per side.
const IMG_MAX: f64 = 1568.0;
/// A box center this close to an AX element snaps to it.
const SNAP_PT: f64 = 24.0;

// ---------- the log + the command_step event ----------

static LOG: Mutex<Vec<CommandStep>> = Mutex::new(Vec::new());

/// Sends one step to every window and keeps it in the in-memory log. step 1 / fast path (0) starts a new run.
pub fn publish(app: &AppHandle, s: CommandStep) {
    record(&mut LOG.lock().unwrap(), s.clone());
    let _ = app.emit("command_step", s);
}

/// Step 0 (fast path) or 1 starts a new run: the log holds one run, even when the same instruction repeats.
/// One row per step: a later status of the same step replaces the earlier one.
fn record(log: &mut Vec<CommandStep>, s: CommandStep) {
    if s.step <= 1 {
        log.clear();
    } else if log.last().is_some_and(|l| l.step == s.step) {
        log.pop();
    }
    log.push(s);
}

/// The last voice-control run, oldest step first.
#[tauri::command]
pub fn command_log() -> Vec<CommandStep> {
    LOG.lock().unwrap().clone()
}

// ---------- abort / confirm / pick (session.rs sets these) ----------

static ABORT: AtomicBool = AtomicBool::new(false);
static CONFIRMED: AtomicBool = AtomicBool::new(false);
/// 0 = not waiting, 1 = confirm, 2 = pick.
static WAITING: AtomicU8 = AtomicU8::new(0);
static ANSWER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    Confirm,
    Pick,
}

/// What the runner waits for right now (the command key means "confirm" or "speak the number").
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

pub fn confirm() {
    CONFIRMED.store(true, Ordering::SeqCst);
}

/// The spoken answer to "Which one?".
pub fn answer(text: String) {
    *ANSWER.lock().unwrap() = Some(text);
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Yes,
    Text(String),
    /// Esc, F2 abort, or the timeout.
    No,
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

/// The text of "type hello in the search field", "write 'meet in the lobby'", "busca gatos".
/// ponytail: verb + cut at "in the / into / en el"; an LLM would read odd phrasings (quotes always work).
pub fn type_text(instr: &str) -> Option<String> {
    let t = instr.trim();
    // No ASCII ' pair: "type I'm at Bob's" would type "m at Bob".
    for (o, c) in [('"', '"'), ('“', '”'), ('‘', '’'), ('«', '»')] {
        if let (Some(a), Some(b)) = (t.find(o), t.rfind(c)) {
            let a = a + o.len_utf8();
            if a < b {
                return Some(t[a..b].to_string()).filter(|s| !s.trim().is_empty());
            }
        }
    }
    // ASCII lowercase keeps the byte offsets of `t` (to_lowercase can change them: "İ", "ẞ" -> slice panic).
    let l = t.to_ascii_lowercase();
    const VERBS: [&str; 9] = ["type ", "write ", "enter ", "search for ", "escribe ", "escribir ", "busca ", "buscar ", "pon "];
    let (i, v) = VERBS.iter().filter_map(|v| l.find(v).map(|i| (i, v))).min_by_key(|p| p.0)?;
    let rest = &t[i + v.len()..];
    let lr = rest.to_ascii_lowercase();
    let cut = [" in the ", " into the ", " into ", " in my ", " on the ", " en el ", " en la ", " en mi "].iter().filter_map(|c| lr.rfind(c)).max();
    let s = cut.map_or(rest, |c| &rest[..c]).trim().trim_end_matches(['.', ',']);
    (!s.is_empty()).then(|| s.to_string())
}

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

// ---------- Jev requests (the production builders; the real-API test uses them too) ----------

const ACTIONS: [(&str, &str); 6] = [
    ("click", "click, open, select or toggle one element on screen"),
    ("focus_and_type", "type text into a field on screen"),
    ("scroll_down", "scroll down"),
    ("scroll_up", "scroll up"),
    ("press_key", "press one keyboard key such as Return, Escape or Tab"),
    ("done", "nothing is left to do: the steps done already finish the instruction"),
];

/// (name, macOS virtual keycode, with Cmd).
pub const KEYS: [(&str, u16, bool); 13] = [
    ("return", 36, false), ("escape", 53, false), ("tab", 48, false), ("space", 49, false), ("delete", 51, false),
    ("up", 126, false), ("down", 125, false), ("left", 123, false), ("right", 124, false), ("page_down", 121, false),
    ("page_up", 116, false), ("cmd_f", 3, true), ("cmd_l", 37, true),
];

fn criteria<'a>(it: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    Value::Object(it.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
}

fn risky_q(instr: &str) -> Value {
    json!({ "type": "noul", "instructions": format!("The user said: \"{instr}\". Would the next step itself send a message, email or post to another person, delete data, or spend money?") })
}

/// Fast path, text only: intent + app + "more than one thing?".
pub fn fast_request(instr: &str, apps: &[String]) -> (Value, Value) {
    let intents = criteria([
        ("open_app", "open, launch or switch to an application by its name"),
        ("paste_last", "paste the last dictation"),
        ("new_tab", "open a new tab"),
        ("none", "anything else: it needs to look at the screen (click or type something, or open a channel, chat, file, tab or page inside an app)"),
    ]);
    let mut q = json!({
        "intent": { "type": "choice", "instructions": format!("Which command is \"{instr}\"?"), "criteria": intents },
        "multi": { "type": "noul", "instructions": format!("Does \"{instr}\" ask for more than one thing?") },
    });
    if !apps.is_empty() {
        let mut c: Map<String, Value> = apps.iter().map(|a| (a.clone(), json!(a))).collect();
        c.insert("none".into(), json!("no application is named"));
        q["app"] = json!({ "type": "choice", "instructions": format!("Which application does \"{instr}\" name?"), "criteria": c });
    }
    (json!([format!("The user said this voice command on a Mac: \"{instr}\"")]), q)
}

/// One marks step: the target over the mark ids, the action, the key, risky, and (after step 1) done.
/// `image` = the marked screenshot (the marks window is up when it is taken).
pub fn marks_request(instr: &str, app: &str, done: &[String], els: &[El], image: Option<&Value>) -> (Value, Value) {
    let mut state = vec![json!(match image {
        None => format!("Frontmost app: {app}. The options are the clickable elements on screen, from the accessibility tree."),
        Some(_) => format!(
            "Screenshot of the frontmost app ({app}). Every clickable element has an outline and a numbered mark at its top-left corner. The mark numbers are the option ids."
        ),
    })];
    if !done.is_empty() {
        state.push(json!(format!("Steps done so far: {}", done.join("; "))));
    }
    state.extend(image.cloned());
    let mut targets: Map<String, Value> = els.iter().map(|e| (e.mark.id.to_string(), json!(ax::describe(e)))).collect();
    targets.insert("none".into(), json!("no element on screen fits"));
    let mut q = json!({
        "target": { "type": "choice", "instructions": format!("Which element must the user click or type into to do this: \"{instr}\""), "criteria": targets },
        "action": { "type": "choice", "instructions": format!("Which single next step does \"{instr}\" need now?"), "criteria": criteria(ACTIONS) },
        "key": { "type": "choice", "instructions": format!("If the next step for \"{instr}\" is a key press, which key?"), "criteria": criteria(KEYS.iter().map(|k| (k.0, k.0))) },
        "risky": risky_q(instr),
    });
    if !done.is_empty() {
        q["done"] = json!({ "type": "noul", "instructions": format!("Do the steps done so far already finish \"{instr}\"?") });
    }
    (Value::Array(state), q)
}

/// Box fallback: the tiles in `state` by name, a tile choice, one box and one visible noul per tile.
pub fn box_request(instr: &str, app: &str, imgs: &[Value]) -> (Value, Value) {
    let mut state = Map::new();
    state.insert("app".into(), json!(app));
    let mut tile_c = Map::new();
    let mut q = json!({
        "action": { "type": "choice", "instructions": format!("Which single next step does \"{instr}\" need now?"), "criteria": criteria(ACTIONS) },
        "key": { "type": "choice", "instructions": format!("If the next step for \"{instr}\" is a key press, which key?"), "criteria": criteria(KEYS.iter().map(|k| (k.0, k.0))) },
        "risky": risky_q(instr),
    });
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

/// The answers of one marks step.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub target: Choice,
    pub action: Choice,
    pub key: Option<String>,
    pub risky: f32,
    pub done: f32,
    pub cost: f64,
    pub latency_ms: u32,
    pub used_image: bool,
    /// The text answer was weak and no screenshot could be taken (no Screen Recording).
    pub need_screen: bool,
}

fn parse(r: &Reply) -> anyhow::Result<Decision> {
    let a = &r.answers;
    let none = || Choice { choice: "none".into(), confidence: 0.0, ranked: vec![] };
    Ok(Decision {
        target: jev::choice(a, "target").unwrap_or_else(none),
        action: jev::choice(a, "action").ok_or_else(|| anyhow::anyhow!("Jev: no action in the answer"))?,
        key: jev::choice(a, "key").map(|c| c.choice),
        risky: jev::noul(a, "risky").unwrap_or(1.0), // missing = treat as risky
        done: jev::noul(a, "done").unwrap_or(0.0),
        cost: r.cost,
        latency_ms: r.latency_ms,
        used_image: false,
        need_screen: false,
    })
}

pub type Ask<'a> = dyn FnMut(&Value, &Value) -> anyhow::Result<Reply> + 'a;

fn needs_target(action: &str) -> bool {
    matches!(action, "click" | "focus_and_type")
}

/// Text first; the marked screenshot for spatial words or a weak target; the more confident answer wins.
/// `shot` = the marked screenshot as a Jev image part, None = not available.
pub fn decide(instr: &str, app: &str, done: &[String], els: &[El], ask: &mut Ask, shot: &mut dyn FnMut() -> Option<Value>) -> anyhow::Result<Decision> {
    let (s, q) = marks_request(instr, app, done, els, None);
    let mut d = parse(&ask(&s, &q)?)?;
    if needs_target(&d.action.choice) && (spatial(instr) || d.target.confidence < CONF_MIN) {
        match shot() {
            Some(img) => {
                let (s, q) = marks_request(instr, app, done, els, Some(&img));
                let mut d2 = parse(&ask(&s, &q)?)?;
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

// ---------- the runner ----------

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Mark(u32),
    /// Box fallback with no AX element near: global points.
    Point(f64, f64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Click(Target),
    /// The plan; the runner does it as Focus, the secure input check, then Paste.
    Type(Target, String),
    Focus(Target),
    Paste(String),
    Scroll { down: bool, at: Option<Target> },
    Key(String),
    OpenApp(String),
    PasteLast,
    NewTab,
}

/// What the runner sees: the frontmost app, its window's display, the marks.
#[derive(Debug, Clone, Default)]
pub struct Screen {
    pub app: String,
    pub display: PtRect,
    pub scale: f64,
    pub els: Vec<El>,
}

/// The runner's side effects. Real: `mac::Real` (AX, CGEvent, screencapture). Tests: a fake.
pub trait Desk {
    /// Installed app names (the fast path's app choice).
    fn apps(&mut self) -> Vec<String>;
    /// Err("accessibility") = no AX access; any other Err = nothing to act on.
    fn read(&mut self) -> Result<Screen, String>;
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
}

pub struct Cfg {
    pub confirm_risky: bool,
    pub max_steps: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub status: CmdStatus,
    /// What susurro did, for the History row ("clicked “Send”; typed “hi”").
    pub summary: String,
    pub message: Option<String>,
    pub steps: u32,
}

const NOT_SURE: &str = "Not sure where. Say it again.";
const BLOCKED: &str = "Password field. susurro won't touch it.";
/// Between the focus and the paste of a type step.
const FOCUS_MS: u64 = 150;

struct Run<'a> {
    instr: &'a str,
    cfg: &'a Cfg,
    done: Vec<String>,
}

impl Run<'_> {
    fn step(&self, n: u32, status: CmdStatus) -> CommandStep {
        CommandStep { instruction: self.instr.into(), step: n, max: self.cfg.max_steps, status, ..Default::default() }
    }

    fn end(&self, desk: &mut dyn Desk, mut s: CommandStep, message: Option<&str>) -> End {
        s.message = message.map(String::from);
        desk.emit(&s);
        End { status: s.status, summary: self.done.join("; "), message: s.message, steps: s.step }
    }
}

/// One planned action, after the decision and before the gates.
struct Plan {
    act: Act,
    action: String,
    detail: Option<String>,
    mark: Option<Mark>,
    /// Box fallback: the box as a mark, so the marks window can draw it.
    marks: Vec<Mark>,
    risky: f32,
    /// "clicked “Send”" for the next request and the History row.
    summary: String,
}

pub fn run(instr: &str, cfg: &Cfg, ask: &mut Ask, desk: &mut dyn Desk) -> End {
    let mut r = Run { instr, cfg, done: vec![] };
    if desk.aborted() {
        return r.end(desk, r.step(0, CmdStatus::Stopped), None);
    }
    // 1. fast path
    let apps = desk.apps();
    let (s, q) = fast_request(instr, &apps);
    let fast = match ask(&s, &q) {
        Ok(rep) => rep,
        Err(e) => return r.end(desk, r.step(0, CmdStatus::Error), Some(&format!("{e:#}"))),
    };
    if let Some((act, action, detail)) = fast_intent(&fast.answers, &apps) {
        let mut st = CommandStep { action: Some(action.into()), detail: detail.clone(), latency_ms: fast.latency_ms, cost_usd: fast.cost, ..r.step(0, CmdStatus::Done) };
        if desk.aborted() {
            st.status = CmdStatus::Stopped;
            return r.end(desk, st, None);
        }
        if let Err(e) = desk.act(&act) {
            st.status = CmdStatus::Error;
            return r.end(desk, st, Some(&format!("{e:#}")));
        }
        r.done.push(match detail {
            Some(d) => format!("opened {d}"),
            None => action.replace('_', " "),
        });
        return r.end(desk, st, None);
    }
    // 2. the marks loop
    let mut last: Option<(String, String)> = None;
    for n in 1..=cfg.max_steps {
        if desk.aborted() {
            return r.end(desk, r.step(n, CmdStatus::Stopped), None);
        }
        let mut st = r.step(n, CmdStatus::Thinking);
        desk.emit(&st);
        let scr = match desk.read() {
            Ok(s) => s,
            Err(e) if e == "accessibility" => return r.end(desk, r.step(n, CmdStatus::Setup), Some("accessibility")),
            Err(e) => return r.end(desk, r.step(n, CmdStatus::Unsure), Some(&e)),
        };
        st.display = scr.display;
        let plan = if scr.els.len() < MIN_AX { box_step(&r, &scr, ask, desk, &mut st) } else { marks_step(&r, &scr, ask, desk, &mut st) };
        let plan = match plan {
            Ok(Some(p)) => p,
            Ok(None) => return r.end(desk, CommandStep { status: CmdStatus::Done, action: Some("done".into()), ..st }, None),
            Err(end) => return end,
        };
        if desk.aborted() {
            return r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st }, None);
        }
        // Clicking or typing into the same element twice in a row means the first one did it (never send twice).
        // An unnamed element (an icon, a box point) is told apart by its rect, else two different icons = "done".
        let key = (plan.action.clone(), plan.mark.as_ref().map(|m| if m.name.is_empty() { format!("{:?}", m.rect) } else { m.name.clone() }).unwrap_or_default());
        if needs_target(&plan.action) && last.as_ref() == Some(&key) {
            return r.end(desk, CommandStep { status: CmdStatus::Done, action: Some("done".into()), ..st }, None);
        }
        st.action = Some(plan.action.clone());
        st.detail = plan.detail.clone();
        st.target = plan.mark.as_ref().map(|m| m.id);
        if !plan.marks.is_empty() {
            st.marks = plan.marks.clone();
        }
        // confirm gate (risky)
        if gate(1.0, plan.risky, cfg.confirm_risky) == Gate::Confirm {
            st.status = CmdStatus::Confirm;
            desk.emit(&st);
            if desk.wait(Wait::Confirm, CONFIRM_TIMEOUT) != Answer::Yes {
                return r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st }, None);
            }
        }
        // aim, then the last abort check before the event
        st.status = CmdStatus::Aim;
        desk.emit(&st);
        desk.sleep(AIM_MS);
        if desk.aborted() {
            return r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st }, None);
        }
        let at_point = match &plan.act {
            Act::Click(Target::Point(x, y)) | Act::Type(Target::Point(x, y), _) => Some((*x, *y)),
            _ => None,
        };
        if (matches!(plan.act, Act::Type(..) | Act::Key(_)) && desk.secure_input()) || at_point.is_some_and(|p| desk.secure_at(p)) {
            return r.end(desk, CommandStep { status: CmdStatus::Blocked, ..st }, Some(BLOCKED));
        }
        let done = match &plan.act {
            // Focus first, then check again: the click itself can land in a password field.
            Act::Type(t, text) => desk.act(&Act::Focus(t.clone())).map(|_| desk.sleep(FOCUS_MS)).and_then(|_| {
                if desk.secure_input() { Ok(false) } else { desk.act(&Act::Paste(text.clone())).map(|_| true) }
            }),
            a => desk.act(a).map(|_| true),
        };
        match done {
            Ok(true) => {}
            Ok(false) => return r.end(desk, CommandStep { status: CmdStatus::Blocked, ..st }, Some(BLOCKED)),
            Err(e) => return r.end(desk, CommandStep { status: CmdStatus::Error, ..st }, Some(&format!("{e:#}"))),
        }
        r.done.push(format!("{n}. {}", plan.summary));
        last = Some(key);
        desk.sleep(SETTLE_MS);
    }
    let n = cfg.max_steps;
    r.end(desk, r.step(n, CmdStatus::Stopped), Some(&format!("Stopped after {n} steps")))
}

/// The fast path's answer -> an action, or None (go to the marks loop).
fn fast_intent(a: &Value, apps: &[String]) -> Option<(Act, &'static str, Option<String>)> {
    let intent = jev::choice(a, "intent")?;
    if intent.confidence < INTENT_MIN || jev::noul(a, "multi").unwrap_or(1.0) >= 0.5 {
        return None;
    }
    match intent.choice.as_str() {
        "open_app" => {
            let app = jev::choice(a, "app").filter(|c| c.confidence >= INTENT_MIN && apps.contains(&c.choice))?;
            Some((Act::OpenApp(app.choice.clone()), "open_app", Some(app.choice)))
        }
        "paste_last" => Some((Act::PasteLast, "paste_last", None)),
        "new_tab" => Some((Act::NewTab, "new_tab", None)),
        _ => None,
    }
}

fn quote(s: &str) -> String {
    format!("“{}”", if s.is_empty() { "(no name)" } else { s })
}

/// The non-target actions of a decision (scroll, key, done) or None.
fn simple(r: &Run, action: &Choice, key: Option<&str>, risky: f32) -> Result<Option<Option<Plan>>, &'static str> {
    let plan = |act, summary: String, detail: Option<String>| Some(Some(Plan { act, action: action.choice.clone(), detail, mark: None, marks: vec![], risky, summary }));
    if needs_target(&action.choice) {
        return Ok(None);
    }
    if action.confidence < ACTION_MIN {
        return Err(NOT_SURE);
    }
    Ok(match action.choice.as_str() {
        "done" if !r.done.is_empty() => Some(None),
        "done" => return Err("Nothing to do on this screen. Say it again."),
        "scroll_down" | "scroll_up" => {
            let down = action.choice == "scroll_down";
            plan(Act::Scroll { down, at: None }, format!("scrolled {}", if down { "down" } else { "up" }), None)
        }
        _ => {
            let k = key.filter(|k| KEYS.iter().any(|x| x.0 == *k)).ok_or(NOT_SURE)?.to_string();
            // Return sends in every chat app, whatever the risky noul says (VOICE_CONTROL.md, Confirm).
            let risky = if k == "return" { 1.0 } else { risky };
            Some(Some(Plan { act: Act::Key(k.clone()), action: action.choice.clone(), detail: Some(k.clone()), mark: None, marks: vec![], risky, summary: format!("pressed {k}") }))
        }
    })
}

/// A marks step: emit the marks, decide, gate the target (unsure / pick), build the plan. Ok(None) = done.
fn marks_step(r: &Run, scr: &Screen, ask: &mut Ask, desk: &mut dyn Desk, st: &mut CommandStep) -> Result<Option<Plan>, End> {
    st.marks = scr.els.iter().map(|e| e.mark.clone()).collect();
    st.status = CmdStatus::Marks;
    desk.emit(st);
    let display = scr.display;
    let d = {
        let mut shot = || {
            // Esc during the text call: no screenshot, no 2nd call (the abort check after decide stops the run).
            if desk.aborted() || !desk.screen_ok() {
                return None;
            }
            desk.sleep(MARKS_DRAW_MS);
            desk.capture(display, false).map_err(|e| log::warn!("screenshot: {e:#}")).ok().map(|b| jev::image(&b, "image/jpeg"))
        };
        decide(r.instr, &scr.app, &r.done, &scr.els, ask, &mut shot)
    };
    let d = match d {
        Ok(d) => d,
        Err(e) => return Err(r.end(desk, CommandStep { status: CmdStatus::Error, ..st.clone() }, Some(&format!("{e:#}")))),
    };
    (st.latency_ms, st.cost_usd) = (d.latency_ms, d.cost);
    if desk.aborted() {
        return Err(r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st.clone() }, None));
    }
    if !r.done.is_empty() && d.done >= DONE_MIN {
        return Ok(None);
    }
    match simple(r, &d.action, d.key.as_deref(), d.risky) {
        Ok(Some(p)) => return Ok(p),
        Ok(None) => {}
        Err(msg) => return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some(msg))),
    }
    if d.need_screen {
        return Err(r.end(desk, CommandStep { status: CmdStatus::Setup, ..st.clone() }, Some("screen_recording")));
    }
    let conf = if d.target.choice == "none" { 0.0 } else { d.target.confidence };
    let mut id = d.target.choice.parse::<u32>().ok();
    match gate(conf, 0.0, false) {
        Gate::Unsure => return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some(NOT_SURE))),
        Gate::Pick => {
            st.choices = d.target.ranked.iter().filter_map(|(k, p)| k.parse::<u32>().ok().map(|k| (k, *p))).take(3).collect();
            st.action = Some(d.action.choice.clone());
            st.status = CmdStatus::Pick;
            desk.emit(st);
            let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
            id = match desk.wait(Wait::Pick, PICK_TIMEOUT) {
                Answer::Text(t) => match parse_pick(&t, &st.choices, &ids) {
                    Some(id) => Some(id),
                    None => return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some("Didn't catch a number. Say it again."))),
                },
                _ => return Err(r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st.clone() }, None)),
            };
            st.choices.clear();
        }
        _ => {}
    }
    let el = id.and_then(|id| scr.els.iter().find(|e| e.mark.id == id));
    let Some(el) = el else { return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some(NOT_SURE))) };
    let m = el.mark.clone();
    let (act, summary, detail) = if d.action.choice == "focus_and_type" {
        let Some(text) = type_text(r.instr) else {
            return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some("Say the text in quotes: type \"hello\" in the search field")));
        };
        (Act::Type(Target::Mark(m.id), text.clone()), format!("typed {} into {}", quote(&text), quote(&m.name)), Some(text))
    } else {
        (Act::Click(Target::Mark(m.id)), format!("clicked {} ({})", quote(&m.name), m.role), None)
    };
    Ok(Some(Plan { act, action: d.action.choice, detail, mark: Some(m), marks: vec![], risky: d.risky, summary }))
}

/// The box fallback (an app with < 5 AX elements): full-resolution tiles, Jev box per tile, the visible noul.
fn box_step(r: &Run, scr: &Screen, ask: &mut Ask, desk: &mut dyn Desk, st: &mut CommandStep) -> Result<Option<Plan>, End> {
    if !desk.screen_ok() {
        return Err(r.end(desk, CommandStep { status: CmdStatus::Setup, ..st.clone() }, Some("screen_recording")));
    }
    let ts = tiles(scr.display, scr.scale.max(1.0));
    let mut imgs = vec![];
    for t in &ts {
        match desk.capture(*t, true) {
            Ok(b) => imgs.push(jev::image(&b, "image/jpeg")),
            Err(e) => return Err(r.end(desk, CommandStep { status: CmdStatus::Error, ..st.clone() }, Some(&format!("screenshot: {e:#}")))),
        }
    }
    let (s, q) = box_request(r.instr, &scr.app, &imgs);
    let rep = match ask(&s, &q) {
        Ok(rep) => rep,
        Err(e) => return Err(r.end(desk, CommandStep { status: CmdStatus::Error, ..st.clone() }, Some(&format!("{e:#}")))),
    };
    (st.latency_ms, st.cost_usd) = (rep.latency_ms, rep.cost);
    if desk.aborted() {
        return Err(r.end(desk, CommandStep { status: CmdStatus::Stopped, ..st.clone() }, None));
    }
    let a = &rep.answers;
    let risky = jev::noul(a, "risky").unwrap_or(1.0);
    let Some(action) = jev::choice(a, "action") else {
        return Err(r.end(desk, CommandStep { status: CmdStatus::Error, ..st.clone() }, Some("Jev: no action in the answer")));
    };
    match simple(r, &action, jev::choice(a, "key").map(|c| c.choice).as_deref(), risky) {
        Ok(Some(p)) => return Ok(p),
        Ok(None) => {}
        Err(msg) => return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some(msg))),
    }
    // The tile, its box, and the visible gate: never click on a weak or broken box.
    let found = (|| {
        let i: usize = jev::choice(a, "tile")?.choice.strip_prefix("tile_")?.parse().ok()?;
        let b = jev::bbox(a, &format!("box_{i}")).filter(|b| sane(*b))?;
        (jev::noul(a, &format!("visible_{i}")).unwrap_or(0.0) >= VISIBLE_MIN).then_some(())?;
        Some((b, *ts.get(i)?))
    })();
    let Some((b, tile)) = found else { return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some(NOT_SURE))) };
    let p = box_point(b, tile);
    let rect = PtRect { x: tile.x + b.left * tile.w, y: tile.y + b.top * tile.h, w: (b.right - b.left) * tile.w, h: (b.bottom - b.top) * tile.h };
    let (target, mark) = match snap(p, &scr.els) {
        Some(e) => (Target::Mark(e.mark.id), e.mark.clone()),
        None => (Target::Point(p.0.round(), p.1.round()), Mark { id: 1, rect, name: String::new(), role: "box".into(), region: "screen".into() }),
    };
    let (act, summary, detail) = if action.choice == "focus_and_type" {
        let Some(text) = type_text(r.instr) else {
            return Err(r.end(desk, CommandStep { status: CmdStatus::Unsure, ..st.clone() }, Some("Say the text in quotes: type \"hello\" in the search field")));
        };
        (Act::Type(target, text.clone()), format!("typed {} at ({:.0}, {:.0})", quote(&text), p.0, p.1), Some(text))
    } else {
        (Act::Click(target), format!("clicked at ({:.0}, {:.0})", p.0, p.1), None)
    };
    Ok(Some(Plan { act, action: action.choice, detail, marks: vec![mark.clone()], mark: Some(mark), risky, summary }))
}

// ---------- the command thread ----------

/// The end of a run, for session.rs (overlay idle + the History row).
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub instruction: String,
    pub end: End,
    pub latency_ms: u32,
}

/// Runs one instruction on its own thread; sends `SessionCmd::CommandEnded` at the end.
pub fn spawn(app: AppHandle, instr: String, settings: crate::contract::CommandSettings, key: String) {
    ABORT.store(false, Ordering::SeqCst);
    CONFIRMED.store(false, Ordering::SeqCst);
    WAITING.store(0, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("command".into())
        .spawn(move || {
            use tauri::Manager;
            let t = std::time::Instant::now();
            let cfg = Cfg { confirm_risky: settings.confirm_risky, max_steps: MAX_STEPS };
            let mut ask = |s: &Value, q: &Value| jev::ask(jev::DECISIONS_URL, &key, &settings.model, s, q);
            #[cfg(target_os = "macos")]
            let end = run(&instr, &cfg, &mut ask, &mut mac::Real::new(app.clone()));
            #[cfg(not(target_os = "macos"))]
            let end = {
                let _ = (&cfg, &mut ask);
                End { status: CmdStatus::Error, summary: String::new(), message: Some("macOS only".into()), steps: 0 }
            };
            log::info!("command {instr:?}: {:?} after {} steps ({}), {:?}", end.status, end.steps, end.summary, end.message);
            #[cfg(target_os = "macos")]
            crate::platform::macos::show_marks(&app, None);
            WAITING.store(0, Ordering::SeqCst);
            let ended = Ended { instruction: instr, end, latency_ms: t.elapsed().as_millis() as u32 };
            let _ = app.state::<crate::AppState>().session_tx.send(crate::session::SessionCmd::CommandEnded(Box::new(ended)));
        })
        .expect("spawn command thread");
}

/// Polls the flags (50 ms): the command key (confirm), the spoken answer (pick), Esc / F2 abort, the timeout.
fn wait_flags(w: Wait, timeout: Duration) -> Answer {
    CONFIRMED.store(false, Ordering::SeqCst);
    *ANSWER.lock().unwrap() = None;
    WAITING.store(if w == Wait::Confirm { 1 } else { 2 }, Ordering::SeqCst);
    let until = std::time::Instant::now() + timeout;
    let out = loop {
        if ABORT.load(Ordering::SeqCst) {
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

#[cfg(target_os = "macos")]
mod mac {
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

    /// Statuses that show the marks window (Marks.tsx SHOWN).
    const SHOWN: [CmdStatus; 4] = [CmdStatus::Marks, CmdStatus::Aim, CmdStatus::Pick, CmdStatus::Confirm];

    pub struct Real {
        app: AppHandle,
        handles: Vec<CFType>,
        els: Vec<El>,
    }

    impl Real {
        pub fn new(app: AppHandle) -> Self {
            Self { app, handles: vec![], els: vec![] }
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
        e.post(CGEventTapLocation::HID);
        Ok(())
    }

    impl Desk for Real {
        fn apps(&mut self) -> Vec<String> {
            let home = std::env::var("HOME").map(|h| format!("{h}/Applications")).unwrap_or_default();
            let mut out: Vec<String> = ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities", home.as_str()]
                .iter()
                .filter_map(|d| std::fs::read_dir(d).ok())
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".app").map(String::from))
                .collect();
            out.sort();
            out.dedup();
            // ponytail: Jev's choice accuracy over ~200 options is not measured; capped.
            out.truncate(200);
            out
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
            match a {
                Act::Click(t) => {
                    if !self.handle(t).is_some_and(ax::mac::press) {
                        click_at(self.point(t).ok_or_else(|| anyhow::anyhow!("the element is gone"))?)?;
                    }
                }
                Act::Type(..) => anyhow::bail!("Type runs as Focus + Paste"),
                Act::Focus(t) => {
                    if !self.handle(t).is_some_and(ax::mac::focus) {
                        click_at(self.point(t).ok_or_else(|| anyhow::anyhow!("the element is gone"))?)?;
                    }
                }
                Act::Paste(text) => {
                    let opts = self.app.state::<crate::AppState>().settings.read().unwrap().paste.clone();
                    match crate::paste::paste(text, &opts, None) {
                        crate::paste::PasteOutcome::Pasted => {}
                        o => anyhow::bail!("could not type: {o:?}"),
                    }
                }
                Act::Scroll { down, at } => scroll(*down, at.as_ref().and_then(|t| self.point(t)))?,
                Act::Key(k) => {
                    let (_, code, cmd) = KEYS.iter().find(|x| x.0 == k).ok_or_else(|| anyhow::anyhow!("unknown key {k}"))?;
                    crate::paste::key_press(*code, *cmd);
                }
                Act::OpenApp(name) => {
                    let ok = std::process::Command::new("/usr/bin/open").args(["-a", name]).status()?.success();
                    anyhow::ensure!(ok, "could not open {name}");
                }
                Act::PasteLast => {
                    let _ = self.app.state::<crate::AppState>().session_tx.send(crate::session::SessionCmd::PasteLast);
                }
                Act::NewTab => crate::paste::key_press(17, true), // kVK_ANSI_T
            }
            Ok(())
        }

        fn secure_input(&mut self) -> bool {
            unsafe { IsSecureEventInputEnabled() != 0 }
        }

        fn secure_at(&mut self, p: (f64, f64)) -> bool {
            ax::mac::secure_at(p)
        }

        fn emit(&mut self, s: &CommandStep) {
            let show = SHOWN.contains(&s.status) && !s.marks.is_empty();
            crate::platform::macos::show_marks(&self.app, show.then_some(s.display));
            publish(&self.app, s.clone());
        }

        fn sleep(&mut self, ms: u64) {
            std::thread::sleep(Duration::from_millis(ms));
        }

        fn wait(&mut self, w: Wait, timeout: Duration) -> Answer {
            wait_flags(w, timeout)
        }

        fn aborted(&self) -> bool {
            ABORT.load(Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::tests::n;
    use std::sync::{Arc, Mutex as M};

    #[test]
    fn a_repeated_instruction_starts_a_new_run() {
        let st = |step, i: &str| CommandStep { step, instruction: i.into(), ..Default::default() };
        let mut log = vec![];
        for s in [st(1, "scroll down"), st(2, "scroll down"), st(1, "scroll down")] {
            record(&mut log, s);
        }
        assert_eq!(log.len(), 1, "the second 'scroll down' run is merged into the first");
        record(&mut log, st(0, "open Slack"));
        assert_eq!(log, vec![st(0, "open Slack")]);
        // one row per step: thinking -> marks -> aim of step 2 keep only the aim
        let mut log = vec![st(1, "x")];
        for s in [CmdStatus::Thinking, CmdStatus::Marks, CmdStatus::Aim] {
            record(&mut log, CommandStep { status: s, ..st(2, "x") });
        }
        assert_eq!(log.iter().map(|s| (s.step, s.status.clone())).collect::<Vec<_>>(), [(1, CmdStatus::Thinking), (2, CmdStatus::Aim)]);
    }

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
        assert_eq!(parse_pick("never mind", &choices, &ids), None);
    }

    #[test]
    fn text_to_type() {
        assert_eq!(type_text("type hello in the search field").as_deref(), Some("hello"));
        assert_eq!(type_text("Type \"meet in the lobby\" in the message box").as_deref(), Some("meet in the lobby"));
        assert_eq!(type_text("search for rust tauri").as_deref(), Some("rust tauri"));
        assert_eq!(type_text("escribe hola mundo en el buscador").as_deref(), Some("hola mundo"));
        assert_eq!(type_text("click send"), None);
    }

    #[test]
    fn type_text_contractions_and_unicode() {
        // an apostrophe is not a quote
        assert_eq!(type_text("type I'm at Bob's place in the message box").as_deref(), Some("I'm at Bob's place"));
        assert_eq!(type_text("type ‘hi there’ in the box").as_deref(), Some("hi there"));
        // to_lowercase changes byte lengths ("İ" 2 -> 3 bytes): slicing the original must not panic
        assert_eq!(type_text("İtype éa").as_deref(), Some("éa"));
        assert_eq!(type_text("ẞ write café in the search field").as_deref(), Some("café"));
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
        El { mark: Mark { id, rect: PtRect { x: r.0, y: r.1, w: r.2, h: r.3 }, name: name.into(), role: role.into(), region: "main".into() }, state: None, h: id as usize }
    }

    #[test]
    fn snap_to_the_nearest_element() {
        let els = [el(1, "a", "button", (100.0, 100.0, 20.0, 20.0)), el(2, "b", "button", (200.0, 100.0, 20.0, 20.0))];
        assert_eq!(snap((110.0, 110.0), &els).unwrap().mark.id, 1, "inside");
        assert_eq!(snap((135.0, 110.0), &els).unwrap().mark.id, 1, "15 pt right of it");
        assert!(snap((160.0, 110.0), &els).is_none(), "40 pt from both");
    }

    // ---------- the runner with a fake desk ----------

    #[derive(Default)]
    struct Fake {
        screens: Vec<Screen>,
        reads: usize,
        acts: Vec<Act>,
        steps: Vec<CommandStep>,
        screen_ok: bool,
        captures: usize,
        answers: Vec<Answer>,
        waits: Vec<Wait>,
        /// abort after this many emitted steps / actions
        abort_after: Option<usize>,
        abort_after_acts: Option<usize>,
        secure: bool,
        /// secure input turns on after this many actions (a click that lands in a password field)
        secure_after_acts: Option<usize>,
        /// every point is a password field
        secure_points: bool,
    }

    impl Desk for Fake {
        fn apps(&mut self) -> Vec<String> {
            vec!["Slack".into(), "Notes".into()]
        }
        fn read(&mut self) -> Result<Screen, String> {
            self.reads += 1;
            let i = (self.reads - 1).min(self.screens.len().saturating_sub(1));
            self.screens.get(i).cloned().ok_or_else(|| "no window".into())
        }
        fn screen_ok(&mut self) -> bool {
            self.screen_ok
        }
        fn capture(&mut self, _: PtRect, _: bool) -> anyhow::Result<Vec<u8>> {
            self.captures += 1;
            Ok(b"\xff\xd8jpeg".to_vec())
        }
        fn act(&mut self, a: &Act) -> anyhow::Result<()> {
            assert!(!self.aborted(), "an action ran after the abort");
            self.acts.push(a.clone());
            Ok(())
        }
        fn secure_input(&mut self) -> bool {
            self.secure || self.secure_after_acts.is_some_and(|n| self.acts.len() >= n)
        }
        fn secure_at(&mut self, _: (f64, f64)) -> bool {
            self.secure_points
        }
        fn emit(&mut self, s: &CommandStep) {
            self.steps.push(s.clone());
        }
        fn sleep(&mut self, _: u64) {}
        fn wait(&mut self, w: Wait, _: Duration) -> Answer {
            self.waits.push(w);
            if self.answers.is_empty() { Answer::No } else { self.answers.remove(0) }
        }
        fn aborted(&self) -> bool {
            self.abort_after.is_some_and(|n| self.steps.len() >= n) || self.abort_after_acts.is_some_and(|n| self.acts.len() >= n)
        }
    }

    fn slack() -> Screen {
        let els = vec![
            el(1, "# general", "link", (64.0, 120.0, 260.0, 26.0)),
            el(2, "# design", "link", (64.0, 146.0, 260.0, 26.0)),
            el(3, "Search", "text field", (500.0, 10.0, 300.0, 28.0)),
            el(4, "Message #general", "text field", (400.0, 840.0, 900.0, 44.0)),
            el(5, "Send now", "button", (1380.0, 850.0, 32.0, 32.0)),
        ];
        Screen { app: "Slack".into(), display: PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }, scale: 2.0, els }
    }

    fn reply(answers: Value) -> Reply {
        Reply { answers, cost: 0.0001, latency_ms: 700 }
    }

    fn fast_none() -> Value {
        json!({ "intent": { "choice": "none", "probabilities": { "none": 0.9 }, "confidence": 0.9 }, "multi": { "noul": 0.1 } })
    }

    fn step(target: &str, conf: f32, action: &str, risky: f32, done: f32) -> Value {
        json!({
            "target": { "choice": target, "probabilities": { target: conf, "2": 0.1, "1": 0.05 }, "confidence": conf },
            "action": { "choice": action, "probabilities": { action: 0.9 }, "confidence": 0.9 },
            "key": { "choice": "return", "probabilities": { "return": 0.9 }, "confidence": 0.9 },
            "risky": { "noul": risky },
            "done": { "noul": done },
        })
    }

    /// An `ask` that plays the replies in order and records each request.
    fn script(replies: Vec<Value>) -> (impl FnMut(&Value, &Value) -> anyhow::Result<Reply>, Arc<M<Vec<(Value, Value)>>>) {
        let seen = Arc::new(M::new(vec![]));
        let s2 = seen.clone();
        let mut it = replies.into_iter();
        (
            move |s: &Value, q: &Value| {
                s2.lock().unwrap().push((s.clone(), q.clone()));
                it.next().map(reply).ok_or_else(|| anyhow::anyhow!("no more scripted replies"))
            },
            seen,
        )
    }

    const CFG: Cfg = Cfg { confirm_risky: true, max_steps: MAX_STEPS };

    #[test]
    fn click_then_done_text_only() {
        let (mut ask, seen) = script(vec![fast_none(), step("2", 0.95, "click", 0.02, 0.0), step("none", 0.1, "done", 0.0, 0.95)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let end = run("open the design channel", &CFG, &mut ask, &mut d);
        assert_eq!(end.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        assert_eq!(d.captures, 0, "a confident text answer needs no screenshot");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[2].0.to_string().contains("1. clicked “# design” (link)"), "the next request says what was done");
        let statuses: Vec<_> = d.steps.iter().map(|s| (s.step, s.status.clone())).collect();
        use CmdStatus::*;
        assert_eq!(statuses, [(1, Thinking), (1, Marks), (1, Aim), (2, Thinking), (2, Marks), (2, Done)]);
        assert_eq!(d.steps[2].target, Some(2));
    }

    #[test]
    fn fast_path_opens_the_app_without_reading_the_screen() {
        let fast = json!({ "intent": { "choice": "open_app", "confidence": 0.93 }, "app": { "choice": "Slack", "confidence": 0.9 }, "multi": { "noul": 0.05 } });
        let (mut ask, seen) = script(vec![fast]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let end = run("open slack", &CFG, &mut ask, &mut d);
        assert_eq!(end.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::OpenApp("Slack".into())]);
        assert_eq!((d.reads, seen.lock().unwrap().len()), (0, 1));
        assert_eq!((d.steps[0].step, d.steps[0].detail.as_deref()), (0, Some("Slack")));
        // "open slack and send hi": more than one thing -> the loop runs
        let fast = json!({ "intent": { "choice": "open_app", "confidence": 0.93 }, "app": { "choice": "Slack", "confidence": 0.9 }, "multi": { "noul": 0.8 } });
        let (mut ask, _) = script(vec![fast, step("none", 0.1, "done", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        run("open slack and send hi", &CFG, &mut ask, &mut d);
        assert_eq!(d.reads, 1);
    }

    #[test]
    fn spatial_word_retries_with_the_marked_screenshot() {
        // text 0.95 on #general, image 0.97 on #design: the image wins
        let (mut ask, seen) = script(vec![fast_none(), step("1", 0.95, "click", 0.0, 0.0), step("2", 0.97, "click", 0.0, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, ..Default::default() };
        run("open the second channel", &CFG, &mut ask, &mut d);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        assert_eq!(d.captures, 1);
        let seen = seen.lock().unwrap();
        assert!(seen[2].0[1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        assert!(seen[1].0.as_array().unwrap().iter().all(|v| v.is_string()), "the first marks call is text only");
        assert_eq!(seen[1].1["target"]["criteria"]["5"], "Send now (button, main)");
    }

    #[test]
    fn the_more_confident_answer_wins() {
        // RESULTS.md "second channel": text right (0.9), image wrong (0.56) -> keep the text answer
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.9, "click", 0.0, 0.0), step("1", 0.56, "click", 0.0, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, ..Default::default() };
        run("open the second channel", &CFG, &mut ask, &mut d);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
    }

    #[test]
    fn low_confidence_asks_which_one() {
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.6, "click", 0.0, 0.0), step("2", 0.56, "click", 0.0, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, answers: vec![Answer::Text("the second one".into())], ..Default::default() };
        let end = run("open the channel", &CFG, &mut ask, &mut d);
        let pick = d.steps.iter().find(|s| s.status == CmdStatus::Pick).expect("a pick step");
        assert_eq!(pick.choices.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 1], "top ids, no 'none'");
        assert_eq!(d.acts, [Act::Click(Target::Mark(1))], "'the second one' = the 2nd in the list");
        assert_eq!(end.status, CmdStatus::Done);
        // no answer -> stopped, nothing clicked
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.6, "click", 0.0, 0.0), step("2", 0.56, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, ..Default::default() };
        assert_eq!(run("open the channel", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn weak_text_answer_without_screen_recording_asks_for_it() {
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.6, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: false, ..Default::default() };
        let end = run("click the blue thing", &CFG, &mut ask, &mut d);
        assert_eq!((end.status, end.message.as_deref()), (CmdStatus::Setup, Some("screen_recording")));
        assert!(d.acts.is_empty());
    }

    #[test]
    fn unsure_never_acts() {
        let (mut ask, _) = script(vec![fast_none(), step("none", 0.9, "click", 0.0, 0.0), step("none", 0.9, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, ..Default::default() };
        let end = run("click the purple unicorn", &CFG, &mut ask, &mut d);
        assert_eq!(end.status, CmdStatus::Unsure);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn risky_waits_for_the_command_key() {
        let mut d = Fake { screens: vec![slack()], answers: vec![Answer::Yes], ..Default::default() };
        let (mut ask, _) = script(vec![fast_none(), step("5", 0.97, "click", 0.9, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        run("send the message", &CFG, &mut ask, &mut d);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
        // timeout / Esc -> stopped, nothing sent
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let (mut ask, _) = script(vec![fast_none(), step("5", 0.97, "click", 0.9, 0.0)]);
        assert_eq!(run("send the message", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert!(d.acts.is_empty());
        // confirm_risky off: acts at once
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let (mut ask, _) = script(vec![fast_none(), step("5", 0.97, "click", 0.9, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        run("send the message", &Cfg { confirm_risky: false, max_steps: 6 }, &mut ask, &mut d);
        assert!(d.waits.is_empty());
        assert_eq!(d.acts.len(), 1);
    }

    #[test]
    fn abort_is_checked_before_every_action() {
        // abort lands with the aim step (emitted step 3): nothing is clicked
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.95, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], abort_after: Some(3), ..Default::default() };
        assert_eq!(run("open the design channel", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert!(d.acts.is_empty());
        // abort right after the Jev answer (marks emitted = 2 steps)
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.95, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], abort_after: Some(2), ..Default::default() };
        assert_eq!(run("open the design channel", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert!(d.acts.is_empty());
        // abort before anything: no Jev call at all
        let (mut ask, seen) = script(vec![]);
        let mut d = Fake { screens: vec![slack()], abort_after: Some(0), ..Default::default() };
        assert_eq!(run("open slack", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert!(seen.lock().unwrap().is_empty());
        // abort between steps: the 2nd step never reads the screen
        let (mut ask, _) = script(vec![fast_none(), step("2", 0.95, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], abort_after_acts: Some(1), ..Default::default() };
        assert_eq!(run("open the design channel", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert_eq!((d.acts.len(), d.reads), (1, 1));
    }

    #[test]
    fn step_cap_and_no_double_send() {
        // scroll forever: stops after 6 steps
        let replies = std::iter::once(fast_none()).chain((0..6).map(|_| step("none", 0.0, "scroll_down", 0.0, 0.0))).collect();
        let (mut ask, _) = script(replies);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let end = run("scroll down forever", &CFG, &mut ask, &mut d);
        assert_eq!((end.status, d.acts.len()), (CmdStatus::Stopped, 6));
        // the same click twice in a row = done after the first
        let (mut ask, _) = script(vec![fast_none(), step("5", 0.97, "click", 0.0, 0.0), step("5", 0.97, "click", 0.0, 0.1)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        assert_eq!(run("click send", &Cfg { confirm_risky: false, max_steps: 6 }, &mut ask, &mut d).status, CmdStatus::Done);
        assert_eq!(d.acts.len(), 1);
    }

    #[test]
    fn return_always_confirms() {
        // VOICE_CONTROL.md: Return sends in chat apps; risky 0.0 still waits for F2
        let (mut ask, _) = script(vec![fast_none(), step("none", 0.0, "press_key", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        assert_eq!(run("press enter", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert_eq!((d.waits.as_slice(), d.acts.len()), ([Wait::Confirm].as_slice(), 0));
    }

    #[test]
    fn a_click_into_a_password_field_never_gets_the_text() {
        // the focus click turns secure input on: the paste never runs
        let (mut ask, _) = script(vec![fast_none(), step("3", 0.95, "focus_and_type", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], secure_after_acts: Some(1), ..Default::default() };
        assert_eq!(run("type hunter2 in the field", &CFG, &mut ask, &mut d).status, CmdStatus::Blocked);
        assert_eq!(d.acts, [Act::Focus(Target::Mark(3))]);
    }

    #[test]
    fn box_click_on_a_password_field_is_blocked() {
        let bare = Screen { els: vec![], ..slack() };
        let b = json!({ "top": 0.45, "bottom": 0.55, "left": 0.04, "right": 0.06 });
        let ans = json!({ "tile": { "choice": "tile_3", "confidence": 0.9 }, "box_3": { "box": b, "confidence": 0.9 }, "visible_3": { "noul": 0.9 },
                          "action": { "choice": "click", "confidence": 0.9 }, "risky": { "noul": 0.0 } });
        let (mut ask, _) = script(vec![fast_none(), ans]);
        let mut d = Fake { screens: vec![bare], screen_ok: true, secure_points: true, ..Default::default() };
        assert_eq!(run("click the field", &CFG, &mut ask, &mut d).status, CmdStatus::Blocked);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn two_different_unnamed_icons_are_two_steps() {
        let mut scr = slack();
        scr.els.push(el(6, "", "button", (10.0, 10.0, 20.0, 20.0)));
        scr.els.push(el(7, "", "button", (40.0, 10.0, 20.0, 20.0)));
        let (mut ask, _) = script(vec![fast_none(), step("6", 0.95, "click", 0.0, 0.0), step("7", 0.95, "click", 0.0, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![scr], ..Default::default() };
        run("click the two icons", &CFG, &mut ask, &mut d);
        assert_eq!(d.acts, [Act::Click(Target::Mark(6)), Act::Click(Target::Mark(7))]);
    }

    #[test]
    fn abort_during_the_text_call_skips_the_screenshot() {
        // abort lands with the marks emit; the weak text answer must not trigger a capture + 2nd call
        let (mut ask, seen) = script(vec![fast_none(), step("2", 0.6, "click", 0.0, 0.0), step("2", 0.9, "click", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], screen_ok: true, abort_after: Some(2), ..Default::default() };
        assert_eq!(run("open the channel", &CFG, &mut ask, &mut d).status, CmdStatus::Stopped);
        assert_eq!((d.captures, seen.lock().unwrap().len()), (0, 2));
    }

    #[test]
    fn typing_and_secure_input() {
        let (mut ask, _) = script(vec![fast_none(), step("3", 0.95, "focus_and_type", 0.0, 0.0), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        run("type hello in the search field", &CFG, &mut ask, &mut d);
        assert_eq!(d.acts, [Act::Focus(Target::Mark(3)), Act::Paste("hello".into())]);
        let (mut ask, _) = script(vec![fast_none(), step("3", 0.95, "focus_and_type", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], secure: true, ..Default::default() };
        assert_eq!(run("type hunter2 in the field", &CFG, &mut ask, &mut d).status, CmdStatus::Blocked);
        assert!(d.acts.is_empty());
        // no text in the instruction: unsure, nothing typed
        let (mut ask, _) = script(vec![fast_none(), step("3", 0.95, "focus_and_type", 0.0, 0.0)]);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        assert_eq!(run("fill in the search field", &CFG, &mut ask, &mut d).status, CmdStatus::Unsure);
    }

    #[test]
    fn box_fallback_for_a_bare_ax_tree() {
        let bare = Screen { els: vec![el(1, "OK", "button", (750.0, 720.0, 40.0, 20.0))], ..slack() };
        // 1440x900 @2 = 2880x1800 px -> 2x2 tiles of 720x450 pt; tile 3 (bottom-right), box center (0.05, 0.5) -> (756, 675)
        let ans = |vis: f32, b: Value| {
            json!({ "tile": { "choice": "tile_3", "confidence": 0.9 }, "box_3": { "box": b, "confidence": 0.4 }, "visible_3": { "noul": vis },
                    "action": { "choice": "click", "confidence": 0.9 }, "risky": { "noul": 0.0 } })
        };
        let good = json!({ "top": 0.45, "bottom": 0.55, "left": 0.04, "right": 0.06 });
        let (mut ask, seen) = script(vec![fast_none(), ans(0.9, good.clone()), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![bare.clone(), slack()], screen_ok: true, ..Default::default() };
        run("click the gear", &CFG, &mut ask, &mut d);
        assert_eq!(d.captures, 4, "four native tiles");
        assert_eq!(d.acts, [Act::Click(Target::Point(756.0, 675.0))], "no element within 24 pt");
        assert_eq!(seen.lock().unwrap()[1].1["box_3"]["type"], "box");
        // near the OK button (750..790 x 720..740): snaps to it
        let near = json!({ "top": 0.58, "bottom": 0.62, "left": 0.06, "right": 0.08 }); // (770, 720)
        let (mut ask, _) = script(vec![fast_none(), ans(0.9, near), step("none", 0.0, "done", 0.0, 0.9)]);
        let mut d = Fake { screens: vec![bare.clone(), slack()], screen_ok: true, ..Default::default() };
        run("click ok", &CFG, &mut ask, &mut d);
        assert_eq!(d.acts, [Act::Click(Target::Mark(1))]);
        // not visible, or a broken box: never click
        for (vis, b) in [(0.2, good.clone()), (0.9, json!({ "top": 0.03, "bottom": 0.97, "left": 0.1, "right": 0.2 }))] {
            let (mut ask, _) = script(vec![fast_none(), ans(vis, b)]);
            let mut d = Fake { screens: vec![bare.clone()], screen_ok: true, ..Default::default() };
            assert_eq!(run("click the gear", &CFG, &mut ask, &mut d).status, CmdStatus::Unsure);
            assert!(d.acts.is_empty());
        }
        // no Screen Recording: setup, no capture
        let (mut ask, _) = script(vec![fast_none()]);
        let mut d = Fake { screens: vec![bare], ..Default::default() };
        assert_eq!(run("click the gear", &CFG, &mut ask, &mut d).message.as_deref(), Some("screen_recording"));
        assert_eq!(d.captures, 0);
    }

    /// The full decision sequence over HTTP: the real jev::ask against a mock Decisions API.
    #[test]
    fn mock_http_decision_sequence() {
        let mut srv = mockito::Server::new();
        let bodies = Arc::new(M::new(Vec::<Value>::new()));
        let b2 = bodies.clone();
        let replies = [fast_none(), step("5", 0.97, "click", 0.9, 0.0), step("none", 0.0, "done", 0.0, 0.92)];
        let m = srv
            .mock("POST", "/api/alpha/decisions")
            .match_header("authorization", "Bearer sk-or")
            .with_body_from_request(move |r| {
                let mut b = b2.lock().unwrap();
                b.push(serde_json::from_slice(r.body().unwrap()).unwrap());
                json!({ "answers": replies[b.len() - 1], "usage": { "cost": 0.0001 } }).to_string().into_bytes()
            })
            .expect(3)
            .create();
        let url = format!("{}/api/alpha/decisions", srv.url());
        let mut ask = |s: &Value, q: &Value| jev::ask(&url, "sk-or", "typesafe/jev-1.14", s, q);
        let mut d = Fake { screens: vec![slack()], answers: vec![Answer::Yes], ..Default::default() };
        let end = run("send the message", &CFG, &mut ask, &mut d);
        m.assert();
        assert_eq!(end.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
        assert_eq!(d.waits, [Wait::Confirm]);
        let b = bodies.lock().unwrap();
        assert!(b.iter().all(|b| b["model"] == "typesafe/jev-1.14"));
        assert_eq!(b[0]["questions"]["intent"]["type"], "choice");
        assert_eq!(b[1]["questions"]["target"]["criteria"]["none"], "no element on screen fits");
        assert!(b[1]["questions"].get("done").is_none() && b[2]["questions"]["done"]["type"] == "noul", "done is asked from step 2");
        assert!(end.summary.contains("clicked “Send now” (button)"));
        // a 401 ends the run as an error, nothing clicked
        let m401 = srv.mock("POST", "/x").with_status(401).with_body("{}").create();
        let url = format!("{}/x", srv.url());
        let mut ask = |s: &Value, q: &Value| jev::ask(&url, "bad", "typesafe/jev-1.14", s, q);
        let mut d = Fake { screens: vec![slack()], ..Default::default() };
        let end = run("send it", &CFG, &mut ask, &mut d);
        m401.assert();
        assert_eq!(end.status, CmdStatus::Error);
        assert!(end.message.unwrap().contains("invalid or missing API key"));
        assert!(d.acts.is_empty());
    }

    #[test]
    fn ax_tree_to_marks_request() {
        let tree = n("AXWindow", "", (0.0, 0.0, 1440.0, 900.0), (0..6).map(|i| n("AXButton", &format!("B{i}"), (100.0 * i as f64 + 400.0, 400.0, 50.0, 20.0), vec![])).collect());
        let els = ax::collect(&tree, PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 });
        let (s, q) = marks_request("click B3", "Notes", &[], &els, None);
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
                let d = decide(instr, page, &[], &els, &mut ask, &mut shot).unwrap();
                let hit = d.target.choice == want.to_string();
                (hits, total, cost) = (hits + hit as u32, total + 1, cost + d.cost);
                println!("{page:9} {instr:50} want {want:3} got {:5} conf {:.2} image {} {}", d.target.choice, d.target.confidence, d.used_image, if hit { "HIT" } else { "MISS" });
            }
        }
        println!("{hits}/{total} hits, {} calls, ${cost:.5}", calls.get());
        assert!(hits >= 22, "{hits}/24");
    }
}
