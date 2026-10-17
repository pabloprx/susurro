//! WP4: mic / accessibility / input monitoring status, request, deep links, fn-key setting check (PLAN 8).

use crate::contract::{PermKind, PermState, Permissions};

const PANE: &str = "x-apple.systempreferences:com.apple.preference.security?";
const KEYBOARD_PANE: &str = "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";

/// `AppleFnUsageType` (com.apple.HIToolbox): 0 Do Nothing, 1 Change Input Source, 2 Show Emoji & Symbols,
/// 3 Start Dictation (macos-defaults.com, nix-darwin docs). The key is missing until the user changes the
/// setting; sources disagree on what "missing" means (Do Nothing vs Emoji), so missing = not free (PLAN 8).
pub fn fn_free(value: Option<i64>) -> bool {
    value == Some(0)
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

    pub fn input_monitoring() -> bool {
        objc2_core_graphics::CGPreflightListenEventAccess()
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
    #[cfg(not(target_os = "macos"))]
    false
}

#[tauri::command]
pub fn permissions_status() -> Result<Permissions, String> {
    #[cfg(target_os = "macos")]
    return Ok(Permissions {
        microphone: mac::microphone(),
        accessibility: mac::accessibility(),
        input_monitoring: mac::input_monitoring(),
        fn_key_free: fn_usage_type().map(fn_free),
    });
    #[cfg(not(target_os = "macos"))]
    Err("not supported on this OS".into())
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
        PermKind::Accessibility => {
            if !mac::prompt_accessibility() {
                open(&format!("{PANE}Privacy_Accessibility"));
            }
        }
        PermKind::InputMonitoring => {
            // The hotkey module polls the grant and starts the fn tap by itself.
            if !mac::prompt_input_monitoring() {
                open(&format!("{PANE}Privacy_ListenEvent"));
            }
        }
        PermKind::FnKey => open(KEYBOARD_PANE),
    }
    #[cfg(not(target_os = "macos"))]
    let _ = kind;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_key_mapping() {
        assert!(fn_free(Some(0)));
        for v in [1, 2, 3] {
            assert!(!fn_free(Some(v)));
        }
        assert!(!fn_free(None), "missing key = OS default = not free");
    }
}
