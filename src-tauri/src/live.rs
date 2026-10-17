//! The live command session (`.lo/VOICE_CONTROL_V2.md` section 1): actions run while Pablo keeps talking (case 10).
//! session.rs cuts the recording into chunks (audio.rs COMMAND cuts) and hands them here:
//!   chunk -> `cmd-stt` thread (one per chunk, max 3 in flight) -> `cmd-live` (re-order by seq, buffer, local yes/no,
//!   planner) -> the queue -> `cmd-exec` (command.rs `exec`, one action at a time) -> SessionCmd::CommandEnded.
//! The marks window stays up for the whole session; Pablo's own mouse or keys hide it until the next order (case 13).

use crate::AppState;
use crate::command::{self, Cfg, End, Ended, Memory, Wait};
use crate::contract::{CmdStatus, CommandSettings, CommandStep, LiveState, MemKind, MemoryEntry};
use crate::planner::{self, Action, Plan};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, mpsc};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

/// What susurro did (and what Pablo asked), across sessions while the app runs. RAM only.
/// ponytail: memory in RAM only; persist it if Pablo wants "undo what you did yesterday".
pub static MEMORY: Mutex<Memory> = Mutex::new(VecDeque::new());
/// Entries older than this do not go to the planner.
const MEMORY_FOR_PLANNER_MS: i64 = 10 * 60 * 1000;
/// MAI requests in flight at once.
const MAX_IN_FLIGHT: usize = 3;
/// Brand names the command STT hears often wrong (vc-feedback 18: "ElevenLabs" -> "the 11 tabs").
const BRANDS: [&str; 20] = [
    "ElevenLabs", "Ghostty", "WhatsApp", "Slack", "ChatGPT", "GitHub", "Chrome", "Safari", "Cursor", "VS Code", "Finder",
    "Notion", "Figma", "Spotify", "YouTube", "OpenRouter", "Claude", "Gmail", "Arc", "Terminal",
];

static LIVE: LazyLock<Mutex<LiveState>> = LazyLock::new(Default::default);
static TOOK_OVER: AtomicBool = AtomicBool::new(false);
/// ms of the last event susurro posted itself (backup to SUSURRO_TAG: some events lose the user data field).
static OWN_AT: AtomicI64 = AtomicI64::new(0);
/// Pablo's input within this long after one of susurro's own events is susurro's.
const OWN_MS: i64 = 300;

/// The marks window hides (Pablo took over): until the next order.
pub fn took_over() -> bool {
    TOOK_OVER.load(Ordering::SeqCst)
}

/// command.rs calls this right before every event it posts.
pub fn own_input() {
    OWN_AT.store(crate::history::now_ms(), Ordering::SeqCst);
}

/// Visible while the session runs or actions are left, unless Pablo took over (VOICE_CONTROL_V2 3).
pub fn marks_visible(on: bool, queued: usize, executing: bool, took_over: bool) -> bool {
    (on || queued > 0 || executing) && !took_over
}

/// One event from the NSEvent global monitor is Pablo's own input (VOICE_CONTROL_V2 3): not tagged by susurro, not
/// a bound hotkey (the command key itself, Esc), and a mouse move only past 6 pt in total.
/// ponytail: 6 pt is a first guess.
pub fn is_users(user_data: i64, keycode: Option<u16>, bound: &[u16], moved: f64) -> bool {
    if user_data == crate::paste::SUSURRO_TAG {
        return false;
    }
    match keycode {
        Some(k) => !bound.contains(&k),
        None => moved >= 6.0,
    }
}

/// The input monitor saw Pablo's own event.
pub fn user_input(app: &AppHandle) {
    if crate::history::now_ms() - OWN_AT.load(Ordering::SeqCst) < OWN_MS || TOOK_OVER.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!("live: Pablo took over, marks hidden");
    #[cfg(target_os = "macos")]
    crate::platform::macos::show_marks(app, None);
    update(app, |l| l.took_over = true);
}

fn update(app: &AppHandle, f: impl FnOnce(&mut LiveState)) {
    let s = {
        let mut l = LIVE.lock().unwrap();
        f(&mut l);
        l.clone()
    };
    let _ = app.emit("command_live", s);
}

/// command.rs `Real::emit`: a confirm or pick question goes to the pill; any other status clears it.
pub fn step_seen(app: &AppHandle, s: &CommandStep) {
    let q = matches!(s.status, CmdStatus::Confirm | CmdStatus::Pick).then(|| s.message.clone()).flatten();
    if LIVE.lock().unwrap().pending != q {
        update(app, |l| l.pending = q);
    }
}

/// The VoiceControl screen's memory list, oldest first.
#[tauri::command]
pub fn command_memory() -> Vec<MemoryEntry> {
    MEMORY.lock().unwrap().iter().map(|e| e.e.clone()).collect()
}

/// The VoiceControl screen's "Clear": forget what susurro did (the next "remove what you typed" finds nothing).
#[tauri::command]
pub fn command_memory_clear() {
    MEMORY.lock().unwrap().clear();
}

/// The planner's memory lines: "21:10:25 typed 'ls' into Ghostty TextArea", the last 10 minutes, no Said lines.
pub fn memory_lines(mem: &Memory, now_ms: i64) -> Vec<String> {
    mem.iter()
        .filter(|e| now_ms - e.e.at < MEMORY_FOR_PLANNER_MS && e.e.kind != MemKind::Said)
        .map(|e| {
            let t = chrono::DateTime::from_timestamp_millis(e.e.at).map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()).unwrap_or_default();
            let what = match e.e.kind {
                MemKind::Typed => "typed",
                MemKind::Deleted => "typed then deleted",
                MemKind::Copied => "copied",
                MemKind::Pasted => "pasted",
                MemKind::Clicked => "clicked",
                MemKind::Key => "pressed",
                MemKind::Opened => "opened",
                MemKind::Failed => "failed",
                MemKind::Said => "heard",
            };
            let field = if e.e.field.is_empty() { String::new() } else { format!(" {}", e.e.field) };
            format!("{t} {what} '{}' in {}{field}", e.e.text, e.e.app)
        })
        .collect()
}

// ---------- the live core (pure: re-order, buffer, local answers, planner) ----------

/// What one heard text leads to.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    Nothing,
    /// A yes / no to the waiting confirm.
    Confirm(bool),
    /// The spoken answer to "Which one?" (command::parse_pick reads it).
    Pick(String),
    /// A planned order: its number, the actions, and what to say.
    Order(u32, Vec<Action>, Option<String>),
    Error(String),
}

#[derive(Debug, Default)]
pub struct Core {
    next: u32,
    ready: BTreeMap<u32, Option<String>>,
    /// Words heard, not planned yet.
    pub buffer: String,
    pub order: u32,
    pub said: Vec<String>,
}

pub type PlanFn<'a> = dyn FnMut(&str, bool, Option<&str>) -> anyhow::Result<Plan> + 'a;

impl Core {
    /// A chunk text arrived (None = its STT failed). Returns the texts now in seq order.
    pub fn arrive(&mut self, seq: u32, text: Option<String>) -> Vec<Option<String>> {
        self.ready.insert(seq, text);
        let mut out = vec![];
        while let Some(t) = self.ready.remove(&self.next) {
            out.push(t);
            self.next += 1;
        }
        out
    }

    /// One text, in order. `last` = the session ended (plan what is left). `pending` = what the executor waits for,
    /// with its question.
    pub fn hear(&mut self, text: &str, last: bool, pending: Option<(Wait, &str)>, plan: &mut PlanFn) -> Heard {
        let text = text.trim();
        if !text.is_empty() {
            self.said.push(text.into());
        }
        match pending {
            Some((Wait::Pick, _)) if !text.is_empty() => return Heard::Pick(text.into()),
            Some((Wait::Confirm, q)) if !text.is_empty() => {
                if let Some(yes) = command::yes_no(text) {
                    return Heard::Confirm(yes);
                }
                // An odd phrasing ("go ahead and send it"): the planner reads it against the question.
                return match plan(text, false, Some(q)) {
                    Ok(p) => match p.answer.as_deref().map(str::to_lowercase).as_deref() {
                        Some("yes" | "sí" | "si") => Heard::Confirm(true),
                        Some("no") => Heard::Confirm(false),
                        _ if p.actions.is_empty() => Heard::Nothing,
                        _ => self.order(p.actions, p.say),
                    },
                    Err(e) => Heard::Error(format!("{e:#}")),
                };
            }
            _ => {}
        }
        if !text.is_empty() {
            self.buffer = format!("{} {text}", self.buffer).trim().to_string();
        }
        if self.buffer.is_empty() {
            return Heard::Nothing;
        }
        match plan(&self.buffer, last, None) {
            Ok(p) if !last && hold(&p, &self.buffer) => {
                log::info!("live: holding {:?} for the next words", self.buffer);
                Heard::Nothing
            }
            Ok(p) => {
                if p.complete || last {
                    self.buffer.clear();
                } else if !p.rest.trim().is_empty() {
                    self.buffer = p.rest.trim().to_string();
                } else if !p.actions.is_empty() {
                    // ponytail: incomplete with no rest = the model planned all but kept nothing: drop the buffer.
                    self.buffer.clear();
                }
                if p.actions.is_empty() { Heard::Nothing } else { self.order(p.actions, p.say) }
            }
            Err(e) => {
                if last {
                    self.buffer.clear();
                }
                Heard::Error(format!("{e:#}"))
            }
        }
    }

    fn order(&mut self, actions: Vec<Action>, say: Option<String>) -> Heard {
        self.order += 1;
        Heard::Order(self.order, actions, say)
    }
}

/// Run nothing yet, keep the whole buffer for the next chunk (or the final flush):
/// 1. The plan ends in a `type` with no target: the next words may name the field ("type hello" | "onto the search
///    input"). Measured on gpt-oss-120b: it plans "Type hello" as complete and types into the wrong field (case 5).
/// 2. The plan keeps the whole buffer as `rest` but has actions: the next replan would run them again.
/// ponytail: the held order waits one chunk (or the release); add a silence timer if hands-free feels slow.
pub fn hold(p: &Plan, buffer: &str) -> bool {
    let trailing_type = p.actions.last().is_some_and(|a| a.op == "type" && a.target.as_deref().is_none_or(|t| t.trim().is_empty()));
    let rest_is_all = !p.complete && !p.actions.is_empty() && p.rest.trim() == buffer.trim();
    trailing_type || rest_is_all
}

// ---------- the queue between cmd-live and cmd-exec ----------

enum Item {
    Do { order: u32, said: String, action: Action },
    End { instruction: String },
}

#[derive(Default)]
struct Queue {
    items: Mutex<VecDeque<Item>>,
    cv: Condvar,
}

impl Queue {
    fn push(&self, i: Item) {
        self.items.lock().unwrap().push_back(i);
        self.cv.notify_one();
    }

    fn pop(&self) -> Item {
        let mut q = self.items.lock().unwrap();
        loop {
            if let Some(i) = q.pop_front() {
                return i;
            }
            q = self.cv.wait(q).unwrap();
        }
    }

    /// Esc: drop every queued action (the End stays, so the session still closes).
    fn clear(&self) {
        self.items.lock().unwrap().retain(|i| matches!(i, Item::End { .. }));
    }

    fn labels(&self) -> Vec<String> {
        self.items.lock().unwrap().iter().filter_map(|i| if let Item::Do { action, .. } = i { Some(planner::label(action)) } else { None }).collect()
    }
}

// ---------- the session threads ----------

enum Msg {
    Chunk(Vec<f32>),
    Text(u32, anyhow::Result<Option<String>>),
    End(Option<Vec<f32>>),
    Abort,
}

/// session.rs holds this while a live session records.
pub struct Handle {
    tx: mpsc::Sender<Msg>,
}

impl Handle {
    /// A finished chunk of 16 kHz audio.
    pub fn chunk(&self, audio: Vec<f32>) {
        let _ = self.tx.send(Msg::Chunk(audio));
    }

    /// The recording stopped: the tail after the last cut, then drain the queue and end.
    pub fn end(&self, tail: Option<Vec<f32>>) {
        let _ = self.tx.send(Msg::End(tail));
    }

    /// Esc: drop the queue and end now.
    pub fn abort(&self) {
        command::abort();
        let _ = self.tx.send(Msg::Abort);
    }
}

/// Starts the live and exec threads for one session. `key` = the OpenRouter key (planner, MAI, Jev).
pub fn start(app: AppHandle, key: String, cfg: CommandSettings) -> Handle {
    command::reset();
    command::clear_log();
    TOOK_OVER.store(false, Ordering::SeqCst);
    let local = app.state::<AppState>().settings.read().unwrap().stt.provider == crate::contract::SttProvider::WhisperLocal;
    update(&app, |l| *l = LiveState { on: true, ..Default::default() });
    #[cfg(target_os = "macos")]
    crate::platform::macos::watch_user_input(&app, true);
    let (tx, rx) = mpsc::channel();
    let queue = Arc::new(Queue::default());
    {
        let (app, queue, key, cfg) = (app.clone(), queue.clone(), key.clone(), cfg.clone());
        std::thread::Builder::new().name("cmd-exec".into()).spawn(move || exec_loop(app, queue, key, cfg)).expect("spawn cmd-exec");
    }
    let tx2 = tx.clone();
    std::thread::Builder::new()
        .name("cmd-live".into())
        .spawn(move || live_loop(app, rx, tx2, queue, key, cfg, if local { 1 } else { MAX_IN_FLIGHT }))
        .expect("spawn cmd-live");
    Handle { tx }
}

#[allow(clippy::too_many_arguments)]
fn live_loop(app: AppHandle, rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<Msg>, queue: Arc<Queue>, key: String, cfg: CommandSettings, max_in_flight: usize) {
    let t0 = Instant::now();
    let mut core = Core::default();
    let (mut seq, mut in_flight, mut ended) = (0u32, 0usize, false);
    let mut waiting: VecDeque<(u32, Vec<f32>)> = VecDeque::new();
    let mut planner = |text: &str, last: bool, pending: Option<&str>| -> anyhow::Result<Plan> {
        let mut input = context(&app);
        (input.utterance, input.last) = (text.to_string(), last);
        input.pending = pending.map(String::from);
        let (p, m) = planner::plan_openrouter(&key, &cfg.planner_model, &input)?;
        log::info!("live: planner {} ms ${:.5} {text:?} -> complete={} {:?} rest={:?}", m.latency_ms, m.cost, p.complete, p.actions.iter().map(planner::label).collect::<Vec<_>>(), p.rest);
        Ok(p)
    };
    loop {
        let Ok(msg) = rx.recv() else { break };
        match msg {
            Msg::Chunk(a) => {
                waiting.push_back((seq, a));
                seq += 1;
            }
            Msg::End(tail) => {
                if let Some(t) = tail {
                    waiting.push_back((seq, t));
                    seq += 1;
                }
                ended = true;
            }
            Msg::Abort => {
                queue.clear();
                queue.push(Item::End { instruction: core.said.join(" ") });
                break;
            }
            Msg::Text(n, r) => {
                in_flight -= 1;
                let text = match r {
                    Ok(t) => t,
                    Err(e) => {
                        log::warn!("live: chunk {n} STT failed: {e:#}");
                        update(&app, |l| l.last_said = "(missed a bit)".into());
                        None
                    }
                };
                for t in core.arrive(n, text) {
                    let Some(t) = t else { continue };
                    update(&app, |l| l.last_said = t.clone());
                    let pending = command::waiting().map(|w| (w, LIVE.lock().unwrap().pending.clone().unwrap_or_default()));
                    let heard = core.hear(&t, false, pending.as_ref().map(|(w, q)| (*w, q.as_str())), &mut planner);
                    handle(&app, &queue, &core, heard);
                }
            }
        }
        while in_flight < max_in_flight {
            let Some((n, audio)) = waiting.pop_front() else { break };
            in_flight += 1;
            let (app, tx) = (app.clone(), tx.clone());
            std::thread::Builder::new()
                .name("cmd-stt".into())
                .spawn(move || {
                    let t = Instant::now();
                    let r = chunk_text(&app, audio);
                    log::info!("live: chunk {n} STT {} ms: {:?}", t.elapsed().as_millis(), r.as_ref().ok());
                    let _ = tx.send(Msg::Text(n, r));
                })
                .expect("spawn cmd-stt");
        }
        if ended && in_flight == 0 && waiting.is_empty() {
            if !core.buffer.is_empty() && !command::aborted() {
                let heard = core.hear("", true, None, &mut planner);
                handle(&app, &queue, &core, heard);
            }
            queue.push(Item::End { instruction: core.said.join(" ") });
            break;
        }
    }
    update(&app, |l| (l.on, l.buffer) = (false, String::new()));
    log::info!("live: recording side done after {} ms, {} orders", t0.elapsed().as_millis(), core.order);
}

fn handle(app: &AppHandle, queue: &Queue, core: &Core, h: Heard) {
    match h {
        Heard::Nothing => {}
        Heard::Confirm(true) => command::confirm(),
        Heard::Confirm(false) => command::deny(),
        Heard::Pick(t) => command::answer(t),
        Heard::Order(order, actions, say) => {
            let said = core.said.last().cloned().unwrap_or_default();
            for a in actions {
                queue.push(Item::Do { order, said: said.clone(), action: a });
            }
            // The next order shows the marks again (case 13).
            TOOK_OVER.store(false, Ordering::SeqCst);
            if let Some(s) = say {
                crate::session::toast(app, Some("overlay"), "live-say", crate::contract::ToastKind::Info, s, &[], 4000);
            }
        }
        Heard::Error(e) => {
            log::warn!("live: planner failed: {e}");
            crate::session::toast(app, Some("overlay"), "live-planner", crate::contract::ToastKind::Error, format!("Voice control: {e}"), &[], 6000);
        }
    }
    let labels = queue.labels();
    let buffer = core.buffer.clone();
    update(app, |l| (l.queue, l.buffer, l.took_over) = (labels, buffer, took_over()));
}

fn exec_loop(app: AppHandle, queue: Arc<Queue>, key: String, settings: CommandSettings) {
    let t0 = Instant::now();
    let cfg = Cfg { confirm_risky: settings.confirm_risky };
    let mut ask = |s: &serde_json::Value, q: &serde_json::Value| crate::jev::ask(crate::jev::DECISIONS_URL, &key, &settings.model, s, q);
    let (mut step, mut summary, mut worst, mut message) = (0u32, vec![], CmdStatus::Done, None);
    let mut skip: Option<u32> = None;
    let mut last_order = 0;
    #[cfg(target_os = "macos")]
    let mut desk = command::mac::Real::new(app.clone());
    let instruction = loop {
        match queue.pop() {
            Item::End { instruction } => break instruction,
            Item::Do { order, said, action } => {
                if command::aborted() || skip == Some(order) {
                    continue;
                }
                step += 1;
                let mut mem = MEMORY.lock().unwrap().clone();
                if order != last_order {
                    last_order = order;
                    let f = crate::ax::Field { app: crate::paste::target_app().and_then(|a| a.name).unwrap_or_default(), ..Default::default() };
                    command::remember(&mut mem, MemKind::Said, &said, &f, None);
                }
                let labels = queue.labels();
                update(&app, |l| l.queue = labels);
                #[cfg(target_os = "macos")]
                let end = command::exec(&action, step, order, &cfg, &mut ask, &mut desk, &mut mem);
                #[cfg(not(target_os = "macos"))]
                let end = {
                    let _ = (&cfg, &mut ask, &action);
                    End { status: CmdStatus::Error, summary: String::new(), message: Some("macOS only".into()), steps: step }
                };
                log::info!("live: step {step} (order {order}) {}: {:?} {} {:?}", planner::label(&action), end.status, end.summary, end.message);
                if end.status != CmdStatus::Done {
                    // The rest of this order depends on this action.
                    skip = Some(order);
                    (worst, message) = (end.status.clone(), end.message.clone());
                    command::remember(&mut mem, MemKind::Failed, &planner::label(&action), &crate::ax::Field::default(), None);
                } else {
                    summary.push(end.summary);
                }
                *MEMORY.lock().unwrap() = mem;
                let queued = queue.labels().len();
                #[cfg(target_os = "macos")]
                if !marks_visible(LIVE.lock().unwrap().on, queued, false, took_over()) {
                    crate::platform::macos::show_marks(&app, None);
                }
                let _ = queued;
            }
        }
    };
    if command::aborted() {
        worst = CmdStatus::Stopped;
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::show_marks(&app, None);
        crate::platform::macos::watch_user_input(&app, false);
    }
    update(&app, |l| *l = LiveState::default());
    let end = End { status: if summary.is_empty() && worst == CmdStatus::Done { CmdStatus::Unsure } else { worst }, summary: summary.join("; "), message, steps: step };
    let end = if step == 0 && instruction.trim().is_empty() { End { status: CmdStatus::Stopped, ..end } } else { end };
    let ended = Ended { instruction, end, latency_ms: t0.elapsed().as_millis() as u32 };
    let _ = app.state::<AppState>().session_tx.send(crate::session::SessionCmd::CommandEnded(Box::new(ended)));
}

/// The planner's view right before the call: app, focused field, marks (ids dropped).
fn context(app: &AppHandle) -> planner::Input {
    let _ = app;
    let t = Instant::now();
    let mut input = planner::Input { memory: memory_lines(&MEMORY.lock().unwrap(), crate::history::now_ms()), ..Default::default() };
    #[cfg(target_os = "macos")]
    if crate::permissions::accessibility_trusted() {
        if let Some(front) = crate::paste::target_app() {
            input.app = front.name.clone().unwrap_or_default();
            if let Some((mut f, _)) = crate::ax::mac::focused() {
                (f.app, f.bundle) = (input.app.clone(), front.bundle_id.clone().unwrap_or_default());
                input.focused = f;
            }
            if let Some(tree) = crate::ax::mac::read(front.pid) {
                input.marks = crate::ax::collect(&tree.root, tree.window).iter().map(crate::ax::describe).collect();
            }
        }
    }
    log::info!("live: context read {} ms, {} marks", t.elapsed().as_millis(), input.marks.len());
    input
}

/// The command STT hints: Pablo's dictionary first, then BRANDS (vc-feedback 18a).
pub fn with_brands(h: &crate::dictionary::Hints) -> crate::dictionary::Hints {
    let mut h = h.clone();
    let extra: Vec<String> = BRANDS.iter().map(|b| b.to_string()).filter(|b| !h.terms.contains(b)).collect();
    h.terms.extend(extra);
    h
}

/// One chunk through the command STT: the pipeline (hallucination filter, dictionary corrections), no cleanup LLM,
/// plus BRANDS as extra hint words. Ok(None) = nothing heard.
fn chunk_text(app: &AppHandle, audio: Vec<f32>) -> anyhow::Result<Option<String>> {
    use crate::pipeline::{self, Outcome};
    let state = app.state::<AppState>();
    let s = state.settings.read().unwrap().clone();
    let d = crate::dictionary::Dictionary::new(crate::dictionary::load(&state.db.lock().unwrap()).unwrap_or_default(), s.dictionary.clone());
    let peak = audio.iter().fold(0f32, |m, x| m.max(x.abs()));
    let clip = crate::audio::Clip { duration_ms: (audio.len() / 16) as u32, peak_db: crate::audio::db(peak), samples: audio, device_names: vec![] };
    let stt = crate::stt::build(&s, &state)?;
    let stt_fn = |r: &crate::stt::SttRequest| stt.transcribe(&crate::stt::SttRequest { audio: r.audio, language: r.language, hints: &with_brands(r.hints) });
    let opts = pipeline::Opts { language: Some(s.language.as_str()), style: s.styles.style.clone(), raw_app: true, press_enter_command: false };
    match pipeline::run(&clip, &opts, &d, &stt_fn, None) {
        Outcome::Text(d) => Ok(Some(d.final_text)),
        Outcome::Skip(_) => Ok(None),
        Outcome::Failed(e) => Err(anyhow::anyhow!(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::MemoryEntry;

    fn a(op: &str) -> Action {
        Action { op: op.into(), ..Default::default() }
    }

    fn plan(complete: bool, rest: &str, ops: &[&str]) -> Plan {
        Plan { complete, rest: rest.into(), actions: ops.iter().map(|o| a(o)).collect(), ..Default::default() }
    }

    #[test]
    fn marks_visible_table() {
        // (session on, queued, executing, took over) -> visible
        let rows = [
            (true, 0, false, false, true),
            (true, 0, false, true, false),
            (false, 2, false, false, true),
            (false, 0, true, false, true),
            (false, 0, false, false, false),
            (false, 3, true, true, false),
        ];
        for (on, q, ex, took, want) in rows {
            assert_eq!(marks_visible(on, q, ex, took), want, "{on} {q} {ex} {took}");
        }
    }

    #[test]
    fn is_users_table() {
        let bound = [103u16, 53]; // F11, Esc
        assert!(!is_users(crate::paste::SUSURRO_TAG, Some(0), &bound, 0.0), "susurro's own key");
        assert!(!is_users(0, Some(103), &bound, 0.0), "the command key itself");
        assert!(!is_users(0, Some(53), &bound, 0.0), "Esc");
        assert!(is_users(0, Some(0), &bound, 0.0), "Pablo types an 'a'");
        assert!(!is_users(0, None, &bound, 3.0), "3 pt trackpad noise");
        assert!(is_users(0, None, &bound, 20.0), "a real move");
        assert!(!is_users(crate::paste::SUSURRO_TAG, None, &bound, 50.0), "susurro's own click");
    }

    #[test]
    fn chunks_reorder_by_seq() {
        let mut c = Core::default();
        assert!(c.arrive(1, Some("b".into())).is_empty(), "chunk 1 waits for chunk 0");
        assert!(c.arrive(2, None).is_empty());
        assert_eq!(c.arrive(0, Some("a".into())), [Some("a".into()), Some("b".into()), None]);
        assert_eq!(c.arrive(3, Some("d".into())), [Some("d".into())]);
    }

    #[test]
    fn actions_run_while_chunks_still_arrive() {
        // case 10: order 1 is queued from chunk 1 before chunk 3 is planned; a cut order waits in the buffer
        let mut c = Core::default();
        let mut seen = vec![];
        let mut p = |t: &str, last: bool, _: Option<&str>| {
            seen.push((t.to_string(), last));
            Ok(match t {
                "open ghostty" => plan(true, "", &["open_app"]),
                "create a new tab and" => plan(false, "create a new tab and", &[]),
                "create a new tab and run ls" => plan(true, "", &["key", "type", "key"]),
                _ => plan(true, "", &[]),
            })
        };
        assert!(matches!(c.hear("open ghostty", false, None, &mut p), Heard::Order(1, ref v, None) if v.len() == 1));
        assert_eq!(c.hear("create a new tab and", false, None, &mut p), Heard::Nothing);
        assert_eq!(c.buffer, "create a new tab and", "partial_newtab: rest kept");
        assert!(matches!(c.hear("run ls", false, None, &mut p), Heard::Order(2, ref v, None) if v.len() == 3));
        assert_eq!(c.buffer, "");
        assert_eq!(seen.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["open ghostty", "create a new tab and", "create a new tab and run ls"]);
    }

    #[test]
    fn a_trailing_type_waits_for_its_target_and_nothing_runs_twice() {
        // case 5 streamed (planner_stream_cases, real gpt-oss-120b): "Type hello" | "onto the search input."
        let mut c = Core::default();
        let ty = |target: Option<&str>| Action { op: "type".into(), text: Some("hello".into()), target: target.map(Into::into), ..Default::default() };
        let mut p = |t: &str, _: bool, _: Option<&str>| {
            Ok(match t {
                "Type hello" => Plan { complete: true, actions: vec![ty(None)], ..Default::default() },
                _ => Plan { complete: true, actions: vec![ty(Some("Search (search field, toolbar)"))], ..Default::default() },
            })
        };
        assert_eq!(c.hear("Type hello", false, None, &mut p), Heard::Nothing, "held: the target may come next");
        assert_eq!(c.buffer, "Type hello");
        let Heard::Order(1, a, _) = c.hear("onto the search input.", false, None, &mut p) else { panic!("one order") };
        assert_eq!((a.len(), a[0].text.as_deref(), a[0].target.is_some()), (1, Some("hello"), true));
        // the release flushes a held type
        let mut c = Core::default();
        let mut p = |_: &str, _: bool, _: Option<&str>| Ok(Plan { complete: true, actions: vec![ty(None)], ..Default::default() });
        assert_eq!(c.hear("type ls", false, None, &mut p), Heard::Nothing);
        assert!(matches!(c.hear("", true, None, &mut p), Heard::Order(1, ..)));
        // a plan that keeps the whole buffer as rest would run its actions again on the next chunk: held
        let mut c = Core::default();
        let mut runs = 0;
        let mut p = |t: &str, last: bool, _: Option<&str>| {
            Ok(if last { plan(true, "", &["key"]) } else { plan(false, t, &["key"]) })
        };
        for chunk in ["create a new tab", "and"] {
            if matches!(c.hear(chunk, false, None, &mut p), Heard::Order(..)) {
                runs += 1;
            }
        }
        assert!(matches!(c.hear("", true, None, &mut p), Heard::Order(..)));
        assert_eq!(runs, 0, "nothing ran before the final flush");
        assert!(!hold(&plan(false, "and", &["key"]), "create a new tab and"), "a real rest: the leading order runs");
    }

    #[test]
    fn final_flush_plans_the_rest() {
        let mut c = Core::default();
        let mut p = |t: &str, last: bool, _: Option<&str>| Ok(if last { plan(true, "", &["open_app"]) } else { plan(false, t, &[]) });
        assert_eq!(c.hear("open", false, None, &mut p), Heard::Nothing);
        assert!(matches!(c.hear("", true, None, &mut p), Heard::Order(1, ..)));
        assert_eq!(c.buffer, "");
        // a failed planner at the end drops the buffer, mid-session keeps it
        let mut bad = |_: &str, _: bool, _: Option<&str>| -> anyhow::Result<Plan> { anyhow::bail!("Planner timed out") };
        c.buffer = "x".into();
        assert!(matches!(c.hear("y", false, None, &mut bad), Heard::Error(_)));
        assert_eq!(c.buffer, "x y");
        assert!(matches!(c.hear("", true, None, &mut bad), Heard::Error(_)));
        assert_eq!(c.buffer, "");
    }

    #[test]
    fn voice_answers_resolve_locally_first() {
        let mut c = Core::default();
        let calls = std::cell::Cell::new(0);
        let mut p = |_: &str, _: bool, q: Option<&str>| {
            calls.set(calls.get() + 1);
            assert_eq!(q, Some("Press Return?"));
            Ok(Plan { complete: true, answer: Some("yes".into()), ..Default::default() })
        };
        assert_eq!(c.hear("Sí.", false, Some((Wait::Confirm, "Press Return?")), &mut p), Heard::Confirm(true));
        assert_eq!(c.hear("no", false, Some((Wait::Confirm, "Press Return?")), &mut p), Heard::Confirm(false));
        assert_eq!(c.hear("go ahead and do it", false, Some((Wait::Confirm, "Press Return?")), &mut p), Heard::Confirm(true), "planner fallback");
        assert_eq!(calls.get(), 1);
        assert_eq!(c.hear("la dos", false, Some((Wait::Pick, "Which one?")), &mut p), Heard::Pick("la dos".into()));
        assert_eq!(c.buffer, "", "answers never enter the order buffer");
    }

    #[test]
    fn command_stt_hints_keep_the_dictionary_and_add_brands() {
        // case 18a: "ElevenLabs" was heard as "the 11 tabs"
        let d = crate::dictionary::Hints { terms: vec!["Contaflow".into(), "Slack".into()], ..Default::default() };
        let h = with_brands(&d);
        assert_eq!(&h.terms[..2], ["Contaflow", "Slack"], "the dictionary stays first");
        assert!(h.terms.contains(&"ElevenLabs".to_string()));
        assert_eq!(h.terms.iter().filter(|t| *t == "Slack").count(), 1, "no duplicates");
        let body = crate::stt::openrouter::body("microsoft/mai-transcribe-2", b"RIFF", Some("en"), &h.terms, true).to_string();
        assert!(body.contains("phraseList") && body.contains("ElevenLabs"), "MAI gets the phrase list");
    }

    #[test]
    fn memory_lines_skip_old_and_said() {
        let e = |at, kind, text: &str| command::Entry { e: MemoryEntry { at, kind, text: text.into(), app: "Ghostty".into(), field: "TextArea".into() }, pid: 1, role: String::new(), value_after: None };
        let now = 1_760_000_000_000;
        let mem: Memory = [e(now - 11 * 60_000, MemKind::Typed, "old"), e(now - 1000, MemKind::Said, "type ls"), e(now - 500, MemKind::Typed, "ls")].into();
        let l = memory_lines(&mem, now);
        assert_eq!(l.len(), 1);
        assert!(l[0].ends_with(" typed 'ls' in Ghostty TextArea"), "{}", l[0]);
    }
}
