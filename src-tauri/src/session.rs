//! WP5: the dictation actor thread (state machine + pipeline). Only owner of the Recorder (cpal Stream is not Send).
//! PLAN 4.2. The pure `Machine` decides; the `Actor` does the side effects (recorder, pipeline, paste, history, events).

use crate::AppState;
use crate::audio::{self, Clip, Recorder};
use crate::cleanup;
use crate::command;
use crate::contract::{CleanupLevel, CmdStatus, CommandStep, HistoryItem, HistoryKind, HistoryStatus, KeyName, Phase, Recents, SessionState, SttProvider, Toast, ToastActionDef, ToastKind};
use crate::dictionary::{self, Dictionary, Hints};
use crate::history;
use crate::paste::{self, AppRef, PasteOutcome};
use crate::pipeline::{self, Done, Outcome};
use crate::secrets;
use crate::stt::{self, SttRequest, whisper_local};
use std::sync::{LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Debug, Clone, PartialEq)]
pub enum SessionCmd {
    PttDown,
    PttUp,
    Toggle,
    /// Overlay Stop button: stop and transcribe (never locks).
    Stop,
    Cancel,
    PasteLast,
    /// Copy the last transcript to the clipboard, no paste.
    CopyLast,
    DeviceLost,
    Retry(i64),
    ToastAction { toast_id: String, action_id: String },
    /// history_paste: hide susurro, paste into the app behind it.
    PasteText(String),
    /// Voice control key (hotkeys.command): hold or double-tap = a live command session; while a step waits: yes.
    CmdDown,
    CmdUp,
    /// The live session finished its queue (live.rs).
    CommandEnded(Box<crate::command::Ended>),
    /// hotkeys.recents: open (or close) the recents list.
    Recents,
    /// 1-9 (paste) or Cmd+1-9 (copy) while the recents list is open.
    RecentPick { n: u32, copy: bool },
    RecentsClose,
    Shutdown,
}

// ---------- pure state machine (PLAN 4.2) ----------

/// A hold shorter than this is a tap (double-tap candidate).
const TAP: Duration = Duration::from_millis(250);
/// Window for the 2nd tap when the grace is off (else the grace: "tap-tap" and "hold-then-tap" feel the same).
const DOUBLE: Duration = Duration::from_millis(500);
/// A Cancel this soon after PttDown is the fn+key chord (hotkey.rs): silent, no Undo.
const CHORD: Duration = crate::hotkey::CHORD_WINDOW;
const UNDO: Duration = Duration::from_secs(5);
/// Scribe Realtime's last commit answers within this after the release (measured ~0.2 s, TESTING.md "Latency").
const RT_FINISH: Duration = Duration::from_secs(4);
/// The recents list closes after this long without a pick.
const RECENTS_IDLE: Duration = Duration::from_secs(10);
const WARN_BEFORE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ev {
    Down,
    Up,
    Toggle,
    Cancel,
    Undo,
    /// No device left: stop and transcribe what we have.
    Stop,
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Open the recorder; `locked` = hands-free from the start.
    Start { locked: bool },
    /// Recording became hands-free (2nd tap or the hands-free combo).
    Lock,
    /// Stop the recorder and run the pipeline.
    Process,
    /// Stop the recorder, drop the clip, no toast.
    Discard,
    /// Stop the recorder, keep the clip 5 s for Undo, toast "Cancelled" [Undo].
    CancelUndoable,
    /// Run the pipeline on the kept clip.
    ProcessKept,
    /// Toast "1 minute left".
    Warn,
    /// A hold was released: the recording goes on `ms` more for a press that locks hands-free (overlay countdown).
    Grace { ms: u32 },
    /// A hold was released (dictation, vc-feedback 17): process what was said now, keep the mic open for the window.
    ProcessKeep,
    /// A press in that window: hands-free, continuing the same session.
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum St {
    Idle,
    /// Holding PTT.
    Rec { start: Instant, down: Instant },
    /// Released after a short tap: wait for the 2nd tap (the recorder keeps running).
    TapWait { start: Instant, until: Instant },
    /// Released after a hold: a press before `until` locks hands-free and keeps this recording; else Process.
    Grace { start: Instant, until: Instant },
    Locked { start: Instant },
    /// Pressed again in the grace: hands-free (Locked) unless this press turns out to be a hold, whose release processes.
    Regrab { start: Instant, down: Instant },
    Busy,
    /// Dictation was processed at the release; a press before `until` continues hands-free (the mic is still open).
    ContWait { until: Instant },
}

#[derive(Debug, Clone)]
pub struct Machine {
    st: St,
    pub double_tap: bool,
    pub max: Duration,
    /// Hands-free grace after a hold (hotkeys.hands_free_grace_ms). Zero = a release processes at once.
    pub grace: Duration,
    /// Dictation: the release processes at once and a press in the grace continues (vc-feedback 17).
    /// Off (voice control): the recording goes on through the grace.
    pub cont: bool,
    cont_until: Option<Instant>,
    warned: bool,
    undo_until: Option<Instant>,
}

impl Machine {
    pub fn new(double_tap: bool, max: Duration) -> Self {
        Self { st: St::Idle, double_tap, max, grace: Duration::ZERO, cont: false, cont_until: None, warned: false, undo_until: None }
    }

    fn double(&self) -> Duration {
        if self.grace.is_zero() { DOUBLE } else { self.grace }
    }

    /// The press of a hold (not a tap wait, a grace or hands-free): the stuck-key watchdog polls the key then.
    pub fn holding(&self) -> Option<Instant> {
        match self.st {
            St::Rec { down, .. } => Some(down),
            _ => None,
        }
    }

    /// The continuation window is open (a late press from the busy queue still counts, with its own time).
    pub fn continuing(&self) -> bool {
        matches!(self.st, St::ContWait { .. })
    }

    pub fn phase(&self) -> Phase {
        match self.st {
            St::Idle | St::ContWait { .. } => Phase::Idle,
            St::Rec { .. } | St::TapWait { .. } | St::Grace { .. } => Phase::Recording,
            St::Locked { .. } | St::Regrab { .. } => Phase::Locked,
            St::Busy => Phase::Processing,
        }
    }

    /// The pipeline finished (or a start failed). After a ProcessKeep the continuation window opens.
    pub fn idle(&mut self) {
        self.st = match self.cont_until.take() {
            Some(until) => St::ContWait { until },
            None => St::Idle,
        };
    }

    pub fn step(&mut self, ev: Ev, now: Instant) -> Option<Act> {
        use St::*;
        let (st, act) = match (self.st, ev) {
            (Idle, Ev::Down) => (Rec { start: now, down: now }, Some(Act::Start { locked: false })),
            (Idle, Ev::Toggle) => (Locked { start: now }, Some(Act::Start { locked: true })),
            (Idle, Ev::Undo) if self.undo_until.is_some_and(|t| now < t) => {
                self.undo_until = None;
                (Busy, Some(Act::ProcessKept))
            }
            (ContWait { until }, Ev::Down) if now < until => (Regrab { start: now, down: now }, Some(Act::Continue)),
            (ContWait { until }, Ev::Toggle) if now < until => (Locked { start: now }, Some(Act::Continue)),
            (ContWait { until }, Ev::Tick) if now >= until => (Idle, Some(Act::Discard)),
            (ContWait { .. }, Ev::Cancel) => (Idle, Some(Act::Discard)),
            // The tick did not close the window yet: a late press starts a new session (start() drops the old mic).
            (ContWait { .. }, Ev::Down) => (Rec { start: now, down: now }, Some(Act::Start { locked: false })),
            (ContWait { .. }, Ev::Toggle) => (Locked { start: now }, Some(Act::Start { locked: true })),
            (Rec { start, down }, Ev::Up) if self.double_tap && now - down < TAP => (TapWait { start, until: now + self.double() }, None),
            (Rec { .. } | TapWait { .. } | Grace { .. } | Locked { .. } | Regrab { .. }, Ev::Stop) => (Busy, Some(Act::Process)),
            (Rec { .. }, Ev::Up) if !self.grace.is_zero() && self.cont => {
                self.cont_until = Some(now + self.grace);
                (Busy, Some(Act::ProcessKeep))
            }
            (Rec { start, .. }, Ev::Up) if !self.grace.is_zero() => (Grace { start, until: now + self.grace }, Some(Act::Grace { ms: self.grace.as_millis() as u32 })),
            (Rec { .. }, Ev::Up) => (Busy, Some(Act::Process)),
            (Grace { start, until }, Ev::Down) if now < until => (Regrab { start, down: now }, Some(Act::Lock)),
            (Regrab { down, .. }, Ev::Up) if now - down >= TAP => (Busy, Some(Act::Process)),
            (Regrab { start, .. }, Ev::Up) => (Locked { start }, None),
            (Regrab { .. }, Ev::Toggle) => (Busy, Some(Act::Process)),
            // The tick did not run yet: a late press still ends the grace (and is dropped by the busy guard).
            (Grace { .. }, Ev::Down) => (Busy, Some(Act::Process)),
            (Grace { start, until }, Ev::Tick) if now >= until || now - start >= self.max => (Busy, Some(Act::Process)),
            (Rec { start, .. } | TapWait { start, .. } | Grace { start, .. }, Ev::Toggle) | (TapWait { start, .. }, Ev::Down) => (Locked { start }, Some(Act::Lock)),
            (TapWait { until, .. }, Ev::Tick) if now >= until => (Idle, Some(Act::Discard)),
            (Locked { .. }, Ev::Toggle | Ev::Down) => (Busy, Some(Act::Process)),
            (Rec { down, .. }, Ev::Cancel) if now - down < CHORD => (Idle, Some(Act::Discard)),
            (Rec { .. } | TapWait { .. } | Grace { .. } | Locked { .. } | Regrab { .. }, Ev::Cancel) => {
                self.undo_until = Some(now + UNDO);
                (Idle, Some(Act::CancelUndoable))
            }
            (Rec { start, .. } | Locked { start } | Regrab { start, .. }, Ev::Tick) => {
                let el = now - start;
                if el >= self.max {
                    (Busy, Some(Act::Process))
                } else if !self.warned && el + WARN_BEFORE >= self.max {
                    self.warned = true;
                    (self.st, Some(Act::Warn))
                } else {
                    (self.st, None)
                }
            }
            // Busy guard, stray releases, and everything else: no change.
            (s, _) => (s, None),
        };
        if matches!(act, Some(Act::Start { .. } | Act::Continue)) {
            self.warned = false;
        }
        self.st = st;
        act
    }
}

/// When the hotkey thread sent the last press / release: a key that waited in the queue while the pipeline ran
/// still counts at the time it was pressed (the continuation window, vc-feedback 17).
static PRESSED_AT: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));

pub fn stamp(c: &SessionCmd) {
    let mut p = PRESSED_AT.lock().unwrap();
    match c {
        SessionCmd::PttDown => p.0 = Some(Instant::now()),
        SessionCmd::PttUp => p.1 = Some(Instant::now()),
        _ => {}
    }
}

fn stamped(c: &SessionCmd) -> Option<Instant> {
    let p = PRESSED_AT.lock().unwrap();
    match c {
        SessionCmd::PttDown => p.0,
        SessionCmd::PttUp => p.1,
        _ => None,
    }
}

/// The stuck-key watchdog (VOICE_CONTROL_V2 4.2) after one poll of the physical key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Watch {
    /// The key is down, or the poll could not tell.
    Fine,
    /// The key is up but no release came: feed the lost Up.
    Lost,
    /// The very first poll of a hold reads "up": this process cannot see the key. Turn the watchdog off.
    Blind,
}

pub fn watch(seen_down: bool, down: Option<bool>) -> Watch {
    match down {
        Some(false) if !seen_down => Watch::Blind,
        Some(false) => Watch::Lost,
        _ => Watch::Fine,
    }
}

const WATCH_EVERY: Duration = Duration::from_millis(250);
/// No voice this long while recording = stop and process (a backup for a lost release). ponytail: a start value.
pub const SILENCE_STOP: Duration = Duration::from_secs(30);

/// The physical state of a binding's key. None = unknown (no keycode for it).
fn key_down(binding: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
            fn CGEventSourceFlagsState(state: i32) -> u64;
        }
        if let Some(m) = crate::hotkey::mod_key(binding) {
            return Some(unsafe { CGEventSourceFlagsState(0) } & m.down_flags() != 0);
        }
        let code = crate::paste::keycode(binding.rsplit('+').next()?)?;
        Some(unsafe { CGEventSourceKeyState(0, code) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = binding;
        None
    }
}

/// Recents row `n` (1-based).
pub fn recent_at(items: &[HistoryItem], n: u32) -> Option<&HistoryItem> {
    items.get((n as usize).checked_sub(1)?)
}

// ---------- pipeline glue (shared with history_retry) ----------

pub struct Run {
    pub outcome: Outcome,
    /// "provider:model" (the History meta line splits it).
    pub provider: String,
    pub level: CleanupLevel,
}

impl Run {
    /// History `cleanup` column: the level when the LLM output was used, else None ("Raw").
    pub fn cleanup(&self, d: &Done) -> Option<String> {
        d.cleaned.then(|| match self.level {
            CleanupLevel::None => "none",
            CleanupLevel::Light => "light",
            CleanupLevel::Medium => "medium",
        })
        .map(String::from)
    }
}

fn provider_label(s: &crate::contract::Settings, fell_back: bool) -> String {
    let st = &s.stt;
    match st.provider {
        _ if fell_back => format!("whisper_local:{}", st.whisper_model),
        SttProvider::WhisperLocal => format!("whisper_local:{}", st.whisper_model),
        SttProvider::OpenAiCompat => format!("openai_compat:{}", st.openai_compat.model),
        SttProvider::ElevenLabs => format!("elevenlabs:{}", st.elevenlabs.model_id),
        SttProvider::OpenRouter => format!("openrouter:{}", st.openrouter_model),
        SttProvider::ElevenLabsRealtime => "elevenlabs:scribe_v2_realtime".into(),
    }
}

/// Section 0 with the current settings and dictionary. Blocking (STT, LLM).
pub fn run_pipeline(state: &AppState, clip: &Clip, bundle: Option<&str>) -> Run {
    run_pipeline_with(state, clip, bundle, None)
}

/// `streamed`: the STT text the streaming worker already made for this clip (local whisper). None = single-shot.
fn run_pipeline_with(state: &AppState, clip: &Clip, bundle: Option<&str>, streamed: Option<stt::SttOutput>) -> Run {
    let s = state.settings.read().unwrap().clone();
    let entries = dictionary::load(&state.db.lock().unwrap()).unwrap_or_else(|e| {
        log::warn!("dictionary load: {e}");
        vec![]
    });
    let mut d = Dictionary::new(entries, s.dictionary.clone());
    d.spoken_punctuation = s.spoken_punctuation;
    let raw_app = bundle.is_some_and(|b| s.styles.raw_apps.iter().any(|r| r == b));
    let key = if raw_app { None } else { secrets::get(cleanup::key_name(&s)) };
    let level = cleanup::effective_level(&s, key.as_deref());
    let stt_fn = |r: &SttRequest| match &streamed {
        Some(o) => Ok(o.clone()),
        None => stt::build(&s, state)?.transcribe(r),
    };
    let clean_fn = |t: &str, h: &Hints| cleanup::cleanup(&s, key.as_deref(), &level, t, h);
    let clean: pipeline::Cleaner = (level != CleanupLevel::None).then_some(&clean_fn as &dyn Fn(&str, &Hints) -> anyhow::Result<String>);
    let in_app = |list: &[String]| bundle.is_some_and(|b| list.iter().any(|r| r == b));
    let slash_names = bundle.filter(|b| crate::slash::app_takes_slash(b)).and_then(|_| std::env::var_os("HOME")).map(|h| crate::slash::names(&std::path::Path::new(&h).join(".claude")));
    let opts = pipeline::Opts {
        language: Some(s.language.as_str()),
        style: s.styles.style.clone(),
        raw_app,
        press_enter_command: s.paste.press_enter_command,
        strip_inverted: s.styles.strip_inverted && !in_app(&s.styles.inverted_apps),
        slash: slash_names.as_deref(),
    };
    let outcome = pipeline::run(clip, &opts, &d, &stt_fn, clean);
    let fell_back = matches!(&outcome, Outcome::Text(d) if d.fell_back);
    Run { outcome, provider: provider_label(&s, fell_back), level }
}

/// A voice command's text: the pipeline STT + dictionary corrections, no cleanup LLM, no spoken punctuation.
fn command_text(state: &AppState, clip: &Clip) -> Outcome {
    let mut s = state.settings.read().unwrap().clone();
    s.stt.provider = stt::command_provider(&s, secrets::get(KeyName::Elevenlabs).is_some());
    let entries = dictionary::load(&state.db.lock().unwrap()).unwrap_or_default();
    let d = Dictionary::new(entries, s.dictionary.clone());
    let stt_fn = |r: &SttRequest| stt::build(&s, state)?.transcribe(r);
    let opts = pipeline::Opts { language: Some(s.language.as_str()), style: s.styles.style.clone(), raw_app: true, press_enter_command: false, strip_inverted: false, slash: None };
    pipeline::run(clip, &opts, &d, &stt_fn, None)
}

/// History status of a finished command (contract: stopped / unsure / blocked = cancelled, setup / error = failed).
fn command_status(s: &CmdStatus) -> HistoryStatus {
    match s {
        CmdStatus::Done => HistoryStatus::Done,
        CmdStatus::Setup | CmdStatus::Error => HistoryStatus::Failed,
        _ => HistoryStatus::Cancelled,
    }
}

// ---------- actor ----------

static CURRENT: LazyLock<Mutex<SessionState>> = LazyLock::new(Default::default);
const RETENTION_EVERY: Duration = Duration::from_secs(6 * 3600);

/// The one toast builder. `to`: one window label ("main"), or None for every window.
pub fn toast(app: &AppHandle, to: Option<&str>, id: &str, kind: ToastKind, text: impl Into<String>, actions: &[(&str, &str)], ttl_ms: u32) {
    let t = Toast {
        id: id.into(),
        kind,
        text: text.into(),
        actions: actions.iter().map(|(id, label)| ToastActionDef { id: (*id).into(), label: (*label).into() }).collect(),
        ttl_ms,
    };
    log::info!("toast: {}", t.text);
    let _ = match to {
        Some(label) => app.emit_to(label, "toast", t),
        None => app.emit("toast", t),
    };
}

#[derive(Clone)]
struct Ctx {
    app: Option<AppRef>,
    started_at: i64,
    device: Option<String>,
}

struct Actor {
    app: AppHandle,
    m: Machine,
    rec: Option<Recorder>,
    ctx: Option<Ctx>,
    kept: Option<(Clip, Ctx)>,
    /// A failed dictation with no history row (store_history off): toast "failed" [Retry] re-runs it.
    failed: Option<(Clip, Ctx)>,
    /// Done/Error stay on screen until then, then Idle.
    idle_at: Option<Instant>,
    /// In-memory last result for paste_last (also when store_history is off).
    last_text: Option<String>,
    last_sweep: Option<Instant>,
    /// The current recording / live session belongs to voice control.
    command: bool,
    /// The live command session (live.rs): set from the start of the recording until CommandEnded.
    live: Option<crate::live::Handle>,
    /// Cuts the command recording into chunks for the live session.
    cmd_chunker: Option<audio::Chunker>,
    /// This recording answers the executor's "Which one?" (the live session's mic is already off).
    answering: bool,
    /// The app and start time of the running command (its History row).
    cmd_ctx: Option<Ctx>,
    /// Local whisper transcribes finished chunks while the user speaks (None: cloud STT, model not loaded, a command).
    stream: Option<(audio::Chunker, whisper_local::Stream)>,
    /// Dictation over Scribe Realtime: every new sample goes out, the Chunker's pauses commit (next-queue 1).
    rt: Option<(audio::Chunker, stt::scribe_rt::Live, mpsc::Receiver<stt::scribe_rt::Ev>)>,
    /// The next session_state carries this hands-free grace countdown.
    grace: Option<u32>,
    /// A continuation (vc-feedback 17): the 16 kHz sample where it starts in the still-open recording, and the History
    /// row it joins.
    cont_from: Option<usize>,
    cont_row: Option<i64>,
    /// The clip being processed is a continuation.
    cont_now: bool,
    /// The History row of the last dictation (a continuation joins it).
    last_row: Option<i64>,
    /// Stuck-key watchdog: last poll, the key was seen down in this hold, off for the process (Blind).
    watch_at: Option<Instant>,
    watch_seen: bool,
    watch_off: bool,
    /// Silence stop: the last time the mic heard voice.
    voice_at: Instant,
    /// The open recents list and when it closes.
    recents: Option<(Vec<HistoryItem>, Instant)>,
}

/// Start the actor: std::thread with a recv_timeout(100ms) loop. lib.rs setup already calls this.
pub fn spawn(app: AppHandle, rx: mpsc::Receiver<SessionCmd>) {
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let mut a = Actor {
                app,
                m: Machine::new(true, Duration::from_secs(600)),
                rec: None,
                ctx: None,
                kept: None,
                failed: None,
                idle_at: None,
                last_text: None,
                last_sweep: None,
                command: false,
                live: None,
                cmd_chunker: None,
                answering: false,
                cmd_ctx: None,
                stream: None,
                rt: None,
                grace: None,
                cont_from: None,
                cont_row: None,
                cont_now: false,
                last_row: None,
                watch_at: None,
                watch_seen: false,
                watch_off: false,
                voice_at: Instant::now(),
                recents: None,
            };
            // Busy guard: hotkey input that queued up while the (blocking) pipeline ran is dropped.
            // Exceptions: Esc / the command key still stop a live session; a press in the continuation window counts
            // at the time it was pressed (vc-feedback 17).
            let drain = |a: &mut Actor, rx: &mpsc::Receiver<SessionCmd>| {
                for c in rx.try_iter().collect::<Vec<_>>() {
                    if !is_hotkey_input(&c) {
                        a.handle(c);
                    } else if stops_command(a.live.is_some() && a.m.phase() == Phase::Idle, &c) {
                        a.abort_live();
                    } else if a.m.continuing() || matches!(a.m.st, St::Regrab { .. } | St::Locked { .. }) && a.cont_from.is_some() {
                        if let (Some(t), true) = (stamped(&c), matches!(c, SessionCmd::PttDown | SessionCmd::PttUp | SessionCmd::Toggle)) {
                            a.handle_at(c, t);
                        }
                    }
                }
            };
            loop {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(SessionCmd::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(cmd) => {
                        if a.handle(cmd) {
                            drain(&mut a, &rx);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                // A max-length stop runs the pipeline from tick() too.
                if a.tick() {
                    drain(&mut a, &rx);
                }
            }
            log::info!("session actor stopped");
        })
        .expect("spawn session actor");
}

/// A dropped key that still stops a draining live session.
fn stops_command(draining: bool, c: &SessionCmd) -> bool {
    draining && matches!(c, SessionCmd::Cancel | SessionCmd::CmdDown)
}

/// Input the busy guard drops (pressed while the pipeline ran).
fn is_hotkey_input(c: &SessionCmd) -> bool {
    matches!(c, SessionCmd::PttDown | SessionCmd::PttUp | SessionCmd::Toggle | SessionCmd::Stop | SessionCmd::Cancel | SessionCmd::DeviceLost | SessionCmd::CmdDown | SessionCmd::CmdUp)
}

impl Actor {
    fn state(&self) -> &AppState {
        self.app.state::<AppState>().inner()
    }

    fn set(&mut self, phase: Phase, message: Option<String>) {
        let s = SessionState {
            phase,
            started_at: self.ctx.as_ref().map(|c| c.started_at),
            device: self.ctx.as_ref().and_then(|c| c.device.clone()),
            app: self.ctx.as_ref().and_then(|c| c.app.as_ref().and_then(|a| a.name.clone())),
            message,
            command: self.command,
            grace_ms: self.grace.take(),
        };
        *CURRENT.lock().unwrap() = s.clone();
        let _ = self.app.emit("session_state", s);
    }

    fn sounds(&self) -> bool {
        self.state().settings.read().unwrap().recording.sounds
    }

    fn ping(&self, p: Ping) {
        #[cfg(target_os = "macos")]
        if self.sounds() {
            crate::platform::macos::ping(p);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = p;
    }

    /// A click in susurro's own window (History, a toast) made susurro the frontmost app:
    /// hide it so Cmd+V reaches the app behind it.
    fn step_aside(&self) {
        if paste::frontmost_app().is_some_and(|a| a.pid == std::process::id() as i32) {
            let _ = self.app.hide();
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn fail(&mut self, msg: String) {
        log::warn!("session error: {msg}");
        self.ping(Ping::Error);
        self.set(Phase::Error, Some(msg));
        self.idle_at = Some(Instant::now() + Duration::from_secs(2));
    }

    fn handle(&mut self, cmd: SessionCmd) -> bool {
        self.handle_at(cmd, Instant::now())
    }

    /// Returns true if the command ran the (blocking) pipeline. `now` = when the key was pressed.
    fn handle_at(&mut self, cmd: SessionCmd, now: Instant) -> bool {
        let (dt, max, grace) = {
            let s = self.state().settings.read().unwrap();
            (s.hotkeys.double_tap, Duration::from_secs(s.recording.max_seconds.max(1) as u64), Duration::from_millis(s.hotkeys.hands_free_grace_ms.min(1500) as u64))
        };
        (self.m.double_tap, self.m.max, self.m.grace) = (dt, max, grace);
        // A command (recording or acting) owns the session: the dictation keys wait.
        if self.command && matches!(cmd, SessionCmd::PttDown | SessionCmd::PttUp | SessionCmd::Toggle) {
            return false;
        }
        let recording = self.m.phase() != Phase::Idle;
        let ev = match cmd {
            // While a confirm waits, the command key = yes (v1 rule), not the end of the session.
            SessionCmd::CmdDown if command::waiting() == Some(command::Wait::Confirm) => {
                command::confirm();
                return false;
            }
            // Double-tap lock, hands-free stop, the press in the grace.
            SessionCmd::CmdDown if self.command && recording => Ev::Down,
            SessionCmd::CmdDown if self.live.is_some() => {
                if command::waiting() == Some(command::Wait::Pick) {
                    self.answering = true;
                    Ev::Down
                } else {
                    // the command key while the queue drains = stop it
                    self.abort_live();
                    return false;
                }
            }
            SessionCmd::CmdDown if !recording && self.state().settings.read().unwrap().command.enabled => {
                self.command = true;
                Ev::Down
            }
            SessionCmd::CmdDown => return false,
            SessionCmd::CmdUp if self.command && recording => Ev::Up,
            SessionCmd::CmdUp => return false,
            SessionCmd::Cancel if self.live.is_some() && !recording => {
                self.abort_live();
                return false;
            }
            SessionCmd::Cancel if self.recents.is_some() => {
                self.close_recents(None);
                return false;
            }
            SessionCmd::CommandEnded(e) => {
                self.command_ended(*e);
                return false;
            }
            SessionCmd::Recents => {
                self.toggle_recents();
                return false;
            }
            SessionCmd::RecentPick { n, copy } => {
                self.pick_recent(n, copy);
                return false;
            }
            SessionCmd::RecentsClose => {
                self.close_recents(None);
                return false;
            }
            SessionCmd::PttDown => Ev::Down,
            SessionCmd::PttUp => Ev::Up,
            SessionCmd::Toggle => Ev::Toggle,
            SessionCmd::Stop => Ev::Stop,
            SessionCmd::Cancel => Ev::Cancel,
            SessionCmd::DeviceLost => {
                let app = self.app.clone();
                match self.rec.as_mut().map(|rec| audio::recover_session(&app, rec)) {
                    Some(false) => Ev::Stop,
                    _ => {
                        // The session continues on another mic: report its name, not the lost one.
                        let name = self.rec.as_ref().map(|r| r.device().name.clone());
                        if let (Some(ctx), Some(n)) = (self.ctx.as_mut(), name) {
                            if ctx.device.as_ref() != Some(&n) {
                                ctx.device = Some(n);
                                let (phase, message) = { let c = CURRENT.lock().unwrap(); (c.phase.clone(), c.message.clone()) };
                                self.set(phase, message);
                            }
                        }
                        return false;
                    }
                }
            }
            SessionCmd::ToastAction { toast_id, action_id } => {
                if audio::handle_toast_action(&self.app, &toast_id, &action_id) || crate::learn::handle_toast_action(&self.app, &toast_id, &action_id) {
                    return false;
                }
                match (toast_id.as_str(), action_id.as_str()) {
                    ("cancelled", "undo") => Ev::Undo,
                    (_, "fix-ax") => {
                        let _ = crate::permissions::request_permission(crate::contract::PermKind::Accessibility);
                        return false;
                    }
                    ("failed", "retry") => return self.retry_failed(),
                    (id, "retry") => match id.strip_prefix("failed:").and_then(|n| n.parse().ok()) {
                        Some(id) => return self.retry(id),
                        None => return false,
                    },
                    _ => return false,
                }
            }
            SessionCmd::Retry(id) => return self.retry(id),
            SessionCmd::PasteLast => {
                self.paste_last();
                return false;
            }
            SessionCmd::CopyLast => {
                // Pablo: the copy-last key (Shift+F12) pastes the last transcript AND leaves it on the
                // clipboard, so it is ready to paste anywhere else too.
                if self.m.phase() != Phase::Idle {
                    return false;
                }
                match self.last() {
                    Some(t) => match paste::external_target() {
                        Some(target) => {
                            let mut opts = self.state().settings.read().unwrap().paste.clone();
                            opts.restore_clipboard = false;
                            let msg = match paste::paste(&t, &opts, Some(&target)) {
                                PasteOutcome::Pasted => "Pasted, and it is on your clipboard",
                                _ => "Copied: press ⌘V to paste",
                            };
                            toast(&self.app, None, "copy-last", ToastKind::Info, msg, &[], 2500);
                        }
                        None => {
                            paste::copy(&t);
                            toast(&self.app, None, "copy-last", ToastKind::Info, "Copied: press ⌘V to paste", &[], 2500);
                        }
                    },
                    None => toast(&self.app, None, "copy-last", ToastKind::Info, "Nothing to paste yet", &[], 3000),
                }
                return false;
            }
            SessionCmd::PasteText(t) => {
                // A paste now would break a running dictation (the overlay would show Done while recording).
                if self.m.phase() != Phase::Idle {
                    toast(&self.app, None, "paste-busy", ToastKind::Info, "Finish the dictation first", &[], 3000);
                    return false;
                }
                self.step_aside();
                self.deliver(&t, false, None);
                return false;
            }
            SessionCmd::Shutdown => return false,
        };
        // Dictation: the release pastes at once, a press in the grace continues. Voice control: the recording goes on.
        self.m.cont = !self.command;
        self.act(ev, now)
    }

    /// Esc, or the command key while the queue drains: stop the live session now.
    fn abort_live(&mut self) {
        match &self.live {
            Some(l) => l.abort(),
            None => command::abort(),
        }
    }

    fn act(&mut self, ev: Ev, now: Instant) -> bool {
        let Some(act) = self.m.step(ev, now) else { return false };
        match act {
            Act::Start { locked } => self.start(locked),
            Act::Lock => self.set(Phase::Locked, None),
            Act::Grace { ms } => {
                self.grace = Some(ms);
                self.set(Phase::Recording, None);
            }
            Act::Warn => toast(&self.app, None, "time-left", ToastKind::Warn, "1 minute left", &[], 4000),
            // A lone tap of the command key (no double-tap followed): nothing was said, the session just ends.
            Act::Discard if self.command && !self.answering => {
                self.rec = None;
                self.ctx = None;
                self.cmd_chunker = None;
                match &self.live {
                    Some(l) => {
                        l.end(None);
                        self.set(Phase::Acting, None);
                    }
                    None => {
                        self.command = false;
                        self.set(Phase::Idle, None);
                    }
                }
            }
            Act::Discard | Act::CancelUndoable if self.command => self.drop_command_recording(),
            Act::Discard => {
                (self.stream, self.rt) = (None, None);
                self.rec = None;
                self.ctx = None;
                (self.cont_from, self.cont_row) = (None, None);
                self.set(Phase::Idle, None);
            }
            Act::ProcessKeep => {
                self.pump(); // the last samples before the peek
                let (Some(rec), Some(ctx)) = (self.rec.as_ref(), self.ctx.take()) else {
                    self.m.idle();
                    return true;
                };
                let clip = rec.peek();
                self.cont_from = Some(clip.samples.len());
                (self.cont_row, self.last_row) = (None, None);
                self.process(clip, ctx);
                self.cont_row = self.last_row;
                return true;
            }
            Act::Continue => {
                // The mic never closed: the words said since the release are in the recording already.
                self.idle_at = None;
                let front = paste::target_app();
                let device = self.rec.as_ref().map(|r| r.device().name.clone());
                self.ctx = Some(Ctx { app: front, started_at: history::now_ms(), device });
                self.voice_at = now;
                log::info!("hands-free continuation of the last dictation");
                self.set(Phase::Locked, None);
            }
            Act::CancelUndoable => {
                (self.stream, self.rt) = (None, None);
                if let (Some(rec), Some(ctx)) = (self.rec.take(), self.ctx.take()) {
                    self.kept = Some((rec.stop(), ctx));
                }
                self.set(Phase::Idle, None);
                toast(&self.app, None, "cancelled", ToastKind::Info, "Cancelled", &[("undo", "Undo")], UNDO.as_millis() as u32);
            }
            Act::Process => {
                self.pump(); // the last samples before the stop
                if let (Some(rec), Some(ctx)) = (self.rec.take(), self.ctx.take()) {
                    let mut clip = rec.stop();
                    self.cont_now = match self.cont_from.take() {
                        Some(from) => {
                            clip.samples.drain(..from.min(clip.samples.len()));
                            clip.duration_ms = (clip.samples.len() as u64 * 1000 / audio::RATE as u64) as u32;
                            clip.peak_db = audio::db(clip.samples.iter().fold(0f32, |m, s| m.max(s.abs())));
                            true
                        }
                        None => false,
                    };
                    self.process(clip, ctx);
                    (self.cont_now, self.cont_row) = (false, None);
                } else {
                    self.m.idle();
                }
                return true;
            }
            Act::ProcessKept => {
                match self.kept.take() {
                    Some((clip, ctx)) => {
                        self.step_aside(); // Undo was clicked, maybe in the main window
                        self.process(clip, ctx)
                    }
                    None => self.m.idle(),
                }
                return true;
            }
        }
        false
    }

    fn start(&mut self, locked: bool) {
        self.idle_at = None;
        self.kept = None;
        // A late press after the continuation window: the old mic closes, a new session starts.
        self.rec = None;
        (self.cont_from, self.cont_row) = (None, None);
        if self.recents.is_some() {
            self.close_recents(None);
        }
        (self.watch_at, self.watch_seen, self.voice_at) = (None, false, Instant::now());
        // A live command session needs the key and Accessibility before the mic opens.
        if self.command && !self.answering {
            let instr = String::new();
            let setup = |a: &AppHandle, what: &str| {
                let st = CommandStep { instruction: instr.clone(), step: 1, status: CmdStatus::Setup, message: Some(what.into()), ..Default::default() };
                command::publish(a, st);
            };
            if secrets::get(KeyName::Openrouter).is_none() {
                setup(&self.app, "key");
                self.m.idle();
                self.command = false;
                return self.set(Phase::Idle, None);
            }
            if !crate::permissions::accessibility_trusted() {
                setup(&self.app, "accessibility");
                self.m.idle();
                self.command = false;
                return self.set(Phase::Idle, None);
            }
        }
        let front = paste::target_app(); // a click on the overlay bar activates susurro
        // PLAN 3.2: resolve on a fresh list (about 1 ms), so a mic plugged in under 1.5 s ago counts.
        let Some(dev) = audio::resolve_current(&self.app, true) else {
            self.m.idle();
            self.ctx = None;
            return self.fail("No microphone".into());
        };
        self.ping(Ping::Start);
        #[cfg(target_os = "macos")]
        crate::platform::macos::setup_overlay(&self.app);
        // The mic can vanish between the list and the open: retry once on a fresh list.
        let rec = Recorder::start(&self.app, &dev, "overlay").or_else(|e| match audio::resolve_current(&self.app, true) {
            Some(d) if d.info.id != dev.info.id => Recorder::start(&self.app, &d, "overlay"),
            _ => Err(e),
        });
        match rec {
            Ok(r) => {
                let ctx = Ctx { app: front, started_at: history::now_ms(), device: Some(r.device().name.clone()) };
                self.ctx = Some(ctx.clone());
                self.stream = if self.command { None } else { self.streamer(&r) };
                self.rt = if self.command { None } else { self.realtime(&r) };
                if self.command && !self.answering {
                    let cfg = self.state().settings.read().unwrap().command.clone();
                    let key = secrets::get(KeyName::Openrouter).unwrap_or_default();
                    let ch = r.chunker_with(audio::COMMAND);
                    let rate = ch.rate();
                    self.cmd_chunker = Some(ch);
                    self.cmd_ctx = Some(ctx);
                    self.live = Some(crate::live::start(self.app.clone(), key, cfg, rate));
                    log::info!("live: command session started");
                }
                self.rec = Some(r);
                self.set(if locked { Phase::Locked } else { Phase::Recording }, None);
            }
            Err(e) => {
                self.m.idle();
                self.ctx = None;
                self.fail(format!("{}: {e:#}", dev.info.name));
            }
        }
    }

    /// Streaming needs local whisper with its context already loaded (the preload): else single-shot at the end.
    fn streamer(&self, rec: &Recorder) -> Option<(audio::Chunker, whisper_local::Stream)> {
        let state = self.state();
        let s = state.settings.read().unwrap().clone();
        if s.stt.provider != SttProvider::WhisperLocal {
            return None;
        }
        let (file, note) = whisper_local::usable(&state.data_dir, &s.stt.whisper_model);
        let ctx = state.whisper.try_lock().ok()?.as_ref().filter(|(f, _)| *f == file).map(|(_, c)| c.clone())?;
        let mut d = Dictionary::new(dictionary::load(&state.db.lock().unwrap()).unwrap_or_default(), s.dictionary.clone());
        d.spoken_punctuation = s.spoken_punctuation;
        let mut w = whisper_local::Whisper::new(ctx);
        w.note = note;
        Some((rec.chunker(), whisper_local::Stream::spawn(w, Some(s.language.clone()), d.hints())))
    }

    /// Scribe Realtime dictation: the provider is picked and the ElevenLabs key is saved.
    fn realtime(&self, rec: &Recorder) -> Option<(audio::Chunker, stt::scribe_rt::Live, mpsc::Receiver<stt::scribe_rt::Ev>)> {
        let state = self.state();
        let s = state.settings.read().unwrap().clone();
        if s.stt.provider != SttProvider::ElevenLabsRealtime {
            return None;
        }
        let ch = rec.chunker();
        let hints = Dictionary::new(dictionary::load(&state.db.lock().unwrap()).unwrap_or_default(), s.dictionary.clone()).hints();
        let o = stt::realtime_opts(&s, ch.rate(), &hints, s.stt.elevenlabs.send_keyterms)?;
        let (tx, rx) = mpsc::channel();
        Some((ch, stt::scribe_rt::Live::start(o, move |e| drop(tx.send(e))), rx))
    }

    /// Every new sample to the streams (local whisper chunks, Scribe Realtime, the live command session).
    fn pump(&mut self) {
        if let (Some((ch, st)), Some(rec)) = (self.stream.as_mut(), self.rec.as_ref()) {
            match ch.poll(rec) {
                Ok(Some(chunk)) => st.push(chunk),
                Ok(None) => {}
                Err(()) => {
                    log::info!("whisper stream: the mic changed, the clip goes single-shot");
                    self.stream = None;
                }
            }
        }
        if let (Some((ch, live, _)), Some(rec)) = (self.rt.as_mut(), self.rec.as_ref()) {
            match ch.poll_fresh(rec) {
                Ok((new, cut)) => live.send(&new, cut.is_some()),
                Err(()) => {
                    log::info!("scribe-rt: the mic changed, the clip goes single-shot");
                    self.rt = None;
                }
            }
        }
        // The live command session: each finished chunk (or, on Scribe Realtime, every sample) goes out while
        // Pablo keeps talking.
        if let (Some(ch), Some(rec), Some(live)) = (self.cmd_chunker.as_mut(), self.rec.as_ref(), self.live.as_ref()) {
            let r = if live.realtime() {
                ch.poll_fresh(rec).map(|(new, cut)| {
                    live.audio(new, cut.is_some());
                    None
                })
            } else {
                ch.poll(rec)
            };
            match r {
                Ok(Some(chunk)) => {
                    log::info!("live: chunk {} ms", chunk.len() / 16);
                    live.chunk(chunk);
                }
                Ok(None) => {}
                Err(()) => {
                    log::info!("live: the mic changed, the rest goes with the tail");
                    self.cmd_chunker = None;
                }
            }
        }
    }

    /// The Scribe Realtime text: commit the rest, collect every committed segment. None = failed: single-shot.
    fn finish_realtime(&mut self) -> Option<stt::SttOutput> {
        let (_, live, rx) = self.rt.take()?;
        let t = Instant::now();
        live.finish();
        let mut texts = vec![];
        loop {
            match rx.recv_timeout(RT_FINISH.saturating_sub(t.elapsed())) {
                Ok(stt::scribe_rt::Ev::Committed(c)) if !c.is_empty() => texts.push(c),
                Ok(stt::scribe_rt::Ev::Closed) => break,
                Ok(stt::scribe_rt::Ev::Failed(e)) => {
                    log::warn!("scribe-rt failed, running the clip single-shot: {e}");
                    return None;
                }
                Ok(_) => {}
                Err(_) => {
                    log::warn!("scribe-rt: no final text {} ms after the stop, running the clip single-shot", t.elapsed().as_millis());
                    return None;
                }
            }
        }
        log::info!("scribe-rt: final text {} ms after the stop", t.elapsed().as_millis());
        Some(stt::SttOutput { text: texts.join(" "), language: None, note: None, fell_back: false })
    }

    /// The streaming transcript of `clip`: the chunks so far plus the tail. None = run it single-shot.
    fn finish_stream(&mut self, clip: &Clip) -> Option<stt::SttOutput> {
        if self.rt.is_some() {
            return self.finish_realtime();
        }
        let (ch, st) = self.stream.take()?;
        let t = Instant::now();
        match st.finish(ch.tail(clip)) {
            Ok(o) => {
                log::info!("whisper stream: final text {} ms after the stop", t.elapsed().as_millis());
                Some(o)
            }
            Err(e) => {
                log::warn!("whisper stream failed, running the clip single-shot: {e:#}");
                None
            }
        }
    }

    /// Esc while speaking a command: no Undo toast (Undo would paste it as a dictation). It ends the live session.
    fn drop_command_recording(&mut self) {
        self.rec = None;
        self.ctx = None;
        self.cmd_chunker = None;
        self.answering = false;
        match &self.live {
            Some(l) => {
                l.abort();
                self.set(Phase::Acting, None);
            }
            None => {
                self.command = false;
                self.set(Phase::Idle, None);
            }
        }
    }

    /// The command recording stopped: the tail goes to the live session, which drains its queue (Phase::Acting
    /// until CommandEnded). A pick answer (the session's mic was off) goes to the executor.
    fn process_command(&mut self, clip: Clip, ctx: Ctx) {
        self.ping(Ping::Stop);
        self.m.idle();
        if self.answering {
            // Off the actor thread: Esc must end the session while this STT request is still in flight.
            let _ = ctx;
            self.answering = false;
            let app = self.app.clone();
            std::thread::spawn(move || {
                let text = match command_text(&app.state::<AppState>(), &clip) {
                    Outcome::Text(d) => d.final_text,
                    _ => String::new(),
                };
                command::answer(text);
            });
            return self.set(Phase::Acting, None);
        }
        let tail = self.cmd_chunker.take().and_then(|c| c.tail(&clip));
        match &self.live {
            Some(l) => l.end(tail),
            None => self.command = false,
        }
        self.ctx = None;
        self.set(if self.live.is_some() { Phase::Acting } else { Phase::Idle }, None);
    }

    /// The live session ended: back to Idle, and a History row (kind command) when history is on and words were heard.
    fn command_ended(&mut self, e: command::Ended) {
        (self.answering, self.command) = (false, false);
        self.live = None;
        self.cmd_chunker = None;
        if self.rec.take().is_some() {
            self.ctx = None;
            self.m.idle();
        }
        let ctx = self.cmd_ctx.take();
        let (s, data_dir) = (self.state().settings.read().unwrap().clone(), self.state().data_dir.clone());
        if s.privacy.store_history && !e.instruction.trim().is_empty() {
            let status = command_status(&e.end.status);
            let item = HistoryItem {
                id: 0,
                created_at: ctx.as_ref().map_or_else(history::now_ms, |c| c.started_at),
                kind: HistoryKind::Command,
                app_bundle: ctx.as_ref().and_then(|c| c.app.as_ref().and_then(|a| a.bundle_id.clone())),
                app_name: ctx.as_ref().and_then(|c| c.app.as_ref().and_then(|a| a.name.clone())),
                words: history::words(&e.instruction),
                raw: e.instruction,
                final_text: e.end.summary,
                provider: format!("planner:{}", s.command.planner_model),
                cleanup: None,
                duration_ms: 0,
                latency_ms: e.latency_ms,
                error: e.end.message.filter(|_| status != HistoryStatus::Done),
                status,
                has_audio: false,
            };
            let db = self.state().db.lock().unwrap();
            match history::insert(&db, &data_dir, &item, None) {
                Ok(item) => {
                    let _ = self.app.emit_to("main", "history_added", item);
                }
                Err(e) => log::error!("history insert (command): {e:#}"),
            }
        }
        self.set(Phase::Idle, None);
    }

    // ---------- recents (Ctrl+F12, VOICE_CONTROL_V2 4.1) ----------

    fn toggle_recents(&mut self) {
        if self.recents.is_some() {
            return self.close_recents(None);
        }
        // While a dictation or a command session runs the key does nothing.
        if self.m.phase() != Phase::Idle || self.command || self.live.is_some() {
            return;
        }
        let items: Vec<HistoryItem> = match history::list(&self.state().db.lock().unwrap(), None, None, 50) {
            Ok(v) => v.into_iter().filter(|h| h.kind == HistoryKind::Dictation && !h.final_text.trim().is_empty()).take(9).collect(),
            Err(e) => return log::warn!("recents: {e}"),
        };
        if items.is_empty() {
            return toast(&self.app, None, "recents", ToastKind::Info, "Nothing dictated yet", &[], 2500);
        }
        #[cfg(target_os = "macos")]
        crate::platform::macos::setup_overlay(&self.app);
        let _ = self.app.emit_to("overlay", "recents", Recents { items: items.clone(), picked: None });
        if let Some(h) = self.app.try_state::<crate::hotkey::HotkeyHandle>() {
            h.set_recents(true);
        }
        self.recents = Some((items, Instant::now() + RECENTS_IDLE));
    }

    fn pick_recent(&mut self, n: u32, copy: bool) {
        let Some(item) = self.recents.as_ref().and_then(|(items, _)| recent_at(items, n).cloned()) else { return };
        self.close_recents(Some(n));
        match paste::external_target().filter(|_| !copy) {
            Some(target) => {
                self.deliver(&item.final_text, false, Some(&target));
            }
            None => {
                paste::copy(&item.final_text);
                let msg = if copy { "Copied" } else { "Copied: press ⌘V to paste" };
                toast(&self.app, None, "recents", ToastKind::Info, msg, &[], 2500);
            }
        }
    }

    /// `picked` flashes that row first (the overlay closes the list after it).
    fn close_recents(&mut self, picked: Option<u32>) {
        if self.recents.take().is_none() {
            return;
        }
        if let Some(h) = self.app.try_state::<crate::hotkey::HotkeyHandle>() {
            h.set_recents(false);
        }
        let _ = self.app.emit_to("overlay", "recents", Recents { items: vec![], picked });
    }

    fn process(&mut self, clip: Clip, ctx: Ctx) {
        if self.command {
            return self.process_command(clip, ctx);
        }
        self.ping(Ping::Stop);
        self.ctx = Some(ctx);
        self.set(Phase::Processing, None);
        let t = Instant::now();
        let bundle = self.ctx.as_ref().and_then(|c| c.app.as_ref().and_then(|a| a.bundle_id.clone()));
        let streamed = self.finish_stream(&clip);
        let run = run_pipeline_with(self.state(), &clip, bundle.as_deref(), streamed);
        let (s, data_dir) = (self.state().settings.read().unwrap().clone(), self.state().data_dir.clone());
        let ctx = self.ctx.take().unwrap();
        let row = |raw: String, final_text: String, status, error| HistoryItem {
            id: 0,
            created_at: ctx.started_at,
            app_bundle: ctx.app.as_ref().and_then(|a| a.bundle_id.clone()),
            app_name: ctx.app.as_ref().and_then(|a| a.name.clone()),
            words: history::words(&final_text),
            raw,
            final_text,
            provider: run.provider.clone(),
            cleanup: None,
            duration_ms: clip.duration_ms,
            latency_ms: t.elapsed().as_millis() as u32,
            status,
            has_audio: false,
            error,
            kind: HistoryKind::Dictation,
        };
        match &run.outcome {
            Outcome::Skip(None) => {
                self.m.idle();
                self.set(Phase::Idle, None);
            }
            Outcome::Skip(Some(msg)) => {
                self.m.idle();
                self.fail(msg.clone());
            }
            Outcome::Failed(e) => {
                // Never lose a dictation: the failed row keeps the WAV even with store_audio off.
                // With store_history off (or a failed insert) the clip stays in memory for Retry.
                let item = s.privacy.store_history.then(|| {
                    let db = self.state().db.lock().unwrap();
                    history::insert(&db, &data_dir, &row(String::new(), String::new(), HistoryStatus::Failed, Some(e.clone())), Some(&clip.samples))
                });
                let id = match item {
                    Some(Ok(item)) => {
                        let id = item.id;
                        let _ = self.app.emit_to("main", "history_added", item);
                        Some(id)
                    }
                    Some(Err(err)) => {
                        log::error!("history insert: {err:#}");
                        None
                    }
                    None => None,
                };
                let toast_id = match id {
                    Some(id) => format!("failed:{id}"),
                    None => {
                        self.failed = Some((clip.clone(), ctx.clone()));
                        "failed".into()
                    }
                };
                toast(&self.app, None, &toast_id, ToastKind::Error, format!("Transcription failed: {e}"), &[("retry", "Retry")], 10_000);
                self.m.idle();
                self.fail(short(e));
            }
            Outcome::Text(d) => {
                let latency_ms = t.elapsed().as_millis() as u32; // STT + cleanup, not the paste
                let cont = self.cont_now;
                let mut d = d.clone();
                if cont && d.final_text.chars().next().is_some_and(char::is_alphanumeric) {
                    // The continuation goes right after the previous text (smart spacing cannot see Electron fields).
                    d.final_text = format!(" {}", d.final_text);
                }
                let prev = self.last_text.clone();
                let status = self.deliver_done(&d, ctx.app.as_ref());
                if cont {
                    self.last_text = prev.map(|p| format!("{p}{}", d.final_text));
                }
                let d = &d;
                let mut row_id = None;
                let mut item = row(d.raw.clone(), d.final_text.trim().to_string(), status, None);
                item.latency_ms = latency_ms;
                item.cleanup = run.cleanup(d);
                let db = self.state().db.lock().unwrap();
                if let Err(e) = dictionary::record_uses(&db, &d.used) {
                    log::warn!("record_uses: {e}");
                }
                if s.privacy.store_history {
                    let r = match self.cont_row.filter(|_| cont) {
                        Some(id) => history::append(&db, id, &d.raw, d.final_text.trim(), clip.duration_ms).map_err(anyhow::Error::from).and_then(|i| i.ok_or_else(|| anyhow::anyhow!("row {id} gone"))),
                        None => history::insert(&db, &data_dir, &item, s.privacy.store_audio.then_some(&clip.samples[..])),
                    };
                    match r {
                        Ok(item) => {
                            row_id = Some(item.id);
                            let _ = self.app.emit_to("main", "history_added", item);
                        }
                        Err(e) => log::error!("history insert: {e:#}"),
                    }
                }
                drop(db);
                self.last_row = row_id;
                for n in &d.notes {
                    toast(&self.app, None, "note", ToastKind::Info, n.clone(), &[], 4000);
                }
                self.m.idle();
            }
        }
    }

    /// Paste the pipeline result, press Return if asked, then show Done.
    fn deliver_done(&mut self, d: &Done, target: Option<&AppRef>) -> HistoryStatus {
        let status = self.deliver(&d.final_text, d.press_enter, target);
        self.last_text = Some(d.final_text.clone());
        status
    }

    fn deliver(&mut self, text: &str, press_enter: bool, target: Option<&AppRef>) -> HistoryStatus {
        let opts = self.state().settings.read().unwrap().paste.clone();
        let out = if text.is_empty() { PasteOutcome::Pasted } else { paste::paste(text, &opts, target) };
        let status = match out {
            PasteOutcome::Pasted => {
                if press_enter {
                    std::thread::sleep(Duration::from_millis(100)); // let the target app take the Cmd+V first
                    paste::press_enter();
                }
                HistoryStatus::Pasted
            }
            PasteOutcome::CopiedOnly => {
                let actions = [("fix-ax", "Turn on")];
                toast(&self.app, None, "copied", ToastKind::Warn, "Copied. Turn on Accessibility to paste", &actions, 8000);
                HistoryStatus::Copied
            }
            PasteOutcome::NoField => {
                toast(&self.app, None, "no-field", ToastKind::Info, "No text field. Copied: press ⌘V to paste", &[], 5000);
                HistoryStatus::Copied
            }
        };
        let (msg, hold) = if status == HistoryStatus::Pasted { ("pasted", 700) } else { ("copied", 2500) };
        self.set(Phase::Done, Some(msg.into()));
        self.idle_at = Some(Instant::now() + Duration::from_millis(hold));
        status
    }

    fn paste_last(&mut self) {
        if self.m.phase() != Phase::Idle {
            return;
        }
        match self.last() {
            Some(t) => match paste::external_target() {
                // The overlay's right-click menu or the main window can make susurro the front app.
                Some(target) => {
                    self.deliver(&t, false, Some(&target));
                }
                // Nothing but susurro seen since launch: never paste into susurro itself.
                None => {
                    paste::copy(&t);
                    toast(&self.app, None, "paste-last", ToastKind::Info, "Copied: press ⌘V to paste", &[], 3000);
                }
            },
            None => toast(&self.app, None, "paste-last", ToastKind::Info, "Nothing to paste yet", &[], 3000),
        }
    }

    /// The last transcript: this run's, else the newest history row.
    fn last(&self) -> Option<String> {
        self.last_text.clone().or_else(|| history::latest(&self.state().db.lock().unwrap()).ok().flatten().map(|h| h.final_text))
    }

    /// Toast Retry after a failed STT: re-run the WAV and paste the result into the frontmost app.
    fn retry(&mut self, id: i64) -> bool {
        if self.m.phase() != Phase::Idle {
            return false;
        }
        // The same toast shows in the overlay and the main window: a 2nd Retry click finds the row done.
        let still_failed = history::get(&self.state().db.lock().unwrap(), id).ok().flatten().is_some_and(|h| h.status == HistoryStatus::Failed);
        if !still_failed {
            return false;
        }
        self.step_aside();
        self.set(Phase::Processing, None);
        match history::retry(&self.app, id) {
            Ok(item) => {
                let status = self.deliver(&item.final_text, false, None);
                self.last_text = Some(item.final_text.clone());
                let db = self.state().db.lock().unwrap();
                if let Err(e) = history::set_status(&db, id, &status) {
                    log::error!("history status {id}: {e}");
                }
                if let Ok(Some(item)) = history::get(&db, id) {
                    let _ = self.app.emit_to("main", "history_added", item);
                }
            }
            Err(e) => self.fail(short(&format!("{e:#}"))),
        }
        true
    }

    /// Toast Retry for a failed dictation that has no history row: run the kept clip again.
    fn retry_failed(&mut self) -> bool {
        if self.m.phase() != Phase::Idle {
            return false;
        }
        let Some((clip, ctx)) = self.failed.take() else { return false };
        self.step_aside();
        self.process(clip, ctx);
        true
    }

    /// The stuck-key watchdog and the silence stop (VOICE_CONTROL_V2 4.2): a lost release or 30 s of silence
    /// ends the recording.
    fn watchdog(&mut self, now: Instant) -> Option<Ev> {
        if let Some(rec) = self.rec.as_ref().filter(|_| matches!(self.m.phase(), Phase::Recording | Phase::Locked)) {
            if rec.tail_peak_db(100) >= audio::SILENCE_DB {
                self.voice_at = now;
            } else if now - self.voice_at >= SILENCE_STOP {
                log::info!("silence stop: no voice for {} s", SILENCE_STOP.as_secs());
                toast(&self.app, None, "silence", ToastKind::Info, "Stopped after 30 s of silence", &[], 4000);
                self.voice_at = now;
                return Some(Ev::Stop);
            }
        }
        let down = self.m.holding()?;
        if self.watch_off || now - down < WATCH_EVERY || self.watch_at.is_some_and(|t| now - t < WATCH_EVERY) {
            return None;
        }
        self.watch_at = Some(now);
        let binding = {
            let s = self.state().settings.read().unwrap();
            if self.command { s.hotkeys.command.clone() } else { s.hotkeys.ptt.clone() }
        };
        match watch(self.watch_seen, key_down(&binding)) {
            Watch::Fine => {
                self.watch_seen = true;
                None
            }
            Watch::Blind => {
                log::info!("watchdog: {binding} reads up on the first poll; this process cannot see the key, watchdog off");
                self.watch_off = true;
                None
            }
            Watch::Lost => {
                log::warn!("watchdog: release lost, {binding} is up ({} ms into the hold)", (now - down).as_millis());
                Some(Ev::Up)
            }
        }
    }

    /// Returns true if a max-length stop ran the (blocking) pipeline.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.pump();
        if self.recents.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.close_recents(None);
        }
        if let Some(ev) = self.watchdog(now) {
            if self.act(ev, now) {
                return true;
            }
        }
        let busy = self.act(Ev::Tick, now);
        if self.idle_at.is_some_and(|t| now >= t) {
            self.idle_at = None;
            if self.m.phase() == Phase::Idle && self.live.is_none() {
                self.set(Phase::Idle, None);
            }
        }
        if self.last_sweep.is_none_or(|t| now - t >= RETENTION_EVERY) {
            self.last_sweep = Some(now);
            let state = self.state();
            let days = state.settings.read().unwrap().privacy.audio_retention_days;
            match history::sweep(&state.db.lock().unwrap(), &state.data_dir, days, history::now_ms()) {
                Ok(n) if n > 0 => log::info!("retention: deleted {n} WAVs older than {days} days"),
                Ok(_) => {}
                Err(e) => log::warn!("retention sweep: {e}"),
            }
        }
        busy
    }
}

/// The start / stop / error sound (platform::macos::ping plays it).
#[derive(Clone, Copy)]
pub enum Ping {
    Start,
    Stop,
    Error,
}

/// The pill shows ~300 px of text: first line, 80 chars.
fn short(e: &str) -> String {
    let l = e.lines().next().unwrap_or(e);
    if l.chars().count() > 80 { format!("{}…", l.chars().take(79).collect::<String>()) } else { l.to_string() }
}

// ---------- commands ----------

fn send(state: &AppState, cmd: SessionCmd) -> Result<(), String> {
    state.session_tx.send(cmd).map_err(|_| "session is not running".to_string())
}

#[tauri::command]
pub fn dictation_toggle(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, SessionCmd::Toggle)
}

#[tauri::command]
pub fn dictation_stop(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, SessionCmd::Stop)
}

#[tauri::command]
pub fn dictation_cancel(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, SessionCmd::Cancel)
}

#[tauri::command]
pub fn paste_last(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, SessionCmd::PasteLast)
}

#[tauri::command]
pub fn get_session_state() -> SessionState {
    CURRENT.lock().unwrap().clone()
}

#[tauri::command]
pub fn toast_action(state: State<'_, AppState>, toast_id: String, action_id: String) -> Result<(), String> {
    send(&state, SessionCmd::ToastAction { toast_id, action_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn keys_dropped_during_the_command_stt_still_stop_it() {
        assert!(stops_command(true, &SessionCmd::Cancel) && stops_command(true, &SessionCmd::CmdDown));
        assert!(!stops_command(true, &SessionCmd::PttDown) && !stops_command(true, &SessionCmd::CmdUp));
        assert!(!stops_command(false, &SessionCmd::Cancel), "a dictation: dropped as before");
    }

    fn m() -> (Machine, Instant) {
        (Machine::new(true, Duration::from_secs(600)), Instant::now())
    }

    #[test]
    fn hold_records_and_processes() {
        let (mut m, t) = m();
        assert_eq!(m.step(Ev::Down, t), Some(Act::Start { locked: false }));
        assert_eq!(m.phase(), Phase::Recording);
        assert_eq!(m.step(Ev::Up, t + MS(1200)), Some(Act::Process));
        assert_eq!(m.phase(), Phase::Processing);
        m.idle();
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn double_tap_locks_and_next_press_stops() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(120)), None, "short tap waits for a 2nd tap");
        assert_eq!(m.step(Ev::Down, t + MS(400)), Some(Act::Lock));
        assert_eq!(m.phase(), Phase::Locked);
        assert_eq!(m.step(Ev::Up, t + MS(500)), None, "the 2nd tap's release does not stop");
        assert_eq!(m.step(Ev::Tick, t + MS(5000)), None);
        assert_eq!(m.step(Ev::Down, t + MS(9000)), Some(Act::Process));
    }

    #[test]
    fn single_short_tap_is_discarded_after_the_window() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(100));
        assert_eq!(m.step(Ev::Tick, t + MS(400)), None);
        assert_eq!(m.step(Ev::Tick, t + MS(700)), Some(Act::Discard));
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn short_tap_without_double_tap_processes() {
        let (mut m, t) = m();
        m.double_tap = false;
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(100)), Some(Act::Process), "the pipeline drops it as too short");
    }

    #[test]
    fn toggle_and_overlay_click() {
        let (mut m, t) = m();
        assert_eq!(m.step(Ev::Toggle, t), Some(Act::Start { locked: true }));
        assert_eq!(m.phase(), Phase::Locked);
        assert_eq!(m.step(Ev::Toggle, t + MS(3000)), Some(Act::Process));
        // hands-free combo while holding PTT locks
        m.idle();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Toggle, t + MS(800)), Some(Act::Lock));
    }

    #[test]
    fn busy_guard_ignores_input() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(1000));
        for ev in [Ev::Down, Ev::Up, Ev::Toggle, Ev::Cancel, Ev::Tick, Ev::Undo, Ev::Stop] {
            assert_eq!(m.step(ev, t + MS(1100)), None, "{ev:?} while processing");
            assert_eq!(m.phase(), Phase::Processing);
        }
    }

    #[test]
    fn max_length_warns_then_stops() {
        let (mut m, t) = m();
        m.max = Duration::from_secs(120);
        m.step(Ev::Toggle, t);
        assert_eq!(m.step(Ev::Tick, t + Duration::from_secs(59)), None);
        assert_eq!(m.step(Ev::Tick, t + Duration::from_secs(60)), Some(Act::Warn));
        assert_eq!(m.step(Ev::Tick, t + Duration::from_secs(61)), None, "warn once");
        assert_eq!(m.step(Ev::Tick, t + Duration::from_secs(120)), Some(Act::Process));
        // a new session warns again
        m.idle();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Tick, t + Duration::from_secs(61)), Some(Act::Warn));
    }

    #[test]
    fn cancel_then_undo_within_5s() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Cancel, t + MS(2000)), Some(Act::CancelUndoable));
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.step(Ev::Undo, t + MS(6000)), Some(Act::ProcessKept));
        assert_eq!(m.phase(), Phase::Processing);
        m.idle();
        assert_eq!(m.step(Ev::Undo, t + MS(6500)), None, "undo only once");
    }

    #[test]
    fn undo_expires() {
        let (mut m, t) = m();
        m.step(Ev::Toggle, t);
        m.step(Ev::Cancel, t + MS(2000));
        assert_eq!(m.step(Ev::Undo, t + MS(7001)), None);
    }

    #[test]
    fn fn_chord_cancel_is_silent() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Cancel, t + MS(80)), Some(Act::Discard));
        assert_eq!(m.step(Ev::Undo, t + MS(500)), None);
    }

    #[test]
    fn device_gone_stops_and_processes() {
        let (mut m, t) = m();
        m.step(Ev::Toggle, t);
        assert_eq!(m.step(Ev::Stop, t + MS(3000)), Some(Act::Process));
        assert_eq!(m.step(Ev::Stop, t + MS(3100)), None);
    }

    #[test]
    fn overlay_stop_while_holding_processes_instead_of_locking() {
        let (mut m, t) = m();
        m.step(Ev::Down, t);
        assert_eq!(m.clone().step(Ev::Toggle, t + MS(1000)), Some(Act::Lock));
        assert_eq!(m.step(Ev::Stop, t + MS(1000)), Some(Act::Process));
    }

    #[test]
    fn failed_stt_keeps_audio_and_writes_a_failed_row() {
        // The actor writes this row on Outcome::Failed; check the history side of that contract.
        let dir = std::env::temp_dir().join(format!("susurro-session-failed-{}", std::process::id()));
        let c = crate::db::open_memory().unwrap();
        let clip = Clip { samples: vec![0.2; 16000], duration_ms: 1000, peak_db: -14.0, device_names: vec!["Mic".into()] };
        let failed = pipeline::run(
            &clip,
            &pipeline::Opts { language: None, style: crate::contract::Style::Formal, raw_app: false, press_enter_command: false, strip_inverted: false, slash: None },
            &Dictionary::new(vec![], Default::default()),
            &|_: &SttRequest| anyhow::bail!("503 upstream"),
            None,
        );
        let Outcome::Failed(e) = failed else { panic!("{failed:?}") };
        let row = HistoryItem {
            id: 0,
            created_at: 1,
            app_bundle: None,
            app_name: None,
            raw: String::new(),
            final_text: String::new(),
            provider: "openrouter:x".into(),
            cleanup: None,
            duration_ms: 1000,
            words: 0,
            latency_ms: 1,
            status: HistoryStatus::Failed,
            has_audio: false,
            error: Some(e),
            kind: crate::contract::HistoryKind::Dictation,
        };
        let item = history::insert(&c, &dir, &row, Some(&clip.samples)).unwrap();
        assert_eq!(item.status, HistoryStatus::Failed);
        assert!(item.has_audio);
        assert_eq!(history::read_wav(&history::audio_dir(&dir).join(format!("{}.wav", item.id))).unwrap().len(), 16000);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---------- hands-free grace after a hold ----------

    /// A 2 s hold, released at t+2000, with a 400 ms grace.
    fn released(t: Instant) -> Machine {
        let mut m = Machine::new(true, Duration::from_secs(600));
        m.grace = MS(400);
        assert_eq!(m.step(Ev::Down, t), Some(Act::Start { locked: false }));
        assert_eq!(m.step(Ev::Up, t + MS(2000)), Some(Act::Grace { ms: 400 }));
        assert_eq!(m.phase(), Phase::Recording, "the recorder keeps running in the grace window");
        m
    }

    #[test]
    fn grace_window_timing_table() {
        let t = Instant::now();
        // (press after the release, ticks before it) -> what the press does
        let cases: [(u64, &[u64], Option<Act>, Phase); 6] = [
            (0, &[], Some(Act::Lock), Phase::Locked),
            (200, &[100], Some(Act::Lock), Phase::Locked),
            (399, &[100, 200, 300], Some(Act::Lock), Phase::Locked),
            // 400 = the window is over: the press ends the grace (tick not run yet) and is not a lock
            (400, &[], Some(Act::Process), Phase::Processing),
            (401, &[], Some(Act::Process), Phase::Processing),
            // the tick at 400 already processed: the late press is a busy no-op
            (401, &[100, 200, 300, 400], None, Phase::Processing),
        ];
        for (at, ticks, want, phase) in cases {
            let mut m = released(t);
            for &k in ticks {
                let a = m.step(Ev::Tick, t + MS(2000 + k));
                assert_eq!(a, (k >= 400).then_some(Act::Process), "tick {k} for press {at}");
            }
            assert_eq!(m.step(Ev::Down, t + MS(2000 + at)), want, "press {at} ms after the release");
            assert_eq!(m.phase(), phase, "press {at}");
        }
    }

    #[test]
    fn no_press_in_the_grace_processes_once() {
        let t = Instant::now();
        let mut m = released(t);
        assert_eq!(m.step(Ev::Tick, t + MS(2399)), None);
        assert_eq!(m.step(Ev::Tick, t + MS(2400)), Some(Act::Process));
        assert_eq!(m.step(Ev::Tick, t + MS(2500)), None);
        assert_eq!(m.step(Ev::Up, t + MS(2500)), None, "a stray release does nothing");
    }

    #[test]
    fn release_press_release_stays_hands_free_until_the_next_press() {
        let t = Instant::now();
        let mut m = released(t);
        assert_eq!(m.step(Ev::Down, t + MS(2150)), Some(Act::Lock));
        assert_eq!(m.step(Ev::Up, t + MS(2300)), None, "the locking press's release does not stop");
        assert_eq!(m.step(Ev::Tick, t + MS(9000)), None, "no grace timer left in hands-free");
        assert_eq!(m.phase(), Phase::Locked);
        assert_eq!(m.step(Ev::Down, t + MS(12_000)), Some(Act::Process));
    }

    #[test]
    fn a_hold_in_the_grace_ends_at_its_release() {
        // Let go too early, grab the key again and keep talking: the second release ends it (no stuck hands-free).
        let t = Instant::now();
        let mut m = released(t);
        assert_eq!(m.step(Ev::Down, t + MS(2100)), Some(Act::Lock));
        assert_eq!(m.step(Ev::Down, t + MS(2200)), None, "key repeat");
        assert_eq!(m.step(Ev::Up, t + MS(4000)), Some(Act::Process), "a hold, not a tap: process");
        // the regrab keeps Esc and the max
        let mut m = released(t);
        m.step(Ev::Down, t + MS(2100));
        assert_eq!(m.step(Ev::Cancel, t + MS(3000)), Some(Act::CancelUndoable));
        let mut m = Machine::new(true, Duration::from_secs(10));
        m.grace = MS(400);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(5000));
        m.step(Ev::Down, t + MS(5100));
        assert_eq!(m.step(Ev::Tick, t + MS(10_000)), Some(Act::Process));
    }

    #[test]
    fn esc_stop_and_toggle_in_the_grace() {
        let t = Instant::now();
        let mut m = released(t);
        assert_eq!(m.step(Ev::Cancel, t + MS(2100)), Some(Act::CancelUndoable), "Esc in the window: undoable cancel, not the silent chord");
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.step(Ev::Undo, t + MS(3000)), Some(Act::ProcessKept));
        let mut m = released(t);
        assert_eq!(m.step(Ev::Stop, t + MS(2100)), Some(Act::Process), "overlay Stop / mic lost: process now");
        let mut m = released(t);
        assert_eq!(m.step(Ev::Toggle, t + MS(2100)), Some(Act::Lock), "the hands-free combo locks too");
    }

    #[test]
    fn max_length_inside_the_grace_processes() {
        let t = Instant::now();
        let mut m = Machine::new(true, Duration::from_secs(10));
        m.grace = MS(800);
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(9800)), Some(Act::Grace { ms: 800 }));
        assert_eq!(m.step(Ev::Tick, t + MS(9900)), None);
        assert_eq!(m.step(Ev::Tick, t + MS(10_000)), Some(Act::Process), "the max wins over the 800 ms window");
        // locked from the grace: the max still counts from the first press
        let mut m = Machine::new(true, Duration::from_secs(10));
        m.grace = MS(400);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(5000));
        m.step(Ev::Down, t + MS(5100));
        assert_eq!(m.step(Ev::Tick, t + MS(10_000)), Some(Act::Process));
    }

    #[test]
    fn grace_zero_and_double_tap_from_idle_are_unchanged() {
        let t = Instant::now();
        let mut m = Machine::new(true, Duration::from_secs(600));
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(2000)), Some(Act::Process), "grace 0 = process at the release");
        m.idle();
        m.grace = MS(400);
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(100)), None, "a short tap is the double-tap path, not the grace");
        assert_eq!(m.step(Ev::Down, t + MS(450)), Some(Act::Lock));
        let mut m = Machine::new(true, Duration::from_secs(600));
        m.grace = MS(400);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(100));
        assert_eq!(m.step(Ev::Tick, t + MS(600)), Some(Act::Discard), "a lone short tap still discards");
    }

    // ---------- vc-feedback 12 + 17: the continuation window, the watchdog, recents ----------

    fn dictation(grace: u64) -> (Machine, Instant) {
        let mut m = Machine::new(true, Duration::from_secs(600));
        (m.grace, m.cont) = (MS(grace), true);
        (m, Instant::now())
    }

    #[test]
    fn machine_grace_default_and_clamp() {
        assert_eq!(crate::contract::Hotkeys::default().hands_free_grace_ms, 1000, "vc-feedback 17: 1000 ms");
    }

    #[test]
    fn release_pastes_at_once_and_a_press_in_the_window_continues() {
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(2000)), Some(Act::ProcessKeep), "no wait at the release");
        assert_eq!(m.phase(), Phase::Processing);
        m.idle(); // the paste is done
        assert_eq!(m.phase(), Phase::Idle, "the pill shows done, the mic stays open");
        assert!(m.continuing());
        // the press came 600 ms after the release (it waited in the busy queue): hands-free continuation
        assert_eq!(m.step(Ev::Down, t + MS(2600)), Some(Act::Continue));
        assert_eq!(m.step(Ev::Up, t + MS(2700)), None, "a tap: stays hands-free");
        assert_eq!(m.phase(), Phase::Locked);
        assert_eq!(m.step(Ev::Down, t + MS(9000)), Some(Act::Process), "the next press ends it");
        m.idle();
        assert!(!m.continuing(), "a hands-free stop opens no new window");
    }

    #[test]
    fn the_window_closes_and_a_late_press_is_a_new_session() {
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(2000));
        m.idle();
        assert_eq!(m.step(Ev::Tick, t + MS(2999)), None);
        assert_eq!(m.step(Ev::Tick, t + MS(3000)), Some(Act::Discard), "1000 ms after the release the mic closes");
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.step(Ev::Down, t + MS(3100)), Some(Act::Start { locked: false }));
        // tick not run yet: a late press starts fresh, not a continuation
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(2000));
        m.idle();
        assert_eq!(m.step(Ev::Down, t + MS(3500)), Some(Act::Start { locked: false }));
        // Esc in the window closes the mic silently
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(2000));
        m.idle();
        assert_eq!(m.step(Ev::Cancel, t + MS(2100)), Some(Act::Discard));
        // a hold in the window: its release processes the continuation
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(2000));
        m.idle();
        assert_eq!(m.step(Ev::Down, t + MS(2300)), Some(Act::Continue));
        assert_eq!(m.step(Ev::Up, t + MS(5000)), Some(Act::Process));
    }

    #[test]
    fn double_tap_window_follows_grace() {
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(100)), None);
        assert_eq!(m.step(Ev::Tick, t + MS(1000)), None, "500 ms was the old window");
        assert_eq!(m.step(Ev::Down, t + MS(1050)), Some(Act::Lock), "tap-tap within the grace locks");
        let (mut m, t) = dictation(0);
        m.step(Ev::Down, t);
        m.step(Ev::Up, t + MS(100));
        assert_eq!(m.step(Ev::Tick, t + MS(600)), Some(Act::Discard), "grace off: the 500 ms window");
    }

    #[test]
    fn command_key_double_tap_starts_a_live_session() {
        // voice control: cont off, double-tap on (the v1 hold-only override is gone)
        let mut m = Machine::new(true, Duration::from_secs(600));
        m.grace = MS(1000);
        let t = Instant::now();
        assert_eq!(m.step(Ev::Down, t), Some(Act::Start { locked: false }));
        m.step(Ev::Up, t + MS(120));
        assert_eq!(m.step(Ev::Down, t + MS(400)), Some(Act::Lock), "F11 double-tap = hands-free live session");
        assert_eq!(m.step(Ev::Down, t + MS(9000)), Some(Act::Process));
        // a hold keeps recording through the grace (chunks go on to the planner)
        m.idle();
        m.step(Ev::Down, t);
        assert_eq!(m.step(Ev::Up, t + MS(3000)), Some(Act::Grace { ms: 1000 }));
        assert_eq!(m.phase(), Phase::Recording);
    }

    #[test]
    fn watchdog_table() {
        assert_eq!(watch(false, Some(true)), Watch::Fine);
        assert_eq!(watch(true, Some(false)), Watch::Lost, "watchdog_feeds_up_when_key_reads_up");
        assert_eq!(watch(false, Some(false)), Watch::Blind, "watchdog_turns_off_when_first_poll_reads_up");
        assert_eq!(watch(true, None), Watch::Fine, "no keycode: nothing to say");
        // watchdog_ignores_locked_and_grace: only a hold is polled
        let (mut m, t) = dictation(1000);
        m.step(Ev::Down, t);
        assert_eq!(m.holding(), Some(t));
        m.step(Ev::Up, t + MS(100));
        assert_eq!(m.holding(), None, "tap wait");
        m.step(Ev::Down, t + MS(300));
        assert_eq!((m.phase(), m.holding()), (Phase::Locked, None));
        let mut c = Machine::new(true, Duration::from_secs(600));
        c.grace = MS(1000);
        c.step(Ev::Down, t);
        c.step(Ev::Up, t + MS(2000));
        assert_eq!(c.holding(), None, "grace");
    }

    #[test]
    fn recents_digit_maps_to_row() {
        let item = |id| HistoryItem {
            id,
            created_at: 0,
            kind: HistoryKind::Dictation,
            app_bundle: None,
            app_name: None,
            raw: String::new(),
            final_text: format!("t{id}"),
            provider: String::new(),
            cleanup: None,
            duration_ms: 0,
            words: 1,
            latency_ms: 0,
            status: HistoryStatus::Pasted,
            has_audio: false,
            error: None,
        };
        let items: Vec<HistoryItem> = (1..=4).map(item).collect();
        assert_eq!(recent_at(&items, 1).map(|h| h.id), Some(1));
        assert_eq!(recent_at(&items, 4).map(|h| h.id), Some(4));
        assert_eq!(recent_at(&items, 5), None, "fewer than 9 rows");
        assert_eq!(recent_at(&items, 0), None);
    }

    #[test]
    fn short_error_for_the_pill() {
        assert_eq!(short("a\nb"), "a");
        assert_eq!(short(&"x".repeat(100)).chars().count(), 80);
    }
}
