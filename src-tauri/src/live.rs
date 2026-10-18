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
use crate::stt::scribe_rt;
use std::time::{Duration, Instant};
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
/// Pablo pressed a key or clicked: the executor waits until his next spoken order (open risk a).
static PAUSED: AtomicBool = AtomicBool::new(false);
/// ms of the last event susurro posted itself (backup to SUSURRO_TAG: some events lose the user data field).
static OWN_AT: AtomicI64 = AtomicI64::new(0);
/// Pablo's input within this long after one of susurro's own events is susurro's.
const OWN_MS: i64 = 300;

/// The marks drawn on screen now (their numbers are what Pablo says). Set by every draw.
static BOARD: Mutex<Vec<crate::ax::El>> = Mutex::new(Vec::new());
/// The executor runs a step: only its own reads draw the marks (a pick list must not change under Pablo).
static BUSY: AtomicBool = AtomicBool::new(false);
/// Steps started; a late board refresh checks that no new step began.
static STEPS: AtomicI64 = AtomicI64::new(0);

pub fn board() -> Vec<crate::ax::El> {
    BOARD.lock().unwrap().clone()
}

pub fn set_board(els: Vec<crate::ax::El>) {
    *BOARD.lock().unwrap() = els;
}

/// Draws a fresh read as the marks board (step 0 = board only, Marks.tsx), so the numbers on screen are the ones
/// the planner and the next step see (live log 2026-10-18: Pablo read "41" off a board from before a page load).
#[cfg(target_os = "macos")]
fn draw_board(app: &AppHandle, els: &[crate::ax::El], window: crate::contract::PtRect) {
    if els.is_empty() || took_over() || command::waiting().is_some() {
        return;
    }
    let (display, _) = command::display_for(command::center(window), &command::mac::displays()).unwrap_or((window, 1.0));
    crate::platform::macos::show_marks(app, Some(display));
    set_board(els.to_vec());
    let _ = app.emit("command_step", CommandStep { status: CmdStatus::Marks, display, marks: els.iter().map(|e| e.mark.clone()).collect(), ..Default::default() });
}

/// Re-read the front app and draw it (after a step, when nothing else is queued).
#[cfg(target_os = "macos")]
fn refresh_board(app: &AppHandle) {
    if !crate::permissions::accessibility_trusted() {
        return;
    }
    let Some(front) = crate::paste::target_app() else { return };
    if let Some(tree) = crate::ax::mac::read(front.pid) {
        draw_board(app, &crate::ax::collect(&tree.root, tree.window), tree.window);
    }
}

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

/// The queue waits for Pablo's next order.
pub fn paused() -> bool {
    PAUSED.load(Ordering::SeqCst)
}

/// The input monitor saw Pablo's own event. `hard` = a key or a click (not a mouse move or scroll): the queue
/// pauses too, so susurro never types over what he does by hand.
/// `click` = where a left click landed (global points): a click on a pick choice answers the pick instead.
pub fn user_input(app: &AppHandle, hard: bool, click: Option<(f64, f64)>) {
    if crate::history::now_ms() - OWN_AT.load(Ordering::SeqCst) < OWN_MS {
        return;
    }
    if click.is_some_and(command::user_clicked) {
        log::info!("live: Pablo clicked a pick choice himself");
        return;
    }
    let pause = hard && !PAUSED.swap(true, Ordering::SeqCst);
    if pause {
        log::info!("live: Pablo pressed a key or clicked, the queue pauses until his next order");
    }
    let hide = !TOOK_OVER.swap(true, Ordering::SeqCst);
    if hide {
        log::info!("live: Pablo took over, marks hidden");
        #[cfg(target_os = "macos")]
        crate::platform::macos::show_marks(app, None);
    }
    if pause || hide {
        update(app, |l| (l.took_over, l.paused) = (true, paused()));
    }
}

/// "continue" and friends: resume a paused queue without a new order.
pub fn resume_word(text: &str) -> bool {
    const W: [&str; 10] = ["continue", "continúa", "continua", "sigue", "go on", "resume", "keep going", "dale", "seguí", "sigue nomás"];
    let t = text.trim().trim_end_matches(['.', '!', ',']).trim().to_lowercase();
    W.contains(&t.as_str())
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
    /// The order held for the next words (`hold`), with the buffer it was planned from. A pause runs it.
    held: Option<(String, Plan)>,
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
            // Only a number, an ordinal or a shown name answers "Which one?"; a full new order is planned (open risk c).
            Some((Wait::Pick, q)) if !text.is_empty() && command::looks_like_pick(text, q) => return Heard::Pick(text.into()),
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
        if text.is_empty() && !last {
            // A pause (a silent chunk, or no new words for HOLD_MS): the held order runs; nothing else to plan.
            return self.run_held();
        }
        if !text.is_empty() {
            self.buffer = format!("{} {text}", self.buffer).trim().to_string();
        }
        if self.buffer.is_empty() {
            return Heard::Nothing;
        }
        // The release with no new words since the hold: the held plan is the final plan.
        if last && self.held.as_ref().is_some_and(|(b, _)| *b == self.buffer) {
            return self.run_held();
        }
        match plan(&self.buffer, last, None) {
            Ok(p) if !last && hold(&p, &self.buffer) => {
                log::info!("live: holding {:?} for the next words", self.buffer);
                self.held = Some((self.buffer.clone(), p));
                Heard::Nothing
            }
            Ok(p) => {
                let held = self.held.take();
                if p.complete || last {
                    self.buffer.clear();
                } else if !p.rest.trim().is_empty() {
                    self.buffer = p.rest.trim().to_string();
                } else if !p.actions.is_empty() {
                    // ponytail: incomplete with no rest = the model planned all but kept nothing: drop the buffer.
                    self.buffer.clear();
                }
                if !p.actions.is_empty() {
                    return self.order(p.actions, p.say);
                }
                match held {
                    // The replan kept the held words in its rest (the target may still come): hold on.
                    Some((b, hp)) if !last && self.buffer.to_lowercase().contains(&b.to_lowercase()) => {
                        self.held = Some((b, hp));
                        Heard::Nothing
                    }
                    // The replan dropped the held words with nothing to run (log 2026-10-17: "De-" -> rest "De-"):
                    // the held order runs, the new words stay in the buffer.
                    Some((_, hp)) => {
                        log::info!("live: the replan dropped the held order, it runs now");
                        self.order(hp.actions, hp.say)
                    }
                    None => Heard::Nothing,
                }
            }
            Err(e) => {
                if last {
                    self.buffer.clear();
                    if let Some((_, hp)) = self.held.take() {
                        return self.order(hp.actions, hp.say);
                    }
                }
                Heard::Error(format!("{e:#}"))
            }
        }
    }

    /// The held order runs now (a pause or the release); its words leave the buffer.
    fn run_held(&mut self) -> Heard {
        let Some((b, p)) = self.held.take() else { return Heard::Nothing };
        log::info!("live: pause, the held order runs: {b:?}");
        self.buffer = self.buffer.strip_prefix(b.as_str()).unwrap_or("").trim().to_string();
        self.order(p.actions, p.say)
    }

    /// An order waits for the next words (`hold`).
    pub fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// The held order will not run (Pablo moved to another app): its words leave the buffer, the new ones stay.
    pub fn drop_held(&mut self) {
        let Some((b, _)) = self.held.take() else { return };
        log::info!("live: the front app changed, the held order is dropped: {b:?}");
        self.buffer = self.buffer.strip_prefix(b.as_str()).unwrap_or("").trim().to_string();
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
/// No hold for 1 when there is no other text input to name (`type_here`: a terminal, a canvas app, the only field).
/// The held order runs at the next pause: a silent chunk, HOLD_MS with no new words, or the release.
pub fn hold(p: &Plan, buffer: &str) -> bool {
    let trailing_type = !p.type_here && p.actions.last().is_some_and(|a| a.op == "type" && a.target.as_deref().is_none_or(|t| t.trim().is_empty()));
    let rest_is_all = !p.complete && !p.actions.is_empty() && p.rest.trim() == buffer.trim();
    trailing_type || rest_is_all
}

/// No new words for this long = a pause: a held order runs. ponytail: 800 ms is a first guess (Pablo's ask).
const HOLD_MS: u64 = 800;

/// An untargeted `type` means "type here": the screen has no text input besides the focused one (Ghostty and
/// Claude Code are one AXTextArea; a canvas app has none). `inputs` = text-input marks on screen.
pub fn type_here(focused_role: &str, inputs: usize) -> bool {
    let focused_text = matches!(focused_role, "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField");
    inputs.saturating_sub(usize::from(focused_text)) == 0
}

// ---------- Scribe Realtime bookkeeping (pure) ----------

/// Scribe Realtime rejects a commit with under 0.3 s of uncommitted audio (`commit_throttled`), and every replay of
/// that segment fails again (log 2026-10-17: 3 reconnects, 6 s lost, then the batch fallback).
const RT_MIN_COMMIT_MS: usize = 300;

/// The device audio of each commit not answered yet, and of the open segment. A dead socket or a lost answer sends
/// it to the batch STT. Each commit takes the next seq, so the answers and a fallback's chunks keep the spoken order.
#[derive(Debug, Default)]
struct RtAudio {
    rate: u32,
    unanswered: VecDeque<(u32, Vec<f32>)>,
    open: Vec<f32>,
}

#[derive(Debug, PartialEq)]
enum RtAnswer {
    Text(u32, String),
    /// Run this audio through the batch STT.
    Batch(u32, Vec<f32>),
    Nothing,
}

impl RtAudio {
    fn min(&self) -> usize {
        self.rate as usize * RT_MIN_COMMIT_MS / 1000
    }

    /// New samples; `cut` = a pause cut. true = commit them now (a cut with enough uncommitted audio).
    fn audio(&mut self, a: &[f32], cut: bool, seq: &mut u32) -> bool {
        self.open.extend_from_slice(a);
        let commit = cut && self.open.len() >= self.min();
        if commit {
            self.unanswered.push_back((*seq, std::mem::take(&mut self.open)));
            *seq += 1;
        }
        commit
    }

    /// The release: the open segment is the last commit. Some(silence) = send it first, so the commit is long enough.
    fn end(&mut self, seq: &mut u32) -> Option<Vec<f32>> {
        if self.open.is_empty() {
            return None;
        }
        let pad = vec![0.0; self.min().saturating_sub(self.open.len())];
        self.open.extend_from_slice(&pad);
        self.unanswered.push_back((*seq, std::mem::take(&mut self.open)));
        *seq += 1;
        Some(pad)
    }

    /// Scribe answered the oldest commit. An empty answer for audio with speech in it goes to the batch STT at once,
    /// mid-session too (live log 2026-10-18: 9 empty commits, orders lost until the release).
    /// ponytail: if Scribe sends those words later in the next commit, they come twice; not seen in the log.
    fn committed(&mut self, t: &str) -> RtAnswer {
        let Some((n, a)) = self.unanswered.pop_front() else { return RtAnswer::Nothing };
        if t.trim().is_empty() && crate::audio::worth(&crate::audio::resample(&a, self.rate)) {
            return RtAnswer::Batch(n, a);
        }
        RtAnswer::Text(n, t.to_string())
    }

    /// The socket is gone (failed, closed early, no answer in time): everything not answered goes batch.
    fn drain(&mut self, seq: &mut u32) -> Vec<(u32, Vec<f32>)> {
        if !self.open.is_empty() {
            self.unanswered.push_back((*seq, std::mem::take(&mut self.open)));
            *seq += 1;
        }
        self.unanswered.drain(..).collect()
    }
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

    fn has_end(&self) -> bool {
        self.items.lock().unwrap().iter().any(|i| matches!(i, Item::End { .. }))
    }

    fn labels(&self) -> Vec<String> {
        self.items.lock().unwrap().iter().filter_map(|i| if let Item::Do { action, .. } = i { Some(planner::label(action)) } else { None }).collect()
    }
}

// ---------- the session threads ----------

enum Msg {
    Chunk(Vec<f32>),
    /// Scribe Realtime: new device-rate samples; `true` = a pause cut (commit).
    Audio(Vec<f32>, bool),
    Rt(scribe_rt::Ev),
    Text(u32, anyhow::Result<Option<String>>),
    End(Option<Vec<f32>>),
    Abort,
    /// No message for 100 ms (the timers run).
    Tick,
}

/// One session's stop flag and the words heard so far. Esc ends the session through it at once, while a planner,
/// STT or Jev request may still be in flight (open risk b); the late threads then see `stopped` and do nothing.
#[derive(Default)]
struct Ctl {
    stopped: AtomicBool,
    ended: AtomicBool,
    /// The Chunker hears Pablo talking (no pause yet): a held order waits.
    speaking: AtomicBool,
    said: Mutex<String>,
}

/// session.rs holds this while a live session records.
pub struct Handle {
    tx: mpsc::Sender<Msg>,
    app: AppHandle,
    queue: Arc<Queue>,
    ctl: Arc<Ctl>,
    realtime: bool,
}

impl Handle {
    /// A finished chunk of 16 kHz audio.
    pub fn chunk(&self, audio: Vec<f32>) {
        let _ = self.tx.send(Msg::Chunk(audio));
    }

    /// The command STT is Scribe Realtime: session.rs streams every new sample (`audio`) instead of chunks.
    pub fn realtime(&self) -> bool {
        self.realtime
    }

    /// New device-rate samples; `cut` = the Chunker found a pause (Scribe commits there).
    pub fn audio(&self, samples: Vec<f32>, cut: bool) {
        let _ = self.tx.send(Msg::Audio(samples, cut));
    }

    /// The Chunker's `speaking`, every poll.
    pub fn speaking(&self, on: bool) {
        self.ctl.speaking.store(on, Ordering::SeqCst);
    }

    /// The recording stopped: the tail after the last cut, then drain the queue and end.
    pub fn end(&self, tail: Option<Vec<f32>>) {
        let _ = self.tx.send(Msg::End(tail));
    }

    /// Esc: drop the queue and end now. No wait for the requests in flight.
    pub fn abort(&self) {
        command::abort();
        self.ctl.stopped.store(true, Ordering::SeqCst);
        self.queue.clear();
        let _ = self.tx.send(Msg::Abort);
        let instruction = self.ctl.said.lock().unwrap().clone();
        let end = End { status: CmdStatus::Stopped, summary: String::new(), message: None, steps: 0 };
        finish(&self.app, &self.ctl, Ended { instruction, end, latency_ms: 0 });
    }
}

/// The session end, once: marks off, the input monitor off, the pill idle, SessionCmd::CommandEnded.
fn finish(app: &AppHandle, ctl: &Ctl, ended: Ended) {
    if ctl.ended.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::show_marks(app, None);
        crate::platform::macos::watch_user_input(app, false);
    }
    update(app, |l| *l = LiveState::default());
    let _ = app.state::<AppState>().session_tx.send(crate::session::SessionCmd::CommandEnded(Box::new(ended)));
}

/// Starts the live and exec threads for one session. `key` = the OpenRouter key (planner, MAI, Jev).
/// `rate` = the mic's device rate (Scribe Realtime gets the samples as recorded).
pub fn start(app: AppHandle, key: String, cfg: CommandSettings, rate: u32) -> Handle {
    command::reset();
    command::clear_log();
    TOOK_OVER.store(false, Ordering::SeqCst);
    PAUSED.store(false, Ordering::SeqCst);
    // a number names a mark on this session's screen, never one drawn in an earlier session
    set_board(vec![]);
    let (provider, rt_opts) = {
        let st = app.state::<AppState>();
        let s = st.settings.read().unwrap().clone();
        let el = crate::secrets::get(crate::contract::KeyName::Elevenlabs).is_some();
        let p = crate::stt::command_provider(&s, el);
        let hints = crate::dictionary::Dictionary::new(crate::dictionary::load(&st.db.lock().unwrap()).unwrap_or_default(), s.dictionary.clone()).hints();
        let o = (p == crate::contract::SttProvider::ElevenLabsRealtime).then(|| crate::stt::realtime_opts(&s, rate, &with_brands(&hints), true)).flatten();
        (p, o)
    };
    log::info!("live: command STT {provider:?}{}", if rt_opts.is_some() { " (streaming)" } else { "" });
    update(&app, |l| *l = LiveState { on: true, ..Default::default() });
    #[cfg(target_os = "macos")]
    crate::platform::macos::watch_user_input(&app, true);
    let (tx, rx) = mpsc::channel();
    let queue = Arc::new(Queue::default());
    let ctl = Arc::new(Ctl::default());
    {
        let (app, queue, key, cfg, ctl) = (app.clone(), queue.clone(), key.clone(), cfg.clone(), ctl.clone());
        std::thread::Builder::new().name("cmd-exec".into()).spawn(move || exec_loop(app, queue, key, cfg, ctl)).expect("spawn cmd-exec");
    }
    let realtime = rt_opts.is_some();
    let rt = rt_opts.map(|o| {
        let tx = tx.clone();
        scribe_rt::Live::start(o, move |e| drop(tx.send(Msg::Rt(e))))
    });
    let max_in_flight = if provider == crate::contract::SttProvider::WhisperLocal { 1 } else { MAX_IN_FLIGHT };
    let (tx2, app2, queue2, ctl2) = (tx.clone(), app.clone(), queue.clone(), ctl.clone());
    std::thread::Builder::new()
        .name("cmd-live".into())
        .spawn(move || live_loop(app2, rx, tx2, queue2, key, cfg, max_in_flight, rt, rate, provider, ctl2))
        .expect("spawn cmd-live");
    Handle { tx, app, queue, ctl, realtime }
}

#[allow(clippy::too_many_arguments)]
fn live_loop(
    app: AppHandle,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    queue: Arc<Queue>,
    key: String,
    cfg: CommandSettings,
    max_in_flight: usize,
    mut rt: Option<scribe_rt::Live>,
    rate: u32,
    provider: crate::contract::SttProvider,
    ctl: Arc<Ctl>,
) {
    let t0 = Instant::now();
    let mut core = Core::default();
    let (mut seq, mut in_flight, mut ended) = (0u32, 0usize, false);
    let mut waiting: VecDeque<(u32, Vec<f32>)> = VecDeque::new();
    // Scribe Realtime: if the socket dies, the unanswered audio goes to the batch STT and so does the rest.
    let mut rta = RtAudio { rate, ..Default::default() };
    // The release (Scribe gets RT_FINISH_MS from here) and the last new words (a held order runs HOLD_MS after).
    let (mut released, mut words_at): (Option<Instant>, Instant) = (None, Instant::now());
    let mut planner = |text: &str, last: bool, pending: Option<&str>| -> anyhow::Result<Plan> {
        // "41", "la 41", "open the first link, 41": the mark Pablo reads on screen, no LLM.
        if pending.is_none() {
            let shown = board();
            let ids: Vec<u32> = shown.iter().map(|e| e.mark.id).collect();
            if let Some(e) = planner::mark_ref(text, &ids).and_then(|n| shown.iter().find(|e| e.mark.id == n)) {
                let target = format!("[{}] {}", e.mark.id, crate::ax::describe(e));
                log::info!("live: mark number {text:?} -> click {target:?}");
                return Ok(Plan { complete: true, actions: vec![Action { op: "click".into(), target: Some(target), ..Default::default() }], ..Default::default() });
            }
        }
        let (mut input, here) = context(&app);
        (input.utterance, input.last) = (text.to_string(), last);
        input.pending = pending.map(String::from);
        let (mut p, m) = planner::plan_openrouter(&key, &cfg.planner_model, &input)?;
        p.type_here = here;
        log::info!("live: planner {} ms ${:.5} {text:?} -> complete={} {:?} rest={:?}", m.latency_ms, m.cost, p.complete, p.actions.iter().map(planner::label).collect::<Vec<_>>(), p.rest);
        Ok(p)
    };
    let to_16k = |a: &[f32]| crate::audio::resample(a, rate);
    // The front app when the order was held: Some(pid) while holding.
    let mut held_front: Option<Option<i32>> = None;
    loop {
        if ctl.stopped.load(Ordering::SeqCst) {
            // Esc: the exec thread waits on the queue; the End lets it go.
            queue.push(Item::End { instruction: core.said.join(" ") });
            break;
        }
        let msg = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(m) => m,
            // The finish waits at most RT_FINISH_MS for Scribe's last answers, then the unanswered audio goes batch.
            Err(mpsc::RecvTimeoutError::Timeout) if rt.is_some() && released.is_some_and(|t| t.elapsed() > Duration::from_millis(RT_FINISH_MS)) => {
                Msg::Rt(scribe_rt::Ev::Failed("no answer after the release".into()))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Msg::Tick,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        same_front(&mut core, &mut held_front);
        match msg {
            Msg::Tick => {}
            Msg::Chunk(a) => {
                waiting.push_back((seq, a));
                seq += 1;
            }
            Msg::Audio(a, cut) => match &rt {
                Some(l) => {
                    let commit = rta.audio(&a, cut, &mut seq);
                    l.send(&a, commit);
                }
                // the socket failed earlier: cut chunks go to the batch STT
                None => {
                    rta.open.extend_from_slice(&a);
                    if cut {
                        for (n, a) in rta.drain(&mut seq) {
                            waiting.push_back((n, to_16k(&a)));
                        }
                    }
                }
            },
            Msg::Rt(e) => match e {
                scribe_rt::Ev::Partial(p) if !p.is_empty() => {
                    words_at = Instant::now();
                    update(&app, |l| l.last_said = p);
                }
                scribe_rt::Ev::Partial(_) => {}
                // Scribe answers in commit order; the answer goes the way of a finished batch chunk.
                scribe_rt::Ev::Committed(t) => match rta.committed(&t) {
                    RtAnswer::Text(n, t) => {
                        log::info!("live: commit {n}: {t:?}");
                        in_flight += 1;
                        let _ = tx.send(Msg::Text(n, Ok(command_words(&app, &t))));
                    }
                    RtAnswer::Batch(n, a) => {
                        log::info!("live: commit {n} came back empty with speech in it, the batch STT tries it");
                        waiting.push_back((n, to_16k(&a)));
                    }
                    RtAnswer::Nothing => {}
                },
                scribe_rt::Ev::Closed | scribe_rt::Ev::Failed(_) => {
                    if let scribe_rt::Ev::Failed(err) = &e {
                        log::warn!("live: Scribe Realtime failed, {:?} takes the rest: {err}", crate::stt::batch_for_realtime(true));
                    }
                    rt = None;
                    for (n, a) in rta.drain(&mut seq) {
                        waiting.push_back((n, to_16k(&a)));
                    }
                }
            },
            Msg::End(tail) => {
                if realtime_mode(&provider) {
                    // Scribe (or its fallback) has every sample already: the tail is not sent again.
                    match &rt {
                        Some(l) => {
                            log::info!("live: release, {} ms of audio not committed yet", rta.open.len() * 1000 / rate.max(1) as usize);
                            if let Some(pad) = rta.end(&mut seq) {
                                l.send(&pad, false);
                            }
                            l.finish();
                            released = Some(Instant::now());
                        }
                        None => {
                            for (n, a) in rta.drain(&mut seq) {
                                waiting.push_back((n, to_16k(&a)));
                            }
                        }
                    }
                } else if let Some(t) = tail {
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
                    // A silent chunk is a pause: a held order runs (hear "" plans nothing else).
                    let t = t.unwrap_or_default();
                    if t.is_empty() && !core.holding() {
                        continue;
                    }
                    if !t.is_empty() {
                        words_at = Instant::now();
                        update(&app, |l| l.last_said = t.clone());
                    }
                    if paused() && resume_word(&t) {
                        resume(&app);
                        continue;
                    }
                    let pending = command::waiting().map(|w| (w, LIVE.lock().unwrap().pending.clone().unwrap_or_default()));
                    let heard = core.hear(&t, false, pending.as_ref().map(|(w, q)| (*w, q.as_str())), &mut planner);
                    *ctl.said.lock().unwrap() = core.said.join(" ");
                    // Esc while the planner ran: its answer is dropped.
                    if ctl.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    // A new order while "Which one?" waits: Pablo moved on, the pick is dropped.
                    if matches!(heard, Heard::Order(..)) && command::waiting() == Some(Wait::Pick) {
                        command::deny();
                    }
                    handle(&app, &queue, &core, heard);
                }
            }
        }
        // No new words for HOLD_MS and no STT in flight: a pause, the held order runs (log 2026-10-17: it waited
        // forever). Checked after every message: Scribe's audio arrives every ~100 ms, so a timeout alone would starve.
        same_front(&mut core, &mut held_front);
        if ctl.speaking.load(Ordering::SeqCst) {
            words_at = Instant::now();
        }
        if core.holding() && words_at.elapsed() > Duration::from_millis(HOLD_MS) && in_flight == 0 && waiting.is_empty() && rta.unanswered.is_empty() && !ctl.stopped.load(Ordering::SeqCst) {
            let heard = core.hear("", false, None, &mut planner);
            handle(&app, &queue, &core, heard);
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
        if ended && in_flight == 0 && waiting.is_empty() && rt.is_none() {
            if !core.buffer.is_empty() && !command::aborted() && !ctl.stopped.load(Ordering::SeqCst) {
                let heard = core.hear("", true, None, &mut planner);
                handle(&app, &queue, &core, heard);
            }
            queue.push(Item::End { instruction: core.said.join(" ") });
            break;
        }
    }
    if !ctl.ended.load(Ordering::SeqCst) {
        update(&app, |l| (l.on, l.buffer) = (false, String::new()));
    }
    log::info!("live: recording side done after {} ms, {} orders", t0.elapsed().as_millis(), core.order);
}

/// A held order types into the app it was planned for: if Pablo moved to another app by hand since (a key or a click
/// paused the queue), it is dropped. An app an order of his opened is no reason.
fn same_front(core: &mut Core, held_front: &mut Option<Option<i32>>) {
    if !core.holding() {
        *held_front = None;
        return;
    }
    let now = crate::paste::target_app().map(|a| a.pid);
    match *held_front {
        None => *held_front = Some(now),
        Some(f) if f != now && paused() => {
            core.drop_held();
            *held_front = None;
        }
        Some(f) if f != now => *held_front = Some(now),
        Some(_) => {}
    }
}

/// Scribe's last commit answers within this after the release (measured ~0.2 s, TESTING.md "Latency"); later, the
/// unanswered audio goes to the batch STT.
const RT_FINISH_MS: u64 = 1500;

fn realtime_mode(p: &crate::contract::SttProvider) -> bool {
    *p == crate::contract::SttProvider::ElevenLabsRealtime
}

fn resume(app: &AppHandle) {
    PAUSED.store(false, Ordering::SeqCst);
    TOOK_OVER.store(false, Ordering::SeqCst);
    update(app, |l| (l.paused, l.took_over) = (false, false));
}

fn handle(app: &AppHandle, queue: &Queue, core: &Core, h: Heard) {
    match h {
        Heard::Nothing => {}
        Heard::Confirm(true) => command::confirm(),
        Heard::Confirm(false) => command::deny(),
        Heard::Pick(t) => command::answer(t),
        Heard::Order(order, actions, say) => {
            let said = core.said.last().cloned().unwrap_or_default();
            for mut a in actions {
                a.said = said.clone();
                queue.push(Item::Do { order, said: said.clone(), action: a });
            }
            // The next order shows the marks again (case 13) and resumes a paused queue (open risk a).
            TOOK_OVER.store(false, Ordering::SeqCst);
            PAUSED.store(false, Ordering::SeqCst);
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
    update(app, |l| (l.queue, l.buffer, l.took_over, l.paused) = (labels, buffer, took_over(), paused()));
}

fn exec_loop(app: AppHandle, queue: Arc<Queue>, key: String, settings: CommandSettings, ctl: Arc<Ctl>) {
    let t0 = Instant::now();
    let cfg = Cfg { confirm_risky: settings.confirm_risky, choose: !settings.auto_click };
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
                let stopped = || command::aborted() || ctl.stopped.load(Ordering::SeqCst);
                // Pablo took over by hand: wait for his next order; the session end drops what is left.
                while paused() && !stopped() && !queue.has_end() {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if paused() && !stopped() {
                    (worst, message) = (CmdStatus::Stopped, Some("Paused: you took over".into()));
                    continue;
                }
                if stopped() || skip == Some(order) {
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
                BUSY.store(true, Ordering::SeqCst);
                STEPS.fetch_add(1, Ordering::SeqCst);
                #[cfg(target_os = "macos")]
                let end = command::exec(&action, step, order, &cfg, &mut ask, &mut desk, &mut mem);
                BUSY.store(false, Ordering::SeqCst);
                #[cfg(not(target_os = "macos"))]
                let end = {
                    let _ = (&cfg, &mut ask, &action);
                    End { status: CmdStatus::Error, summary: String::new(), message: Some("macOS only".into()), steps: step }
                };
                log::info!("live: step {step} (order {order}) {}: {:?} {} {:?}", planner::label(&action), end.status, end.summary, end.message);
                if ctl.stopped.load(Ordering::SeqCst) {
                    continue; // Esc already ended the session: a new one may own the marks and the pill now
                }
                if end.status != CmdStatus::Done {
                    // The rest of this order depends on this action.
                    skip = Some(order);
                    // A timeout or a miss says so in the pill (live log 2026-10-18: a Return confirm stopped silently).
                    if let Some(m) = end.message.as_deref().filter(|m| !matches!(*m, "accessibility" | "screen_recording" | "key")) {
                        crate::session::toast(&app, Some("overlay"), "live-step", crate::contract::ToastKind::Warn, m.to_string(), &[], 5000);
                    }
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
                } else if queued == 0 && !ctl.stopped.load(Ordering::SeqCst) {
                    // the next order reads its numbers off the screen as it is now; a page load gets a second look
                    refresh_board(&app);
                    let (app, at) = (app.clone(), STEPS.load(Ordering::SeqCst));
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(BOARD_AGAIN_MS));
                        if STEPS.load(Ordering::SeqCst) == at && !BUSY.load(Ordering::SeqCst) && LIVE.lock().unwrap().on {
                            refresh_board(&app);
                        }
                    });
                }
                let _ = queued;
            }
        }
    };
    if command::aborted() {
        worst = CmdStatus::Stopped;
    }
    let end = End { status: if summary.is_empty() && worst == CmdStatus::Done { CmdStatus::Unsure } else { worst }, summary: summary.join("; "), message, steps: step };
    let end = if step == 0 && instruction.trim().is_empty() { End { status: CmdStatus::Stopped, ..end } } else { end };
    finish(&app, &ctl, Ended { instruction, end, latency_ms: t0.elapsed().as_millis() as u32 });
}

/// A page load after a click: the board is read again this long after the step. ponytail: a guess, not measured.
const BOARD_AGAIN_MS: u64 = 1200;

/// The planner's view right before the call: app, focused field, marks ("[N] label"). true = `type_here`.
fn context(app: &AppHandle) -> (planner::Input, bool) {
    let _ = app;
    let t = Instant::now();
    let mut input = planner::Input { memory: memory_lines(&MEMORY.lock().unwrap(), crate::history::now_ms()), ..Default::default() };
    let mut inputs = 0;
    #[cfg(target_os = "macos")]
    if crate::permissions::accessibility_trusted() {
        if let Some(front) = crate::paste::target_app() {
            input.app = front.name.clone().unwrap_or_default();
            if let Some((mut f, _)) = crate::ax::mac::focused() {
                (f.app, f.bundle) = (input.app.clone(), front.bundle_id.clone().unwrap_or_default());
                input.focused = f;
            }
            if let Some(tree) = crate::ax::mac::read(front.pid) {
                let els = crate::ax::collect(&tree.root, tree.window);
                inputs = els.iter().filter(|e| crate::ax::takes_text(&e.mark.role)).count();
                input.marks = els.iter().map(|e| format!("[{}] {}", e.mark.id, crate::ax::describe(e))).collect();
                // what the planner numbers is what Pablo sees (a running step draws its own reads)
                if !BUSY.load(Ordering::SeqCst) {
                    draw_board(app, &els, tree.window);
                }
            }
        }
    }
    let here = type_here(&input.focused.role, inputs);
    log::info!("live: context read {} ms, {} marks, {inputs} text inputs, type here: {here}", t.elapsed().as_millis(), input.marks.len());
    (input, here)
}

/// The command STT hints: Pablo's dictionary first, then BRANDS (vc-feedback 18a).
pub fn with_brands(h: &crate::dictionary::Hints) -> crate::dictionary::Hints {
    let mut h = h.clone();
    let extra: Vec<String> = BRANDS.iter().map(|b| b.to_string()).filter(|b| !h.terms.contains(b)).collect();
    h.terms.extend(extra);
    h
}

/// A Scribe Realtime text through the same pipeline as a batch chunk (hallucination filter, dictionary). None = nothing.
/// ponytail: the Chunker already gated the audio (worth), so a stand-in clip passes the pipeline's audio gates.
fn command_words(app: &AppHandle, text: &str) -> Option<String> {
    let t = text.trim().to_string();
    if t.is_empty() {
        return None;
    }
    let clip = crate::audio::Clip { samples: vec![0.5; 16000], duration_ms: 1000, peak_db: 0.0, device_names: vec![] };
    let out = crate::stt::SttOutput { text: t, language: None, note: None, fell_back: false };
    chunk_text_with(app, clip, &|_: &crate::stt::SttRequest| Ok(out.clone())).ok().flatten()
}

/// One chunk through the command STT: the pipeline (hallucination filter, dictionary corrections), no cleanup LLM,
/// plus BRANDS as extra hint words. Ok(None) = nothing heard.
fn chunk_text(app: &AppHandle, audio: Vec<f32>) -> anyhow::Result<Option<String>> {
    let state = app.state::<AppState>();
    let mut s = state.settings.read().unwrap().clone();
    // the command STT; Scribe Realtime here = its batch fallback (stt::build maps it to MAI or Scribe batch)
    s.stt.provider = crate::stt::command_provider(&s, crate::secrets::get(crate::contract::KeyName::Elevenlabs).is_some());
    let peak = audio.iter().fold(0f32, |m, x| m.max(x.abs()));
    let clip = crate::audio::Clip { duration_ms: (audio.len() / 16) as u32, peak_db: crate::audio::db(peak), samples: audio, device_names: vec![] };
    let stt = crate::stt::build(&s, &state)?;
    chunk_text_with(app, clip, &|r: &crate::stt::SttRequest| stt.transcribe(&crate::stt::SttRequest { audio: r.audio, language: r.language, hints: &with_brands(r.hints) }))
}

fn chunk_text_with(app: &AppHandle, clip: crate::audio::Clip, stt_fn: &dyn Fn(&crate::stt::SttRequest) -> anyhow::Result<crate::stt::SttOutput>) -> anyhow::Result<Option<String>> {
    use crate::pipeline::{self, Outcome};
    let state = app.state::<AppState>();
    let s = state.settings.read().unwrap().clone();
    let d = crate::dictionary::Dictionary::new(crate::dictionary::load(&state.db.lock().unwrap()).unwrap_or_default(), s.dictionary.clone());
    let opts = pipeline::Opts { language: Some(s.language.as_str()), style: s.styles.style.clone(), raw_app: true, press_enter_command: false, strip_inverted: false, slash: None };
    match pipeline::run(&clip, &opts, &d, stt_fn, None) {
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

    fn ty(text: &str) -> Action {
        Action { op: "type".into(), text: Some(text.into()), ..Default::default() }
    }

    #[test]
    fn a_held_type_survives_a_replan_that_drops_it() {
        // susurro.log 2026-10-17 23:38:54-23:39:02, chunks 9-12 replayed: the held type was never typed
        let mut c = Core::default();
        let mut calls = vec![];
        let mut p = |t: &str, _: bool, _: Option<&str>| {
            calls.push(t.to_string());
            Ok(match t {
                "Da type. Make me an application for HTML forms." => Plan { complete: true, actions: vec![ty("Make me an application for HTML forms.")], ..Default::default() },
                "Da type. Make me an application for HTML forms. Use view." => Plan { complete: true, actions: vec![ty("Make me an application for HTML forms. Use view.")], ..Default::default() },
                "Da type. Make me an application for HTML forms. Use view. De-" => plan(false, "De-", &[]),
                _ => plan(true, "", &[]),
            })
        };
        c.buffer = "Da type.".into();
        assert_eq!(c.hear("Make me an application for HTML forms.", false, None, &mut p), Heard::Nothing, "held");
        assert_eq!(c.hear("Use view.", false, None, &mut p), Heard::Nothing, "held again");
        let Heard::Order(1, a, _) = c.hear("De-", false, None, &mut p) else { panic!("the held type runs") };
        assert_eq!(a[0].text.as_deref(), Some("Make me an application for HTML forms. Use view."));
        assert_eq!(c.buffer, "De-", "the new words stay");
        assert_eq!(c.hear("AS.", false, None, &mut p), Heard::Nothing);
        assert_eq!(calls.last().map(String::as_str), Some("De- AS."));
    }

    #[test]
    fn a_pause_runs_the_held_type() {
        // continuous mode: no release comes, a silent chunk or HOLD_MS of quiet is the end of the order
        let mut c = Core::default();
        let calls = std::cell::Cell::new(0);
        let mut p = |_: &str, _: bool, _: Option<&str>| {
            calls.set(calls.get() + 1);
            Ok(Plan { complete: true, actions: vec![ty("hello")], ..Default::default() })
        };
        assert_eq!(c.hear("type hello", false, None, &mut p), Heard::Nothing);
        assert!(c.holding());
        assert!(matches!(c.hear("", false, None, &mut p), Heard::Order(1, ref a, _) if a[0].text.as_deref() == Some("hello")));
        assert_eq!((c.buffer.as_str(), c.holding(), calls.get()), ("", false, 1), "no second planner call");
        assert_eq!(c.hear("", false, None, &mut p), Heard::Nothing, "a pause with nothing held plans nothing");
        assert_eq!(calls.get(), 1);
        // the release runs the held plan as is
        c.hear("type hi", false, None, &mut p);
        assert!(matches!(c.hear("", true, None, &mut p), Heard::Order(2, ..)));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_target_split_over_three_chunks_still_waits() {
        // case 5 with one more cut: "Type hello" | "onto the" | "search input."
        let mut c = Core::default();
        let mut p = |t: &str, _: bool, _: Option<&str>| {
            Ok(match t {
                "Type hello" => Plan { complete: true, actions: vec![ty("hello")], ..Default::default() },
                "Type hello onto the" => plan(false, "Type hello onto the", &[]),
                _ => Plan { complete: true, actions: vec![Action { target: Some("Search".into()), ..ty("hello") }], ..Default::default() },
            })
        };
        assert_eq!(c.hear("Type hello", false, None, &mut p), Heard::Nothing);
        assert_eq!(c.hear("onto the", false, None, &mut p), Heard::Nothing, "the rest keeps the held words: wait on");
        assert!(c.holding());
        let Heard::Order(1, a, _) = c.hear("search input.", false, None, &mut p) else { panic!("one order") };
        assert_eq!((a.len(), a[0].target.as_deref()), (1, Some("Search")));
        assert!(!c.holding());
    }

    #[test]
    fn a_dropped_held_type_never_runs() {
        // Pablo clicked into another app while "type hello" waited: it must not type there, now or at the release
        let mut c = Core::default();
        let mut p = |t: &str, _: bool, _: Option<&str>| Ok(Plan { complete: true, actions: vec![ty(t)], ..Default::default() });
        assert_eq!(c.hear("type hello", false, None, &mut p), Heard::Nothing);
        c.drop_held();
        assert_eq!((c.holding(), c.buffer.as_str()), (false, ""));
        assert_eq!(c.hear("", false, None, &mut p), Heard::Nothing, "a pause runs nothing");
        assert_eq!(c.hear("", true, None, &mut p), Heard::Nothing, "the release runs nothing");
        // a held type runs once: a pause, then the release
        c.hear("type hi", false, None, &mut p);
        assert!(matches!(c.hear("", false, None, &mut p), Heard::Order(1, ..)));
        assert_eq!(c.hear("", true, None, &mut p), Heard::Nothing, "not again at the release");
    }

    #[test]
    fn type_here_runs_at_once() {
        // Ghostty / Claude Code: the focus is the only text input, an untargeted type means "type here"
        let mut c = Core::default();
        let mut p = |_: &str, _: bool, _: Option<&str>| Ok(Plan { complete: true, actions: vec![ty("ls")], type_here: true, ..Default::default() });
        assert!(matches!(c.hear("type ls", false, None, &mut p), Heard::Order(1, ..)));
        // (focused role, text inputs on screen) -> type here
        let rows = [("AXTextArea", 1, true), ("AXTextArea", 0, true), ("", 0, true), ("AXGroup", 0, true), ("AXTextArea", 2, false), ("AXButton", 1, false), ("AXSearchField", 1, true)];
        for (role, n, want) in rows {
            assert_eq!(type_here(role, n), want, "{role} {n}");
        }
    }

    #[test]
    fn a_short_order_released_before_any_cut_reaches_the_planner() {
        // Pablo 2026-10-17 23:39:25: "open Edge", released, 0 orders
        let rate = 48_000;
        let speech = vec![0.3f32; rate as usize]; // 1 s, no pause cut
        let mut r = RtAudio { rate, ..Default::default() };
        let mut seq = 0;
        assert!(!r.audio(&speech, false, &mut seq));
        assert_eq!(r.end(&mut seq), Some(vec![]), "long enough: no pad, one last commit");
        assert_eq!(r.committed("Open Edge."), RtAnswer::Text(0, "Open Edge.".into()));
        // Scribe answers the last commit with nothing: the batch STT tries the audio (no next chunk will catch it)
        let mut r = RtAudio { rate, ..Default::default() };
        r.audio(&speech, false, &mut seq);
        r.end(&mut seq);
        assert!(matches!(r.committed(""), RtAnswer::Batch(1, ref a) if a.len() == speech.len()));
        // mid-session: an empty answer for speech goes batch at once (live log 2026-10-18), silence stays silence
        let mut r = RtAudio { rate, ..Default::default() };
        r.audio(&speech, true, &mut seq);
        assert!(matches!(r.committed(""), RtAnswer::Batch(2, _)));
        seq -= 1;
        let mut r = RtAudio { rate, ..Default::default() };
        r.audio(&vec![0.0; rate as usize], true, &mut seq);
        assert_eq!(r.committed(""), RtAnswer::Text(2, String::new()));
        assert_eq!(r.committed("late"), RtAnswer::Nothing, "no commit left");
        // the socket closes or fails with audio not answered: it all goes batch, in order
        let mut r = RtAudio { rate, ..Default::default() };
        r.audio(&speech, true, &mut seq);
        r.audio(&speech[..1000], false, &mut seq);
        assert_eq!(r.drain(&mut seq).iter().map(|x| (x.0, x.1.len())).collect::<Vec<_>>(), [(3, speech.len()), (4, 1000)]);
        assert!(RT_FINISH_MS <= 1500, "the release waits at most 1.5 s for Scribe");
    }

    #[test]
    fn scribe_commits_are_never_shorter_than_300_ms() {
        // susurro.log 2026-10-17 23:38:42: "commit_throttled: only 0.11s of uncommitted audio", 3 reconnects, 6 s lost
        let rate = 16_000;
        let mut r = RtAudio { rate, ..Default::default() };
        let mut seq = 0;
        assert!(!r.audio(&vec![0.3; 1760], true, &mut seq), "a cut after 0.11 s waits for more audio");
        assert!(r.audio(&vec![0.3; 3200], true, &mut seq), "0.31 s: commit");
        r.audio(&vec![0.3; 1760], false, &mut seq);
        assert_eq!(r.end(&mut seq).map(|p| p.len()), Some(4800 - 1760), "the 0.11 s tail is padded with silence");
        assert_eq!(r.unanswered.back().map(|x| x.1.len()), Some(4800));
        assert_eq!(r.end(&mut seq), None, "nothing open: no commit");
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
    fn a_new_order_during_a_pick_is_planned_not_swallowed() {
        // open risk c: only numbers, ordinals and shown names answer "Which one?"
        let q = "Which one? 4 #general · 7 #design · 9 #random";
        let mut c = Core::default();
        let mut p = |t: &str, _: bool, _: Option<&str>| Ok(if t.starts_with("open") { plan(true, "", &["key", "open_app"]) } else { plan(true, "", &[]) });
        for answer in ["la dos", "7", "the second one", "design", "el de random", "number four"] {
            assert_eq!(c.hear(answer, false, Some((Wait::Pick, q)), &mut p), Heard::Pick(answer.into()), "{answer}");
        }
        assert!(matches!(c.hear("open a new tab and type hello", false, Some((Wait::Pick, q)), &mut p), Heard::Order(1, ..)), "a full order");
        assert_eq!(c.hear("something else entirely", false, Some((Wait::Pick, q)), &mut p), Heard::Nothing, "not a pick, no actions");
    }

    #[test]
    fn resume_words() {
        for w in ["continue", "Sigue.", "go on", "dale"] {
            assert!(resume_word(w), "{w}");
        }
        assert!(!resume_word("continue typing hello"), "a real order goes to the planner");
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
