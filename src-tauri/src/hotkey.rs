//! WP4: binding parse + validation, modifier-only CGEventTap (Fn, Right Option, ...), global-shortcut registration (PLAN 4.1).
//! This module turns keys into SessionCmds. The hold / double-tap timing lives in session.rs (PLAN 4.2).
//! Esc (`cancel`) is registered only while session_state is Recording, Locked, Acting or Processing a voice command.

use crate::contract::{HotkeyStatus, Hotkeys, Phase, SessionState};
use crate::session::SessionCmd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Listener};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// A chord (modifier + another key) cancels the PTT. Within this window of the press it is silent
/// (Option+2 for @); later it is an undoable cancel (session.rs).
pub(crate) const CHORD_WINDOW: Duration = Duration::from_millis(300);
/// hotkey_capture(true) resumes by itself after this, so a closed page never leaves the hotkeys off.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

/// A modifier-only binding the event tap handles.
#[derive(Debug, PartialEq)]
pub struct ModKey {
    pub name: &'static str,
    /// Keycode of its kCGEventFlagsChanged event.
    keycode: i64,
    /// Device-dependent bit of this side (IOLLEvent.h NX_DEVICE*KEYMASK); Fn has only the general bit.
    dev: u64,
    /// Device bits of both sides of the family.
    family_dev: u64,
    /// Device-independent bit (CGEventFlagAlternate, ...).
    general: u64,
}

const ALT: u64 = 0x0008_0000;
const CMD: u64 = 0x0010_0000;
const CTRL: u64 = 0x0004_0000;
const SHIFT: u64 = 0x0002_0000;
const FN_FLAG: u64 = 0x0080_0000;

pub const MOD_KEYS: [ModKey; 9] = [
    ModKey { name: "Fn", keycode: 63, dev: FN_FLAG, family_dev: FN_FLAG, general: FN_FLAG },
    ModKey { name: "RightOption", keycode: 61, dev: 0x40, family_dev: 0x60, general: ALT },
    ModKey { name: "LeftOption", keycode: 58, dev: 0x20, family_dev: 0x60, general: ALT },
    ModKey { name: "RightCommand", keycode: 54, dev: 0x10, family_dev: 0x18, general: CMD },
    ModKey { name: "LeftCommand", keycode: 55, dev: 0x08, family_dev: 0x18, general: CMD },
    ModKey { name: "RightControl", keycode: 62, dev: 0x2000, family_dev: 0x2001, general: CTRL },
    ModKey { name: "LeftControl", keycode: 59, dev: 0x01, family_dev: 0x2001, general: CTRL },
    ModKey { name: "RightShift", keycode: 60, dev: 0x04, family_dev: 0x06, general: SHIFT },
    ModKey { name: "LeftShift", keycode: 56, dev: 0x02, family_dev: 0x06, general: SHIFT },
];

impl ModKey {
    /// The flags a real press of this key carries (the test route posts them).
    pub fn down_flags(&self) -> u64 {
        self.general | self.dev
    }

    pub fn keycode(&self) -> i64 {
        self.keycode
    }

    /// Down state after a FlagsChanged of this keycode. The device bit tells left from right; events
    /// without device bits (some remappers) fall back to the general bit.
    fn is_down(&self, flags: u64) -> bool {
        if flags & self.family_dev != 0 { flags & self.dev != 0 } else { flags & self.general != 0 }
    }
}

/// "RightOption", "right_alt", "ROption"... -> the table entry. Only a sided name or Fn is a modifier-only binding.
pub fn mod_key(s: &str) -> Option<&'static ModKey> {
    let s = s.trim();
    if let Some(k) = MOD_KEYS.iter().find(|k| k.name.eq_ignore_ascii_case(s)) {
        return Some(k);
    }
    let (m, side) = parse_mod(&s.replace(['_', '-', ' '], ""))?;
    let name = match (m, side) {
        (Mod::Alt, Side::Right) => "RightOption",
        (Mod::Alt, Side::Left) => "LeftOption",
        (Mod::Cmd, Side::Right) => "RightCommand",
        (Mod::Cmd, Side::Left) => "LeftCommand",
        (Mod::Ctrl, Side::Right) => "RightControl",
        (Mod::Ctrl, Side::Left) => "LeftControl",
        (Mod::Shift, Side::Right) => "RightShift",
        (Mod::Shift, Side::Left) => "LeftShift",
        _ => return None,
    };
    MOD_KEYS.iter().find(|k| k.name == name)
}

// ---------- parse + validate (pure) ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mod {
    Ctrl,
    Alt,
    Shift,
    Cmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Any,
    Left,
    Right,
}

#[derive(Debug, PartialEq)]
struct Binding {
    /// A modifier-only binding (event tap); mods and key are empty.
    solo: Option<&'static ModKey>,
    mods: Vec<(Mod, Side)>,
    key: Option<String>,
}

impl Binding {
    /// The string global-shortcut parses, sides dropped and mods sorted, e.g. "Ctrl+Cmd+V".
    /// ponytail: global-shortcut cannot tell left from right, so "RightAlt+Space" registers as "Alt+Space".
    fn canonical(&self) -> String {
        let mut mods: Vec<Mod> = self.mods.iter().map(|m| m.0).collect();
        mods.sort();
        mods.dedup();
        let mut parts: Vec<String> = mods.iter().map(|m| format!("{m:?}")).collect();
        parts.extend(self.key.iter().map(|k| k.to_uppercase()));
        parts.join("+")
    }
}

fn parse_mod(t: &str) -> Option<(Mod, Side)> {
    let l = t.to_lowercase();
    let (side, rest) = if let Some(r) = l.strip_prefix("left") {
        (Side::Left, r)
    } else if let Some(r) = l.strip_prefix("right") {
        (Side::Right, r)
    } else {
        (Side::Any, l.as_str())
    };
    let m = match rest {
        "ctrl" | "control" => Mod::Ctrl,
        "alt" | "option" | "opt" => Mod::Alt,
        "shift" => Mod::Shift,
        "cmd" | "command" | "super" => Mod::Cmd,
        _ => return None,
    };
    Some((m, side))
}

fn parse(s: &str) -> Result<Binding, String> {
    let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
    if tokens.iter().any(|t| t.is_empty()) {
        return Err(format!("\"{s}\" has an empty key"));
    }
    if tokens.len() > 3 {
        return Err(format!("\"{s}\" has more than 3 keys"));
    }
    if tokens.len() == 1 {
        if let Some(k) = mod_key(tokens[0]) {
            return Ok(Binding { solo: Some(k), mods: vec![], key: None });
        }
    }
    if tokens.iter().any(|t| t.eq_ignore_ascii_case("fn")) {
        return Err(format!("\"{s}\": fn works only alone"));
    }
    let mut b = Binding { solo: None, mods: vec![], key: None };
    for t in &tokens {
        if b.key.is_some() {
            return Err(format!("\"{s}\": only one non-modifier key, and it goes last"));
        }
        match parse_mod(t) {
            Some(m) => b.mods.push(m),
            None => b.key = Some(t.to_string()),
        }
    }
    if b.key.is_some() {
        // Reuse global-shortcut's own key table as the source of truth for key names.
        b.canonical().parse::<Shortcut>().map_err(|e| format!("\"{s}\": {e}"))?;
    }
    Ok(b)
}

fn check(b: &Binding, s: &str) -> Result<(), String> {
    if b.solo.is_some() {
        return Ok(());
    }
    let Some(key) = &b.key else {
        if b.mods.len() == 1 {
            return Err(format!("\"{s}\": pick the left or right key, e.g. RightOption"));
        }
        return Err(format!("\"{s}\": a modifier alone works only as one key, e.g. RightOption"));
    };
    for m in [Mod::Ctrl, Mod::Alt, Mod::Shift, Mod::Cmd] {
        let has = |side| b.mods.contains(&(m, side));
        if has(Side::Left) && has(Side::Right) {
            return Err(format!("\"{s}\": left and right {m:?} together"));
        }
    }
    let k = key.to_uppercase();
    if b.mods.is_empty() && k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("\"{s}\": a single letter or digit needs a modifier"));
    }
    let only_cmd = !b.mods.is_empty() && b.mods.iter().all(|m| m.0 == Mod::Cmd);
    if only_cmd && ["C", "V", "X", "Z", "Q"].contains(&k.as_str()) {
        return Err(format!("\"{s}\" is a system shortcut"));
    }
    Ok(())
}

fn hands_free(b: &Hotkeys) -> Option<&str> {
    b.hands_free.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

pub fn validate(b: &Hotkeys) -> Result<(), Vec<String>> {
    let actions = [
        ("Push to talk", Some(b.ptt.as_str())),
        ("Hands-free", hands_free(b)),
        ("Cancel", Some(b.cancel.as_str())),
        ("Paste last", Some(b.paste_last.as_str())),
        ("Copy last", Some(b.copy_last.as_str())),
        ("Voice control", Some(b.command.trim()).filter(|s| !s.is_empty())),
    ];
    let mut errs = vec![];
    let mut seen: Vec<(String, &str)> = vec![];
    for (name, s) in actions {
        let Some(s) = s else { continue };
        let parsed = parse(s).and_then(|p| {
            if p.solo.is_some() && name != "Push to talk" {
                return Err(format!("\"{s}\": a modifier alone works only for push to talk"));
            }
            check(&p, s).map(|_| p)
        });
        match parsed {
            Ok(p) => {
                let c = p.solo.map_or_else(|| p.canonical(), |k| k.name.to_string());
                if let Some((_, other)) = seen.iter().find(|(k, _)| *k == c) {
                    errs.push(format!("{name}: \"{s}\" is already used by {other}"));
                }
                seen.push((c, name));
            }
            Err(e) => errs.push(format!("{name}: {e}")),
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

// ---------- modifier key machine (pure) ----------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyInput {
    /// FlagsChanged of the bound key; true = down.
    Mod(bool),
    /// Any KeyDown.
    KeyDown,
}

/// What a tap event means for the bound key. Other modifiers (the other side too) are ignored.
fn classify(k: &ModKey, flags_changed: bool, keycode: i64, flags: u64) -> Option<KeyInput> {
    if !flags_changed {
        return Some(KeyInput::KeyDown);
    }
    (keycode == k.keycode).then(|| KeyInput::Mod(k.is_down(flags)))
}

/// Down -> PttDown, up -> PttUp. Any other key while held is a chord (Option+2 for @): Cancel, and the release
/// sends nothing. The double tap is session.rs's (two short PttDown/PttUp pairs).
#[derive(Debug, Default, Clone, Copy)]
pub struct ModMachine {
    down: bool,
    chord: bool,
}

impl ModMachine {
    pub fn step(&mut self, input: KeyInput) -> Option<SessionCmd> {
        match input {
            KeyInput::Mod(true) if !self.down => {
                (self.down, self.chord) = (true, false);
                Some(SessionCmd::PttDown)
            }
            KeyInput::Mod(false) if self.down => {
                self.down = false;
                (!self.chord).then_some(SessionCmd::PttUp)
            }
            KeyInput::KeyDown if self.down && !self.chord => {
                self.chord = true;
                Some(SessionCmd::Cancel)
            }
            _ => None,
        }
    }
}

// ---------- runtime ----------

/// Esc is live while recording, while a command acts, and while a command's speech is transcribed (so Esc stops it
/// instead of reaching the app in front).
fn cancel_on(s: &SessionState) -> bool {
    matches!(s.phase, Phase::Recording | Phase::Locked | Phase::Acting) || (s.phase == Phase::Processing && s.command)
}

/// Paused drops new input, but a release or cancel still ends a hold that started before the pause.
fn passes(paused: bool, cmd: &SessionCmd) -> bool {
    !paused || matches!(cmd, SessionCmd::PttUp | SessionCmd::CmdUp | SessionCmd::Cancel)
}

const NO_KEY: usize = usize::MAX;
/// Index in MOD_KEYS of the bound modifier-only key, or NO_KEY. One tap per process, so these are global.
static TAP_KEY: AtomicUsize = AtomicUsize::new(NO_KEY);
static TAP_RUNNING: AtomicBool = AtomicBool::new(false);
/// The tap thread could not start after the tap was created (needs a relaunch).
static TAP_FAILED: AtomicBool = AtomicBool::new(false);

/// permissions_status: the tap is wanted and failed to start in this process.
pub fn tap_failed() -> bool {
    TAP_KEY.load(Ordering::Relaxed) != NO_KEY && TAP_FAILED.load(Ordering::Relaxed)
}

/// A modifier-only talk key is bound, so the event tap is wanted.
pub fn tap_wanted() -> bool {
    TAP_KEY.load(Ordering::Relaxed) != NO_KEY
}

/// The event tap runs in this process (Input Monitoring works now).
pub fn tap_running() -> bool {
    TAP_RUNNING.load(Ordering::Relaxed)
}

/// WP5 stores it with `app.manage(handle)`; save_settings finds it with `app.try_state::<HotkeyHandle>()`.
pub struct HotkeyHandle {
    inner: Arc<Inner>,
}

struct Inner {
    app: AppHandle,
    tx: mpsc::Sender<SessionCmd>,
    b: Mutex<Hotkeys>,
    paused: AtomicBool,
    /// The page records a new binding (hotkey_capture): shortcuts unregistered, tap input dropped.
    capturing: AtomicBool,
    capture_gen: AtomicU64,
    /// The cancel shortcut is registered (session is Recording or Locked).
    cancel_on: AtomicBool,
    tap_started: AtomicBool,
    /// Error from the last register_all for the ptt combo.
    ptt_error: Mutex<Option<String>>,
    last_status: Mutex<Option<HotkeyStatus>>,
}

impl Inner {
    fn send(&self, cmd: SessionCmd) {
        let off = self.paused.load(Ordering::Relaxed) || self.capturing.load(Ordering::Relaxed);
        if passes(off, &cmd) {
            let _ = self.tx.send(cmd);
        }
    }

    fn bind(self: &Arc<Self>, s: &str, down: Option<SessionCmd>, up: Option<SessionCmd>) -> anyhow::Result<()> {
        let combo = parse(s).map_err(anyhow::Error::msg)?.canonical();
        let me = self.clone();
        self.app.global_shortcut().on_shortcut(combo.as_str(), move |_, _, ev| {
            let cmd = if ev.state == ShortcutState::Pressed { &down } else { &up };
            if let Some(c) = cmd {
                me.send(c.clone());
            }
        })?;
        Ok(())
    }

    fn register_all(self: &Arc<Self>) -> anyhow::Result<()> {
        let b = self.b.lock().unwrap().clone();
        let gs = self.app.global_shortcut();
        gs.unregister_all()?;
        let key = MOD_KEYS.iter().position(|k| Some(k) == mod_key(&b.ptt));
        TAP_KEY.store(key.unwrap_or(NO_KEY), Ordering::Relaxed);
        let mut errs = vec![];
        if key.is_some() {
            *self.ptt_error.lock().unwrap() = None;
            self.ensure_tap();
        } else if let Err(e) = self.bind(&b.ptt, Some(SessionCmd::PttDown), Some(SessionCmd::PttUp)) {
            *self.ptt_error.lock().unwrap() = Some(e.to_string());
            errs.push(format!("{}: {e}", b.ptt));
        } else {
            *self.ptt_error.lock().unwrap() = None;
        }
        // Voice control: hold to speak; a press while a step waits confirms it (session.rs). "" = off.
        let command = b.command.trim();
        if !command.is_empty() {
            if let Err(e) = self.bind(command, Some(SessionCmd::CmdDown), Some(SessionCmd::CmdUp)) {
                errs.push(format!("{command}: {e}"));
            }
        }
        let mut rest = vec![(b.paste_last.clone(), SessionCmd::PasteLast), (b.copy_last.clone(), SessionCmd::CopyLast)];
        if let Some(h) = hands_free(&b) {
            rest.push((h.to_string(), SessionCmd::Toggle));
        }
        if self.cancel_on.load(Ordering::Relaxed) {
            rest.push((b.cancel.clone(), SessionCmd::Cancel));
        }
        for (s, cmd) in rest {
            if let Err(e) = self.bind(&s, Some(cmd), None) {
                errs.push(format!("{s}: {e}"));
            }
        }
        self.emit_status();
        if errs.is_empty() { Ok(()) } else { Err(anyhow::anyhow!(errs.join("; "))) }
    }

    fn status(&self) -> HotkeyStatus {
        let binding = self.b.lock().unwrap().ptt.clone();
        let (active, error) = if self.capturing.load(Ordering::Relaxed) {
            (false, Some("Paused while you record a new shortcut".to_string()))
        } else if self.paused.load(Ordering::Relaxed) {
            (false, Some("Paused from the menu bar".to_string()))
        } else if TAP_KEY.load(Ordering::Relaxed) == NO_KEY {
            let e = self.ptt_error.lock().unwrap().clone();
            (e.is_none(), e)
        } else if TAP_RUNNING.load(Ordering::Relaxed) {
            (true, None)
        } else if TAP_FAILED.load(Ordering::Relaxed) {
            (false, Some("The key listener failed to start: relaunch susurro".to_string()))
        } else {
            (false, Some("Waiting for Input Monitoring: allow susurro in System Settings > Privacy & Security".to_string()))
        };
        HotkeyStatus { binding, active, error }
    }

    /// Emits `hotkey_status` when it changed.
    fn emit_status(&self) {
        let s = self.status();
        let mut last = self.last_status.lock().unwrap();
        if last.as_ref() != Some(&s) {
            log::info!("hotkey status: {s:?}");
            let _ = self.app.emit("hotkey_status", s.clone());
            *last = Some(s);
        }
    }

    fn set_cancel(self: &Arc<Self>, on: bool) {
        if self.cancel_on.swap(on, Ordering::Relaxed) == on || self.capturing.load(Ordering::Relaxed) {
            return;
        }
        let cancel = self.b.lock().unwrap().cancel.clone();
        let r = if on {
            self.bind(&cancel, Some(SessionCmd::Cancel), None)
        } else {
            parse(&cancel)
                .map_err(anyhow::Error::msg)
                .and_then(|p| Ok(self.app.global_shortcut().unregister(p.canonical().as_str())?))
        };
        if let Err(e) = r {
            log::warn!("cancel shortcut {cancel}: {e}");
        }
    }

    fn ensure_tap(self: &Arc<Self>) {
        if self.tap_started.swap(true, Ordering::Relaxed) {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let me = self.clone();
            let me2 = self.clone();
            spawn_tap(move |cmd| me.send(cmd), move || me2.emit_status());
        }
    }

    fn set_capture(self: &Arc<Self>, on: bool) -> anyhow::Result<()> {
        let generation = self.capture_gen.fetch_add(1, Ordering::Relaxed) + 1;
        if self.capturing.swap(on, Ordering::Relaxed) == on {
            return Ok(());
        }
        if !on {
            return self.register_all();
        }
        self.app.global_shortcut().unregister_all()?;
        self.emit_status();
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(CAPTURE_TIMEOUT);
            if me.capture_gen.load(Ordering::Relaxed) == generation {
                log::warn!("hotkey capture: no answer from the page in 30 s, resuming");
                let _ = me.set_capture(false);
            }
        });
        Ok(())
    }
}

pub fn start(app: &AppHandle, b: &Hotkeys, tx: mpsc::Sender<SessionCmd>) -> anyhow::Result<HotkeyHandle> {
    let inner = Arc::new(Inner {
        app: app.clone(),
        tx,
        b: Mutex::new(b.clone()),
        paused: AtomicBool::new(false),
        capturing: AtomicBool::new(false),
        capture_gen: AtomicU64::new(0),
        cancel_on: AtomicBool::new(false),
        tap_started: AtomicBool::new(false),
        ptt_error: Mutex::new(None),
        last_status: Mutex::new(None),
    });
    // A bad binding in settings.json must not kill startup: log it, keep the rest registered.
    if let Err(e) = inner.register_all() {
        log::warn!("hotkeys: {e}");
    }
    let me = inner.clone();
    app.listen("session_state", move |ev| {
        if let Ok(s) = serde_json::from_str::<SessionState>(ev.payload()) {
            me.set_cancel(cancel_on(&s));
        }
    });
    Ok(HotkeyHandle { inner })
}

impl HotkeyHandle {
    /// On failure the old bindings are restored and registered again, so the saved settings stay true.
    pub fn update(&self, b: &Hotkeys) -> anyhow::Result<()> {
        let old = std::mem::replace(&mut *self.inner.b.lock().unwrap(), b.clone());
        // While the page records a binding, register_all runs when the capture ends.
        if self.inner.capturing.load(Ordering::Relaxed) {
            return Ok(());
        }
        let r = self.inner.register_all();
        if r.is_err() {
            *self.inner.b.lock().unwrap() = old;
            if let Err(e) = self.inner.register_all() {
                log::warn!("hotkeys: restoring the old bindings: {e}");
            }
        }
        r
    }

    pub fn paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    /// Tray "Pause hotkey". ponytail: the combos stay registered and are ignored; unregister if other apps need them.
    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::Relaxed);
        self.inner.emit_status();
    }

    pub fn status(&self) -> HotkeyStatus {
        self.inner.status()
    }

    pub fn bindings(&self) -> Hotkeys {
        self.inner.b.lock().unwrap().clone()
    }
}

/// The page records a new binding: on = unregister every shortcut (so the combo reaches the page) and drop
/// tap input; off = register again. Resumes by itself after 30 s.
#[tauri::command]
pub fn hotkey_capture(app: AppHandle, on: bool) -> Result<(), String> {
    use tauri::Manager;
    let h = app.try_state::<HotkeyHandle>().ok_or("hotkeys are not running")?;
    h.inner.set_capture(on).map_err(|e| e.to_string())
}

/// Listen-only tap for the modifier-only binding (TAP_KEY) on its own CFRunLoop thread. Never swallows a key.
/// Retries every second until the tap can be created, so a grant during onboarding starts it without a restart.
#[cfg(target_os = "macos")]
fn spawn_tap(on_cmd: impl Fn(SessionCmd) + Send + Sync + 'static, on_status: impl Fn() + Send + 'static) {
    use core_foundation::base::TCFType;
    use core_foundation::mach_port::CFMachPortRef;
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, CallbackResult, EventField,
    };
    use std::sync::atomic::AtomicPtr;

    unsafe extern "C" {
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    }
    static PORT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    std::thread::Builder::new()
        .name("hotkey-tap".into())
        .spawn(move || {
            let on_cmd = Arc::new(on_cmd);
            let tap = loop {
                // (machine, key index it runs for): a binding change mid-hold resets the machine.
                let machine = Mutex::new((ModMachine::default(), NO_KEY));
                let on_cmd = on_cmd.clone();
                let tap = CGEventTap::new(
                    CGEventTapLocation::Session,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::ListenOnly,
                    vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
                    move |_, ty, ev| {
                        let flags_changed = match ty {
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                                let p = PORT.load(Ordering::Relaxed);
                                if !p.is_null() {
                                    unsafe { CGEventTapEnable(p as CFMachPortRef, true) };
                                }
                                return CallbackResult::Keep;
                            }
                            CGEventType::FlagsChanged => true,
                            CGEventType::KeyDown => false,
                            _ => return CallbackResult::Keep,
                        };
                        let idx = TAP_KEY.load(Ordering::Relaxed);
                        let mut m = machine.lock().unwrap();
                        if m.1 != idx {
                            *m = (ModMachine::default(), idx);
                        }
                        let Some(k) = MOD_KEYS.get(idx) else { return CallbackResult::Keep };
                        let code = ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                        if let Some(i) = classify(k, flags_changed, code, ev.get_flags().bits()) {
                            if let Some(c) = m.0.step(i) {
                                on_cmd(c);
                            }
                        }
                        CallbackResult::Keep
                    },
                );
                if let Ok(t) = tap {
                    break t;
                }
                // ponytail: 1 s poll, macOS has no grant callback.
                std::thread::sleep(Duration::from_secs(1));
            };
            PORT.store(tap.mach_port().as_concrete_TypeRef() as *mut _, Ordering::Relaxed);
            let Ok(src) = tap.mach_port().create_runloop_source(0) else {
                log::error!("hotkey tap: no runloop source");
                TAP_FAILED.store(true, Ordering::Relaxed);
                on_status();
                return;
            };
            CFRunLoop::get_current().add_source(&src, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            TAP_RUNNING.store(true, Ordering::Relaxed);
            log::info!("hotkey tap running");
            on_status();
            CFRunLoop::run_current();
            TAP_RUNNING.store(false, Ordering::Relaxed);
            on_status();
        })
        .expect("spawn hotkey-tap");
}

/// Can this process create a listen-only tap right now? Live TCC check: CGPreflightListenEventAccess
/// can stay false after a grant until the app restarts.
#[cfg(target_os = "macos")]
pub fn probe_tap() -> bool {
    use core_graphics::event::{CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, CallbackResult};
    CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::TailAppendEventTap,
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::FlagsChanged],
        |_, _, _| CallbackResult::Keep,
    )
    .is_ok() // dropped here: the port is released, the tap was never enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionCmd::*;

    #[test]
    fn esc_is_live_while_a_command_is_transcribed() {
        let st = |phase, command| SessionState { phase, command, ..Default::default() };
        assert!(cancel_on(&st(Phase::Processing, true)), "Esc during the command's STT must stop it");
        assert!(!cancel_on(&st(Phase::Processing, false)), "a dictation's processing keeps Esc for the app");
        assert!(cancel_on(&st(Phase::Acting, true)) && cancel_on(&st(Phase::Recording, false)));
        assert!(!cancel_on(&st(Phase::Idle, false)));
    }

    #[test]
    fn pause_still_ends_a_hold() {
        for c in [PttDown, Toggle, PasteLast, CopyLast, CmdDown] {
            assert!(!passes(true, &c), "{c:?}");
        }
        for c in [PttUp, Cancel, CmdUp] {
            assert!(passes(true, &c), "{c:?}");
        }
        assert!(passes(false, &PttDown));
    }

    fn hk(ptt: &str, hf: Option<&str>, cancel: &str, paste: &str) -> Hotkeys {
        Hotkeys {
            ptt: ptt.into(),
            hands_free: hf.map(Into::into),
            double_tap: true,
            hands_free_grace_ms: 400,
            cancel: cancel.into(),
            paste_last: paste.into(),
            copy_last: "Ctrl+Cmd+C".into(),
            command: "F2".into(),
        }
    }

    fn err(h: Hotkeys) -> String {
        validate(&h).unwrap_err().join("; ")
    }

    #[test]
    fn defaults_are_valid() {
        assert_eq!(validate(&Hotkeys::default()), Ok(()));
        assert_eq!(validate(&hk("Alt+Space", Some("Ctrl+Alt+Space"), "Escape", "Ctrl+Cmd+V")), Ok(()));
        assert_eq!(validate(&hk("RightAlt+Space", Some(""), "Esc", "Shift+Cmd+V")), Ok(()));
        assert_eq!(validate(&hk("F5", None, "Escape", "Ctrl+Cmd+V")), Ok(()));
    }

    #[test]
    fn parse_canonical() {
        assert_eq!(parse("cmd+ctrl+v").unwrap().canonical(), "Ctrl+Cmd+V");
        assert_eq!(parse("Option + space").unwrap().canonical(), "Alt+SPACE");
        assert_eq!(parse("Fn").unwrap().solo.unwrap().name, "Fn");
        assert!(parse("Ctrl+Bogus").is_err());
        assert!(parse("V+Cmd").is_err());
        assert!(parse("Ctrl++V").is_err());
    }

    #[test]
    fn supr_is_forward_delete() {
        // global-hotkey 0.8 maps Code::Delete to 0x75 = kVK_ForwardDelete (fn+⌫ on Apple keyboards), not ⌫ (0x33).
        assert_eq!(validate(&hk("Delete", None, "Escape", "Ctrl+Cmd+V")), Ok(()));
        let sc: Shortcut = parse("Delete").unwrap().canonical().parse().unwrap();
        assert_eq!(sc.key, tauri_plugin_global_shortcut::Code::Delete);
        assert!(sc.mods.is_empty());
    }

    #[test]
    fn modifier_only_names() {
        for k in &MOD_KEYS {
            assert_eq!(mod_key(k.name), Some(k), "{}", k.name);
            assert_eq!(mod_key(&k.name.to_lowercase()), Some(k));
        }
        assert_eq!(mod_key("RightAlt").unwrap().name, "RightOption");
        assert_eq!(mod_key("left_cmd").unwrap().name, "LeftCommand");
        assert_eq!(mod_key("ROption"), None);
        assert_eq!(mod_key("Option"), None, "a side is required");
        assert_eq!(mod_key("Space"), None);
        for k in &MOD_KEYS {
            assert_eq!(validate(&hk(k.name, None, "Escape", "Ctrl+Cmd+V")), Ok(()), "{}", k.name);
        }
        // RightOption+Space stays a global-shortcut combo.
        assert_eq!(parse("RightOption+Space").unwrap().solo, None);
    }

    #[test]
    fn rejects() {
        assert!(err(hk("Cmd+V", None, "Escape", "Ctrl+Cmd+V")).contains("system shortcut"));
        for k in ["Cmd+C", "Cmd+X", "Cmd+Z", "Command+Q"] {
            assert!(err(hk(k, None, "Escape", "Ctrl+Cmd+V")).contains("system shortcut"), "{k}");
        }
        assert!(err(hk("A", None, "Escape", "Ctrl+Cmd+V")).contains("single letter"));
        assert!(err(hk("7", None, "Escape", "Ctrl+Cmd+V")).contains("single letter"));
        assert!(err(hk("LeftCmd+RightCmd+K", None, "Escape", "Ctrl+Cmd+V")).contains("left and right"));
        assert!(err(hk("Alt+Space", Some("Alt+Space"), "Escape", "Ctrl+Cmd+V")).contains("already used"));
        assert!(err(hk("Ctrl+Alt+Shift+K", None, "Escape", "Ctrl+Cmd+V")).contains("more than 3"));
        assert!(err(hk("Ctrl+Alt", None, "Escape", "Ctrl+Cmd+V")).contains("modifier alone"));
        assert!(err(hk("Option", None, "Escape", "Ctrl+Cmd+V")).contains("left or right"));
        assert!(err(hk("Fn+Space", None, "Escape", "Ctrl+Cmd+V")).contains("fn works only alone"));
        assert!(err(hk("Fn", Some("Fn"), "Escape", "Ctrl+Cmd+V")).contains("only for push to talk"));
        assert!(err(hk("Alt+Space", Some("RightOption"), "Escape", "Ctrl+Cmd+V")).contains("only for push to talk"));
        assert!(err(hk("Alt+Space", None, "Escape", "LeftShift")).contains("only for push to talk"));
        assert!(err(hk("Ctrl+Cmd+C", None, "Escape", "Ctrl+Cmd+V")).contains("already used by Push to talk"));
    }

    #[test]
    fn duplicates_compare_canonical_forms() {
        assert!(err(hk("Alt+Space", None, "Escape", "Option+SPACE")).contains("already used by Push to talk"));
    }

    /// A real FlagsChanged of `k` (down) or a release with nothing else held.
    fn press(k: &ModKey, down: bool) -> (bool, i64, u64) {
        (true, k.keycode, if down { k.down_flags() } else { 0 })
    }

    fn run(k: &ModKey, events: &[(bool, i64, u64)]) -> Vec<SessionCmd> {
        let mut m = ModMachine::default();
        events.iter().filter_map(|&(fc, code, flags)| classify(k, fc, code, flags)).filter_map(|i| m.step(i)).collect()
    }

    const KEY: (bool, i64, u64) = (false, 19, 0); // KeyDown "2"

    #[test]
    fn every_modifier_hold_double_tap_chord() {
        for k in &MOD_KEYS {
            let (d, u) = (press(k, true), press(k, false));
            assert_eq!(run(k, &[d, d, u, u]), [PttDown, PttUp], "{} hold (repeat down ignored)", k.name);
            // Double tap = two short pairs; session.rs turns them into hands-free.
            assert_eq!(run(k, &[d, u, d, u]), [PttDown, PttUp, PttDown, PttUp], "{} double tap", k.name);
            // Option+2 = @: cancel once, no PttUp, and the next press works again.
            assert_eq!(run(k, &[d, KEY, KEY, u, d, u]), [PttDown, Cancel, PttDown, PttUp], "{} chord", k.name);
            assert_eq!(run(k, &[KEY, KEY]), [], "{} keys without the modifier", k.name);
        }
    }

    #[test]
    fn sides_and_other_modifiers_do_not_trigger() {
        for k in &MOD_KEYS {
            for other in MOD_KEYS.iter().filter(|o| o.name != k.name) {
                let (d, u) = (press(other, true), press(other, false));
                assert_eq!(run(k, &[d, u]), [], "{} must ignore {}", k.name, other.name);
            }
        }
        let (r, l) = (mod_key("RightOption").unwrap(), mod_key("LeftOption").unwrap());
        // Hold left, press and release right: right's release still carries left's bits.
        let events = [press(l, true), (true, 61, ALT | 0x60), (true, 61, ALT | 0x20), press(l, false)];
        assert_eq!(run(r, &events), [PttDown, PttUp]);
        assert_eq!(run(l, &events), [PttDown, PttUp], "left: down at the first event, up at the last");
        // No device bits (a remapper): the general bit decides.
        assert_eq!(run(r, &[(true, 61, ALT), (true, 61, 0)]), [PttDown, PttUp]);
    }
}
