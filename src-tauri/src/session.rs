//! WP5: the dictation actor thread (state machine + pipeline). Only owner of the Recorder (cpal Stream is not Send).
//! PLAN 4.2. The pure `Machine` decides; the `Actor` does the side effects (recorder, pipeline, paste, history, events).

use crate::AppState;
use crate::audio::{self, Clip, Recorder};
use crate::cleanup;
use crate::command;
use crate::contract::{CleanupLevel, CmdStatus, CommandStep, HistoryItem, HistoryKind, HistoryStatus, KeyName, Phase, SessionState, SttProvider, Toast, ToastActionDef, ToastKind};
use crate::dictionary::{self, Dictionary, Hints};
use crate::history;
use crate::paste::{self, AppRef, PasteOutcome};
use crate::pipeline::{self, Done, Outcome};
use crate::secrets;
use crate::stt::{self, SttRequest};
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
    /// Voice control key (hotkeys.command): hold = speak a command; while a step waits: confirm / answer.
    CmdDown,
    CmdUp,
    /// The command thread finished (command.rs).
    CommandEnded(Box<crate::command::Ended>),
    Shutdown,
}

// ---------- pure state machine (PLAN 4.2) ----------

/// A hold shorter than this is a tap (double-tap candidate).
const TAP: Duration = Duration::from_millis(250);
/// Window for the 2nd tap.
const DOUBLE: Duration = Duration::from_millis(500);
/// A Cancel this soon after PttDown is the fn+key chord (hotkey.rs): silent, no Undo.
const CHORD: Duration = crate::hotkey::CHORD_WINDOW;
const UNDO: Duration = Duration::from_secs(5);
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
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum St {
    Idle,
    /// Holding PTT.
    Rec { start: Instant, down: Instant },
    /// Released after a short tap: wait for the 2nd tap (the recorder keeps running).
    TapWait { start: Instant, until: Instant },
    Locked { start: Instant },
    Busy,
}

#[derive(Debug, Clone)]
pub struct Machine {
    st: St,
    pub double_tap: bool,
    pub max: Duration,
    warned: bool,
    undo_until: Option<Instant>,
}

impl Machine {
    pub fn new(double_tap: bool, max: Duration) -> Self {
        Self { st: St::Idle, double_tap, max, warned: false, undo_until: None }
    }

    pub fn phase(&self) -> Phase {
        match self.st {
            St::Idle => Phase::Idle,
            St::Rec { .. } | St::TapWait { .. } => Phase::Recording,
            St::Locked { .. } => Phase::Locked,
            St::Busy => Phase::Processing,
        }
    }

    /// The pipeline finished (or a start failed).
    pub fn idle(&mut self) {
        self.st = St::Idle;
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
            (Rec { start, down }, Ev::Up) if self.double_tap && now - down < TAP => (TapWait { start, until: now + DOUBLE }, None),
            (Rec { .. } | TapWait { .. } | Locked { .. }, Ev::Stop) => (Busy, Some(Act::Process)),
            (Rec { .. }, Ev::Up) => (Busy, Some(Act::Process)),
            (Rec { start, .. } | TapWait { start, .. }, Ev::Toggle) | (TapWait { start, .. }, Ev::Down) => (Locked { start }, Some(Act::Lock)),
            (TapWait { until, .. }, Ev::Tick) if now >= until => (Idle, Some(Act::Discard)),
            (Locked { .. }, Ev::Toggle | Ev::Down) => (Busy, Some(Act::Process)),
            (Rec { down, .. }, Ev::Cancel) if now - down < CHORD => (Idle, Some(Act::Discard)),
            (Rec { .. } | TapWait { .. } | Locked { .. }, Ev::Cancel) => {
                self.undo_until = Some(now + UNDO);
                (Idle, Some(Act::CancelUndoable))
            }
            (Rec { start, .. } | Locked { start }, Ev::Tick) => {
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
        if matches!(act, Some(Act::Start { .. })) {
            self.warned = false;
        }
        self.st = st;
        act
    }
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
    }
}

/// Section 0 with the current settings and dictionary. Blocking (STT, LLM).
pub fn run_pipeline(state: &AppState, clip: &Clip, bundle: Option<&str>) -> Run {
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
    let stt_fn = |r: &SttRequest| stt::build(&s, state)?.transcribe(r);
    let clean_fn = |t: &str, h: &Hints| cleanup::cleanup(&s, key.as_deref(), &level, t, h);
    let clean: pipeline::Cleaner = (level != CleanupLevel::None).then_some(&clean_fn as &dyn Fn(&str, &Hints) -> anyhow::Result<String>);
    let opts = pipeline::Opts {
        language: Some(s.language.as_str()),
        style: s.styles.style.clone(),
        raw_app,
        press_enter_command: s.paste.press_enter_command,
    };
    let outcome = pipeline::run(clip, &opts, &d, &stt_fn, clean);
    let fell_back = matches!(&outcome, Outcome::Text(d) if d.fell_back);
    Run { outcome, provider: provider_label(&s, fell_back), level }
}

/// A voice command's text: the pipeline STT + dictionary corrections, no cleanup LLM, no spoken punctuation.
fn command_text(state: &AppState, clip: &Clip) -> Outcome {
    let s = state.settings.read().unwrap().clone();
    let entries = dictionary::load(&state.db.lock().unwrap()).unwrap_or_default();
    let d = Dictionary::new(entries, s.dictionary.clone());
    let stt_fn = |r: &SttRequest| stt::build(&s, state)?.transcribe(r);
    let opts = pipeline::Opts { language: Some(s.language.as_str()), style: s.styles.style.clone(), raw_app: true, press_enter_command: false };
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
    /// The current recording / processing / acting belongs to a voice command.
    command: bool,
    /// The command thread runs (Phase::Acting). The machine is idle meanwhile.
    acting: bool,
    /// This recording answers the runner's "Which one?".
    answering: bool,
    /// The app and start time of the running command (its History row).
    cmd_ctx: Option<Ctx>,
}

/// Start the actor: std::thread with a recv_timeout(100ms) loop. lib.rs setup already calls this.
pub fn spawn(app: AppHandle, rx: mpsc::Receiver<SessionCmd>) {
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let mut a = Actor { app, m: Machine::new(true, Duration::from_secs(600)), rec: None, ctx: None, kept: None, failed: None, idle_at: None, last_text: None, last_sweep: None, command: false, acting: false, answering: false, cmd_ctx: None };
            // Busy guard: hotkey input that queued up while the (blocking) pipeline ran is dropped.
            // Exception: Esc / F2 pressed during a command's STT still stop the command it starts.
            let drain = |a: &mut Actor, rx: &mpsc::Receiver<SessionCmd>| {
                for c in rx.try_iter().collect::<Vec<_>>() {
                    if !is_hotkey_input(&c) {
                        a.handle(c);
                    } else if stops_command(a.acting, &c) {
                        command::abort();
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

/// A dropped key that still stops the command the pipeline just started (ABORT is reset at its spawn, before the drain).
fn stops_command(acting: bool, c: &SessionCmd) -> bool {
    acting && matches!(c, SessionCmd::Cancel | SessionCmd::CmdDown)
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

    /// Returns true if the command ran the (blocking) pipeline.
    fn handle(&mut self, cmd: SessionCmd) -> bool {
        let now = Instant::now();
        let (dt, max) = {
            let s = self.state().settings.read().unwrap();
            (s.hotkeys.double_tap, Duration::from_secs(s.recording.max_seconds.max(1) as u64))
        };
        (self.m.double_tap, self.m.max) = (dt, max);
        // A command (recording or acting) owns the session: the dictation keys wait.
        if self.command && matches!(cmd, SessionCmd::PttDown | SessionCmd::PttUp | SessionCmd::Toggle) {
            return false;
        }
        let ev = match cmd {
            SessionCmd::CmdDown => match (self.acting, command::waiting()) {
                (true, Some(command::Wait::Confirm)) => {
                    command::confirm();
                    return false;
                }
                (true, Some(command::Wait::Pick)) if self.m.phase() == Phase::Idle => {
                    self.answering = true;
                    Ev::Down
                }
                (true, Some(command::Wait::Pick)) => return false,
                // F2 while a command acts = stop it
                (true, None) => {
                    command::abort();
                    return false;
                }
                (false, _) if self.m.phase() == Phase::Idle && self.state().settings.read().unwrap().command.enabled => {
                    self.command = true;
                    Ev::Down
                }
                _ => return false,
            },
            SessionCmd::CmdUp if self.command && (!self.acting || self.answering) => Ev::Up,
            SessionCmd::CmdUp => return false,
            SessionCmd::Cancel if self.acting && self.m.phase() == Phase::Idle => {
                command::abort();
                return false;
            }
            SessionCmd::CommandEnded(e) => {
                self.command_ended(*e);
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
                if audio::handle_toast_action(&self.app, &toast_id, &action_id) {
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
                match self.last() {
                    Some(t) => {
                        paste::copy(&t);
                        toast(&self.app, None, "copy-last", ToastKind::Info, "Copied the last transcript", &[], 2500);
                    }
                    None => toast(&self.app, None, "copy-last", ToastKind::Info, "Nothing to copy yet", &[], 3000),
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
        if self.command {
            self.m.double_tap = false; // a command is hold-only
        }
        self.act(ev, now)
    }

    fn act(&mut self, ev: Ev, now: Instant) -> bool {
        let Some(act) = self.m.step(ev, now) else { return false };
        match act {
            Act::Start { locked } => self.start(locked),
            Act::Lock => self.set(Phase::Locked, None),
            Act::Warn => toast(&self.app, None, "time-left", ToastKind::Warn, "1 minute left", &[], 4000),
            Act::Discard | Act::CancelUndoable if self.command => self.drop_command_recording(),
            Act::Discard => {
                self.rec = None;
                self.ctx = None;
                self.set(Phase::Idle, None);
            }
            Act::CancelUndoable => {
                if let (Some(rec), Some(ctx)) = (self.rec.take(), self.ctx.take()) {
                    self.kept = Some((rec.stop(), ctx));
                }
                self.set(Phase::Idle, None);
                toast(&self.app, None, "cancelled", ToastKind::Info, "Cancelled", &[("undo", "Undo")], UNDO.as_millis() as u32);
            }
            Act::Process => {
                if let (Some(rec), Some(ctx)) = (self.rec.take(), self.ctx.take()) {
                    self.process(rec.stop(), ctx);
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
                self.ctx = Some(Ctx { app: front, started_at: history::now_ms(), device: Some(r.device().name.clone()) });
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

    /// Esc while speaking a command: no Undo toast (Undo would paste it as a dictation). During a pick it stops the run.
    fn drop_command_recording(&mut self) {
        self.rec = None;
        self.ctx = None;
        if self.acting {
            self.answering = false;
            command::abort();
            self.set(Phase::Acting, None);
        } else {
            self.command = false;
            self.set(Phase::Idle, None);
        }
    }

    /// STT the spoken command, then run it on the command thread (Phase::Acting until CommandEnded).
    fn process_command(&mut self, clip: Clip, ctx: Ctx) {
        self.ping(Ping::Stop);
        self.ctx = Some(ctx);
        self.set(Phase::Processing, None);
        let outcome = command_text(self.state(), &clip);
        self.m.idle();
        let ctx = self.ctx.take().unwrap();
        let text = match outcome {
            Outcome::Text(d) => Some(d.final_text),
            Outcome::Skip(None) => None,
            Outcome::Skip(Some(m)) | Outcome::Failed(m) if !self.answering => {
                self.command = false;
                return self.fail(short(&m));
            }
            Outcome::Skip(Some(_)) | Outcome::Failed(_) => None,
        };
        if self.answering {
            self.answering = false;
            command::answer(text.unwrap_or_default());
            return self.set(Phase::Acting, None);
        }
        let Some(instr) = text.filter(|t| !t.trim().is_empty()) else {
            self.command = false;
            return self.set(Phase::Idle, None);
        };
        let cfg = self.state().settings.read().unwrap().command.clone();
        let setup = |a: &AppHandle, what: &str| {
            let st = CommandStep { instruction: instr.clone(), step: 1, max: command::MAX_STEPS, status: CmdStatus::Setup, message: Some(what.into()), ..Default::default() };
            command::publish(a, st);
        };
        let Some(key) = secrets::get(KeyName::Openrouter) else {
            setup(&self.app, "key");
            self.command = false;
            return self.set(Phase::Idle, None);
        };
        if !crate::permissions::accessibility_trusted() {
            setup(&self.app, "accessibility");
            self.command = false;
            return self.set(Phase::Idle, None);
        }
        log::info!("command: {instr:?}");
        self.cmd_ctx = Some(ctx);
        self.acting = true;
        self.set(Phase::Acting, None);
        command::spawn(self.app.clone(), instr, cfg, key);
    }

    /// The command thread ended: back to Idle, and a History row (kind command) when history is on.
    fn command_ended(&mut self, e: command::Ended) {
        (self.acting, self.answering, self.command) = (false, false, false);
        if self.rec.take().is_some() {
            self.ctx = None;
            self.m.idle();
        }
        let ctx = self.cmd_ctx.take();
        let (s, data_dir) = (self.state().settings.read().unwrap().clone(), self.state().data_dir.clone());
        if s.privacy.store_history {
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
                provider: format!("jev:{}", s.command.model),
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

    fn process(&mut self, clip: Clip, ctx: Ctx) {
        if self.command {
            return self.process_command(clip, ctx);
        }
        self.ping(Ping::Stop);
        self.ctx = Some(ctx);
        self.set(Phase::Processing, None);
        let t = Instant::now();
        let bundle = self.ctx.as_ref().and_then(|c| c.app.as_ref().and_then(|a| a.bundle_id.clone()));
        let run = run_pipeline(self.state(), &clip, bundle.as_deref());
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
                let status = self.deliver_done(d, ctx.app.as_ref());
                let mut item = row(d.raw.clone(), d.final_text.clone(), status, None);
                item.latency_ms = latency_ms;
                item.cleanup = run.cleanup(d);
                let db = self.state().db.lock().unwrap();
                if let Err(e) = dictionary::record_uses(&db, &d.used) {
                    log::warn!("record_uses: {e}");
                }
                if s.privacy.store_history {
                    match history::insert(&db, &data_dir, &item, s.privacy.store_audio.then_some(&clip.samples[..])) {
                        Ok(item) => {
                            let _ = self.app.emit_to("main", "history_added", item);
                        }
                        Err(e) => log::error!("history insert: {e:#}"),
                    }
                }
                drop(db);
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
            Some(t) => {
                // The overlay's right-click menu or the main window can make susurro the front app.
                let target = paste::target_app();
                self.deliver(&t, false, target.as_ref());
            }
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

    /// Returns true if a max-length stop ran the (blocking) pipeline.
    fn tick(&mut self) -> bool {
        let now = Instant::now();
        let busy = self.act(Ev::Tick, now);
        if self.idle_at.is_some_and(|t| now >= t) {
            self.idle_at = None;
            if self.m.phase() == Phase::Idle && !self.acting {
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
            &pipeline::Opts { language: None, style: crate::contract::Style::Formal, raw_app: false, press_enter_command: false },
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

    #[test]
    fn short_error_for_the_pill() {
        assert_eq!(short("a\nb"), "a");
        assert_eq!(short(&"x".repeat(100)).chars().count(), 80);
    }
}
