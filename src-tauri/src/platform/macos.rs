//! WP4: overlay NSWindow level/behavior/position, NSSound pings. Every NSWindow call runs in `app.run_on_main_thread`.
//! Activation policy: lib.rs uses tauri's `set_activation_policy(Accessory)`.

use objc2_app_kit::{NSEvent, NSScreen, NSSound, NSWindow, NSWindowCollectionBehavior};
use objc2_foundation::{MainThreadMarker, NSPoint, NSPointInRect, NSString};
use crate::session::Ping;
use tauri::{AppHandle, Manager};

/// px between the pill window and the bottom of the visible frame (above the Dock).
const BOTTOM_GAP: f64 = 12.0;

/// Level 25, CanJoinAllSpaces | FullScreenAuxiliary | Stationary | IgnoresCycle, bottom-center of the mouse screen.
/// Idempotent and cheap: call it before every overlay show, so the pill follows the mouse to the right screen
/// and the level survives tauri's own show/alwaysOnTop calls.
pub fn setup_overlay(app: &AppHandle) {
    let Some(w) = app.get_webview_window("overlay") else { return };
    let r = app.run_on_main_thread(move || {
        let Ok(ptr) = w.ns_window() else { return };
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY: tauri hands out the live NSWindow pointer, and we are on the main thread.
        let win: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
        win.setLevel(25); // kCGStatusWindowLevel: above floating panels, below the menu bar's menus
        win.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        let mouse = NSEvent::mouseLocation();
        let screens = NSScreen::screens(mtm);
        let screen = screens
            .iter()
            .find(|s| NSPointInRect(mouse, s.frame()))
            .or_else(|| NSScreen::mainScreen(mtm));
        if let Some(s) = screen {
            let vf = s.visibleFrame();
            let size = win.frame().size;
            // Cocoa coordinates: origin bottom-left, so y grows upwards from the visible frame's bottom.
            win.setFrameOrigin(NSPoint::new(
                (vf.origin.x + (vf.size.width - size.width) / 2.0).round(),
                vf.origin.y + BOTTOM_GAP,
            ));
        }
    });
    if let Err(e) = r {
        log::warn!("setup_overlay: {e}");
    }
}

pub fn ping(p: Ping) {
    let name = match p {
        Ping::Start => "Tink",
        Ping::Stop => "Pop",
        Ping::Error => "Basso",
    };
    // soundNamed returns a cached system instance, so it keeps playing after our handle drops.
    if let Some(s) = NSSound::soundNamed(&NSString::from_str(name)) {
        s.play();
    }
}
