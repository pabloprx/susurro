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
pub const MARKS_DRAW_MS: u64 = 250;
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
pub const PICK_TIMEOUT: Duration = Duration::from_secs(20);
/// Jev's image limit per side.
pub const IMG_MAX: f64 = 1568.0;
/// A drag moves at most this far between two events (points).
pub const DRAG_STEP_PT: f64 = 8.0;
/// Between two drag events (ms): ~120 events/s, slow enough for a canvas app to draw each.
pub const DRAG_EVENT_MS: u64 = 8;
/// The agent's screenshot JPEG quality (sips formatOptions, 0-100).
pub const AGENT_JPEG_QUALITY: u32 = 60;

/// A drag path with a point every DRAG_STEP_PT at most (the first point excluded: the press is there), so a canvas
/// sees a line, not two dots.
pub fn drag_points(path: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        let n = ((b.0 - a.0).hypot(b.1 - a.1) / DRAG_STEP_PT).ceil().max(1.0) as usize;
        out.extend((1..=n).map(|i| (a.0 + (b.0 - a.0) * i as f64 / n as f64, a.1 + (b.1 - a.1) * i as f64 / n as f64)));
    }
    out
}
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
/// Debug test hook only (test_hooks POST /command): every confirm answers yes.
pub static AUTO_CONFIRM: AtomicBool = AtomicBool::new(false);
/// The live session number. A Real desk of an older session (Esc left its thread in a Jev call) is aborted for
/// good once a new session starts, even though `reset` clears ABORT.
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CONFIRMED: AtomicBool = AtomicBool::new(false);
static DENIED: AtomicBool = AtomicBool::new(false);
/// 0 = not waiting, 1 = confirm, 2 = pick.
static WAITING: AtomicU8 = AtomicU8::new(0);
static ANSWER: Mutex<Option<String>> = Mutex::new(None);
/// Pick mode: the choices on screen (global points), and the one Pablo clicked by hand.
static PICK_RECTS: Mutex<Vec<PtRect>> = Mutex::new(Vec::new());
static CLICKED: Mutex<Option<usize>> = Mutex::new(None);

/// The marks window shows these pick choices (Real::emit).
pub fn set_pick_rects(r: Vec<PtRect>) {
    *PICK_RECTS.lock().unwrap() = r;
}

/// Pablo's own left click at `p` (global points) while a pick waits: true = it hit a choice (the app got the click
/// too, so the executor must not click again).
pub fn user_clicked(p: (f64, f64)) -> bool {
    if waiting() != Some(Wait::Pick) {
        return false;
    }
    let hit = choice_at(&PICK_RECTS.lock().unwrap(), p);
    if let Some(i) = hit {
        *CLICKED.lock().unwrap() = Some(i);
    }
    hit.is_some()
}

/// The choice under a click: the smallest one, since a big choice (a web area) may hold the others.
pub fn choice_at(rects: &[PtRect], p: (f64, f64)) -> Option<usize> {
    let inside = |r: &PtRect| p.0 >= r.x && p.0 <= r.x + r.w && p.1 >= r.y && p.1 <= r.y + r.h;
    (0..rects.len()).filter(|&i| inside(&rects[i])).min_by(|&a, &b| (rects[a].w * rects[a].h).total_cmp(&(rects[b].w * rects[b].h)))
}

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
    /// Pick mode: Pablo clicked choice N (0-based) himself; the click already happened.
    Clicked(usize),
    /// Esc or a spoken no.
    No,
    /// Nobody answered in time.
    Timeout,
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
            // a name with at most one word around it ("open youtube music"): "ok search youtube" is an order
            let near = said.split(' ').count() <= name.split(' ').count() + 1;
            name.chars().count() >= 3 && ((near && said.contains(&name)) || (said.chars().count() >= 3 && name.contains(&said)))
        })
        .map(|o| o.0)
        .collect();
    (fits.len() == 1).then(|| fits[0])
}

/// Ordinals: a place in the pick list (0-based). Word starts, so "segunda", "tercero", "cuarta" fit.
const ORDINALS: [(&str, usize); 10] =
    [("first", 0), ("primer", 0), ("second", 1), ("segund", 1), ("third", 2), ("tercer", 2), ("fourth", 3), ("cuart", 3), ("fifth", 4), ("quint", 4)];
/// Words around a pick answer that name nothing ("click on the second one", "go with two", "en la segunda").
/// No verb that acts on its own ("type 5", "open 2" stay orders).
const PICK_AROUND: [&str; 18] = ["click", "clic", "pick", "choose", "elige", "option", "opcion", "on", "en", "a", "to", "go", "with", "take", "quiero", "now", "ahora", "in"];

/// The spoken text answers "Which one?": a shown option number ("2", "dos", "number 4"), an ordinal ("la segunda",
/// "the last one"), "sí / ese / that one", or a shown option name; every other word is filler. Anything else is a new
/// order (live log 2026-10-18: "Ultra Miami 2015." had a number, was taken as the answer, and the order was lost).
pub fn looks_like_pick(text: &str, question: &str) -> bool {
    let opts = pick_options(question);
    let n = crate::text::norm(text);
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() || words.len() > 5 {
        return false;
    }
    if this_one(&words) || pick_by_name(text, &opts).is_some() {
        return true;
    }
    let shown = |k: u32| opts.iter().any(|o| o.0 == k);
    let core: Vec<&str> = words.iter().copied().filter(|w| !PICK_FILLER.contains(w) && !PICK_AROUND.contains(w)).collect();
    if this_one(&core) {
        return true; // "yes please"
    }
    if core.is_empty() {
        // "One.", "the one": the filler "one" is the answer
        return words.contains(&"one") && shown(1);
    }
    // "la de la izquierda", "the top one", "la de la sidebar" (vc-feedback 22)
    if spatial_only(&core) {
        return true;
    }
    core.iter().all(|w| match digits(w).or_else(|| NUMBERS.iter().find(|(s, _)| s == w).map(|p| p.1)) {
        Some(k) => shown(k),
        None => ORDINALS.iter().find(|(o, _)| w.starts_with(o)).is_some_and(|(_, k)| *k < opts.len()) || ((w.starts_with("last") || w.starts_with("ultim")) && !opts.is_empty()),
    })
}

/// "2", "2nd", "3rd" -> the number.
fn digits(w: &str) -> Option<u32> {
    ["", "st", "nd", "rd", "th"].iter().find_map(|s| w.strip_suffix(s)?.parse().ok())
}

/// Pick mode ("Clicks: I choose"): what Pablo said to the numbered choices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Picked {
    /// The choice at this place (0-based): "dos", "the second", "3", its name, "sí" for a highlighted #1.
    Nth(usize),
    /// A mark that is not a choice: a number above the choices that names a mark on screen ("41").
    Mark(u32),
}

/// "ese", "sí", "that one": the highlighted choice #1. `words` are normalized (text::norm).
fn this_one(words: &[&str]) -> bool {
    const W: [&str; 12] = ["si", "yes", "yeah", "yep", "ok", "okay", "dale", "ese", "esa", "este", "esta", "eso"];
    // every word: "ok search youtube" is an order, not a yes to choice 1
    !words.is_empty() && words.len() <= 3 && words.iter().all(|w| W.contains(w) || ["that", "this", "one"].contains(w)) && words.iter().any(|w| *w != "one")
}

/// The answer to the numbered choices (1..=names.len()). "sí" / "ese" takes #1, Jev's best.
pub fn parse_choice(text: &str, names: &[String], ids: &[u32]) -> Option<Picked> {
    let l = crate::text::norm(text);
    let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let n = names.len();
    let num = |k: u32| if k >= 1 && k as usize <= n { Some(Picked::Nth(k as usize - 1)) } else { ids.contains(&k).then_some(Picked::Mark(k)) };
    for w in &words {
        if let Some(k) = digits(w) {
            return num(k);
        }
        if let Some((_, k)) = ORDINALS.iter().find(|(o, _)| w.starts_with(o)) {
            return (*k < n).then_some(Picked::Nth(*k));
        }
        if (w.starts_with("last") || w.starts_with("ultim")) && n > 0 {
            return Some(Picked::Nth(n - 1));
        }
    }
    let opts: Vec<(u32, String)> = names.iter().enumerate().map(|(i, s)| (i as u32 + 1, s.clone())).collect();
    if let Some(k) = pick_by_name(text, &opts) {
        return Some(Picked::Nth(k as usize - 1));
    }
    if let Some(k) = words.iter().find_map(|w| NUMBERS.iter().find(|(s, _)| s == w).map(|p| p.1)) {
        return num(k);
    }
    let core: Vec<&str> = words.iter().copied().filter(|w| !PICK_FILLER.contains(w) && !PICK_AROUND.contains(w)).collect();
    (this_one(&words) || this_one(&core)).then_some(Picked::Nth(0))
}

/// "[41] YouTube Music (link, main)" -> (Some(41), "YouTube Music (link, main)"). The planner copies the mark text with
/// its on-screen number.
pub fn split_mark(words: &str) -> (Option<u32>, &str) {
    let w = words.trim();
    if let Ok(n) = w.trim_start_matches('#').parse() {
        return (Some(n), ""); // "41", "#41"
    }
    let Some(rest) = w.strip_prefix('[') else { return (None, w) };
    match rest.split_once(']') {
        Some((n, tail)) => match n.trim().parse() {
            Ok(n) => (Some(n), tail.trim()),
            Err(_) => (None, w),
        },
        None => (None, w),
    }
}

/// The element a "[N] label" target means on a fresh read: the label when it is unique, else the mark N that was
/// drawn (`board`: what Pablo saw) found again by its rect, else the fresh mark N if it has the same label.
pub fn mark_target<'a>(words: &str, els: &'a [El], board: &[El]) -> Option<&'a El> {
    let (n, rest) = split_mark(words);
    if let Some(e) = local_match(rest, els) {
        return Some(e);
    }
    let n = n?;
    let same = |a: &El, b: &El| ax::describe(a) == ax::describe(b);
    match board.iter().find(|b| b.mark.id == n) {
        Some(b) => els.iter().find(|e| ax::same_rect(e.mark.rect, b.mark.rect) && same(e, b)).or_else(|| els.iter().find(|e| e.mark.id == n && same(e, b))),
        // no board (tests, a first order): only a bare "[N]" may take the fresh mark N
        None => rest.is_empty().then(|| els.iter().find(|e| e.mark.id == n)).flatten(),
    }
}

/// Pablo said this number himself ("open the first link, 41"): he already chose, no pick list.
pub fn said_number(said: &str, n: u32) -> bool {
    said.split(|c: char| !c.is_ascii_digit()).any(|w| w.parse() == Ok(n))
}

/// Return sends a message only in chat and mail apps (Claude desktop counts: its chat input sends). In browsers,
/// terminals, address bars and any search field it runs at once.
/// ponytail: a fixed app list; add an app here when Return sends there.
const CHAT_APPS: [&str; 18] = [
    "Slack", "WhatsApp", "Messages", "Mail", "Discord", "Microsoft Teams", "Teams", "Telegram", "Claude", "com.tinyspeck.slackmacgap",
    "net.whatsapp.WhatsApp", "com.apple.MobileSMS", "com.apple.mail", "com.hnc.Discord", "com.microsoft.teams2", "com.microsoft.teams",
    "ru.keepcoder.Telegram", "com.anthropic.claudefordesktop",
];

/// cmd+delete / cmd+forward delete outside a text field moves the selection to the Trash (Finder): risky
/// (GUI round 2: the agent trashed a folder with no confirm). In a text field it only deletes text.
pub fn trashes(code: u16, flags: u64, f: &Field) -> bool {
    let text = ["AXTextField", "AXTextArea", "AXSearchField", "AXComboBox"].contains(&f.role.as_str());
    (code == DELETE || code == 117) && flags & crate::paste::CMD != 0 && !text
}

pub fn return_asks(f: &Field) -> bool {
    let chat = CHAT_APPS.iter().any(|a| a.eq_ignore_ascii_case(&f.app) || a.eq_ignore_ascii_case(&f.bundle));
    let n = crate::text::norm(&f.name);
    let search = f.role == "AXSearchField" || ["search", "buscar", "busca", "find"].iter().any(|w| n.contains(w));
    chat && !search
}

/// A Return right after a jump palette (Slack / Discord / Claude cmd+k) opens what was typed; it sends nothing, so it
/// needs no confirm. `mem` newest last: [.., key <palette>, typed <name>]. All in the field `f` focused now: the same
/// app with a palette, the same pid, the value susurro left (nothing typed by hand since), under PALETTE_AGE_MS old.
/// Anything else (the cmd+k went to WhatsApp, Pablo typed a message himself) asks as usual.
pub fn after_palette(mem: &Memory, f: &Field) -> bool {
    const PALETTE: [&str; 3] = ["cmd+k", "cmd+p", "cmd+shift+p"];
    const APPS: [&str; 6] = ["Slack", "Discord", "Claude", "com.tinyspeck.slackmacgap", "com.hnc.Discord", "com.anthropic.claudefordesktop"];
    const PALETTE_AGE_MS: i64 = 30_000;
    let palette_app = APPS.iter().any(|a| a.eq_ignore_ascii_case(&f.app) || a.eq_ignore_ascii_case(&f.bundle));
    let mut it = mem.iter().rev().filter(|e| e.e.kind != MemKind::Said);
    let (Some(t), Some(k)) = (it.next(), it.next()) else { return false };
    palette_app
        && t.e.kind == MemKind::Typed
        && k.e.kind == MemKind::Key
        && PALETTE.contains(&k.e.text.as_str())
        && t.pid == f.pid
        && k.pid == f.pid
        && t.value_after == f.value
        && crate::history::now_ms() - t.e.at < PALETTE_AGE_MS
}

// ---------- per-app strategy (next-build 6): data, not branches ----------

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Kind {
    /// Cocoa (Finder, Mail, Notes, Settings, WhatsApp here: research 3): the AX tree, AXPress on leaves.
    #[default]
    Native,
    /// web.rs DOM marks first; the AX tree when JS from Apple Events is off.
    Browser,
    /// AXManualAccessibility (ax::mac::read sets it), keyboard first (planner hints), real clicks on rows and links.
    Electron,
    /// Keys and typing only.
    Terminal,
}

/// Bundle ids and names. Unknown = Native. ponytail: a fixed list; add an app when it shows up.
const APP_KINDS: [(&str, Kind); 26] = [
    ("com.google.Chrome", Kind::Browser), ("com.microsoft.edgemac", Kind::Browser), ("com.apple.Safari", Kind::Browser),
    ("company.thebrowser.Browser", Kind::Browser), ("com.brave.Browser", Kind::Browser), ("company.thebrowser.dia", Kind::Browser),
    ("Google Chrome", Kind::Browser), ("Microsoft Edge", Kind::Browser), ("Safari", Kind::Browser), ("Arc", Kind::Browser),
    ("Brave Browser", Kind::Browser), ("com.tinyspeck.slackmacgap", Kind::Electron), ("com.hnc.Discord", Kind::Electron),
    ("com.todesktop.230313mzl4w4u92", Kind::Electron), ("com.microsoft.VSCode", Kind::Electron), ("com.anthropic.claudefordesktop", Kind::Electron),
    ("notion.id", Kind::Electron), ("com.linear", Kind::Electron), ("com.figma.Desktop", Kind::Electron), ("Slack", Kind::Electron),
    ("Discord", Kind::Electron), ("Cursor", Kind::Electron), ("Visual Studio Code", Kind::Electron), ("Claude", Kind::Electron),
    ("Notion", Kind::Electron), ("Linear", Kind::Electron),
];

pub fn app_kind(app: &str, bundle: &str) -> Kind {
    let is = |a: &str| a.eq_ignore_ascii_case(app) || a.eq_ignore_ascii_case(bundle);
    if TERMINALS.iter().any(|t| is(t)) {
        return Kind::Terminal;
    }
    APP_KINDS.iter().find(|(a, _)| is(a)).map_or(Kind::Native, |k| k.1)
}

/// Electron rows, cells and links ignore AXPress (vc-feedback 21: the Slack DM row "clicked", nothing opened):
/// a real click at the leaf from the start.
pub fn real_click_first(kind: Kind, role: &str) -> bool {
    kind == Kind::Electron && matches!(role, "row" | "link" | "tab" | "menu item")
}

// ---------- ambiguity: context priors, then 2-3 candidates with spatial answers (vc-feedback 22, 23) ----------

/// #2 within this of #1 = a real tie (case 23: act on #1 otherwise). ponytail: a first guess, not measured.
pub const TIE_GAP: f32 = 0.15;

/// The candidates tied with #1 (at most 3), from an on-screen ranking (`choices`). One = act on it.
pub fn tied(c: &[(u32, f32)]) -> Vec<(u32, f32)> {
    let Some(top) = c.first().map(|x| x.1) else { return vec![] };
    c.iter().take(3).filter(|x| top - x.1 <= TIE_GAP).copied().collect()
}

/// Region words in an order or an answer -> the mark region they name.
const REGION_WORDS: [(&[&str], &str); 4] = [
    (&["sidebar", "lateral"], "sidebar"),
    (&["bottom", "abajo", "inferior", "debajo"], "bottom"),
    (&["toolbar", "barra", "pestaña", "pestana", "tab"], "toolbar"),
    (&["principal", "main", "contenido", "content", "pagina", "page", "resultado", "result"], "main"),
];
/// Words that name the browser's own chrome: then the page prior is off.
const CHROME_WORDS: [&str; 10] = ["tab", "pestaña", "pestana", "sidebar", "lateral", "barra", "toolbar", "address", "direcciones", "url"];

fn said_words(text: &str) -> Vec<String> {
    crate::text::norm(text).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

fn says(words: &[String], stems: &[&str]) -> bool {
    words.iter().any(|w| stems.iter().any(|s| w.starts_with(&crate::text::norm(s))))
}

/// "la de la izquierda", "the top one", "la de la sidebar", "the one in the header": a candidate by place. A region
/// word names the only candidate in that region; left / right / top / bottom compare the candidates' centers (8 pt
/// apart at least). None = no such word, or no single answer.
pub fn spatial_choice(text: &str, cands: &[(PtRect, &str)]) -> Option<usize> {
    let w = said_words(text);
    for (stems, region) in REGION_WORDS {
        let m: Vec<usize> = (0..cands.len()).filter(|&i| cands[i].1 == region).collect();
        if says(&w, stems) && m.len() == 1 {
            return Some(m[0]);
        }
    }
    let ends: [(&[&str], fn((f64, f64)) -> f64, bool); 4] = [
        (&["izquierd", "left"], |p| p.0, false),
        (&["derech", "right"], |p| p.0, true),
        (&["arriba", "top", "header", "encabezado", "superior", "above"], |p| p.1, false),
        (&["abajo", "bottom", "inferior", "below", "under", "debajo"], |p| p.1, true),
    ];
    let (_, key, max) = ends.iter().find(|(stems, _, _)| says(&w, stems))?;
    let mut v: Vec<(usize, f64)> = cands.iter().enumerate().map(|(i, c)| (i, key(center(c.0)))).collect();
    v.sort_by(|a, b| if *max { b.1.total_cmp(&a.1) } else { a.1.total_cmp(&b.1) });
    match v.as_slice() {
        [a, b, ..] if (a.1 - b.1).abs() < 8.0 => None,
        [a, ..] => Some(a.0),
        [] => None,
    }
}

/// Words that only place a candidate (an answer made of them is a pick, not an order).
pub fn spatial_only(words: &[&str]) -> bool {
    const W: [&str; 26] = [
        "izquierd", "left", "derech", "right", "arriba", "top", "header", "encabezado", "superior", "above", "abajo", "bottom",
        "inferior", "below", "under", "debajo", "sidebar", "lateral", "toolbar", "barra", "principal", "main", "contenido",
        "pagina", "page", "medio",
    ];
    !words.is_empty() && words.iter().all(|w| W.iter().any(|s| w.starts_with(s)))
}

/// Context priors before asking (case 23), on the tied candidates: the region Pablo named in the order, his place
/// words ("the left one"), then on a browser page the page content over the browser's own chrome unless he names
/// the chrome (tab / pestaña / sidebar / barra). Each prior applies only when it keeps at least one candidate.
pub fn prior(c: &[u32], els: &[El], order: &str, page: Option<PtRect>) -> Vec<u32> {
    let el = |id: &u32| els.iter().find(|e| e.mark.id == *id);
    let mut c: Vec<u32> = c.iter().copied().filter(|id| el(id).is_some()).collect();
    let w = said_words(order);
    let keep = |c: &mut Vec<u32>, f: &dyn Fn(&El) -> bool| {
        let k: Vec<u32> = c.iter().copied().filter(|id| el(id).is_some_and(f)).collect();
        if !k.is_empty() {
            *c = k;
        }
    };
    for (stems, region) in REGION_WORDS {
        if says(&w, stems) {
            keep(&mut c, &|e| e.mark.region == region);
        }
    }
    if c.len() > 1 {
        let places: Vec<(PtRect, &str)> = c.iter().filter_map(el).map(|e| (e.mark.rect, e.mark.region.as_str())).collect();
        if let Some(i) = spatial_choice(order, &places) {
            c = vec![c[i]];
        }
    }
    if let Some(p) = page.filter(|_| c.len() > 1 && !says(&w, &CHROME_WORDS)) {
        let inside = |r: PtRect| {
            let (x, y) = center(r);
            x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h
        };
        keep(&mut c, &|e| inside(e.mark.rect));
    }
    c
}

/// Case 24 + research 5: where to click `r` so the element there is the target. Probes the center, then the
/// centers of its left and right thirds and its top and bottom halves; `hit(p)` = Some(true) the element at p is the
/// target or inside it, Some(false) something else covers it, None no AX answer (the center then). None = every probe
/// hit something else: do not click.
pub fn aim_point(r: PtRect, hit: &mut dyn FnMut((f64, f64)) -> Option<bool>) -> Option<(f64, f64)> {
    let c = center(r);
    let probes = [c, ((r.x + r.w / 6.0).round(), c.1), ((r.x + r.w * 5.0 / 6.0).round(), c.1), (c.0, (r.y + r.h / 4.0).round()), (c.0, (r.y + r.h * 3.0 / 4.0).round())];
    for (i, p) in probes.into_iter().enumerate() {
        match hit(p) {
            Some(true) => return Some(p),
            None if i == 0 => return Some(p),
            _ => {}
        }
    }
    None
}

/// Vision lines (ocr::Line) of a capture of `win` -> marks, reading order (role "text", region by geometry).
pub fn ocr_els(lines: &[crate::ocr::Line], win: PtRect) -> Vec<El> {
    let mut out: Vec<El> = lines
        .iter()
        .filter(|l| !l.0.trim().is_empty())
        .map(|(t, b)| {
            let rect = crate::ocr::to_points(*b, win);
            El { mark: Mark { id: 0, rect, name: t.trim().chars().take(60).collect(), role: "text".into(), region: ax::region_of(rect, win).into() }, state: None, h: usize::MAX, url: None, value: None }
        })
        .take(ax::MAX_MARKS)
        .collect();
    out.sort_by(|a, b| ((a.mark.rect.y / 4.0).round(), a.mark.rect.x).partial_cmp(&((b.mark.rect.y / 4.0).round(), b.mark.rect.x)).unwrap());
    for (i, e) in out.iter_mut().enumerate() {
        e.mark.id = i as u32 + 1;
    }
    out
}

/// The text of "type hello in the search field", "write 'meet in the lobby'", "busca gatos".
/// ponytail: verb + cut at "in the / into / en el"; an LLM would read odd phrasings (quotes always work).

/// The second box pass crops this many native px a side at least (research 6: 448-640 px; Jev's minimum is 280).
/// ponytail: the research's pick, not measured on a real icon yet.
pub const CROP_MIN_PX: f64 = 448.0;

/// Coarse-to-fine (next-build 4): the crop for the second Jev box, around the first box `b` on an image of `img`
/// (global points): 3x the box, never under CROP_MIN_PX native px, at most IMG_MAX px, kept inside `img`.
pub fn refine_crop(b: Bbox, img: PtRect, scale: f64) -> PtRect {
    let s = scale.max(1.0);
    let side = |v: f64, max: f64| (3.0 * v).max(CROP_MIN_PX / s).min(IMG_MAX / s).min(max);
    let (w, h) = (side((b.right - b.left) * img.w, img.w), side((b.bottom - b.top) * img.h, img.h));
    let (cx, cy) = box_point(b, img);
    PtRect { x: (cx - w / 2.0).clamp(img.x, img.x + img.w - w), y: (cy - h / 2.0).clamp(img.y, img.y + img.h - h), w, h }
}

/// The rect of box `b` on an image of `img` (global points).
pub fn box_rect(b: Bbox, img: PtRect) -> PtRect {
    PtRect { x: img.x + b.left * img.w, y: img.y + b.top * img.h, w: (b.right - b.left) * img.w, h: (b.bottom - b.top) * img.h }
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

/// One Jev box pass on one image (`screen`): the box, the "is it visible" gate, and risky on the first pass.
pub fn box_request(instr: &str, app: &str, img: &Value, first: bool) -> (Value, Value) {
    let what = if first { "the frontmost app's window" } else { "a zoomed part of the frontmost app's window" };
    let state = json!({ "app": app, "about": format!("`screen` is {what}."), "screen": img });
    let mut q = json!({
        "box": { "type": "box", "instructions": format!("The element to click to do this: \"{instr}\", in `screen`.") },
        "visible": { "type": "noul", "instructions": format!("Is the element to click for \"{instr}\" visible in `screen`?") },
    });
    if first {
        q["risky"] = risky_q(instr);
    }
    (state, q)
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

/// Words of a target that name no element: articles, ordinals, roles, places, verbs ("the first link", "el botón
/// de arriba"). ponytail: a word list; a missing word only sends a good match to the box.
const GENERIC: &[&str] = &[
    "the", "a", "an", "el", "la", "los", "las", "un", "una", "de", "del", "on", "in", "at", "to", "of", "en", "and", "or", "y",
    "with", "con", "for", "para", "first", "second", "third", "last", "primer", "primera", "primero", "segundo", "segunda",
    "tercer", "tercera", "ultimo", "ultima", "one", "uno", "link", "enlace", "button", "boton", "tab", "pestana", "field",
    "campo", "box", "input", "result", "resultado", "item", "icon", "icono", "menu", "option", "opcion", "this", "that",
    "it", "esto", "eso", "ese", "esa", "este", "esta", "top", "bottom", "left", "right", "arriba", "abajo", "izquierda",
    "derecha", "sidebar", "toolbar", "main", "page", "pagina", "bar", "barra", "click", "open", "abre", "select", "press",
    "not", "no", "other", "otra", "otro", "thing", "cosa",
];

/// The target words name this element (case 26): some content word of `words` starts a word of `name`, or the
/// other way round ("pricing" / "Pricing | Acme", "la factura" / "Enviar factura"). Only generic words
/// ("the first link") = true: Jev's pick stands. "red" / "Colors" = false: no fuzzy map to another element.
pub fn names(words: &str, name: &str) -> bool {
    let words = words.split(" (").next().unwrap_or(words);
    let content: Vec<String> = said_words(words).into_iter().filter(|w| w.chars().count() >= 2 && !GENERIC.contains(&w.as_str())).collect();
    let name = said_words(name);
    content.is_empty() || content.iter().any(|w| name.iter().any(|n| (w.len() >= 3 && n.starts_with(w.as_str())) || (n.len() >= 3 && w.starts_with(n.as_str())) || n == w))
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
    /// A real CGEvent click at the target (hit-tested): the retry when a click changed nothing (vc-feedback 21).
    RealClick(Target),
    /// Agent mouse control (global points): the left button down at the first point, dragged through every point, up
    /// at the last; `flags` (CGEventFlags bits) held the whole way (shift = a straight line in paint apps).
    Drag(Vec<(f64, f64)>, u64),
    MouseMove(f64, f64),
    /// A raw click at a point: the right button or not, 1-3 clicks (2 = double click), modifier flags.
    ClickAt { at: (f64, f64), right: bool, count: u8, flags: u64 },
    /// keycode + flags, held down this long (ms).
    KeyHold(u16, u64, u64),
}

impl Act {
    /// The test guard (SUSURRO_TEST_PASTE_TARGET) checks the front app before this act. Not before OpenApp: that one
    /// brings the target to the front (GUI round 1: the guard blocked "abre Slack" from another app).
    pub fn guarded(&self) -> bool {
        !matches!(self, Act::OpenApp(_))
    }
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
    /// The front window (global points). Zero = unknown (the display stands in).
    pub window: PtRect,
    pub kind: Kind,
    /// The web page inside a browser window (ax::web_area): the case-23 page prior.
    pub page: Option<PtRect>,
    /// The front window's title (the page title in a browser). Empty = unknown.
    pub title: String,
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
    /// The agent's screenshot: JPEG of `r`, the long side fitted to `max_px`. Default: `capture`.
    fn snapshot(&mut self, r: PtRect, _max_px: f64) -> anyhow::Result<Vec<u8>> {
        self.capture(r, false)
    }
    fn act(&mut self, a: &Act) -> anyhow::Result<()>;
    /// macOS secure input is on (a password field has the focus somewhere): no keys, no typing.
    fn secure_input(&mut self) -> bool;
    /// The element at this global point is a password field (box fallback clicks: no AX mark to check).
    fn secure_at(&mut self, p: (f64, f64)) -> bool;
    fn emit(&mut self, s: &CommandStep);
    fn sleep(&mut self, ms: u64);
    fn wait(&mut self, w: Wait, timeout: Duration) -> Answer;
    fn aborted(&self) -> bool;
    /// Pablo pressed a key or clicked by hand (live.rs input monitor): the agent stops, the queue waits.
    fn paused(&self) -> bool {
        false
    }
    /// correct_word: add `heard` as a correction of the dictionary word `word`. Err = not saved (the reason).
    fn learn_word(&mut self, _word: &str, _heard: &str) -> Result<(), String> {
        Ok(())
    }
    /// The marks drawn on screen right now (what Pablo reads a number from). Empty = none known.
    fn board(&mut self) -> Vec<El> {
        vec![]
    }
    /// The installed app names (open_app matches the spoken name against them). Empty = open the name as said.
    fn apps(&mut self) -> Vec<String> {
        vec![]
    }
    /// What the screen shows, hashed (focused element, window title, URL, AX tree): equal before and after a click =
    /// nothing changed. None = cannot tell (no verify).
    fn sig(&mut self) -> Option<u64> {
        None
    }
    /// Vision text lines of `r` (global points). Empty = none or no OCR.
    fn ocr(&mut self, _r: PtRect) -> anyhow::Result<Vec<crate::ocr::Line>> {
        Ok(vec![])
    }
}

/// Spoken name -> app name.
pub const APP_ALIASES: [(&str, &str); 9] = [
    ("vs code", "Visual Studio Code"), ("vscode", "Visual Studio Code"), ("visual studio", "Visual Studio Code"), ("code", "Visual Studio Code"),
    ("edge", "Microsoft Edge"), ("chrome", "Google Chrome"), ("claude", "Claude"), ("ghosty", "Ghostty"), ("ghostie", "Ghostty"),
];

/// The installed app a spoken name means: "edge" -> Microsoft Edge, "ghosty" -> Ghostty. Rungs, first hit wins:
/// 1. the same name, 2. a whole word of the name, 3. a name that starts with it, 4. 1-2 typos (names of 5+ letters).
/// Ties: the shortest name ("chrome" -> Google Chrome, not Chrome Remote Desktop).
pub fn match_app(spoken: &str, installed: &[String]) -> Option<String> {
    let squash = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let want = squash(spoken.trim().trim_end_matches(".app"));
    if want.is_empty() {
        return None;
    }
    // The names STT and Pablo use for apps (live log 2026-10-18): the alias wins, installed or not.
    if let Some((_, name)) = APP_ALIASES.iter().find(|(a, _)| squash(a) == want) {
        return Some(installed.iter().find(|n| n.eq_ignore_ascii_case(name)).cloned().unwrap_or_else(|| name.to_string()));
    }
    let wc: Vec<char> = want.chars().collect();
    let max_typos = match wc.len() { 0..=4 => 0, 5..=8 => 1, _ => 2 };
    let rungs: [&dyn Fn(&str) -> bool; 4] = [
        &|n| squash(n) == want,
        &|n| n.split(|c: char| !c.is_alphanumeric()).any(|w| squash(w) == want),
        &|n| squash(n).starts_with(&want),
        &|n| {
            let names = std::iter::once(squash(n)).chain(n.split_whitespace().map(squash));
            names.into_iter().any(|x| crate::dictionary::dl(&x.chars().collect::<Vec<_>>(), &wc) <= max_typos)
        },
    ];
    rungs.iter().find_map(|hit| installed.iter().filter(|n| hit(n)).min_by_key(|n| n.chars().count()).cloned())
}

pub struct Cfg {
    pub confirm_risky: bool,
    /// "Clicks: I choose": every click target shows numbered choices and waits for Pablo's pick.
    pub choose: bool,
    /// Agent mode: the step cap (the pill shows "step N: ..."). 0 = a live session.
    pub max: u32,
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
    /// Clicked: the element's rect, so a retry skips that one element and not its namesakes (two "Share" buttons).
    pub rect: Option<PtRect>,
}

pub type Memory = VecDeque<Entry>;

pub fn remember(mem: &mut Memory, kind: MemKind, text: &str, f: &Field, value_after: Option<String>) {
    let field = if f.name.is_empty() { f.role.trim_start_matches("AX").to_string() } else { f.name.clone() };
    mem.push_back(Entry {
        e: MemoryEntry { at: crate::history::now_ms(), kind, text: text.into(), app: f.app.clone(), field },
        pid: f.pid,
        role: f.role.clone(),
        value_after,
        rect: None,
    });
    while mem.len() > MEMORY_MAX {
        mem.pop_front();
    }
}

/// Remember a click with the element's rect (a retry skips that one element).
fn clicked(mem: &mut Memory, h: &Hit, f: &Field) {
    remember(mem, MemKind::Clicked, &h.mark.name, f, None);
    if let Some(m) = mem.back_mut() {
        m.rect = Some(h.mark.rect);
    }
}

fn quote(s: &str) -> String {
    format!("“{}”", if s.is_empty() { "(no name)" } else { s })
}

/// One action in flight: its step row and the end helpers.
struct Run {
    st: CommandStep,
    choose: bool,
    /// The words of the order (Action::said).
    said: String,
    /// The front app's kind, from the last screen read.
    kind: Kind,
    /// retry (case 27): the last click (name, rect, app); that element is never the answer again.
    not: Option<(String, Option<PtRect>, String)>,
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

    /// Wait for a yes. Err = the run ended Stopped: no / Esc, or a timeout that says so (the pill shows it).
    fn confirm(&mut self, desk: &mut dyn Desk, question: &str, what: &str) -> Result<(), End> {
        self.st.status = CmdStatus::Confirm;
        self.st.message = Some(question.into());
        desk.emit(&self.st);
        let a = desk.wait(Wait::Confirm, CONFIRM_TIMEOUT);
        self.st.message = None;
        match a {
            Answer::Yes => Ok(()),
            Answer::Timeout => Err(self.fail(desk, CmdStatus::Stopped, &format!("No answer in {} s: {what}", CONFIRM_TIMEOUT.as_secs()))),
            _ => Err(self.end(desk, CmdStatus::Stopped, None, String::new())),
        }
    }
}

/// A grounded target.
struct Hit {
    target: Target,
    mark: Mark,
    url: Option<String>,
    value: Option<String>,
    risky: f32,
    /// Pablo chose it (pick mode): no risky confirm on top.
    picked: bool,
    /// Pablo clicked it himself: the app already got the click.
    by_user: bool,
    /// Jev was sure of it (>= CONF_MIN): a synonym, not a guess; no "not sure" in the pill.
    sure: bool,
}

impl Hit {
    fn of(e: &El, risky: f32) -> Self {
        Hit { target: Target::Mark(e.mark.id), mark: e.mark.clone(), url: e.url.clone(), value: e.value.clone(), risky, picked: false, by_user: false, sure: false }
    }
}

/// Read the screen and find `words` on it. Err = the run ended (unsure, setup, pick refused...).
fn ground(r: &mut Run, verb: &str, words: &str, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    let mut scr = match desk.read() {
        Ok(s) => s,
        Err(e) if e == "accessibility" => return Err(r.fail(desk, CmdStatus::Setup, "accessibility")),
        Err(e) => return Err(r.fail(desk, CmdStatus::Unsure, &e)),
    };
    if let Some((name, rect, app)) = r.not.clone() {
        // Pablo changed apps since the click: "no, the other one" is not about this window
        if !app.is_empty() && !scr.app.is_empty() && app != scr.app {
            return Err(r.fail(desk, CmdStatus::Unsure, &format!("The last click was in {app}, not {}", scr.app)));
        }
        let same = |e: &El| e.mark.name.eq_ignore_ascii_case(&name);
        // namesakes ("the other Share"): drop only the one nearest to where the last click was
        let near = rect.filter(|_| scr.els.iter().filter(|e| same(e)).count() > 1).and_then(|rc| {
            let (x, y) = center(rc);
            let d = |e: &&El| (center(e.mark.rect).0 - x).powi(2) + (center(e.mark.rect).1 - y).powi(2);
            scr.els.iter().filter(|e| same(e)).min_by(|a, b| d(a).total_cmp(&d(b))).map(|e| e.mark.id)
        });
        // a box click has no name: its rect alone keeps the next box off that spot (box_ground)
        scr.els.retain(|e| if near.is_some() { Some(e.mark.id) != near } else { name.is_empty() || !same(e) });
    }
    r.kind = scr.kind;
    // the marks Pablo read a number from, before this read replaces them on screen
    let board = desk.board();
    r.st.display = scr.display;
    r.st.marks = scr.els.iter().map(|e| e.mark.clone()).collect();
    r.st.status = CmdStatus::Marks;
    desk.emit(&r.st);
    let local = mark_target(words, &scr.els, &board);
    // pick mode: every click; another target (the field to type into...) only when no mark has its exact name
    let pick_mode = r.choose && (verb == "click" || local.is_none());
    // A number Pablo said himself ("41") is his choice already.
    let said_it = split_mark(words).0.is_some_and(|n| said_number(&r.said, n));
    if let Some(e) = local.filter(|_| !pick_mode || said_it) {
        let risky = if risky_name(&e.mark.name) { 1.0 } else { 0.0 };
        return Ok(Hit { picked: said_it, ..Hit::of(e, risky) });
    }
    // Pick mode with nothing to choose: the only search box (combo box, text field...) of its kind on screen.
    if let Some(e) = local.filter(|e| only_of_its_kind(e, &scr.els)) {
        let h = Hit::of(e, if risky_name(&e.mark.name) { 1.0 } else { 0.0 });
        flash(r, desk, &h);
        return Ok(h);
    }
    let words = split_mark(words).1;
    let instr = format!("{verb} {words}");
    // the box sees what Pablo said too: "red" + "on the bottom" (case 26)
    let instr_said = if r.said.trim().is_empty() { instr.clone() } else { format!("{instr} (the user said: \"{}\")", r.said.trim()) };
    if scr.els.len() < MIN_AX {
        return ocr_ground(r, &instr_said, words, &scr, ask, desk);
    }
    if pick_mode {
        return choose(r, &instr_said, &scr, local.map(|e| e.mark.id), ask, desk);
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
    // Jev: no mark fits (a palette color, an icon on a canvas, case 26): look at the pixels
    if d.target.choice == "none" && desk.screen_ok() {
        log::warn!("{words:?} has no mark (Jev: none): the box");
        return box_ground(r, &instr_said, &scr, ask, desk);
    }
    let conf = if d.target.choice == "none" { 0.0 } else { d.target.confidence };
    if d.need_screen && conf < CONF_MIN {
        return Err(r.fail(desk, CmdStatus::Setup, "screen_recording"));
    }
    if gate(conf, 0.0, false) == Gate::Unsure {
        return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE));
    }
    // Auto (case 23): #1 unless #1 and #2 truly tie and the context priors cannot break the tie; then only the 2-3
    // tied candidates get boxes and Pablo answers by number, place or name (case 22).
    let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
    let mut ranked = choices(&d.target.ranked, &ids, None);
    if ranked.is_empty() {
        // no probabilities in the answer: the choice alone
        ranked.extend(d.target.choice.parse::<u32>().ok().filter(|id| ids.contains(id)).map(|id| (id, conf)));
    }
    let c = tied(&ranked);
    let order = format!("{words} {}", r.said);
    let keep = prior(&c.iter().map(|x| x.0).collect::<Vec<_>>(), &scr.els, &order, scr.page);
    let el = |id: u32| scr.els.iter().find(|e| e.mark.id == id);
    match keep.as_slice() {
        // Jev's pick has another name ("red" -> the menu "Colors", case 26): no such mark, so look at the pixels
        // Jev sure of a synonym ("search" -> "Buscar" 0.98, "settings" -> "Preferencias" 0.91; "red" -> "Colors" was
        // 0.32, measured 2026-10-18) is no mismatch: only a weak pick with another name goes to the box.
        [one] if conf < CONF_MIN && el(*one).is_some_and(|e| !names(words, &e.mark.name)) => {
            log::warn!("{words:?} has no mark (Jev: {:?}): the box", el(*one).map(|e| e.mark.name.as_str()));
            box_ground(r, &instr_said, &scr, ask, desk)
        }
        [one] => el(*one).map(|e| Hit { sure: conf >= CONF_MIN, ..Hit::of(e, d.risky) }).ok_or_else(|| r.fail(desk, CmdStatus::Unsure, NOT_SURE)),
        [] => Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)),
        many => {
            let c: Vec<(u32, f32)> = c.into_iter().filter(|x| many.contains(&x.0)).collect();
            ask_pick(r, c, &scr, d.risky, desk)
        }
    }
}

/// Choices shown in pick mode.
pub const CHOICES_MAX: usize = 5;
/// Past the first 3, a choice needs this much of Jev's probability to be shown.
const CHOICE_MIN: f32 = 0.05;
/// Jev this sure of #1, and #2 under SURE_NEXT_MAX: no choices, #1 is highlighted for HIGHLIGHT_MS and clicked.
/// ponytail: first guesses, not measured; RESULTS.md: every miss was < 0.8.
pub const SURE_TOP_MIN: f32 = 0.9;
pub const SURE_NEXT_MAX: f32 = 0.3;
/// An obvious target shows its brackets this long before the action (Pablo sees what is clicked).
pub const HIGHLIGHT_MS: u64 = 300;

/// The choices Pablo sees: Jev's ranking (ids on screen only), the planner's exact match kept in, 3 to 5 of them.
pub fn choices(ranked: &[(String, f32)], ids: &[u32], local: Option<u32>) -> Vec<(u32, f32)> {
    let mut c: Vec<(u32, f32)> = ranked.iter().filter_map(|(k, p)| k.parse::<u32>().ok().filter(|k| ids.contains(k)).map(|k| (k, *p))).collect();
    if let Some(l) = local.filter(|l| !c.iter().take(CHOICES_MAX).any(|x| x.0 == *l)) {
        c.retain(|x| x.0 != l);
        c.insert(c.len().min(CHOICES_MAX - 1), (l, 0.0));
    }
    c.truncate(CHOICES_MAX);
    let mut i = 0;
    c.retain(|x| {
        i += 1;
        i <= 3 || x.1 >= CHOICE_MIN || Some(x.0) == local
    });
    c
}

/// Jev is sure of #1 and #2 is far behind: not ambiguous, no choices.
pub fn sure(c: &[(u32, f32)]) -> bool {
    c.first().is_some_and(|x| x.1 >= SURE_TOP_MIN) && c.get(1).is_none_or(|x| x.1 < SURE_NEXT_MAX)
}

/// A text input with no other of its role on screen ("Buscar (combo box)" next to the address bar's text field).
pub fn only_of_its_kind(e: &El, els: &[El]) -> bool {
    ax::takes_text(&e.mark.role) && els.iter().filter(|x| x.mark.role == e.mark.role).count() == 1
}

/// The target is obvious: its brackets show for HIGHLIGHT_MS, then the action runs.
fn flash(r: &mut Run, desk: &mut dyn Desk, h: &Hit) {
    aim(r, desk, h);
    desk.sleep(HIGHLIGHT_MS);
}

/// Pick mode: marks + the marked screenshot -> Jev ranks the marks -> the top 3-5 as numbered choices -> Pablo
/// answers by voice ("dos", "the second", the name, "41"), with the command key or "sí" for a highlighted #1, or
/// clicks the choice himself.
fn choose(r: &mut Run, instr: &str, scr: &Screen, local: Option<u32>, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    let shot = if !desk.aborted() && desk.screen_ok() {
        desk.sleep(MARKS_DRAW_MS);
        desk.capture(scr.display, false).map_err(|e| log::warn!("screenshot: {e:#}")).ok().map(|b| jev::image(&b, "image/jpeg"))
    } else {
        None
    };
    let (s, q) = marks_request(instr, &scr.app, &scr.els, shot.as_ref());
    let rep = ask(&s, &q).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    (r.st.latency_ms, r.st.cost_usd) = (r.st.latency_ms + rep.latency_ms, r.st.cost_usd + rep.cost);
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    let d = parse(&rep);
    let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
    let c = choices(&d.target.ranked, &ids, local);
    if c.is_empty() {
        return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE));
    }
    let el = |id: u32| scr.els.iter().find(|e| e.mark.id == id);
    if let Some(e) = el(c[0].0).filter(|_| sure(&c)) {
        let h = Hit::of(e, d.risky);
        flash(r, desk, &h);
        return Ok(h);
    }
    ask_pick(r, c, scr, d.risky, desk)
}

/// The numbered choices on screen (boxes on the candidates only, labeled 1.. + region) -> Pablo answers by voice:
/// "dos", "the second", the name, "41", "la de la izquierda", "the top one", "la de la sidebar"; or "sí" / the command
/// key for #1, or clicks one himself. A new order drops it (live.rs).
fn ask_pick(r: &mut Run, c: Vec<(u32, f32)>, scr: &Screen, risky: f32, desk: &mut dyn Desk) -> Result<Hit, End> {
    let el = |id: u32| scr.els.iter().find(|e| e.mark.id == id);
    let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
    let names: Vec<String> = c.iter().map(|(id, _)| el(*id).map(|e| e.mark.name.clone()).unwrap_or_default()).collect();
    let places: Vec<(PtRect, &str)> = c.iter().filter_map(|(id, _)| el(*id).map(|e| (e.mark.rect, e.mark.region.as_str()))).collect();
    let list = names.iter().enumerate().map(|(i, n)| format!("{} {}", i + 1, if n.is_empty() { "(no name)" } else { n })).collect::<Vec<_>>().join(" · ");
    (r.st.choices, r.st.choose, r.st.preselect, r.st.status) = (c.clone(), true, false, CmdStatus::Pick);
    r.st.target = None;
    r.st.message = Some(format!("Which one? {list}"));
    desk.emit(&r.st);
    let until = std::time::Instant::now() + PICK_TIMEOUT;
    let got = loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        match desk.wait(Wait::Pick, left) {
            Answer::Text(t) => match parse_choice(&t, &names, &ids).or_else(|| spatial_choice(&t, &places).map(Picked::Nth)) {
                Some(Picked::Nth(i)) => break (c[i].0, false),
                Some(Picked::Mark(n)) => break (n, false),
                // not an answer: the list stays up, the time keeps running
                None => continue,
            },
            Answer::Yes => continue,
            Answer::Clicked(i) if i < c.len() => break (c[i].0, true),
            Answer::Clicked(_) => continue,
            Answer::Timeout => return Err(r.fail(desk, CmdStatus::Stopped, &format!("No answer in {} s: nothing clicked", PICK_TIMEOUT.as_secs()))),
            Answer::No => return Err(r.end(desk, CmdStatus::Stopped, None, String::new())),
        }
    };
    (r.st.choices, r.st.choose, r.st.preselect, r.st.message) = (vec![], false, false, None);
    let Some(e) = el(got.0) else { return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)) };
    Ok(Hit { picked: true, by_user: got.1, ..Hit::of(e, risky) })
}

/// Thin tree (canvas, games, custom UIs; next-build 4 and 6): the Vision text lines of the window become marks ->
/// the exact name, else Jev's text-only choice over them -> the line's center. No text fits (an icon): the Jev box.
fn ocr_ground(r: &mut Run, instr: &str, words: &str, scr: &Screen, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    if !desk.screen_ok() {
        return Err(r.fail(desk, CmdStatus::Setup, "screen_recording"));
    }
    let win = if scr.window.w > 0.0 { scr.window } else { scr.display };
    let lines = desk.ocr(win).unwrap_or_else(|e| {
        log::warn!("ocr: {e:#}");
        vec![]
    });
    let els = ocr_els(&lines, win);
    if els.is_empty() {
        return box_ground(r, instr, scr, ask, desk);
    }
    r.st.marks = els.iter().map(|e| e.mark.clone()).collect();
    desk.emit(&r.st);
    let found = match local_match(words, &els) {
        Some(e) => Some((e, if risky_name(&e.mark.name) { 1.0 } else { 0.0 })),
        None => {
            let d = decide(instr, &scr.app, &els, ask, &mut || None).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
            (r.st.latency_ms, r.st.cost_usd) = (r.st.latency_ms + d.latency_ms, r.st.cost_usd + d.cost);
            let id = d.target.choice.parse::<u32>().ok().filter(|_| d.target.confidence >= UNSURE_MIN);
            let sure = d.target.confidence >= CONF_MIN;
            id.and_then(|id| els.iter().find(|e| e.mark.id == id)).filter(|e| sure || names(words, &e.mark.name)).map(|e| (e, d.risky))
        }
    };
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    match found {
        Some((e, risky)) => {
            let (x, y) = center(e.mark.rect);
            Ok(Hit { target: Target::Point(x, y), mark: e.mark.clone(), url: None, value: None, risky, picked: false, by_user: false, sure: false })
        }
        None => box_ground(r, instr, scr, ask, desk),
    }
}

/// The last resort (no AX, no text): Jev box coarse-to-fine. Box on the window (at most 1568 px a side), then on a
/// native-resolution crop around that box (`refine_crop`), mapped back. Each pass has the "is it visible" gate;
/// a failed second pass keeps the first box.
fn box_ground(r: &mut Run, instr: &str, scr: &Screen, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Hit, End> {
    if !desk.screen_ok() {
        return Err(r.fail(desk, CmdStatus::Setup, "screen_recording"));
    }
    let win = if scr.window.w > 0.0 { scr.window } else { scr.display };
    // never click on a weak or broken box
    let Some((b1, risky)) = box_pass(r, instr, &scr.app, win, true, ask, desk)? else { return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE)) };
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    let crop = refine_crop(b1, win, scr.scale);
    let rect = match box_pass(r, instr, &scr.app, crop, false, ask, desk)? {
        Some((b2, _)) => box_rect(b2, crop),
        None => box_rect(b1, win),
    };
    if desk.aborted() {
        return Err(r.end(desk, CmdStatus::Stopped, None, String::new()));
    }
    let p = center(rect);
    // a retry's box must not land on the element the last click hit (only its namesakes left the marks)
    if let Some((_, Some(rc), _)) = &r.not {
        if p.0 >= rc.x && p.0 <= rc.x + rc.w && p.1 >= rc.y && p.1 <= rc.y + rc.h {
            return Err(r.fail(desk, CmdStatus::Unsure, NOT_SURE));
        }
    }
    Ok(match snap(p, &scr.els) {
        Some(e) => Hit::of(e, risky),
        None => {
            let mark = Mark { id: 1, rect, name: String::new(), role: "box".into(), region: "screen".into() };
            r.st.marks = vec![mark.clone()];
            Hit { target: Target::Point(p.0, p.1), mark, url: None, value: None, risky, picked: false, by_user: false, sure: false }
        }
    })
}

/// One box pass on `img`: the first on the whole window (downscaled, with risky), the second on a crop at native
/// resolution. None = no sane, visible box.
fn box_pass(r: &mut Run, instr: &str, app: &str, img: PtRect, first: bool, ask: &mut Ask, desk: &mut dyn Desk) -> Result<Option<(Bbox, f32)>, End> {
    let b = desk.capture(img, !first).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("screenshot: {e:#}")))?;
    let (s, q) = box_request(instr, app, &jev::image(&b, "image/jpeg"), first);
    let rep = ask(&s, &q).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    (r.st.latency_ms, r.st.cost_usd) = (r.st.latency_ms + rep.latency_ms, r.st.cost_usd + rep.cost);
    let a = &rep.answers;
    let visible = jev::noul(a, "visible").unwrap_or(0.0) >= VISIBLE_MIN;
    Ok(jev::bbox(a, "box").filter(|b| sane(*b) && visible).map(|b| (b, jev::noul(a, "risky").unwrap_or(1.0))))
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
    if !h.by_user {
        desk.act(&Act::Focus(h.target.clone())).map_err(|e| r.fail(desk, CmdStatus::Error, &format!("{e:#}")))?;
    }
    desk.sleep(FOCUS_MS);
    Ok(Some(h))
}

/// Runs one planner action. `step` numbers the action in the session, `order` the planner reply it came from.
pub fn exec(a: &Action, step: u32, order: u32, cfg: &Cfg, ask: &mut Ask, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    let label = crate::planner::label(a);
    let instruction = if cfg.max > 0 { format!("step {step}: {label}") } else { label };
    let st = CommandStep { instruction, step, max: cfg.max, status: CmdStatus::Thinking, op: Some(a.op.clone()), order, ..Default::default() };
    let mut r = Run { st, choose: cfg.choose, said: a.said.clone(), kind: Kind::Native, not: None };
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
            // Return sends in chat and mail apps, whatever the planner says; elsewhere it runs at once.
            let f = desk.focused();
            let enter = (code == 36 || code == 76) && return_asks(&f) && !after_palette(mem, &f);
            if enter || ((a.risky || trashes(code, flags, &f)) && cfg.confirm_risky) {
                let (q, what) = if enter { ("Press Return? Say yes · F11 · Esc", "Return not pressed") } else { ("Do it? Say yes · F11 · Esc", "not done") };
                if let Err(end) = r.confirm(desk, q, what) {
                    return end;
                }
            }
            if desk.secure_input() {
                return r.fail(desk, CmdStatus::Blocked, BLOCKED);
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
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
        "click" | "retry" => {
            let Some(t) = target else { return r.fail(desk, CmdStatus::Unsure, NOT_SURE) };
            r.st.action = Some("click".into());
            if a.op == "retry" {
                // case 27: the last target again, with Pablo's constraint ("bottom, not the top"), never the same element
                let constraint = text.trim();
                r.said = format!("{t} {constraint} {}", r.said).trim().to_string();
                r.not = mem.iter().rev().find(|m| m.e.kind == MemKind::Clicked).map(|m| (m.e.text.clone(), m.rect, m.e.app.clone()));
            }
            let h = match ground(&mut r, "click", t, ask, desk) {
                Ok(h) => h,
                Err(end) => return end,
            };
            let words = split_mark(t).1;
            // case 25: a text field that has the keys already (cmd+t / cmd+l focus the omnibox) needs no click; a
            // toolbar field takes AXFocused before any click (a real click there can hit the dropdown over it)
            let has_keys = |f: &Field| ax::takes_text(&h.mark.role) && !h.mark.name.is_empty() && f.name == h.mark.name && f.role.starts_with("AX");
            if ax::takes_text(&h.mark.role) && !h.by_user {
                let f = desk.focused();
                if has_keys(&f) {
                    aim(&mut r, desk, &h);
                    clicked(mem, &h, &f);
                    return r.done(desk, format!("{} has the keys", quote(&h.mark.name)));
                }
                if h.mark.region == "toolbar" && matches!(h.target, Target::Mark(_)) && !desk.aborted() {
                    aim(&mut r, desk, &h);
                    if desk.act(&Act::Focus(h.target.clone())).is_ok() {
                        desk.sleep(FOCUS_MS);
                        let f = desk.focused();
                        if has_keys(&f) {
                            clicked(mem, &h, &f);
                            return r.done(desk, format!("focused {}", quote(&h.mark.name)));
                        }
                    }
                }
            }
            // Pablo picked or clicked it himself: that was his yes.
            let risky = !h.picked && (a.risky || h.risky >= RISKY_MIN);
            if gate(1.0, if risky { 1.0 } else { 0.0 }, cfg.confirm_risky) == Gate::Confirm {
                r.st.target = Some(h.mark.id);
                if let Err(end) = r.confirm(desk, &format!("Click {}? Say yes · F11 · Esc", quote(&h.mark.name)), "nothing clicked") {
                    return end;
                }
            }
            aim(&mut r, desk, &h);
            if h.by_user {
                let f = desk.focused();
                clicked(mem, &h, &f);
                return r.done(desk, format!("you clicked {} ({})", quote(&h.mark.name), h.mark.role));
            }
            if let Target::Point(x, y) = h.target {
                if desk.secure_at((x, y)) {
                    return r.fail(desk, CmdStatus::Blocked, BLOCKED);
                }
            }
            if desk.aborted() {
                return r.end(desk, CmdStatus::Stopped, None, String::new());
            }
            let f = desk.focused();
            // Verify (vc-feedback 21): the screen must change. No change -> one real click at the hit-tested leaf
            // (unless the first click was real), still no change -> say so, and the planner reads it as failed.
            let before = desk.sig();
            if let Err(e) = desk.act(&Act::Click(h.target.clone())) {
                return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
            }
            desk.sleep(SETTLE_MS);
            if before.is_some() && desk.sig() == before {
                log::info!("click on {} changed nothing: a real click", h.mark.name);
                if let Err(e) = desk.act(&Act::RealClick(h.target.clone())) {
                    // "something covers" the field we wanted the keys in, and it has them: that is the goal
                    let f = desk.focused();
                    if has_keys(&f) {
                        clicked(mem, &h, &f);
                        return r.done(desk, format!("focused {}", quote(&h.mark.name)));
                    }
                    return r.fail(desk, CmdStatus::Error, &format!("{e:#}"));
                }
                desk.sleep(SETTLE_MS);
                if desk.sig() == before {
                    return r.fail(desk, CmdStatus::Unsure, &format!("Clicked {} but nothing changed", quote(&h.mark.name)));
                }
            }
            clicked(mem, &h, &f);
            // case 28: say it when what was clicked has another name than what Pablo asked for
            let place = if h.mark.region.is_empty() { h.mark.role.clone() } else { format!("{}, {}", h.mark.role, h.mark.region) };
            if h.mark.name.is_empty() {
                return r.done(desk, format!("clicked {} ({place})", quote(words)));
            }
            if !h.sure && !h.picked && !names(words, &h.mark.name) {
                log::warn!("click {words:?} landed on {:?}", h.mark.name);
                return r.done(desk, format!("clicked {} ({place}), not sure it is {}", quote(&h.mark.name), quote(words)));
            }
            r.done(desk, format!("clicked {} ({place})", quote(&h.mark.name)))
        }
        "open_app" => {
            let said = a.app.clone().unwrap_or_default();
            if said.trim().is_empty() {
                return r.fail(desk, CmdStatus::Unsure, "Which app? Say it again.");
            }
            let app = match_app(&said, &desk.apps()).unwrap_or(said);
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
        // "copia lo último" (agent::local_intent): the last transcript on the clipboard, no keys
        "set_clipboard" => {
            r.st.action = Some("copy".into());
            desk.set_clipboard(&text);
            r.done(desk, format!("copied {}", quote(&text.chars().take(40).collect::<String>())))
        }
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
    // the test hook (POST /command {"auto_confirm": true}): a confirm says yes at once
    if w == Wait::Confirm && AUTO_CONFIRM.load(Ordering::SeqCst) {
        return Answer::Yes;
    }
    CONFIRMED.store(false, Ordering::SeqCst);
    DENIED.store(false, Ordering::SeqCst);
    *ANSWER.lock().unwrap() = None;
    *CLICKED.lock().unwrap() = None;
    WAITING.store(if w == Wait::Confirm { 1 } else { 2 }, Ordering::SeqCst);
    let until = std::time::Instant::now() + timeout;
    let out = loop {
        if ABORT.load(Ordering::SeqCst) || DENIED.swap(false, Ordering::SeqCst) {
            break Answer::No;
        }
        // the command key: yes to a confirm, "the highlighted one" to a pick
        if CONFIRMED.swap(false, Ordering::SeqCst) {
            break Answer::Yes;
        }
        if let Some(i) = CLICKED.lock().unwrap().take() {
            break Answer::Clicked(i);
        }
        if let Some(t) = ANSWER.lock().unwrap().take() {
            break Answer::Text(t);
        }
        if std::time::Instant::now() >= until {
            break Answer::Timeout;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    WAITING.store(0, Ordering::SeqCst);
    PICK_RECTS.lock().unwrap().clear();
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
        /// The marks came from a browser page (web.rs): `El::h` is a DOM id, acts go through its JS.
        web: Option<crate::web::Page>,
        /// Where the last web act found its element (screen points, after its scroll into view): a real click goes there.
        web_at: Option<(f64, f64)>,
        /// The front app's kind and pid (last read).
        kind: Kind,
        pid: i32,
    }

    impl Real {
        pub fn new(app: AppHandle) -> Self {
            Self { app, handles: vec![], els: vec![], focus: None, generation: GEN.load(Ordering::SeqCst), web: None, web_at: None, kind: Kind::Native, pid: 0 }
        }

        fn point(&self, t: &Target) -> Option<(f64, f64)> {
            match t {
                Target::Point(x, y) => Some((*x, *y)),
                Target::Mark(id) => self.els.iter().find(|e| e.mark.id == *id).map(|e| center(e.mark.rect)),
            }
        }

        fn handle(&self, t: &Target) -> Option<&CFType> {
            let Target::Mark(id) = t else { return None };
            if self.web.is_some() {
                return None;
            }
            self.els.iter().find(|e| e.mark.id == *id).and_then(|e| self.handles.get(e.h))
        }

        /// A real click on the target (research 5): hit-test the point first so the click lands on the target or
        /// inside it, never on what covers it. susurro's own marks window can answer the hit-test: it hides, and
        /// the test runs again.
        fn real_click(&self, t: &Target) -> anyhow::Result<()> {
            let (rect, name, target) = match t {
                Target::Point(x, y) => (PtRect { x: *x, y: *y, w: 0.0, h: 0.0 }, String::new(), None),
                Target::Mark(id) => {
                    let e = self.els.iter().find(|e| e.mark.id == *id).ok_or_else(|| anyhow::anyhow!("the element is gone"))?;
                    // a web element may have scrolled into view: its fresh rect, else the scanned one
                    let rect = match self.web_at.filter(|_| self.web.is_some()) {
                        Some((x, y)) => PtRect { x, y, w: 0.0, h: 0.0 },
                        None => e.mark.rect,
                    };
                    (rect, e.mark.name.clone(), self.handle(t))
                }
            };
            let own = std::process::id() as i32;
            let mut hid = false;
            let mut hit = |p: (f64, f64)| {
                let mut got = ax::mac::element_at(p);
                if got.as_ref().is_some_and(|g| g.1 == own) && !hid {
                    crate::platform::macos::show_marks(&self.app, None);
                    std::thread::sleep(Duration::from_millis(60));
                    hid = true;
                    got = ax::mac::element_at(p);
                }
                match got {
                    // susurro's pill is there (the marks window hid already): covered, never a click on our own window
                    Some((_, pid)) if pid == own => Some(false),
                    Some((el, pid)) if pid == self.pid => Some(target.is_none_or(|t| ax::mac::within(&el, t))),
                    // no AX handle (web, OCR, box): any element of another app is fine
                    Some(_) if target.is_none() => Some(true),
                    Some(_) => Some(false),
                    None => None,
                }
            };
            let p = aim_point(rect, &mut hit).ok_or_else(|| anyhow::anyhow!("something covers {}", quote(&name)))?;
            click_at(p)
        }
    }

    /// A real click went out since the last `act` began (click_at sets it; web.rs's JS fallback clicks through it too).
    static REAL_CLICKED: AtomicBool = AtomicBool::new(false);

    /// The .app bundles in the usual folders (one level deep, plus Utilities).
    pub fn installed_apps() -> Vec<String> {
        let home = std::env::var("HOME").unwrap_or_default();
        let dirs = ["/Applications".into(), "/Applications/Utilities".into(), "/System/Applications".into(), "/System/Applications/Utilities".into(), format!("{home}/Applications")];
        dirs.iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".app").map(String::from))
            .collect()
    }

    pub fn displays() -> Vec<(PtRect, f64)> {
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
        REAL_CLICKED.store(true, Ordering::SeqCst);
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

    fn event_source() -> anyhow::Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| anyhow::anyhow!("no event source"))
    }

    /// One mouse event, tagged as susurro's own (live.rs input monitor).
    fn mouse(ty: CGEventType, p: (f64, f64), button: CGMouseButton, clicks: i64, flags: u64) -> anyhow::Result<()> {
        let e = CGEvent::new_mouse_event(event_source()?, ty, CGPoint::new(p.0, p.1), button).map_err(|_| anyhow::anyhow!("mouse event"))?;
        if clicks > 0 {
            e.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, clicks);
        }
        if flags != 0 {
            e.set_flags(core_graphics::event::CGEventFlags::from_bits_truncate(flags));
        }
        e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, crate::paste::SUSURRO_TAG);
        e.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn drag(path: &[(f64, f64)], flags: u64) -> anyhow::Result<()> {
        let (Some(&first), Some(&last)) = (path.first(), path.last()) else { anyhow::bail!("an empty drag") };
        REAL_CLICKED.store(true, Ordering::SeqCst);
        mouse(CGEventType::MouseMoved, first, CGMouseButton::Left, 0, flags)?;
        std::thread::sleep(Duration::from_millis(30));
        mouse(CGEventType::LeftMouseDown, first, CGMouseButton::Left, 1, flags)?;
        std::thread::sleep(Duration::from_millis(30));
        for p in drag_points(path) {
            mouse(CGEventType::LeftMouseDragged, p, CGMouseButton::Left, 1, flags)?;
            std::thread::sleep(Duration::from_millis(DRAG_EVENT_MS));
        }
        mouse(CGEventType::LeftMouseUp, last, CGMouseButton::Left, 1, flags)
    }

    fn click_n(at: (f64, f64), right: bool, count: u8, flags: u64) -> anyhow::Result<()> {
        REAL_CLICKED.store(true, Ordering::SeqCst);
        let (down, up, b) = if right {
            (CGEventType::RightMouseDown, CGEventType::RightMouseUp, CGMouseButton::Right)
        } else {
            (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, CGMouseButton::Left)
        };
        mouse(CGEventType::MouseMoved, at, b, 0, flags)?;
        std::thread::sleep(Duration::from_millis(20));
        for n in 1..=count.clamp(1, 3) as i64 {
            mouse(down, at, b, n, flags)?;
            mouse(up, at, b, n, flags)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    }

    fn key_hold(code: u16, flags: u64, ms: u64) -> anyhow::Result<()> {
        let src = event_source()?;
        for down in [true, false] {
            let e = CGEvent::new_keyboard_event(src.clone(), code, down).map_err(|_| anyhow::anyhow!("key event"))?;
            e.set_flags(core_graphics::event::CGEventFlags::from_bits_truncate(flags));
            e.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, crate::paste::SUSURRO_TAG);
            e.post(CGEventTapLocation::HID);
            if down {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        Ok(())
    }

    impl Real {
        /// A browser in front: the page's DOM marks through Apple Events JS. None = not a browser, the setting is
        /// off (the one-time guide shows), or the page has nothing to act on: the AX tree reads it instead.
        fn read_web(&mut self, front: &crate::paste::AppRef) -> Option<Screen> {
            use crate::web;
            let bundle = front.bundle_id.clone()?;
            let kind = web::kind(&bundle)?;
            let name = front.name.clone().unwrap_or_default();
            let allowed = web::js_allowed(&bundle);
            if allowed == Some(false) {
                web::guide(&self.app, &bundle, &name, &web::Error::Off);
                return None;
            }
            let scan = match web::call(kind, &bundle, &serde_json::json!({ "op": "scan" })).and_then(|v| web::parse(v).map_err(web::Error::Other)) {
                Ok(s) => s,
                Err(e) => {
                    log::info!("web scan in {name}: {e:?}, AX instead");
                    // the prefs say on: a failure is something else (no window, a closing tab), never the "turn it on" guide
                    if e == web::Error::Denied || (e == web::Error::Off && allowed != Some(true)) {
                        web::guide(&self.app, &bundle, &name, &e);
                    }
                    return None;
                }
            };
            if scan.els.is_empty() || scan.frames > 0 {
                return None;
            }
            let guess = web::guess_viewport(&scan);
            let viewport = ax::mac::web_area_at(front.pid, center(guess)).unwrap_or(guess);
            let els = web::els(&scan, viewport);
            let (display, scale) = display_for(center(viewport), &displays()).unwrap_or((viewport, 1.0));
            log::info!("web marks in {name}: {} ({})", els.len(), scan.url);
            self.handles.clear();
            self.els = els.clone();
            self.web = Some(web::Page { kind, bundle, scan, viewport });
            (self.web_at, self.kind, self.pid) = (None, Kind::Browser, front.pid);
            let title = self.web.as_ref().map(|p| p.scan.title.clone()).unwrap_or_default();
            Some(Screen { app: name, display, scale, els, kind: Kind::Browser, title, ..Default::default() })
        }

        /// Click / focus a DOM mark through its JS. None = not a web mark (the AX path acts). The click verify is
        /// exec's, on the page signature (`sig`); a real click goes to `web_at`.
        fn act_web(&mut self, a: &Act) -> Option<anyhow::Result<()>> {
            let (op, t) = match a {
                Act::Click(t) => ("click", t),
                Act::Focus(t) => ("focus", t),
                _ => return None,
            };
            let page = self.web.as_ref()?;
            let Target::Mark(id) = t else { return None };
            let e = self.els.iter().find(|e| e.mark.id == *id)?;
            let (dom, name, rect) = (e.h, e.mark.name.clone(), e.mark.rect);
            Some(match page.act(op, dom, &name) {
                Ok((at, keys)) => {
                    self.web_at = at;
                    // the address bar kept the keys: a real click puts them in the field (typing follows)
                    if op == "focus" && !keys { click_at(at.unwrap_or_else(|| center(rect))) } else { Ok(()) }
                }
                // the page navigated away under the script: the click went out, exec's verify reads the new page
                Err(crate::web::Error::Timeout) if op == "click" => Ok(()),
                Err(crate::web::Error::Other(g)) if g == "gone" => Err(anyhow::anyhow!("{} changed on the page; say it again", quote(&name))),
                Err(e) => Err(anyhow::anyhow!("web {op}: {e:?}")),
            })
        }
    }

    impl Desk for Real {
        fn learn_word(&mut self, word: &str, heard: &str) -> Result<(), String> {
            crate::learn::add_correction(&self.app, word, heard)
        }

        fn apps(&mut self) -> Vec<String> {
            installed_apps()
        }

        fn read(&mut self) -> Result<Screen, String> {
            if !crate::permissions::accessibility_trusted() {
                return Err("accessibility".into());
            }
            let front = crate::paste::target_app().ok_or("No app in front")?;
            self.web = None;
            if let Some(scr) = self.read_web(&front) {
                return Ok(scr);
            }
            let tree = ax::mac::read(front.pid).ok_or_else(|| format!("{} has no window to act on", front.name.clone().unwrap_or_default()))?;
            let els = ax::collect(&tree.root, tree.window);
            let (display, scale) = display_for(center(tree.window), &displays()).unwrap_or((tree.window, 1.0));
            let app = front.name.unwrap_or_default();
            let kind = app_kind(&app, front.bundle_id.as_deref().unwrap_or(""));
            (self.handles, self.els, self.kind, self.pid) = (tree.handles, els.clone(), kind, front.pid);
            Ok(Screen { app, display, scale, els, window: tree.window, kind, page: ax::web_area(&tree.root), title: tree.root.title.clone() })
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

        /// screencapture's JPEG, then sips: the long side to `max_px` at AGENT_JPEG_QUALITY.
        fn snapshot(&mut self, r: PtRect, max_px: f64) -> anyhow::Result<Vec<u8>> {
            let path = std::env::temp_dir().join(format!("susurro-agent-{}-{}.jpg", std::process::id(), crate::history::now_ms()));
            let rect = format!("-R{},{},{},{}", r.x.round(), r.y.round(), r.w.round(), r.h.round());
            let ok = std::process::Command::new("/usr/sbin/screencapture").args(["-x", "-t", "jpg", &rect]).arg(&path).status()?.success();
            anyhow::ensure!(ok, "screencapture failed");
            let (px, q) = (format!("{}", max_px.round()), AGENT_JPEG_QUALITY.to_string());
            std::process::Command::new("/usr/bin/sips").args(["-Z", px.as_str(), "-s", "formatOptions", q.as_str()]).arg(&path).output()?;
            let b = std::fs::read(&path);
            let _ = std::fs::remove_file(&path);
            Ok(b?)
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
            if a.guarded() && let Some(why) = crate::test_hooks::paste_blocked() {
                anyhow::bail!("test guard: {why}");
            }
            crate::live::own_input();
            let was_real = REAL_CLICKED.swap(false, Ordering::SeqCst);
            if let Act::RealClick(t) = a {
                // the click that changed nothing was a real one already: never the same click twice
                return if was_real { Ok(()) } else { self.real_click(t) };
            }
            if let Some(r) = self.act_web(a) {
                return r;
            }
            match a {
                Act::Click(t) => {
                    let role = match t {
                        Target::Mark(id) => self.els.iter().find(|e| e.mark.id == *id).map(|e| e.mark.role.as_str()).unwrap_or(""),
                        Target::Point(..) => "",
                    };
                    match self.handle(t) {
                        Some(h) if !real_click_first(self.kind, role) => match ax::mac::press_code(h) {
                            // CannotComplete can still mean pressed: the verify step decides (research 5)
                            0 | ax::mac::CANNOT_COMPLETE => {}
                            _ => self.real_click(t)?,
                        },
                        _ => self.real_click(t)?,
                    }
                }
                Act::RealClick(_) => unreachable!("handled above"),
                Act::Focus(t) => {
                    if !self.handle(t).is_some_and(ax::mac::focus) {
                        click_at(self.point(t).ok_or_else(|| anyhow::anyhow!("the element is gone"))?)?;
                    }
                }
                Act::Combo(code, flags) => crate::paste::combo(*code, *flags),
                Act::Burst(code, flags, n) => crate::paste::burst(*code, *flags, *n),
                Act::Scroll { down, at } => scroll(*down, at.as_ref().and_then(|t| self.point(t)))?,
                Act::Drag(path, flags) => drag(path, *flags)?,
                Act::MouseMove(x, y) => mouse(CGEventType::MouseMoved, (*x, *y), CGMouseButton::Left, 0, 0)?,
                Act::ClickAt { at, right, count, flags } => click_n(*at, *right, *count, *flags)?,
                Act::KeyHold(code, flags, ms) => key_hold(*code, *flags, *ms)?,
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

        /// One more AX read of the front window + the focused element (title, value size, URL), hashed.
        /// ponytail: a full tree read per click (the cost is not measured); an AXObserver would be cheaper.
        fn sig(&mut self) -> Option<u64> {
            use std::hash::{Hash, Hasher};
            if let Some(p) = &self.web {
                // + the front app: a link that opened another app (a Slack deep link) changed the screen too
                let front = crate::paste::target_app().map(|f| f.pid);
                return p.sig().map(|s| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (s, front).hash(&mut h);
                    h.finish()
                });
            }
            let front = crate::paste::target_app()?;
            let tree = ax::mac::read(front.pid)?;
            let f = ax::mac::focused().map(|(f, _)| (f.role, f.name, f.value.map(|v| v.len()), f.url, f.sel));
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (front.pid, ax::tree_hash(&tree.root), f).hash(&mut h);
            Some(h.finish())
        }

        fn ocr(&mut self, r: PtRect) -> anyhow::Result<Vec<crate::ocr::Line>> {
            let img = self.capture(r, true)?;
            let t = std::time::Instant::now();
            let mut lines = crate::ocr::recognize(&img)?;
            if let Some(pill) = crate::platform::macos::pill_rect() {
                lines.retain(|l| {
                    let (x, y) = center(crate::ocr::to_points(l.1, r));
                    !(x >= pill.x && x <= pill.x + pill.w && y >= pill.y && y <= pill.y + pill.h)
                });
            }
            log::info!("ocr: {} lines in {} ms", lines.len(), t.elapsed().as_millis());
            Ok(lines)
        }

        fn secure_at(&mut self, p: (f64, f64)) -> bool {
            ax::mac::secure_at(p)
        }

        /// Rust owns the marks window: it shows on every step with marks; live.rs hides it (session end, took over).
        fn board(&mut self) -> Vec<El> {
            crate::live::board()
        }

        fn emit(&mut self, s: &CommandStep) {
            if !s.marks.is_empty() && !crate::live::took_over() {
                crate::platform::macos::show_marks(&self.app, Some(s.display));
                crate::live::set_board(self.els.clone());
            }
            if s.status == CmdStatus::Pick && s.choose {
                set_pick_rects(s.choices.iter().filter_map(|(id, _)| self.els.iter().find(|e| e.mark.id == *id).map(|e| e.mark.rect)).collect());
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

        fn paused(&self) -> bool {
            crate::live::paused()
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
        // names of the shown options (open risk c)
        let q = "Which one? 4 #general · 7 #design · 9 #random-stuff";
        let opts = pick_options(q);
        assert_eq!(opts, [(4, "#general".into()), (7, "#design".into()), (9, "#random-stuff".into())]);
        assert_eq!(pick_by_name("the design one", &opts), Some(7));
        assert_eq!(pick_by_name("Random stuff.", &opts), Some(9));
        assert_eq!(pick_by_name("ra", &opts), None, "too short to name one");
        assert_eq!(pick_by_name("type hello", &opts), None);
        assert!(looks_like_pick("el tercero", q) && looks_like_pick("General.", q) && looks_like_pick("7", q));
        assert!(!looks_like_pick("20", q), "20 is not shown: a new order (a mark number goes to the planner)");
        assert!(!looks_like_pick("open slack and search for design reviews", q), "a full order");
        assert!(!looks_like_pick("type hello", q));
    }

    const RETINA: PtRect = PtRect { x: 0.0, y: 0.0, w: 1512.0, h: 982.0 };

    #[test]
    fn coarse_to_fine_box_math() {
        let mid = Bbox { top: 0.5, bottom: 0.5, left: 0.5, right: 0.5 };
        assert_eq!(box_point(mid, RETINA), (756.0, 491.0), "full display, any image size");
        // non-Retina display left of the main one: negative x; a display above: negative y
        let left = PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let p = box_point(Bbox { top: 0.1, bottom: 0.2, left: 0.1, right: 0.2 }, left);
        assert!(p.0 < 0.0 && (p.0 - (-1920.0 + 0.15 * 1920.0)).abs() < 1e-9, "{p:?}");
        assert!(box_point(mid, PtRect { x: 0.0, y: -1080.0, w: 1920.0, h: 1080.0 }).1 < 0.0);
        // the crop: a 20 pt icon on a Retina window -> 448 px native (224 pt) a side, centered on the box
        let win = PtRect { x: 100.0, y: 50.0, w: 1200.0, h: 800.0 };
        let icon = Bbox { top: 0.5, bottom: 0.525, left: 0.5, right: 0.5 + 20.0 / 1200.0 };
        let c = refine_crop(icon, win, 2.0);
        assert!((c.w - 224.0).abs() < 1e-9 && (c.h - 224.0).abs() < 1e-9, "{c:?}");
        let (cx, cy) = box_point(icon, win);
        assert!((c.x + c.w / 2.0 - cx).abs() < 1e-9 && (c.y + c.h / 2.0 - cy).abs() < 1e-9);
        // a big box: 3x, at most 1568 px native; near the corner: kept inside the window
        let big = refine_crop(Bbox { top: 0.0, bottom: 0.4, left: 0.0, right: 0.4 }, win, 2.0);
        assert!(big.w <= 784.0 && big.x >= win.x && big.y >= win.y, "{big:?}");
        let corner = refine_crop(Bbox { top: 0.97, bottom: 0.99, left: 0.97, right: 0.99 }, win, 1.0);
        assert!(corner.x + corner.w <= win.x + win.w + 1e-9 && corner.y + corner.h <= win.y + win.h + 1e-9, "{corner:?}");
        // the round trip with Jev's two-decimal edges (research 6): a target at (731.3, 467.7) on the window.
        // pass 1 alone misses by up to 0.005 x 1200 pt; pass 2 inside the 224 pt crop by up to 0.005 x 224 pt.
        let truth = (731.3, 467.7);
        let r2 = |v: f64| (v * 100.0).round() / 100.0;
        let edges = |img: PtRect| {
            let (fx, fy) = ((truth.0 - img.x) / img.w, (truth.1 - img.y) / img.h);
            Bbox { top: r2(fy - 0.004), bottom: r2(fy + 0.004), left: r2(fx - 0.004), right: r2(fx + 0.004) }
        };
        let b1 = edges(win);
        let crop = refine_crop(b1, win, 2.0);
        let p2 = box_point(edges(crop), crop);
        let err = |p: (f64, f64)| ((p.0 - truth.0).powi(2) + (p.1 - truth.1).powi(2)).sqrt();
        assert!(err(p2) < 2.0, "fine {:.2} pt", err(p2));
        assert!(err(p2) < err(box_point(b1, win)), "the second pass is closer");
        assert_eq!(box_rect(Bbox { top: 0.25, bottom: 0.5, left: 0.5, right: 0.75 }, win), PtRect { x: 700.0, y: 250.0, w: 300.0, h: 200.0 });
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
        installed: Vec<String>,
        board: Vec<El>,
        /// The screen signatures `sig` returns, in order (empty = no verify).
        sigs: VecDeque<u64>,
        /// What OCR reads.
        lines: Vec<crate::ocr::Line>,
        /// A real click's hit-test finds something over the target (Edge's omnibox at 15:51:55).
        covered: bool,
        /// `capture` crops this PNG (global points x `scale` = px, python3 + PIL) instead of the fake bytes: real Jev box tests.
        image: Option<(std::path::PathBuf, f64)>,
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
        fn capture(&mut self, r: PtRect, native: bool) -> anyhow::Result<Vec<u8>> {
            let Some((png, scale)) = self.image.clone() else { return Ok(b"\xff\xd8jpeg".to_vec()) };
            // PIL crops (sips -c ignores --cropOffset on this macOS); the first pass is downscaled like the real one
            let out = std::env::temp_dir().join(format!("susurro-fake-{}-{}.jpg", std::process::id(), self.acts.len() + self.sleeps.len()));
            let py = "import sys; from PIL import Image; a = sys.argv; im = Image.open(a[1]).convert('RGB').crop(tuple(round(float(v)) for v in a[3:7])); \
                      im.thumbnail((1568, 1568)) if a[7] == '0' else None; im.save(a[2], 'JPEG', quality=90)";
            let box_px = [r.x, r.y, r.x + r.w, r.y + r.h].map(|v| (v * scale).to_string());
            let ok = std::process::Command::new("python3").args(["-I", "-c", py]).arg(&png).arg(&out).args(box_px).arg(if native { "1" } else { "0" }).status()?.success();
            anyhow::ensure!(ok, "PIL crop failed");
            let b = std::fs::read(&out)?;
            let _ = std::fs::remove_file(&out);
            Ok(b)
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
                Act::Combo(17, CMD) => {
                    // a browser's new tab puts the keys in its address bar
                    self.tabs += 1;
                    if let Some(i) = self.fields.iter().position(|f| f.name == "Address and search bar") {
                        self.focus = i;
                    }
                }
                Act::RealClick(_) if self.covered => anyhow::bail!("something covers it"),
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
        fn apps(&mut self) -> Vec<String> {
            self.installed.clone()
        }
        fn board(&mut self) -> Vec<El> {
            self.board.clone()
        }
        fn sig(&mut self) -> Option<u64> {
            self.sigs.pop_front()
        }
        fn ocr(&mut self, _: PtRect) -> anyhow::Result<Vec<crate::ocr::Line>> {
            Ok(self.lines.clone())
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
        Screen { app: "Google Chrome".into(), display: PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }, scale: 2.0, els, ..Default::default() }
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

    const CFG: Cfg = Cfg { confirm_risky: true, choose: false, max: 0 };
    const CHOOSE: Cfg = Cfg { confirm_risky: true, choose: true, max: 0 };

    const INSTALLED: [&str; 12] = [
        "Microsoft Edge", "Google Chrome", "Chrome Remote Desktop", "Ghostty", "Claude", "Safari", "Slack", "Notes",
        "Visual Studio Code", "WhatsApp", "Terminal", "Arc",
    ];

    #[test]
    fn open_app_matches_spoken_names_to_installed_apps() {
        // Pablo 2026-10-17: "open Edge" did nothing (`open -a Edge` has no such app)
        let apps: Vec<String> = INSTALLED.iter().map(|s| s.to_string()).collect();
        let rows = [
            ("edge", Some("Microsoft Edge")),
            ("Edge", Some("Microsoft Edge")),
            ("chrome", Some("Google Chrome")),
            ("ghosty", Some("Ghostty")),
            ("Ghostty", Some("Ghostty")),
            ("claude", Some("Claude")),
            ("whats app", Some("WhatsApp")),
            ("Safari.app", Some("Safari")),
            ("note", Some("Notes")),
            ("arc", Some("Arc")),
            ("ark", None),
            ("vs code", Some("Visual Studio Code")),
            ("VS Code", Some("Visual Studio Code")),
            ("visual studio", Some("Visual Studio Code")),
            ("Ghosty", Some("Ghostty")),
            ("photoshop", None),
            ("", None),
        ];
        for (said, want) in rows {
            assert_eq!(match_app(said, &apps).as_deref(), want, "{said:?}");
        }
        // the alias table holds when the app list is empty (open -a gets the real name)
        for (said, want) in [("edge", "Microsoft Edge"), ("chrome", "Google Chrome"), ("vs code", "Visual Studio Code"), ("ghosty", "Ghostty"), ("claude", "Claude")] {
            assert_eq!(match_app(said, &[]).as_deref(), Some(want), "{said:?}");
        }
    }

    #[test]
    fn the_test_guard_lets_open_app_bring_the_target_up() {
        assert!(!Act::OpenApp("Slack".into()).guarded());
        assert!(Act::Click(Target::Mark(1)).guarded() && Act::Combo(KEY_V, CMD).guarded());
    }

    #[test]
    fn open_app_opens_and_reports_the_matched_app() {
        let mut d = FakeDesk { installed: INSTALLED.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let (mut ask, _) = script(vec![]);
        let a = Action { op: "open_app".into(), app: Some("edge".into()), ..Default::default() };
        let e = exec(&a, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, e.summary.as_str()), (CmdStatus::Done, "opened Microsoft Edge"));
        assert_eq!(d.acts, [Act::OpenApp("Microsoft Edge".into())]);
        // no match: the name goes to `open -a` as said
        let mut d = FakeDesk::default();
        let a = Action { op: "open_app".into(), app: Some("Photoshop".into()), ..Default::default() };
        let _ = exec(&a, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(d.acts, [Act::OpenApp("Photoshop".into())]);
    }

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

    fn slack() -> FakeDesk {
        FakeDesk { app: "Slack".into(), fields: vec![FField::new("AXTextArea", "Message #general", "", None)], ..Default::default() }
    }

    #[test]
    fn cmd_delete_outside_a_text_field_asks_first() {
        // GUI round 2: cmd+backspace in Finder moved a folder to the Trash with no confirm
        let (mut d, mut mem) = (FakeDesk { app: "Finder".into(), fields: vec![FField::new("AXOutline", "list view", "", None)], ..Default::default() }, Memory::new());
        d.answers = vec![Answer::No];
        run_all(&[key("cmd+backspace")], &mut d, &mut mem);
        assert_eq!((d.waits.as_slice(), d.acts.as_slice()), (&[Wait::Confirm][..], &[][..]), "asked, nothing trashed");
        // in a text field it only deletes text: no confirm
        let (mut d, mut mem) = (slack(), Memory::new());
        run_all(&[key("cmd+backspace")], &mut d, &mut mem);
        assert!(d.waits.is_empty());
        assert_eq!(d.acts, [Act::Combo(DELETE, crate::paste::CMD)]);
    }

    #[test]
    fn return_waits_for_confirm_in_chat_apps_then_voice_si_resolves() {
        // case 2 in a chat app: Return sends, so it asks
        let (mut d, mut mem) = (slack(), Memory::new());
        d.answers = vec![Answer::Yes];
        let e = run_all(&[typ("hola", None), key("return")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Done]);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert_eq!(d.acts, [Act::Combo(36, 0)]);
        assert!(d.steps.iter().any(|s| s.status == CmdStatus::Confirm && s.message.as_deref().is_some_and(|m| m.contains("Return"))));
        assert_eq!(yes_no("Sí."), Some(true));
        assert_eq!(yes_no("dale"), Some(true));
        assert_eq!(yes_no("No."), Some(false));
        assert_eq!(yes_no("open slack"), None);
        // a terminal runs it at once (live log 2026-10-18: Return waited 10 s and stopped)
        let (mut d, mut mem) = (ghostty(), Memory::new());
        let e = run_all(&[typ("ls", None), key("return")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Done]);
        assert!(d.waits.is_empty());
        assert_eq!(d.acts, [Act::Combo(36, 0)]);
    }

    #[test]
    fn return_confirm_only_where_it_sends() {
        let f = |app: &str, bundle: &str, role: &str, name: &str| Field { app: app.into(), bundle: bundle.into(), role: role.into(), name: name.into(), ..Default::default() };
        let rows = [
            (f("Slack", "com.tinyspeck.slackmacgap", "AXTextArea", "Message #general"), true),
            (f("WhatsApp", "", "AXTextArea", "Type a message"), true),
            (f("Messages", "com.apple.MobileSMS", "AXTextArea", ""), true),
            (f("Mail", "", "AXTextArea", ""), true),
            (f("Discord", "", "AXTextArea", "Message @ana"), true),
            (f("Microsoft Teams", "", "AXTextArea", ""), true),
            (f("Telegram", "", "AXTextArea", ""), true),
            (f("Claude", "com.anthropic.claudefordesktop", "AXTextArea", "Write your prompt to Claude"), true),
            (f("Claude", "com.anthropic.claudefordesktop", "AXTextField", "Search chats"), false),
            (f("Slack", "", "AXSearchField", ""), false),
            (f("Microsoft Edge", "com.microsoft.edgemac", "AXTextField", "Address and search bar"), false),
            (f("Google Chrome", "", "AXTextField", ""), false),
            (f("Ghostty", "com.mitchellh.ghostty", "AXTextArea", ""), false),
            (f("Terminal", "", "AXTextArea", ""), false),
            (f("Visual Studio Code", "", "AXTextArea", ""), false),
        ];
        for (field, want) in rows {
            assert_eq!(return_asks(&field), want, "{} {}", field.app, field.name);
        }
    }

    #[test]
    fn a_confirm_that_times_out_says_so() {
        let (mut d, mut mem) = (slack(), Memory::new());
        d.answers = vec![Answer::Timeout];
        let e = run_all(&[key("enter")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Stopped]);
        assert_eq!(e[0].message.as_deref(), Some("No answer in 10 s: Return not pressed"));
        assert!(d.acts.is_empty(), "nothing pressed without a yes");
        // a spoken no stops without a message
        let mut d = slack();
        let e = run_all(&[key("enter")], &mut d, &mut mem);
        assert_eq!((e[0].status.clone(), e[0].message.clone()), (CmdStatus::Stopped, None));
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
    fn words_off_screen_go_to_jev_and_a_weak_but_clear_answer_acts() {
        // "the share button in the top header": not an exact label -> Jev, text only
        let (mut ask, seen) = script(vec![jev_target("2", 0.93, 0.05)]);
        let mut d = chrome_desk();
        let e = exec(&on("click", "share button in the top header"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        let q = &seen.lock().unwrap()[0].1;
        assert_eq!(q["target"]["criteria"]["8"], "Share (button, main)");
        assert!(q.get("action").is_none(), "grounding asks only target + risky");
        // case 23: weak (0.55) but #2 is far (0.2): the screenshot retry, then #1, no question
        let (mut ask, seen) = script(vec![jev_target("8", 0.5, 0.0), jev_target("8", 0.55, 0.0)]);
        let mut d = chrome_desk();
        d.screen_ok = true;
        let e = exec(&on("click", "the share one"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!((e.status, d.waits.len()), (CmdStatus::Done, 0));
        assert_eq!(d.acts, [Act::Click(Target::Mark(8))]);
    }

    /// Case 23: a real tie is broken by context before anyone is asked; case 22: else boxes on the tied 2-3 only,
    /// answered by place.
    #[test]
    fn ties_break_by_context_then_ask_with_places() {
        let tie = || ranking(&[("2", 0.46), ("8", 0.42), ("1", 0.05)]);
        // a browser page: the page's Share (main, inside the web area) beats the toolbar's
        let page = PtRect { x: 0.0, y: 40.0, w: 1440.0, h: 860.0 };
        let mut on_page = chrome();
        on_page.els[1].mark.rect.y = 5.0; // the toolbar Share sits above the page (center y 17)
        on_page.page = Some(page);
        let (mut ask, _) = script(vec![tie(), tie()]);
        let mut d = FakeDesk { screens: vec![on_page.clone()], screen_ok: true, ..chrome_desk() };
        let e = exec(&said(on("click", "share"), "dale a share"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice(), d.waits.len()), (CmdStatus::Done, &[Act::Click(Target::Mark(8))][..], 0));
        // he names the toolbar: the chrome wins
        let (mut ask, _) = script(vec![tie(), tie()]);
        let mut d = FakeDesk { screens: vec![on_page], screen_ok: true, ..chrome_desk() };
        let _ = exec(&said(on("click", "share"), "share en la barra"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        // no page, no region word: 2 boxes, labeled by place, "la de la derecha" = mark 8 (x 800 > x 200)
        let (mut ask, _) = script(vec![tie(), tie()]);
        let mut d = pick_desk(vec![Answer::Text("la de la derecha".into())]);
        let e = exec(&on("click", "share"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.waits.as_slice()), (CmdStatus::Done, &[Wait::Pick][..]));
        let pick = d.steps.iter().find(|s| s.status == CmdStatus::Pick).unwrap();
        assert!(pick.choose, "numbered boxes on the candidates, not all marks");
        assert_eq!(pick.choices.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 8], "only the tied ones");
        assert_eq!(d.acts, [Act::Click(Target::Mark(8))]);
        assert!(looks_like_pick("la de la derecha", pick.message.as_deref().unwrap()));
    }

    #[test]
    fn spatial_answers_table() {
        // three candidates: sidebar left, main middle-top, main right-bottom
        let a = PtRect { x: 20.0, y: 300.0, w: 200.0, h: 24.0 };
        let b = PtRect { x: 600.0, y: 100.0, w: 200.0, h: 24.0 };
        let c = PtRect { x: 1100.0, y: 700.0, w: 200.0, h: 24.0 };
        let cands = [(a, "sidebar"), (b, "main"), (c, "main")];
        let rows: [(&str, Option<usize>); 14] = [
            ("la de la izquierda", Some(0)), ("the left one", Some(0)), ("la de la derecha", Some(2)), ("the right one", Some(2)),
            ("la de arriba", Some(1)), ("the top one", Some(1)), ("the one in the header", Some(1)), ("la de abajo", Some(2)),
            ("la de la sidebar", Some(0)), ("the one in the sidebar", Some(0)), ("la barra lateral", Some(0)),
            ("la principal", None), // two in main: no single answer
            ("la segunda", None), // an ordinal is parse_choice's
            ("hmm", None),
        ];
        for (t, want) in rows {
            assert_eq!(spatial_choice(t, &cands), want, "{t:?}");
        }
        // two side by side within 8 pt: left/right says nothing
        let near = [(PtRect { x: 100.0, ..a }, "main"), (PtRect { x: 104.0, y: 500.0, ..a }, "main")];
        assert_eq!(spatial_choice("the left one", &near), None);
        assert_eq!(spatial_choice("the top one", &near), Some(0));
        let q = "Which one? 1 Pablo (tú) · 2 Pablo";
        for t in ["la de la izquierda", "the left one", "the one in the header", "la de arriba", "la de la sidebar"] {
            assert!(looks_like_pick(t, q), "{t:?}");
        }
        for t in ["abre la de la izquierda", "open slack", "abre Slack y en la sidebar abre Comercial X"] {
            assert!(!looks_like_pick(t, q), "{t:?}");
        }
    }

    #[test]
    fn tie_and_prior_tables() {
        assert_eq!(tied(&[(1, 0.6), (2, 0.3)]), [(1, 0.6)], "15 points apart is no tie");
        assert_eq!(tied(&[(1, 0.45), (2, 0.4), (3, 0.31), (4, 0.3)]), [(1, 0.45), (2, 0.4), (3, 0.31)], "3 at most");
        assert_eq!(tied(&[]), []);
        let els = vec![mk(1, "Docs", "tab", "toolbar", None, None), mk(2, "Docs", "link", "main", None, None), mk(3, "Docs", "link", "sidebar", None, None)];
        let page = Some(PtRect { x: 150.0, y: 0.0, w: 300.0, h: 900.0 }); // marks 2 (x 200..280) inside, 1 (x 100..180) out
        assert_eq!(prior(&[1, 2], &els, "abre docs", page), [2], "the page beats the browser's tab");
        assert_eq!(prior(&[1, 2], &els, "la pestaña docs", page), [1], "he named the tab");
        assert_eq!(prior(&[1, 2, 3], &els, "docs en la sidebar", None), [3]);
        assert_eq!(prior(&[1, 2, 3], &els, "docs", None), [1, 2, 3], "nothing to go on: ask");
        assert_eq!(prior(&[2, 3], &els, "el docs de la derecha", None), [3], "his place words (x 300 > 200)");
        assert_eq!(prior(&[1, 2], &els, "docs del footer", page), [2], "an unknown word changes nothing");
    }

    #[test]
    fn per_app_table() {
        let rows = [
            ("Slack", "com.tinyspeck.slackmacgap", Kind::Electron), ("", "com.microsoft.VSCode", Kind::Electron), ("Cursor", "", Kind::Electron),
            ("Claude", "com.anthropic.claudefordesktop", Kind::Electron), ("Discord", "", Kind::Electron), ("Notion", "", Kind::Electron),
            ("WhatsApp", "net.whatsapp.WhatsApp", Kind::Native), ("Finder", "com.apple.finder", Kind::Native), ("Notes", "", Kind::Native),
            ("Microsoft Edge", "com.microsoft.edgemac", Kind::Browser), ("Safari", "", Kind::Browser), ("Arc", "", Kind::Browser),
            ("Ghostty", "com.mitchellh.ghostty", Kind::Terminal), ("Terminal", "", Kind::Terminal), ("Photoshop", "", Kind::Native),
        ];
        for (app, bundle, want) in rows {
            assert_eq!(app_kind(app, bundle), want, "{app} {bundle}");
        }
        assert!(real_click_first(Kind::Electron, "row") && real_click_first(Kind::Electron, "link"));
        assert!(!real_click_first(Kind::Electron, "button") && !real_click_first(Kind::Native, "row") && !real_click_first(Kind::Browser, "link"));
        // keyboard first: every Electron app has planner hints with its palette key
        for app in ["Slack", "Discord", "Cursor", "Visual Studio Code", "Claude", "Notion", "Linear"] {
            let h = crate::planner::hints_for(app).unwrap_or_else(|| panic!("{app}: no hints"));
            assert!(h.contains("cmd+k") || h.contains("cmd+p"), "{app}: {h}");
        }
    }

    #[test]
    fn aim_point_table() {
        let r = PtRect { x: 100.0, y: 100.0, w: 300.0, h: 40.0 };
        // the center is the target (or inside it): the center
        assert_eq!(aim_point(r, &mut |_| Some(true)), Some((250.0, 120.0)));
        // no AX answer at the center: the center
        assert_eq!(aim_point(r, &mut |_| None), Some((250.0, 120.0)));
        // a hover button covers the center of a Slack row's link: the left third
        assert_eq!(aim_point(r, &mut |p| Some(p.0 < 200.0)), Some((150.0, 120.0)));
        // only the right end is free
        assert_eq!(aim_point(r, &mut |p| Some(p.0 > 300.0)), Some((350.0, 120.0)));
        // a sheet covers it all: no click
        assert_eq!(aim_point(r, &mut |_| Some(false)), None);
    }

    fn sigs(v: &[u64]) -> VecDeque<u64> {
        v.iter().copied().collect()
    }

    /// vc-feedback 21: "[104] Pablo (tú) (row, sidebar)" in Slack was AXPressed, reported Done, nothing opened.
    #[test]
    fn a_click_that_changes_nothing_gets_a_real_click_then_says_so() {
        let slack_scr = || Screen { app: "Slack".into(), kind: Kind::Electron, els: vec![mk(1, "Pablo (tú)", "row", "sidebar", None, None), mk(2, "Comercial X", "link", "sidebar", None, None),
            mk(3, "Huddle", "button", "toolbar", None, None), mk(4, "Message", "text area", "bottom", None, None), mk(5, "Search", "button", "toolbar", None, None)], ..chrome() };
        let desk = |s: &[u64]| FakeDesk { app: "Slack".into(), fields: vec![FField::new("AXTextArea", "Message", "", Some(4))], screens: vec![slack_scr()], sigs: sigs(s), ..Default::default() };
        // the screen changed: one click, done
        let (mut d, mut mem) = (desk(&[1, 2]), Memory::new());
        let e = exec(&on("click", "Pablo (tú) (row, sidebar)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(1))][..]));
        // unchanged after the click, changed after the real click
        let mut d = desk(&[1, 1, 2]);
        let e = exec(&on("click", "Pablo (tú) (row, sidebar)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(1)), Act::RealClick(Target::Mark(1))][..]));
        // unchanged twice: not Done; the pill and the planner read "nothing changed"
        let (mut d, mut mem) = (desk(&[1, 1, 1]), Memory::new());
        let e = exec(&on("click", "Pablo (tú) (row, sidebar)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, e.message.as_deref()), (CmdStatus::Unsure, Some("Clicked “Pablo (tú)” but nothing changed")));
        assert_eq!(d.acts.len(), 2, "one real retry, never a third click");
        assert!(mem.iter().all(|m| m.e.kind != MemKind::Clicked), "no 'clicked' memory for a no-op");
        // no signature (cannot tell): one click, done
        let mut d = desk(&[]);
        assert_eq!(exec(&on("click", "Comercial X (link, sidebar)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut Memory::new()).status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
    }

    /// Replay of Pablo's log sessions on the new paths (the 01:53 / 01:58 UTC lines, rebuilt from vc-feedback 21 + 23:
    /// the log file has rotated). Slack: "104" picked the DM row by number, AXPress did nothing, Done was reported, and
    /// "open it" repeated the no-op twice. Edge: "click the pricing" asked "this or that?" while Jev's #1 was right.
    #[test]
    fn replay_0153_slack_and_0158_edge_on_the_new_paths() {
        // ---- 01:53 Slack: the row by its number; the first click changes nothing ----
        let mut els: Vec<El> = (1..=110).map(|i| mk(i, &format!("channel {i}"), "row", "sidebar", None, None)).collect();
        els[103].mark.name = "Pablo (tú)".into();
        els[89].mark.name = "Comercial X".into();
        let slack_scr = Screen { app: "Slack".into(), kind: Kind::Electron, els, ..chrome() };
        let desk = |s: &[u64]| FakeDesk { app: "Slack".into(), fields: vec![FField::new("AXTextArea", "Message", "", None)], screens: vec![slack_scr.clone()], sigs: sigs(s), ..Default::default() };
        let row = on("click", "[104] Pablo (tú) (row, sidebar)");
        // the real click opens the DM: Done after the retry, no Jev, no pick
        let (mut d, mut mem) = (desk(&[7, 7, 8]), Memory::new());
        let e = exec(&said(row.clone(), "104"), 1, 1, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, d.acts.as_slice(), d.waits.len()), (CmdStatus::Done, &[Act::Click(Target::Mark(104)), Act::RealClick(Target::Mark(104))][..], 0));
        assert!(real_click_first(Kind::Electron, "row"), "the real desk makes the first click real already for a Slack row");
        // nothing opens even then: not Done, the pill says so, and no "clicked" memory for the planner to build on
        let (mut d, mut mem) = (desk(&[7, 7, 7]), Memory::new());
        let e = exec(&said(row, "open it"), 2, 2, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, e.message.as_deref()), (CmdStatus::Unsure, Some("Clicked “Pablo (tú)” but nothing changed")));
        assert_eq!(d.acts.len(), 2, "one retry, never a third click");
        assert!(mem.iter().all(|m| m.e.kind != MemKind::Clicked));
        // "Click 90": the number he said is his choice, straight to mark 90
        let mut d = desk(&[1, 2]);
        let e = exec(&said(on("click", "[90] Comercial X (row, sidebar)"), "Click 90"), 3, 3, &CFG, &mut no_jev, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(90))][..]));

        // ---- 01:58 Edge, web path: the page's DOM marks (no browser tabs among them) ----
        let scan = crate::web::parse(json!({
            "url": "https://acme.com/", "title": "Acme", "iw": 1440.0, "ih": 812.0, "sx": 0.0, "sy": 0.0, "ow": 1440.0, "oh": 900.0,
            "els": [
                {"id": 3, "role": "link", "name": "Pricing", "href": "https://acme.com/pricing", "state": null, "x": 900.0, "y": 20.0, "w": 70.0, "h": 20.0},
                {"id": 4, "role": "link", "name": "Docs", "href": "https://acme.com/docs", "state": null, "x": 990.0, "y": 20.0, "w": 50.0, "h": 20.0},
                {"id": 8, "role": "button", "name": "See all pricing plans", "state": null, "x": 600.0, "y": 500.0, "w": 200.0, "h": 40.0},
                {"id": 9, "role": "link", "name": "Sign in", "href": "https://acme.com/login", "state": null, "x": 1300.0, "y": 20.0, "w": 60.0, "h": 20.0},
                {"id": 12, "role": "text field", "name": "Email", "state": null, "x": 600.0, "y": 600.0, "w": 240.0, "h": 30.0}
            ]
        })).unwrap();
        let vp = crate::web::guess_viewport(&scan);
        let web_els = crate::web::els(&scan, vp);
        assert_eq!((web_els[0].mark.name.as_str(), web_els[0].mark.region.as_str()), ("Pricing", "header"), "a page header, not browser chrome");
        let edge_web = Screen { app: "Microsoft Edge".into(), kind: Kind::Browser, els: web_els, ..chrome() };
        let edge = |scr: &Screen| FakeDesk { app: "Microsoft Edge".into(), fields: vec![FField::new("AXWebArea", "", "", None)], screens: vec![scr.clone()], screen_ok: true, sigs: sigs(&[1, 2]), ..Default::default() };
        // Jev's #1 is clear: act on it, no "this or that?"
        let (mut ask, _) = script(vec![ranking(&[("1", 0.71), ("3", 0.22), ("2", 0.03)]), ranking(&[("1", 0.71), ("3", 0.22), ("2", 0.03)])]);
        let mut d = edge(&edge_web);
        let e = exec(&said(on("click", "the pricing"), "click the pricing"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice(), d.waits.len()), (CmdStatus::Done, &[Act::Click(Target::Mark(1))][..], 0), "{:?}", e.message);
        // ---- 01:58 Edge, AX path (JS from Apple Events off): the tab "Pricing | Acme" ties with the page link ----
        let page = PtRect { x: 0.0, y: 88.0, w: 1440.0, h: 812.0 };
        let mut tab = mk(1, "Pricing | Acme", "tab", "toolbar", None, None);
        tab.mark.rect.y = 10.0;
        let mut link = mk(2, "Pricing", "link", "main", Some("https://acme.com/pricing"), None);
        link.mark.rect.y = 120.0;
        let edge_ax = Screen { app: "Microsoft Edge".into(), kind: Kind::Browser, page: Some(page), els: vec![tab, link, mk(3, "Docs", "link", "main", None, None), mk(4, "Sign in", "link", "main", None, None), mk(5, "Email", "text field", "main", None, None)], ..chrome() };
        let (mut ask, _) = script(vec![ranking(&[("1", 0.48), ("2", 0.44), ("3", 0.02)]), ranking(&[("1", 0.48), ("2", 0.44), ("3", 0.02)])]);
        let mut d = edge(&edge_ax);
        let e = exec(&said(on("click", "the pricing"), "click the pricing"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice(), d.waits.len()), (CmdStatus::Done, &[Act::Click(Target::Mark(2))][..], 0), "the page beats the tab strip");
        // he says pestaña: the tab
        let (mut ask, _) = script(vec![ranking(&[("1", 0.48), ("2", 0.44), ("3", 0.02)]), ranking(&[("1", 0.48), ("2", 0.44), ("3", 0.02)])]);
        let mut d = edge(&edge_ax);
        exec(&said(on("click", "the pricing"), "la pestaña de pricing"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(d.acts, [Act::Click(Target::Mark(1))]);
    }

    /// Thin tree (a canvas app): OCR lines become marks; the box only when no text fits (an icon).
    #[test]
    fn thin_tree_uses_ocr_then_the_box() {
        let win = PtRect { x: 100.0, y: 50.0, w: 800.0, h: 500.0 };
        let canvas = Screen { app: "Figma".into(), display: chrome().display, scale: 2.0, window: win, els: vec![mk(1, "Close", "button", "toolbar", None, None)], ..Default::default() };
        let lines: Vec<crate::ocr::Line> = vec![("Comercial X".into(), (0.05, 0.85, 0.125, 0.03)), ("Enviar factura".into(), (0.56, 0.35, 0.14, 0.03))];
        let desk = || FakeDesk { app: "Figma".into(), fields: vec![FField::new("AXGroup", "", "", None)], screens: vec![canvas.clone()], screen_ok: true, lines: lines.clone(), ..Default::default() };
        // the exact text: no Jev, a click at the line's center (bottom-left Vision box -> top-left points)
        let mut d = desk();
        let e = exec(&on("click", "Comercial X"), 1, 1, &CFG, &mut no_jev, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Point(190.0, 118.0))], "x 100 + 0.1125 * 800, y 50 + (1 - 0.865) * 500");
        // not exact: Jev's text choice over the OCR marks (ids in reading order: Comercial X = 1, Enviar factura = 2)
        let (mut ask, seen) = script(vec![jev_target("2", 0.9, 0.1)]);
        let mut d = desk();
        let e = exec(&on("click", "la factura"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Point(604.0, 368.0))]);
        assert_eq!(seen.lock().unwrap()[0].1["target"]["criteria"]["2"], "Enviar factura (text, main)");
        // no text fits (an icon): Jev box on the window, then on the crop; the point maps back from the crop
        let (mut ask, seen) = script(vec![
            jev_target("none", 0.9, 0.0),
            json!({ "box": { "box": { "top": 0.5, "bottom": 0.54, "left": 0.5, "right": 0.53 } }, "visible": { "noul": 0.9 }, "risky": { "noul": 0.0 } }),
            json!({ "box": { "box": { "top": 0.4, "bottom": 0.6, "left": 0.4, "right": 0.6 } }, "visible": { "noul": 0.9 } }),
        ]);
        let mut d = desk();
        let e = exec(&on("click", "the gear icon"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done, "{e:?}");
        let crop = refine_crop(Bbox { top: 0.5, bottom: 0.54, left: 0.5, right: 0.53 }, win, 2.0);
        assert_eq!(d.acts, [Act::Click(Target::Point(center(crop).0, center(crop).1))]);
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].1.get("risky").is_some() && calls[2].1.get("risky").is_none(), "risky once, on the first pass");
        drop(calls);
        // an invisible box never clicks
        let (mut ask, _) = script(vec![jev_target("none", 0.9, 0.0), json!({ "box": { "box": { "top": 0.5, "bottom": 0.54, "left": 0.5, "right": 0.53 } }, "visible": { "noul": 0.1 }, "risky": { "noul": 0.0 } })]);
        let mut d = desk();
        assert_eq!(exec(&on("click", "the gear icon"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new()).status, CmdStatus::Unsure);
        assert!(d.acts.is_empty());
    }

    /// Keyboard first in Electron (next-build 6): cmd+k, the name, Return opens the channel; that Return sends
    /// nothing, so no confirm. A Return in the message box still asks.
    #[test]
    fn return_after_a_palette_needs_no_confirm() {
        let mut d = slack();
        let mut mem = Memory::new();
        let e = run_all(&[key("cmd+k"), typ("Comercial X", None), key("return")], &mut d, &mut mem);
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Done, CmdStatus::Done]);
        assert!(d.waits.is_empty());
        let mut d = slack();
        let e = run_all(&[typ("hola", None), key("return")], &mut d, &mut Memory::new());
        assert_eq!(statuses(&e), [CmdStatus::Done, CmdStatus::Stopped]);
        assert_eq!(d.waits, [Wait::Confirm]);
        // the cmd+k went to WhatsApp (no palette there): "Ana" sits in the message box, Return would send it
        let mut d = FakeDesk { app: "WhatsApp".into(), fields: vec![FField::new("AXTextArea", "Escribe un mensaje", "", None)], ..Default::default() };
        let e = run_all(&[key("cmd+k"), typ("Ana", None), key("return")], &mut d, &mut Memory::new());
        assert_eq!((statuses(&e)[2].clone(), d.waits.as_slice()), (CmdStatus::Stopped, &[Wait::Confirm][..]));
        // Pablo typed by hand after the palette (the field no longer holds what susurro typed): ask
        let (mut d, mut mem) = (slack(), Memory::new());
        run_all(&[key("cmd+k"), typ("Comercial X", None)], &mut d, &mut mem);
        d.f().insert(" hola equipo");
        let e = run_all(&[key("return")], &mut d, &mut mem);
        assert_eq!((e[0].status.clone(), d.waits.as_slice()), (CmdStatus::Stopped, &[Wait::Confirm][..]));
        // a stale palette (over 30 s ago): ask
        let (mut d, mut mem) = (slack(), Memory::new());
        run_all(&[key("cmd+k"), typ("Comercial X", None)], &mut d, &mut mem);
        for m in mem.iter_mut() {
            m.e.at -= 31_000;
        }
        assert_eq!(run_all(&[key("return")], &mut d, &mut mem)[0].status, CmdStatus::Stopped);
    }

    /// A Jev ranking: (mark id, probability), best first.
    fn ranking(r: &[(&str, f32)]) -> Value {
        let probs: Map<String, Value> = r.iter().map(|(k, p)| (k.to_string(), json!(p))).collect();
        json!({ "target": { "choice": r[0].0, "probabilities": probs, "confidence": r[0].1 }, "risky": { "noul": 0.0 } })
    }

    fn pick_desk(answers: Vec<Answer>) -> FakeDesk {
        FakeDesk { screen_ok: true, answers, ..chrome_desk() }
    }

    fn said(a: Action, words: &str) -> Action {
        Action { said: words.into(), ..a }
    }

    #[test]
    fn pick_mode_ranks_with_the_screenshot_and_waits_for_pablos_number() {
        // "Clicks: I choose": even an exact mark goes to Jev and the numbered choices
        let (mut ask, seen) = script(vec![ranking(&[("2", 0.6), ("8", 0.35), ("3", 0.03), ("9", 0.01)])]);
        let mut d = pick_desk(vec![Answer::Text("la dos".into())]);
        let e = exec(&said(on("click", "[2] Share (button, toolbar)"), "click share"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(8))], "choice 2 = mark 8, not mark 2");
        assert_eq!(d.waits, [Wait::Pick]);
        let (st, q) = seen.lock().unwrap()[0].clone();
        assert!(st.to_string().contains("numbered mark"), "the marked screenshot went to Jev");
        assert!(q["target"]["instructions"].as_str().unwrap().contains("the user said: \"click share\""));
        let pick = d.steps.iter().find(|s| s.status == CmdStatus::Pick).unwrap();
        assert!(pick.choose && !pick.preselect && pick.target.is_none());
        assert_eq!(pick.choices.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 8, 3], "3 shown: the 4th is under 5%");
        assert_eq!(pick.message.as_deref(), Some("Which one? 1 Share · 2 Share · 3 Search"));
        assert!(looks_like_pick("la dos", pick.message.as_deref().unwrap()));
    }

    #[test]
    fn pick_mode_a_sure_first_choice_is_highlighted_and_clicked() {
        // Jev >= 0.9 with #2 far behind: no choices, no wait; the brackets show 300 ms, then the click
        let (mut ask, _) = script(vec![ranking(&[("5", 0.92), ("2", 0.05), ("8", 0.01)])]);
        let mut d = pick_desk(vec![]);
        let e = exec(&on("click", "copy link"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
        assert!(d.waits.is_empty() && !d.steps.iter().any(|s| s.status == CmdStatus::Pick));
        assert!(d.sleeps.contains(&HIGHLIGHT_MS) && d.steps.iter().any(|s| s.status == CmdStatus::Aim && s.target == Some(5)));
        // not sure enough: the list waits; "hmm what" is no answer, "sí" takes #1
        let (mut ask, _) = script(vec![ranking(&[("5", 0.85), ("2", 0.1), ("8", 0.05)])]);
        let mut d = pick_desk(vec![Answer::Text("hmm what".into()), Answer::Yes, Answer::Text("sí".into())]);
        let e = exec(&on("click", "copy link"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.waits.len()), (CmdStatus::Done, 3));
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
    }

    /// Live log 2026-10-18 01:51:58: "Busca em YouTube." -> click "Buscar (combo box, main)" showed choices and waited
    /// 20 s. The only combo box on screen is obvious: no Jev, no list, a 300 ms highlight, then the click, the query and
    /// Return run as one order.
    #[test]
    fn pick_mode_the_only_search_box_is_obvious() {
        let els = vec![
            mk(1, "Atrás", "button", "toolbar", None, None),
            mk(2, "Barra de direcciones y de búsqueda", "text field", "toolbar", None, None),
            mk(3, "Buscar", "combo box", "main", None, None),
            mk(4, "Buscar", "button", "main", None, None),
            mk(5, "Buscar con la voz", "button", "main", None, None),
            mk(6, "Ultra Music Festival", "link", "main", None, None),
        ];
        let yt = Screen { app: "Google Chrome".into(), display: PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 }, scale: 2.0, els, ..Default::default() };
        let mut d = FakeDesk {
            app: "Google Chrome".into(),
            fields: vec![FField::new("AXWebArea", "", "", None), FField::new("AXComboBox", "Buscar", "", Some(3))],
            screens: vec![yt],
            screen_ok: true,
            ..Default::default()
        };
        let order = [said(on("click", "Buscar (combo box, main)"), "Busca en YouTube. Ultra Miami 2015."), typ("Ultra Miami 2015", None), Action { op: "key".into(), keys: Some("return".into()), ..Default::default() }];
        let mut mem = Memory::new();
        for (i, a) in order.iter().enumerate() {
            let e = exec(a, i as u32 + 1, 1, &CHOOSE, &mut no_jev, &mut d, &mut mem);
            assert_eq!(e.status, CmdStatus::Done, "{i}: {e:?}");
        }
        assert!(d.waits.is_empty(), "no pick, no Return confirm in a browser");
        assert_eq!(d.acts[0], Act::Click(Target::Mark(3)));
        assert_eq!(d.sleeps.first(), Some(&HIGHLIGHT_MS));
        assert_eq!(d.value(1), "Ultra Miami 2015");
        assert!(matches!(d.acts.last(), Some(Act::Combo(36, 0))), "{:?}", d.acts);
        // two text fields of the same kind: still a choice
        assert!(!only_of_its_kind(&mk(2, "a", "text field", "main", None, None), &[mk(2, "a", "text field", "main", None, None), mk(7, "b", "text field", "main", None, None)]));
    }

    #[test]
    fn pick_mode_a_click_by_hand_a_mark_number_a_timeout() {
        // Pablo clicks choice 2 himself: the app got that click, susurro does not click again
        let (mut ask, _) = script(vec![ranking(&[("2", 0.6), ("8", 0.35), ("3", 0.05)])]);
        let mut d = pick_desk(vec![Answer::Clicked(1)]);
        let mut mem = Memory::new();
        let e = exec(&on("click", "share"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut mem);
        assert_eq!((e.status, e.summary.as_str()), (CmdStatus::Done, "you clicked “Share” (button)"));
        assert!(d.acts.is_empty());
        assert_eq!(mem.back().unwrap().e.kind, MemKind::Clicked);
        // a number past the choices names a mark: Send (risky by name) needs no confirm, picking it was the yes
        let (mut ask, _) = script(vec![ranking(&[("2", 0.6), ("8", 0.35), ("3", 0.05)])]);
        let mut d = pick_desk(vec![Answer::Text("9".into())]);
        let e = exec(&on("click", "share"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.waits.as_slice()), (CmdStatus::Done, &[Wait::Pick][..]));
        assert_eq!(d.acts, [Act::Click(Target::Mark(9))]);
        // nobody answers: it says so
        let (mut ask, _) = script(vec![ranking(&[("2", 0.6), ("8", 0.35), ("3", 0.05)])]);
        let mut d = pick_desk(vec![Answer::Timeout]);
        let e = exec(&on("click", "share"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, e.message.as_deref()), (CmdStatus::Stopped, Some("No answer in 20 s: nothing clicked")));
        assert!(d.acts.is_empty());
        // type into a choice Pablo clicked: no second focus, the text goes in
        let (mut ask, _) = script(vec![ranking(&[("3", 0.6), ("7", 0.35), ("2", 0.05)])]);
        let mut d = pick_desk(vec![Answer::Clicked(1)]);
        d.focus = 2; // his click focused Message
        let e = exec(&typ("hola", Some("the message box")), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert!(d.acts.is_empty());
        assert_eq!(d.value(2), "hola");
    }

    #[test]
    fn pick_mode_a_number_pablo_said_is_his_choice() {
        // "open the first link, 41": the planner (or mark_ref) wrote "[N] label", he said N: no list, no Jev
        let mut d = pick_desk(vec![]);
        let e = exec(&said(on("click", "[5] Copy link (button, main)"), "click the 5"), 1, 1, &CHOOSE, &mut no_jev, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done);
        assert_eq!(d.acts, [Act::Click(Target::Mark(5))]);
        assert!(d.waits.is_empty());
        // auto mode: today's path, an exact label clicks at once
        let mut d = chrome_desk();
        let e = exec(&on("click", "[5] Copy link (button, main)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(5))][..]));
    }

    #[test]
    fn pick_tables() {
        let names: Vec<String> = ["Search", "New chat", "YouTube Music"].iter().map(|s| s.to_string()).collect();
        let ids: Vec<u32> = (1..=50).collect();
        let rows: [(&str, Option<Picked>); 18] = [
            ("uno", Some(Picked::Nth(0))),
            ("la dos", Some(Picked::Nth(1))),
            ("tres", Some(Picked::Nth(2))),
            ("2", Some(Picked::Nth(1))),
            ("the second one", Some(Picked::Nth(1))),
            ("el tercero", Some(Picked::Nth(2))),
            ("the last one", Some(Picked::Nth(2))),
            ("new chat", Some(Picked::Nth(1))),
            ("youtube music", Some(Picked::Nth(2))),
            ("41", Some(Picked::Mark(41))),
            ("la 41", Some(Picked::Mark(41))),
            ("99", None),
            ("sí", Some(Picked::Nth(0))),
            ("ese", Some(Picked::Nth(0))),
            ("what was that", None),
            ("the 2nd one", Some(Picked::Nth(1))),
            ("yes please", Some(Picked::Nth(0))),
            ("that one", Some(Picked::Nth(0))),
        ];
        for (t, want) in rows {
            assert_eq!(parse_choice(t, &names, &ids), want, "{t:?}");
        }
        // Jev's ranking -> what Pablo sees
        let r = |v: &[(&str, f32)]| v.iter().map(|(k, p)| (k.to_string(), *p)).collect::<Vec<_>>();
        let on_screen = [1, 2, 3, 4, 5, 6, 7];
        assert_eq!(choices(&r(&[("1", 0.5), ("none", 0.3), ("2", 0.1), ("3", 0.06), ("4", 0.03), ("99", 0.01)]), &on_screen, None), [(1, 0.5), (2, 0.1), (3, 0.06)], "none and unknown ids drop, 4th under 5%");
        assert_eq!(choices(&r(&[("1", 0.3), ("2", 0.2), ("3", 0.2), ("4", 0.2), ("5", 0.05), ("6", 0.05)]), &on_screen, None).len(), 5, "5 at most");
        assert_eq!(choices(&r(&[("1", 0.6), ("2", 0.4)]), &on_screen, Some(7)), [(1, 0.6), (2, 0.4), (7, 0.0)], "the planner's exact mark stays in");
        assert!(sure(&[(1, 0.9), (2, 0.29)]) && sure(&[(1, 0.99)]));
        assert!(!sure(&[(1, 0.89), (2, 0.01)]) && !sure(&[(1, 0.98), (2, 0.31)]));
        // only a shown choice answers (live log 2026-10-18: "Ultra Miami 2015." was taken as an answer)
        let q = "Which one? 1 Search · 2 New chat · 3 YouTube Music";
        for t in ["ese", "sí", "that one", "One.", "la dos", "2", "number 3", "the second one", "la segunda", "la última", "click the third", "new chat", "dos por favor",
            "click on the second one", "en la segunda", "go with two", "the 2nd one", "yes please"] {
            assert!(looks_like_pick(t, q), "{t:?}");
        }
        for t in ["Ultra Miami 2015.", "4", "la cuarta", "open a new tab and type hello", "type 5", "busca una canción", "Onto the 104, open it.", "Open it.", "ok search youtube"] {
            assert!(!looks_like_pick(t, q), "{t:?}");
        }
    }

    #[test]
    fn mark_numbers_resolve_against_what_was_drawn() {
        assert_eq!(split_mark("[41] YouTube Music (link, main)"), (Some(41), "YouTube Music (link, main)"));
        assert_eq!(split_mark("[41]"), (Some(41), ""));
        assert_eq!(split_mark("41"), (Some(41), ""));
        assert_eq!(split_mark("#41"), (Some(41), ""));
        assert_eq!(split_mark("Share (button, toolbar)"), (None, "Share (button, toolbar)"));
        assert_eq!(split_mark("[x] y"), (None, "[x] y"));
        assert!(said_number("Open the first link, 41.", 41) && !said_number("open 141", 41) && !said_number("click share", 2));
        let els = chrome().els;
        let id = |e: Option<&El>| e.map(|e| e.mark.id);
        assert_eq!(id(mark_target("[5] Copy link (button, main)", &els, &[])), Some(5));
        assert_eq!(id(mark_target("Copy link (button, main)", &els, &[])), Some(5), "no number: the label");
        // two "Share" buttons: the label is ambiguous, the drawn mark 8 found again by its rect
        assert_eq!(id(mark_target("[8] Share", &els, &els)), Some(8));
        // the page moved on: the board's 41 is gone from the fresh read -> nothing, never another mark 41
        let mut board = els.clone();
        board[4].mark.name = "YouTube Music".into();
        assert_eq!(id(mark_target("[5] YouTube Music (link, main)", &els, &board)), None);
        assert_eq!(id(mark_target("[5]", &els, &[])), Some(5), "a bare number with no board");
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

    #[test]
    fn pick_mode_adversarial() {
        // a big choice (a web area) holds a small one: the click goes to the smallest under it
        let big = PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 };
        let small = PtRect { x: 100.0, y: 50.0, w: 80.0, h: 24.0 };
        assert_eq!(choice_at(&[big, small], (120.0, 60.0)), Some(1));
        assert_eq!(choice_at(&[big, small], (700.0, 700.0)), Some(0));
        assert_eq!(choice_at(&[small], (700.0, 700.0)), None);
        // an order that also holds a yes word or a choice name is an order, not a pick
        let q = "Which one? 1 Share · 2 Share · 3 Search";
        for order in ["ok search youtube", "sí, abre slack", "ok open edge", "search for cats"] {
            assert!(!looks_like_pick(order, q), "{order}");
        }
        for answer in ["ok", "sí", "ese", "that one", "this one", "la dos", "the second one", "search", "open search"] {
            assert!(looks_like_pick(answer, q), "{answer}");
        }
        let names: Vec<String> = ["Share", "Share", "Search"].map(String::from).to_vec();
        assert_eq!(parse_choice("open search", &names, &[2, 8, 3]), Some(Picked::Nth(2)));
        assert_eq!(parse_choice("ok search youtube", &names, &[2, 8, 3]), None);
        assert_eq!(parse_choice("ok", &names, &[2, 8, 3]), Some(Picked::Nth(0)), "a yes takes #1, Jev's best");
        // Esc while the list waits: nothing clicked, no message
        let (mut ask, _) = script(vec![ranking(&[("2", 0.6), ("8", 0.35), ("3", 0.05)])]);
        let mut d = pick_desk(vec![Answer::No]);
        let e = exec(&on("click", "share"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, e.message, d.acts.len()), (CmdStatus::Stopped, None, 0));
        // a number in the order that is on the board: no list, the drawn mark is clicked
        let mut d = FakeDesk { board: chrome().els, ..pick_desk(vec![]) };
        let (mut ask, seen) = script(vec![]);
        let e = exec(&said(on("click", "[8] Share (button, main)"), "share, 8"), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice(), seen.lock().unwrap().len()), (CmdStatus::Done, &[Act::Click(Target::Mark(8))][..], 0));
        // a type target is a field to focus, not a click: no list (replay 2026-10-18: "type YouTube" into the address bar)
        let mut d = pick_desk(vec![]);
        let (mut ask, seen) = script(vec![]);
        let e = exec(&typ("cats", Some("[3] Search (search field, toolbar)")), 1, 1, &CHOOSE, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.typed.as_slice(), seen.lock().unwrap().len(), d.waits.len()), (CmdStatus::Done, &["cats".to_string()][..], 0, 0));
    }

    /// Real API: Pablo's live session (susurro.log 2026-10-18 00:28-00:30 UTC) end to end on a fake desk: the
    /// mark-number pass, the real planner (DEFAULT_PLANNER), then exec in pick mode with a scripted Jev ranking.
    /// `OPENROUTER_API_KEY=... cargo test replay_the_live_session_on_a_fake_desk -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn replay_the_live_session_on_a_fake_desk() {
        use crate::planner::{Input, mark_ref, plan_openrouter};
        let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
        let model = std::env::var("PLANNER").unwrap_or_else(|_| crate::contract::DEFAULT_PLANNER.into());
        let model = model.as_str();
        let claude = Screen { app: "Claude".into(), display: chrome().display, scale: 2.0, els: vec![
            mk(1, "New chat", "button", "sidebar", None, None), mk(2, "Search", "button", "sidebar", None, None),
            mk(3, "Write your prompt to Claude", "text area", "main", None, None), mk(4, "Send message", "button", "main", None, None)], ..Default::default() };
        let edge_new = Screen { app: "Microsoft Edge".into(), display: chrome().display, scale: 2.0, els: vec![
            mk(1, "New Tab", "tab", "top", None, None), mk(2, "Address and search bar", "text field", "toolbar", None, None),
            mk(3, "Search the web", "search field", "main", None, None), mk(4, "YouTube", "link", "main", None, None)], ..Default::default() };
        let res: Vec<El> = [(34, "Address and search bar", "text field", "toolbar"), (35, "Videos", "link", "top"), (40, "YouTube YouTube https://www.youtube.com", "link", "main"),
            (41, "YouTube Music", "link", "main"), (42, "YouTube - Wikipedia", "link", "main")].iter().map(|(i, n, r, g)| mk(*i, n, r, g, None, None)).collect();
        let edge_res = Screen { app: "Microsoft Edge".into(), display: chrome().display, scale: 2.0, els: res, ..Default::default() };
        let fields = |app: &str| match app {
            "Claude" => vec![FField::new("AXTextArea", "Write your prompt to Claude", "", Some(3))],
            _ => vec![FField::new("AXTextField", "Address and search bar", "", Some(2)), FField::new("AXWebArea", "", "", None)],
        };
        // (order, screen, Jev ranking for a pick, Pablo's pick answer)
        let orders: Vec<(&str, Screen, Value, Answer)> = vec![
            ("Open, Microsoft Edge.", claude, json!(null), Answer::No),
            ("search for you- YouTube.", edge_new, json!(null), Answer::No),
            ("Open the first link.", edge_res.clone(), ranking(&[("40", 0.7), ("41", 0.2), ("42", 0.06), ("35", 0.02)]), Answer::Text("uno".into())),
            ("Open the first link, 41.", edge_res, json!(null), Answer::No),
        ];
        let mut mem = Memory::new();
        let (mut cost, mut log) = (0.0, vec![]);
        for (step, (utt, scr, rank, answer)) in orders.into_iter().enumerate() {
            let app = scr.app.clone();
            let mut d = FakeDesk { app: app.clone(), fields: fields(&app), screens: vec![scr.clone()], board: scr.els.clone(), screen_ok: true, answers: vec![answer],
                installed: vec!["Microsoft Edge".into(), "Claude".into(), "Google Chrome".into()], ..Default::default() };
            let ids: Vec<u32> = scr.els.iter().map(|e| e.mark.id).collect();
            let plan = match mark_ref(utt, &ids).and_then(|n| scr.els.iter().find(|e| e.mark.id == n)) {
                Some(e) => crate::planner::Plan { complete: true, actions: vec![Action { op: "click".into(), target: Some(format!("[{}] {}", e.mark.id, ax::describe(e))), ..Default::default() }], ..Default::default() },
                None => {
                    let focused = d.focused();
                    let input = Input { utterance: utt.into(), app: app.clone(), focused, marks: scr.els.iter().map(|e| format!("[{}] {}", e.mark.id, ax::describe(e))).collect(), ..Default::default() };
                    let (p, m) = plan_openrouter(&key, model, &input).unwrap_or_else(|e| panic!("{utt}: {e:#}"));
                    cost += m.cost;
                    p
                }
            };
            let (mut ask, _) = script(if rank.is_null() { vec![] } else { vec![rank] });
            let mut ends = vec![];
            for a in &plan.actions {
                let e = exec(&Action { said: utt.into(), ..a.clone() }, step as u32 + 1, 1, &CHOOSE, &mut ask, &mut d, &mut mem);
                let stop = e.status != CmdStatus::Done;
                ends.push(e);
                if stop {
                    break;
                }
            }
            let pick = d.steps.iter().find(|s| s.status == CmdStatus::Pick).map(|s| s.choices.iter().map(|c| c.0).collect::<Vec<_>>());
            println!("{utt:30} -> {:?}\n   acts {:?} typed {:?} waits {:?} pick {pick:?} ends {:?}", plan.actions.iter().map(crate::planner::label).collect::<Vec<_>>(), d.acts, d.typed, d.waits, statuses(&ends));
            log.push((d.acts.clone(), d.typed.clone(), d.waits.clone(), pick, statuses(&ends)));
        }
        println!("${cost:.5}");
        let done = |e: &Vec<CmdStatus>| !e.is_empty() && e.iter().all(|s| *s == CmdStatus::Done);
        assert!(done(&log[0].4) && log[0].0 == [Act::OpenApp("Microsoft Edge".into())], "open Edge");
        assert!(done(&log[1].4) && matches!(log[1].0.first(), Some(Act::Combo(37, CMD) | Act::Click(Target::Mark(3)))) && log[1].1 == ["YouTube"] && log[1].0.last() == Some(&Act::Combo(36, 0)) && log[1].2.is_empty(), "cmd+l or the only search field, YouTube, Return: no pick, no confirm");
        assert!(done(&log[2].4) && log[2].3.as_ref().is_some_and(|c| c.starts_with(&[40, 41, 42])) && log[2].0 == [Act::Click(Target::Mark(40))], "pick list, 'uno' clicks 40");
        assert!(done(&log[3].4) && log[3].0 == [Act::Click(Target::Mark(41))] && log[3].2.is_empty(), "41 clicks mark 41, no list");
    }


    // ---------- Pablo's session 2026-10-18 15:51-15:53 UTC (vc-feedback 25-28) on a fake desk ----------

    /// The Edge screen around 15:51:55 (JS from Apple Events off: the AX path), a JS Paint page under it.
    fn edge_0151(page: &[El]) -> Screen {
        let mut els = vec![mk(1, "JS Paint", "tab", "toolbar", None, None), mk(2, "New Tab", "button", "toolbar", None, None), mk(3, "Address and search bar", "text field", "toolbar", None, None)];
        els.extend(page.iter().cloned());
        let win = PtRect { x: 0.0, y: 0.0, w: 1200.0, h: 800.0 };
        Screen { app: "Microsoft Edge".into(), display: win, scale: 2.0, window: win, kind: Kind::Browser, els, ..Default::default() }
    }

    fn edge_0151_desk(page: &[El]) -> FakeDesk {
        FakeDesk {
            app: "Microsoft Edge".into(),
            fields: vec![FField::new("AXTextField", "Address and search bar", "", Some(3)), FField::new("AXWebArea", "", "", None)],
            focus: 1,
            screens: vec![edge_0151(page)],
            screen_ok: true,
            ..Default::default()
        }
    }

    /// 15:51:55 "open a new tab and search for GS Paint": cmd+t already put the keys in the omnibox; the planner's
    /// click on it changed nothing, the real click's hit-test said "something covers", and the order died there.
    #[test]
    fn replay_0151_a_new_tab_types_into_the_omnibox_without_a_click() {
        let mut d = FakeDesk { covered: true, sigs: sigs(&[5, 5, 5, 5]), ..edge_0151_desk(&[mk(4, "Search the web", "search field", "main", None, None)]) };
        let mut mem = Memory::new();
        let plan = [key("cmd+t"), on("click", "[3] Address and search bar (text field, toolbar)"), typ("GS Paint", None), key("return")];
        let e = run_all(&plan, &mut d, &mut mem);
        assert_eq!(statuses(&e), vec![CmdStatus::Done; 4], "{:?}", e.last().unwrap().message);
        assert_eq!(d.acts, [Act::Combo(17, CMD), Act::Combo(36, 0)], "no click on the field that has the keys");
        assert_eq!((d.value(0), d.typed.as_slice()), ("GS Paint", &["GS Paint".to_string()][..]));
        // the keys are elsewhere: a toolbar field takes AXFocused, no click at all
        let mut d = FakeDesk { covered: true, sigs: sigs(&[5, 5, 5]), ..edge_0151_desk(&[]) };
        let e = exec(&on("click", "Address and search bar (text field, toolbar)"), 1, 1, &CFG, &mut no_jev, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Focus(Target::Mark(3))][..]));
    }

    /// JS Paint's AX tree (15:52:26): Edge chrome, the menu bar, the tools; the palette is canvas-like, no marks.
    fn jspaint_page() -> Vec<El> {
        let mut v: Vec<El> = [(4, "File"), (5, "Edit"), (6, "Colors"), (7, "Help")].iter().map(|(i, n)| mk(*i, n, "menu item", "main", None, None)).collect();
        for (i, n) in [(8, "Pencil"), (9, "Brush"), (10, "Fill With Color")] {
            let mut e = mk(i, n, "button", "sidebar", None, None);
            e.mark.rect = PtRect { x: 4.0, y: 100.0 + 30.0 * (i - 8) as f64, w: 24.0, h: 24.0 };
            v.push(e);
        }
        v
    }

    fn box_reply(top: f64, left: f64, side: f64, visible: f32) -> Value {
        json!({ "box": { "box": { "top": top, "bottom": top + side, "left": left, "right": left + side } }, "visible": { "noul": visible }, "risky": { "noul": 0.0 } })
    }

    /// 15:52:26 "change the color to red on the bottom" -> click "red" -> Jev's best mark was the menu "Colors":
    /// clicked, reported Done. No mark is named red: the box looks at the window (with "on the bottom" in its words).
    #[test]
    fn replay_0152_red_with_no_mark_goes_to_the_box_not_to_colors() {
        let red = said(on("click", "red"), "Now there select the pencil, it's already selected. Now change the color to red on the bottom.");
        let jev_colors = ranking(&[("6", 0.62), ("2", 0.2), ("4", 0.1)]);
        let (mut ask, seen) = script(vec![jev_colors.clone(), jev_colors.clone(), box_reply(0.92, 0.2, 0.03, 0.9), box_reply(0.75, 0.45, 0.1, 0.9)]);
        let mut d = edge_0151_desk(&jspaint_page());
        let e = exec(&red, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!(e.status, CmdStatus::Done, "{:?}", e.message);
        let [Act::Click(Target::Point(_, y))] = d.acts.as_slice() else { panic!("{:?}", d.acts) };
        assert!(*y > 700.0, "the bottom palette, not the menu bar: {y}");
        assert_eq!(e.summary, "clicked “red” (box, screen)");
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls[2].1["box"]["instructions"].as_str().unwrap().contains("on the bottom"), "{}", calls[2].1);
        drop(calls);
        // case 28: the box lands on an element with another name: it is clicked and the pill says so
        let mut bar = mk(11, "Color box", "group", "bottom", None, None);
        bar.mark.rect = PtRect { x: 200.0, y: 730.0, w: 400.0, h: 40.0 };
        let mut page = jspaint_page();
        page.push(bar);
        let (mut ask, _) = script(vec![jev_colors.clone(), jev_colors.clone(), box_reply(0.92, 0.3, 0.03, 0.9), box_reply(0.75, 0.45, 0.1, 0.9)]);
        let mut d = edge_0151_desk(&page);
        let e = exec(&red, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(11))][..]));
        assert_eq!(e.summary, "clicked “Color box” (group, bottom), not sure it is “red”");
        // the box sees no red: nothing is clicked (never the menu)
        let (mut ask, _) = script(vec![jev_colors.clone(), jev_colors, box_reply(0.92, 0.2, 0.03, 0.1)]);
        let mut d = edge_0151_desk(&jspaint_page());
        assert_eq!(exec(&red, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new()).status, CmdStatus::Unsure);
        assert!(d.acts.is_empty());
    }

    /// 15:52:37 "No, on the bottom, not the top." clicked "New Tab". The planner now answers with retry: the last
    /// target ("red") + the constraint; the element the last click hit is never the answer again.
    #[test]
    fn replay_0152_a_correction_retries_the_last_target_with_its_constraint() {
        let mut mem = Memory::new();
        remember(&mut mem, MemKind::Clicked, "Colors", &Field { app: "Microsoft Edge".into(), ..Default::default() }, None);
        let retry = said(Action { op: "retry".into(), target: Some("red".into()), text: Some("bottom, not the top".into()), ..Default::default() }, "No, on the bottom, not the top.");
        // Jev, without "Colors" on the list, leans to "New Tab" (real Jev 2026-10-18: none 0.45-0.5, a mark 0.22-0.26):
        // weak and not red -> the box
        let (mut ask, seen) = script(vec![ranking(&[("2", 0.4), ("4", 0.05)]), ranking(&[("2", 0.4), ("4", 0.05)]), box_reply(0.92, 0.2, 0.03, 0.9), box_reply(0.75, 0.45, 0.1, 0.9)]);
        let mut d = edge_0151_desk(&jspaint_page());
        let e = exec(&retry, 5, 4, &CFG, &mut ask, &mut d, &mut mem);
        assert_eq!(e.status, CmdStatus::Done, "{:?}", e.message);
        let [Act::Click(Target::Point(_, y))] = d.acts.as_slice() else { panic!("{:?}", d.acts) };
        assert!(*y > 700.0, "{y}");
        let calls = seen.lock().unwrap();
        assert!(calls[0].1["target"]["criteria"].get("6").is_none(), "Colors is off the list");
        assert!(calls[2].1["box"]["instructions"].as_str().unwrap().contains("bottom, not the top"));
        assert_eq!(crate::planner::label(&retry), "retry “red” (bottom, not the top)");
    }

    /// Jev sure of a synonym in another language (measured: "search" -> "Buscar" 0.98): click it, no "not sure".
    #[test]
    fn a_sure_synonym_is_clicked_not_sent_to_the_box() {
        let mut es = chrome();
        es.els[1].mark.name = "Preferencias".into();
        es.els[7].mark.name = "Buscar".into();
        es.els[2].mark.name = "Dirección".into();
        let (mut ask, seen) = script(vec![ranking(&[("8", 0.98), ("none", 0.02)])]);
        let mut d = FakeDesk { screens: vec![es], screen_ok: true, ..chrome_desk() };
        let e = exec(&on("click", "search"), 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(8))][..]), "{:?}", e.message);
        assert_eq!(e.summary, "clicked “Buscar” (button, main)");
        assert_eq!(seen.lock().unwrap().len(), 1, "no box calls");
    }

    /// "no, the other one" after Pablo switched apps: the last click was in Edge, nothing is clicked in Chrome.
    #[test]
    fn a_retry_never_clicks_in_another_app() {
        for name in ["Colors", ""] {
            // "" = the last click was a box click (no element name)
            let mut mem = Memory::new();
            remember(&mut mem, MemKind::Clicked, name, &Field { app: "Microsoft Edge".into(), ..Default::default() }, None);
            let retry = Action { op: "retry".into(), target: Some("red".into()), text: Some("the other one".into()), ..Default::default() };
            let mut d = FakeDesk { screen_ok: true, ..chrome_desk() };
            let e = exec(&retry, 2, 2, &CFG, &mut no_jev, &mut d, &mut mem);
            assert_eq!(e.status, CmdStatus::Unsure, "{name:?}: {:?}", e.message);
            assert!(d.acts.is_empty());
        }
    }

    /// Two "Share" buttons: "no, the other one" skips only the one clicked, so the other Share is the answer.
    #[test]
    fn a_retry_among_namesakes_clicks_the_other_one() {
        let mut mem = Memory::new();
        let h = Hit::of(&chrome().els[1], 0.0);
        clicked(&mut mem, &h, &Field { app: "Google Chrome".into(), ..Default::default() });
        let retry = Action { op: "retry".into(), target: Some("share".into()), text: Some("the other one".into()), ..Default::default() };
        let mut d = chrome_desk();
        let e = exec(&retry, 2, 2, &CFG, &mut no_jev, &mut d, &mut mem);
        assert_eq!((e.status, d.acts.as_slice()), (CmdStatus::Done, &[Act::Click(Target::Mark(8))][..]), "{:?}", e.message);
    }

    #[test]
    fn names_table() {
        let rows = [
            ("red", "Colors", false),
            ("red", "New Tab", false),
            ("the pricing", "Pricing | Acme", true),
            ("la factura", "Enviar factura", true),
            ("the first link", "JS Paint JS Paint https://jspaint.app", true),
            ("Address and search bar (text field, toolbar)", "Address and search bar", true),
            ("the share button in the top header", "Share", true),
            ("comercial", "Comercial X", true),
            ("gear icon", "Settings", false),
            ("", "Anything", true),
        ];
        for (w, n, want) in rows {
            assert_eq!(names(w, n), want, "{w:?} / {n:?}");
        }
    }

    /// Real API (Jev box, coarse-to-fine) on a synthetic JS Paint window (tests/fixtures/jspaint_palette.png, made by
    /// jspaint_palette.py: 2400 x 1600 px = 1200 x 800 pt at 2x, the MS Paint palette at the bottom, red = row 2,
    /// column 3). No capture of Pablo's screen; PIL crops the PNG. 4 Jev calls at most.
    /// `OPENROUTER_API_KEY=... cargo test jev_box_finds_red_in_a_synthetic_palette -- --ignored --nocapture`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn jev_box_finds_red_in_a_synthetic_palette() {
        let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
        let png = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jspaint_palette.png");
        let calls = std::cell::Cell::new(0);
        let mut cost = 0.0;
        let mut ask = |s: &Value, q: &Value| {
            anyhow::ensure!(calls.get() < 4, "call cap");
            calls.set(calls.get() + 1);
            let r = jev::ask(jev::DECISIONS_URL, &key, crate::contract::JEV_MODEL, s, q)?;
            cost += r.cost;
            println!("jev {} ms ${:.5}: {}", r.latency_ms, r.cost, r.answers);
            Ok(r)
        };
        let mut d = FakeDesk { image: Some((png, 2.0)), ..edge_0151_desk(&jspaint_page()) };
        let red = said(on("click", "red"), "Now change the color to red on the bottom.");
        let e = exec(&red, 1, 1, &CFG, &mut ask, &mut d, &mut Memory::new());
        println!("{:?} {:?} {} calls ${cost:.5}", e, d.acts, calls.get());
        // the red swatch, in points (jspaint_palette.py: x 38 + 2 * 17, y 733 + 17, 16 x 16)
        let r = PtRect { x: 72.0, y: 750.0, w: 16.0, h: 16.0 };
        let [Act::Click(Target::Point(x, y))] = d.acts.as_slice() else { panic!("{:?}", d.acts) };
        assert!(*x >= r.x - 2.0 && *x <= r.x + r.w + 2.0 && *y >= r.y - 2.0 && *y <= r.y + r.h + 2.0, "({x}, {y}) is not on red {r:?}");
    }
}
