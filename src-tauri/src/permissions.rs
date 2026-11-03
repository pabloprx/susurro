//! WP4: mic / accessibility / input monitoring status, request, deep links, fn-key setting check, relaunch (PLAN 8).

use crate::contract::{PermKind, PermState, Permissions};

const PANE: &str = "x-apple.systempreferences:com.apple.preference.security?";
const KEYBOARD_PANE: &str = "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";

/// `AppleFnUsageType` (com.apple.HIToolbox): 0 Do Nothing, 1 Change Input Source, 2 Show Emoji & Symbols,
/// 3 Start Dictation (macos-defaults.com, nix-darwin docs). The key is missing until the user changes the
/// setting; sources disagree on what "missing" means (Do Nothing vs Emoji), so missing = not free (PLAN 8).
pub fn fn_free(value: Option<i64>) -> bool {
    value == Some(0)
}

/// Pure: Input Monitoring (granted, usable in this process). `probe` creates a throwaway tap, which can
/// itself trigger the system prompt, so it runs only when preflight already says yes or the tap is wanted
/// (a modifier-only talk key). F1 uses RegisterEventHotKey and needs no Input Monitoring.
fn im_state(preflight: bool, tap_wanted: bool, tap_running: bool, probe: impl FnOnce() -> bool) -> (bool, bool) {
    if tap_running {
        return (true, true);
    }
    if preflight {
        return (true, probe());
    }
    if tap_wanted && probe() {
        // Preflight can stay false after a grant until restart; a live tap means the grant works now.
        return (true, true);
    }
    (false, false)
}

/// Pure: a grant shows in TCC but this process cannot use it.
fn needs_relaunch(granted: bool, usable: bool, tap_failed: bool) -> bool {
    granted && (!usable || tap_failed)
}

/// Pure: is a permission susurro needs right now still missing? Input Monitoring counts only for a
/// modifier-only talk key.
fn missing(p: &Permissions, mod_only: bool) -> bool {
    p.microphone != PermState::Granted || !p.accessibility || p.needs_relaunch || (mod_only && !p.input_monitoring)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::CFString;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    }

    pub fn accessibility() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Shows the system Accessibility prompt (once per app; later calls are silent).
    pub fn prompt_accessibility() -> bool {
        let opts = CFDictionary::from_CFType_pairs(&[(
            CFString::new("AXTrustedCheckOptionPrompt"),
            CFBoolean::true_value(),
        )]);
        unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef()) != 0 }
    }

    pub fn input_monitoring() -> (bool, bool) {
        im_state(
            objc2_core_graphics::CGPreflightListenEventAccess(),
            crate::hotkey::tap_wanted(),
            crate::hotkey::tap_running(),
            crate::hotkey::probe_tap,
        )
    }

    pub fn prompt_input_monitoring() -> bool {
        objc2_core_graphics::CGRequestListenEventAccess()
    }

    pub fn microphone() -> PermState {
        let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return PermState::Undetermined };
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
            AVAuthorizationStatus::Authorized => PermState::Granted,
            AVAuthorizationStatus::NotDetermined => PermState::Undetermined,
            _ => PermState::Denied, // Denied | Restricted
        }
    }

    pub fn prompt_microphone() {
        if let Some(audio) = unsafe { AVMediaTypeAudio } {
            let done = block2::RcBlock::new(|_granted: objc2::runtime::Bool| {});
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &done) };
        }
    }
}

/// `AppleFnUsageType` via `defaults`. None = `defaults` could not run.
fn fn_usage_type() -> Option<Option<i64>> {
    let out = std::process::Command::new("defaults")
        .args(["read", "com.apple.HIToolbox", "AppleFnUsageType"])
        .output()
        .ok()?;
    // A missing key exits 1 with "does not exist" on stderr.
    Some(if out.status.success() { String::from_utf8_lossy(&out.stdout).trim().parse().ok() } else { None })
}

fn open(url: &str) {
    if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
        log::warn!("open {url}: {e}");
    }
}

/// Paste needs it to post Cmd+V (PLAN 5).
pub fn accessibility_trusted() -> bool {
    #[cfg(target_os = "macos")]
    return mac::accessibility();
    #[cfg(target_os = "linux")]
    return crate::platform::linux::can_type();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    false
}

/// Linux has no per-app grants for the mic or typing, so onboarding never waits here. Without a typing
/// helper (wtype, ydotool or xdotool) dictations land on the clipboard, and spawn_watcher says so once.
#[cfg(target_os = "linux")]
fn linux_status() -> Permissions {
    Permissions {
        microphone: PermState::Granted,
        accessibility: true,
        input_monitoring: true,
        fn_key_free: None,
        needs_relaunch: false,
        screen_recording: false,
    }
}

/// Live status: every call asks TCC again (AXIsProcessTrusted, preflight, AV status). `with_fn` also runs
/// `defaults` for the fn-key setting (a subprocess, so the 1 s watcher skips it).
#[cfg(target_os = "macos")]
fn status(with_fn: bool) -> Permissions {
    let (granted, usable) = mac::input_monitoring();
    Permissions {
        microphone: mac::microphone(),
        accessibility: mac::accessibility(),
        input_monitoring: granted,
        fn_key_free: if with_fn { fn_usage_type().map(fn_free) } else { None },
        needs_relaunch: needs_relaunch(granted, usable, crate::hotkey::tap_failed()),
        screen_recording: objc2_core_graphics::CGPreflightScreenCaptureAccess(),
    }
}

#[tauri::command]
pub fn permissions_status() -> Result<Permissions, String> {
    #[cfg(target_os = "macos")]
    return Ok(status(true));
    #[cfg(target_os = "linux")]
    return Ok(linux_status());
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err("not supported on this OS".into())
}

/// Emits `permissions_changed` (Permissions) when a grant changes: every 1 s while something susurro needs
/// is missing, every 5 s otherwise (a revoke). Also one startup toast if paste cannot work.
/// ponytail: polling, macOS has no TCC change callback.
pub fn spawn_watcher(app: tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    std::thread::Builder::new()
        .name("perm-watch".into())
        .spawn(move || {
            use tauri::{Emitter, Manager};
            let mod_only = || {
                let s = app.state::<crate::AppState>();
                let ptt = s.settings.read().unwrap().hotkeys.ptt.clone();
                crate::hotkey::mod_key(&ptt).is_some()
            };
            let mut last = status(false);
            // Let the overlay page load first, so the toast has somewhere to show.
            std::thread::sleep(std::time::Duration::from_secs(3));
            let onboarded = app.state::<crate::AppState>().settings.read().unwrap().onboarded;
            if onboarded && !mac::accessibility() {
                crate::session::toast(&app, None, "ax-missing", crate::contract::ToastKind::Warn, "Turn on Accessibility so susurro can paste", &[("fix-ax", "Turn on")], 10_000);
            }
            loop {
                let now = status(false);
                if now != last {
                    log::info!("permissions changed: {now:?}");
                    let _ = app.emit("permissions_changed", Permissions { fn_key_free: fn_usage_type().map(fn_free), ..now.clone() });
                    last = now;
                }
                let secs = if missing(&last, mod_only()) { 1 } else { 5 };
                std::thread::sleep(std::time::Duration::from_secs(secs));
            }
        })
        .expect("spawn perm-watch");
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if !crate::platform::linux::can_type() {
            let tool = if crate::platform::linux::wayland() { "wtype or ydotool" } else { "xdotool" };
            log::warn!("no typing helper found: dictations go to the clipboard only");
            crate::session::toast(&app, None, "ax-missing", crate::contract::ToastKind::Warn, &format!("Install {tool} so susurro can paste. Until then, text goes to your clipboard"), &[], 10_000);
        }
    });
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = app;
}

/// Shows the system prompt where one exists, and opens the Settings pane when the prompt cannot help.
#[tauri::command]
pub fn request_permission(kind: PermKind) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    match kind {
        PermKind::Microphone => match mac::microphone() {
            PermState::Undetermined => mac::prompt_microphone(),
            PermState::Denied => open(&format!("{PANE}Privacy_Microphone")),
            PermState::Granted => {}
        },
        // Never prompt for a grant that is already there.
        PermKind::Accessibility if mac::accessibility() => {}
        PermKind::Accessibility => {
            if !mac::prompt_accessibility() {
                open(&format!("{PANE}Privacy_Accessibility"));
            }
        }
        PermKind::InputMonitoring if objc2_core_graphics::CGPreflightListenEventAccess() || crate::hotkey::tap_running() => {}
        PermKind::InputMonitoring => {
            // The hotkey module retries its tap every second and starts by itself once the grant is live.
            if !mac::prompt_input_monitoring() {
                open(&format!("{PANE}Privacy_ListenEvent"));
            }
        }
        PermKind::FnKey => open(KEYBOARD_PANE),
        PermKind::ScreenRecording if objc2_core_graphics::CGPreflightScreenCaptureAccess() => {}
        PermKind::ScreenRecording => {
            // The prompt shows once per app; after that only the Settings pane can grant it.
            if !objc2_core_graphics::CGRequestScreenCaptureAccess() {
                open(&format!("{PANE}Privacy_ScreenCapture"));
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
    Ok(())
}

/// Marker for the next launch: show the main window (onboarding resumes on its saved step).
pub const SHOW_MAIN_MARKER: &str = ".show_main_on_launch";

/// Restart susurro, e.g. after an Input Monitoring grant that this process cannot use yet.
/// The new instance opens the main window (lib.rs reads the marker).
#[tauri::command]
pub fn relaunch_app(app: tauri::AppHandle, state: tauri::State<'_, crate::AppState>) {
    if let Err(e) = std::fs::write(state.data_dir.join(SHOW_MAIN_MARKER), b"") {
        log::warn!("relaunch marker: {e}");
    }
    log::info!("relaunch requested");
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_monitoring_decision() {
        let no = || -> bool { panic!("must not probe: a tap can show the system prompt") };
        assert_eq!(im_state(false, false, false, no), (false, false), "F1 user without the grant: no probe, no prompt");
        assert_eq!(im_state(false, true, true, no), (true, true), "running tap");
        assert_eq!(im_state(false, true, false, || true), (true, true), "grant live before preflight sees it");
        assert_eq!(im_state(false, true, false, || false), (false, false));
        assert_eq!(im_state(true, false, false, || true), (true, true));
        assert_eq!(im_state(true, false, false, || false), (true, false), "granted, unusable in this process");
    }

    #[test]
    fn relaunch_only_for_an_unusable_grant() {
        assert!(!needs_relaunch(false, false, false));
        assert!(!needs_relaunch(true, true, false));
        assert!(needs_relaunch(true, false, false));
        assert!(needs_relaunch(true, true, true), "the tap failed to start after creation");
    }

    #[test]
    fn watcher_counts_input_monitoring_only_for_modifier_keys() {
        let all = Permissions { microphone: PermState::Granted, accessibility: true, input_monitoring: false, fn_key_free: None, needs_relaunch: false, screen_recording: false };
        assert!(!missing(&all, false), "F1 needs no Input Monitoring");
        assert!(missing(&all, true));
        assert!(missing(&Permissions { accessibility: false, ..all.clone() }, false));
        assert!(missing(&Permissions { microphone: PermState::Undetermined, ..all.clone() }, false));
        assert!(missing(&Permissions { needs_relaunch: true, ..all }, false));
    }

    #[test]
    fn fn_key_mapping() {
        assert!(fn_free(Some(0)));
        for v in [1, 2, 3] {
            assert!(!fn_free(Some(v)));
        }
        assert!(!fn_free(None), "missing key = OS default = not free");
    }
}
