//! WP4: binding parse + validation, fn CGEventTap, global-shortcut registration (PLAN 4.1).
//! This module turns keys into SessionCmds. The hold / double-tap timing lives in session.rs (PLAN 4.2).
//! Esc (`cancel`) is registered only while session_state is Recording or Locked.

use crate::contract::{Hotkeys, Phase, SessionState};
use crate::session::SessionCmd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Listener};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const FN: &str = "Fn";
/// fn + another key within this window = the user meant fn+arrow / an F-key: cancel the PTT.
pub(crate) const CHORD_WINDOW: Duration = Duration::from_millis(300);

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
    is_fn: bool,
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
    if tokens.iter().any(|t| t.eq_ignore_ascii_case(FN)) {
        if tokens.len() > 1 {
            return Err(format!("\"{s}\": fn works only alone"));
        }
        return Ok(Binding { is_fn: true, mods: vec![], key: None });
    }
    let mut b = Binding { is_fn: false, mods: vec![], key: None };
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
    if b.is_fn {
        return Ok(());
    }
    let Some(key) = &b.key else {
        return Err(format!("\"{s}\": modifier-only bindings other than Fn are not supported"));
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
    ];
    let mut errs = vec![];
    let mut seen: Vec<(String, &str)> = vec![];
    for (name, s) in actions {
        let Some(s) = s else { continue };
        let parsed = parse(s).and_then(|p| {
            if p.is_fn && name != "Push to talk" {
                return Err(format!("\"{s}\": fn works only for push to talk"));
            }
            check(&p, s).map(|_| p)
        });
        match parsed {
            Ok(p) => {
                let c = if p.is_fn { FN.to_string() } else { p.canonical() };
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

// ---------- fn key machine (pure) ----------

#[derive(Debug, Clone, Copy)]
pub enum FnInput {
    /// FlagsChanged for keycode 63; true = fn is down.
    Fn(bool),
    /// Any KeyDown.
    KeyDown,
}

/// fn down -> PttDown, fn up -> PttUp. Another key within 300 ms of the press -> Cancel, and the release sends nothing.
/// WP5: a Cancel that arrives within 300 ms of PttDown is the fn+arrow case and should be silent (no Undo toast).
#[derive(Debug, Default, Clone, Copy)]
pub struct FnMachine {
    down_at: Option<Instant>,
    chord: bool,
}

impl FnMachine {
    pub fn step(&mut self, input: FnInput, now: Instant) -> Option<SessionCmd> {
        match input {
            FnInput::Fn(true) if self.down_at.is_none() => {
                self.down_at = Some(now);
                self.chord = false;
                Some(SessionCmd::PttDown)
            }
            FnInput::Fn(false) if self.down_at.is_some() => {
                self.down_at = None;
                (!self.chord).then_some(SessionCmd::PttUp)
            }
            FnInput::KeyDown => match self.down_at {
                Some(t) if !self.chord && now.duration_since(t) < CHORD_WINDOW => {
                    self.chord = true;
                    Some(SessionCmd::Cancel)
                }
                _ => None,
            },
            _ => None,
        }
    }
}

// ---------- runtime ----------

/// Paused drops new input, but a release or cancel still ends a hold that started before the pause.
fn passes(paused: bool, cmd: &SessionCmd) -> bool {
    !paused || matches!(cmd, SessionCmd::PttUp | SessionCmd::Cancel)
}

/// WP5 stores it with `app.manage(handle)`; save_settings finds it with `app.try_state::<HotkeyHandle>()`.
pub struct HotkeyHandle {
    inner: Arc<Inner>,
}

struct Inner {
    app: AppHandle,
    tx: mpsc::Sender<SessionCmd>,
    b: Mutex<Hotkeys>,
    fn_ptt: AtomicBool,
    paused: AtomicBool,
    /// The cancel shortcut is registered (session is Recording or Locked).
    cancel_on: AtomicBool,
    tap_started: AtomicBool,
}

impl Inner {
    fn send(&self, cmd: SessionCmd) {
        if passes(self.paused.load(Ordering::Relaxed), &cmd) {
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
        let is_fn = b.ptt.trim().eq_ignore_ascii_case(FN);
        self.fn_ptt.store(is_fn, Ordering::Relaxed);
        let mut errs = vec![];
        if is_fn {
            self.ensure_tap();
        } else if let Err(e) = self.bind(&b.ptt, Some(SessionCmd::PttDown), Some(SessionCmd::PttUp)) {
            errs.push(format!("{}: {e}", b.ptt));
        }
        let mut rest = vec![(b.paste_last.clone(), SessionCmd::PasteLast)];
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
        if errs.is_empty() { Ok(()) } else { Err(anyhow::anyhow!(errs.join("; "))) }
    }

    fn set_cancel(self: &Arc<Self>, on: bool) {
        if self.cancel_on.swap(on, Ordering::Relaxed) == on {
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
            spawn_fn_tap(move |cmd| {
                if me.fn_ptt.load(Ordering::Relaxed) {
                    me.send(cmd);
                }
            });
        }
    }
}

pub fn start(app: &AppHandle, b: &Hotkeys, tx: mpsc::Sender<SessionCmd>) -> anyhow::Result<HotkeyHandle> {
    let inner = Arc::new(Inner {
        app: app.clone(),
        tx,
        b: Mutex::new(b.clone()),
        fn_ptt: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        cancel_on: AtomicBool::new(false),
        tap_started: AtomicBool::new(false),
    });
    // A bad binding in settings.json must not kill startup: log it, keep the rest registered.
    if let Err(e) = inner.register_all() {
        log::warn!("hotkeys: {e}");
    }
    let me = inner.clone();
    app.listen("session_state", move |ev| {
        if let Ok(s) = serde_json::from_str::<SessionState>(ev.payload()) {
            me.set_cancel(matches!(s.phase, Phase::Recording | Phase::Locked));
        }
    });
    Ok(HotkeyHandle { inner })
}

impl HotkeyHandle {
    /// On failure the old bindings are restored and registered again, so the saved settings stay true.
    pub fn update(&self, b: &Hotkeys) -> anyhow::Result<()> {
        let old = std::mem::replace(&mut *self.inner.b.lock().unwrap(), b.clone());
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
    }
}

/// Listen-only tap for fn (keycode 63) on its own CFRunLoop thread.
/// Waits for Input Monitoring first, so a grant during onboarding starts the tap without a restart.
#[cfg(target_os = "macos")]
fn spawn_fn_tap(on_cmd: impl Fn(SessionCmd) + Send + 'static) {
    use core_foundation::base::TCFType;
    use core_foundation::mach_port::CFMachPortRef;
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
    use core_graphics::event::{
        CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        CallbackResult, EventField,
    };
    use std::sync::atomic::AtomicPtr;

    unsafe extern "C" {
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    }
    static PORT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

    std::thread::Builder::new()
        .name("fn-tap".into())
        .spawn(move || {
            // ponytail: 2 s poll, macOS has no grant callback.
            while !objc2_core_graphics::CGPreflightListenEventAccess() {
                std::thread::sleep(Duration::from_secs(2));
            }
            let machine = Mutex::new(FnMachine::default());
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
                move |_, ty, ev| {
                    let input = match ty {
                        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                            let p = PORT.load(Ordering::Relaxed);
                            if !p.is_null() {
                                unsafe { CGEventTapEnable(p as CFMachPortRef, true) };
                            }
                            None
                        }
                        CGEventType::FlagsChanged
                            if ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) == 63 =>
                        {
                            Some(FnInput::Fn(ev.get_flags().contains(CGEventFlags::CGEventFlagSecondaryFn)))
                        }
                        CGEventType::KeyDown => Some(FnInput::KeyDown),
                        _ => None,
                    };
                    if let Some(i) = input {
                        let cmd = machine.lock().unwrap().step(i, Instant::now());
                        if let Some(c) = cmd {
                            on_cmd(c);
                        }
                    }
                    CallbackResult::Keep
                },
            );
            let Ok(tap) = tap else {
                log::error!("fn tap: CGEventTapCreate failed (Input Monitoring?)");
                return;
            };
            PORT.store(tap.mach_port().as_concrete_TypeRef() as *mut _, Ordering::Relaxed);
            let src = tap.mach_port().create_runloop_source(0).expect("runloop source");
            CFRunLoop::get_current().add_source(&src, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            log::info!("fn tap running");
            CFRunLoop::run_current();
        })
        .expect("spawn fn-tap");
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionCmd::*;

    #[test]
    fn pause_still_ends_a_hold() {
        for c in [PttDown, Toggle, PasteLast] {
            assert!(!passes(true, &c), "{c:?}");
        }
        for c in [PttUp, Cancel] {
            assert!(passes(true, &c), "{c:?}");
        }
        assert!(passes(false, &PttDown));
    }

    fn hk(ptt: &str, hf: Option<&str>, cancel: &str, paste: &str) -> Hotkeys {
        Hotkeys {
            ptt: ptt.into(),
            hands_free: hf.map(Into::into),
            double_tap: true,
            cancel: cancel.into(),
            paste_last: paste.into(),
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
        assert!(parse("Fn").unwrap().is_fn);
        assert!(parse("Ctrl+Bogus").is_err());
        assert!(parse("V+Cmd").is_err());
        assert!(parse("Ctrl++V").is_err());
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
        assert!(err(hk("Ctrl+Alt", None, "Escape", "Ctrl+Cmd+V")).contains("modifier-only"));
        assert!(err(hk("Fn+Space", None, "Escape", "Ctrl+Cmd+V")).contains("fn works only alone"));
        assert!(err(hk("Fn", Some("Fn"), "Escape", "Ctrl+Cmd+V")).contains("only for push to talk"));
    }

    #[test]
    fn duplicates_compare_canonical_forms() {
        assert!(err(hk("Alt+Space", None, "Escape", "Option+SPACE")).contains("already used by Push to talk"));
    }

    #[test]
    fn fn_hold_and_release() {
        let t = Instant::now();
        let mut m = FnMachine::default();
        assert_eq!(m.step(FnInput::Fn(true), t), Some(PttDown));
        assert_eq!(m.step(FnInput::Fn(true), t), None, "repeat down is ignored");
        assert_eq!(m.step(FnInput::KeyDown, t + Duration::from_millis(500)), None, "late key keeps recording");
        assert_eq!(m.step(FnInput::Fn(false), t + Duration::from_millis(900)), Some(PttUp));
        assert_eq!(m.step(FnInput::Fn(false), t + Duration::from_millis(950)), None);
    }

    #[test]
    fn fn_chord_cancels_silently() {
        let t = Instant::now();
        let mut m = FnMachine::default();
        m.step(FnInput::Fn(true), t);
        assert_eq!(m.step(FnInput::KeyDown, t + Duration::from_millis(120)), Some(Cancel));
        assert_eq!(m.step(FnInput::KeyDown, t + Duration::from_millis(150)), None);
        assert_eq!(m.step(FnInput::Fn(false), t + Duration::from_millis(400)), None, "no PttUp after a chord");
        // next press works again
        assert_eq!(m.step(FnInput::Fn(true), t + Duration::from_millis(600)), Some(PttDown));
        assert_eq!(m.step(FnInput::Fn(false), t + Duration::from_millis(700)), Some(PttUp));
    }

    #[test]
    fn keydown_without_fn_is_ignored() {
        let mut m = FnMachine::default();
        assert_eq!(m.step(FnInput::KeyDown, Instant::now()), None);
    }
}
