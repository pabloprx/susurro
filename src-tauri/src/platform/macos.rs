//! WP4: overlay NSWindow level/behavior/position, NSSound pings. Every NSWindow call runs in `app.run_on_main_thread`.
//! Activation policy: lib.rs uses tauri's `set_activation_policy(Accessory)`.
//! The overlay window ignores the mouse except over the pill: the page reports the pill rect (set_pill_rect),
//! and a 30 Hz thread turns mouse events on only while the cursor is inside it.

use crate::contract::PtRect;
use crate::session::Ping;
use objc2_app_kit::{NSEvent, NSScreen, NSSound, NSWindow, NSWindowCollectionBehavior};
use objc2_foundation::{MainThreadMarker, NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

/// The pill inside the overlay window: CSS px (= points), origin top-left of the window. None = no pill.
static PILL: Mutex<Option<PtRect>> = Mutex::new(None);
/// The overlay window frame in Cocoa screen coordinates (origin bottom-left of the main screen, y up).
static OVERLAY_FRAME: Mutex<Option<PtRect>> = Mutex::new(None);
/// Slack around the pill, in points: the pill's hover state grows a little before the page reports it.
const HIT_PAD: f64 = 2.0;

/// Pure: is the Cocoa mouse point over the pill? `win` is the window frame in Cocoa coordinates (y up),
/// `pill` is the page rect (y down from the window top).
pub fn over_pill(mouse: (f64, f64), win: PtRect, pill: PtRect) -> bool {
    if pill.w <= 0.0 || pill.h <= 0.0 {
        return false;
    }
    let left = win.x + pill.x - HIT_PAD;
    let right = win.x + pill.x + pill.w + HIT_PAD;
    let top = win.y + win.h - pill.y + HIT_PAD; // Cocoa y of the pill's top edge
    let bottom = win.y + win.h - pill.y - pill.h - HIT_PAD;
    (left..=right).contains(&mouse.0) && (bottom..=top).contains(&mouse.1)
}

/// set_pill_rect: the overlay page reports the pill's current rect on every state or size change.
#[tauri::command]
pub fn set_pill_rect(rect: PtRect) {
    *PILL.lock().unwrap() = Some(rect);
}

/// The 30 Hz hit test. The window starts click-through; only a cursor over the pill turns mouse events on.
/// ponytail: polling NSEvent.mouseLocation (cheap, no tap, no permission); a tracking area would need a
/// window that takes mouse events, which is the bug.
pub fn spawn_hit_test(app: &AppHandle) {
    let Some(w) = app.get_webview_window("overlay") else { return };
    let _ = w.set_ignore_cursor_events(true);
    // macOS moves the window itself when its display goes away or the layout changes: re-read the frame then,
    // or the hit zone stays where the pill was until the next setup_overlay.
    let w2 = w.clone();
    w.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. }) {
            let w3 = w2.clone();
            let _ = w2.run_on_main_thread(move || {
                let Ok(ptr) = w3.ns_window() else { return };
                // SAFETY: tauri hands out the live NSWindow pointer, and we are on the main thread.
                store_frame(unsafe { &*(ptr as *const NSWindow) });
            });
        }
    });
    std::thread::Builder::new()
        .name("overlay-hit".into())
        .spawn(move || {
            let mut ignoring = true;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(33));
                let p = NSEvent::mouseLocation();
                let (win, pill) = (*OVERLAY_FRAME.lock().unwrap(), *PILL.lock().unwrap());
                let hit = matches!((win, pill), (Some(win), Some(pill)) if over_pill((p.x, p.y), win, pill));
                if hit == ignoring {
                    ignoring = !hit;
                    if let Err(e) = w.set_ignore_cursor_events(ignoring) {
                        log::warn!("overlay hit test: {e}");
                    }
                }
            }
        })
        .expect("spawn overlay-hit");
}

/// The pill in CG global points (top-left origin, y down). None = no pill. OCR drops the text it reads there: the
/// pill shows Pablo's own words ("click “Comercial X”"), never a target.
pub fn pill_rect() -> Option<PtRect> {
    let (win, pill) = ((*OVERLAY_FRAME.lock().unwrap())?, (*PILL.lock().unwrap())?);
    Some(pill_cg(win, pill, core_graphics::display::CGDisplay::main().bounds().size.height))
}

/// Pure: the pill (page rect, y down from the window top) in a Cocoa window frame `win` -> CG global points.
pub fn pill_cg(win: PtRect, pill: PtRect, main_h: f64) -> PtRect {
    PtRect { x: win.x + pill.x, y: main_h - (win.y + win.h) + pill.y, w: pill.w, h: pill.h }
}

/// CG global rect (top-left origin, y down) -> Cocoa frame (bottom-left origin of the main screen, y up).
pub fn cg_to_cocoa(r: PtRect, main_h: f64) -> PtRect {
    PtRect { x: r.x, y: main_h - (r.y + r.h), w: r.w, h: r.h }
}

/// The marks window covers `display` (global points), click-through, above everything, never focused.
/// None hides it.
pub fn show_marks(app: &AppHandle, display: Option<PtRect>) {
    let Some(w) = app.get_webview_window("marks") else { return };
    let Some(d) = display else {
        let _ = w.hide();
        return;
    };
    let w2 = w.clone();
    let r = app.run_on_main_thread(move || {
        let Ok(ptr) = w2.ns_window() else { return };
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY: tauri hands out the live NSWindow pointer, and we are on the main thread.
        let win: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
        let main_h = NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(d.h);
        let f = cg_to_cocoa(d, main_h);
        win.setLevel(26); // just above the pill (25)
        win.setIgnoresMouseEvents(true);
        win.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        win.setFrame_display(NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.w, f.h)), true);
        win.orderFrontRegardless();
    });
    if let Err(e) = r {
        log::warn!("show_marks: {e}");
    }
}

std::thread_local! {
    /// The NSEvent global monitor of a live session (main thread only).
    static MONITOR: std::cell::RefCell<Option<objc2::rc::Retained<objc2::runtime::AnyObject>>> = const { std::cell::RefCell::new(None) };
}

/// Pablo's own mouse and keys while a live command session runs (VOICE_CONTROL_V2 3): an NSEvent global monitor
/// for MouseMoved | LeftMouseDown | RightMouseDown | ScrollWheel | KeyDown. Global key events need Accessibility,
/// which voice control has already; no Input Monitoring prompt. Installed at the session start, removed at its end.
/// Unverified on macOS 26 until Pablo runs it: the first hit logs "live: Pablo took over".
pub fn watch_user_input(app: &AppHandle, on: bool) {
    use objc2_app_kit::{NSEventMask, NSEventType};
    let app2 = app.clone();
    let bound: Vec<u16> = {
        let s = app.state::<crate::AppState>().settings.read().unwrap().hotkeys.clone();
        [s.command.as_str(), s.cancel.as_str(), s.ptt.as_str()].iter().filter_map(|b| b.rsplit('+').next().and_then(crate::paste::keycode)).collect()
    };
    let r = app.run_on_main_thread(move || {
        MONITOR.with(|m| {
            let mut m = m.borrow_mut();
            if let Some(old) = m.take() {
                // SAFETY: the object came from addGlobalMonitorForEventsMatchingMask on this thread.
                unsafe { NSEvent::removeMonitor(&old) };
            }
            if !on {
                return;
            }
            let moved = std::cell::Cell::new(0.0f64);
            let block = block2::RcBlock::new(move |e: std::ptr::NonNull<NSEvent>| {
                // SAFETY: AppKit hands the block a live event for the duration of the call.
                let e = unsafe { e.as_ref() };
                let ty = e.r#type();
                let user_data = e.CGEvent().map(|c| objc2_core_graphics::CGEvent::integer_value_field(Some(&c), objc2_core_graphics::CGEventField(42))).unwrap_or(0);
                let key = (ty == NSEventType::KeyDown).then(|| e.keyCode());
                // susurro's own tagged moves (click_at jumps the cursor) must not count toward Pablo's 6 pt.
                if ty == NSEventType::MouseMoved && user_data != crate::paste::SUSURRO_TAG {
                    moved.set(moved.get() + e.deltaX().abs() + e.deltaY().abs());
                }
                if crate::live::is_users(user_data, key, &bound, if key.is_some() { 0.0 } else if ty == NSEventType::MouseMoved { moved.get() } else { 99.0 }) {
                    moved.set(0.0);
                    // a left click's spot (CG global points): it may answer a pick
                    let at = (ty == NSEventType::LeftMouseDown).then(|| e.CGEvent().map(|c| objc2_core_graphics::CGEvent::location(Some(&c)))).flatten().map(|p| (p.x, p.y));
                    crate::live::user_input(&app2, ty != NSEventType::MouseMoved && ty != NSEventType::ScrollWheel, at);
                }
            });
            let mask = NSEventMask::MouseMoved | NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::ScrollWheel | NSEventMask::KeyDown;
            *m = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block);
            if m.is_none() {
                log::warn!("live: the user-input monitor did not install (Accessibility?)");
            }
        });
    });
    if let Err(e) = r {
        log::warn!("watch_user_input: {e}");
    }
}

/// Keeps the overlay frame (Cocoa coordinates) for the hit-test thread. Main thread only.
fn store_frame(win: &NSWindow) {
    let f = win.frame();
    *OVERLAY_FRAME.lock().unwrap() = Some(PtRect { x: f.origin.x, y: f.origin.y, w: f.size.width, h: f.size.height });
}

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
            let origin = NSPoint::new((vf.origin.x + (vf.size.width - size.width) / 2.0).round(), vf.origin.y + BOTTOM_GAP);
            win.setFrameOrigin(origin);
            store_frame(win);
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

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: PtRect = PtRect { x: 500.0, y: 40.0, w: 360.0, h: 64.0 }; // Cocoa: bottom edge 40 pt above the screen bottom
    const PILL: PtRect = PtRect { x: 80.0, y: 10.0, w: 200.0, h: 36.0 }; // page px, from the window top

    #[test]
    fn pill_in_cg_points() {
        // main screen 900 pt tall: the window top is at Cocoa y 104 = CG y 796; the pill starts 10 pt below it
        assert_eq!(pill_cg(WIN, PILL, 900.0), PtRect { x: 580.0, y: 806.0, w: 200.0, h: 36.0 });
        // the same pill seen by over_pill (Cocoa y 58..94) = CG y 806..842
        assert!(over_pill((680.0, 900.0 - 824.0), WIN, PILL));
    }

    #[test]
    fn hit_only_on_the_pill() {
        // Cocoa y of the pill: top = 40 + 64 - 10 = 94, bottom = 94 - 36 = 58. x: 580..780.
        assert!(over_pill((680.0, 76.0), WIN, PILL), "center");
        assert!(over_pill((580.0, 58.0), WIN, PILL), "bottom-left corner");
        assert!(over_pill((781.5, 95.5), WIN, PILL), "inside the 2 pt pad");
        assert!(!over_pill((520.0, 76.0), WIN, PILL), "transparent window area left of the pill");
        assert!(!over_pill((680.0, 100.0), WIN, PILL), "transparent area above the pill");
        assert!(!over_pill((680.0, 45.0), WIN, PILL), "transparent area below the pill");
        assert!(!over_pill((785.0, 76.0), WIN, PILL), "past the pad");
    }

    #[test]
    fn empty_pill_never_hits() {
        assert!(!over_pill((680.0, 76.0), WIN, PtRect { w: 0.0, ..PILL }));
    }

    #[test]
    fn idle_pill_is_tiny() {
        // The idle pill is 44 x 8, centered, 18 px above the window bottom.
        let idle = PtRect { x: 158.0, y: 38.0, w: 44.0, h: 8.0 };
        assert!(over_pill((680.0, 62.0), WIN, idle));
        assert!(!over_pill((680.0, 70.0), WIN, idle), "10 pt above it is not a click on the pill");
        assert!(!over_pill((650.0, 62.0), WIN, idle));
    }

    #[test]
    fn secondary_display_and_flip() {
        // A display left of the main one: negative x; Cocoa y flips with the main screen height.
        let r = cg_to_cocoa(PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 }, 982.0);
        assert_eq!(r, PtRect { x: -1920.0, y: -98.0, w: 1920.0, h: 1080.0 });
        let r = cg_to_cocoa(PtRect { x: 0.0, y: 0.0, w: 1512.0, h: 982.0 }, 982.0);
        assert_eq!(r.y, 0.0);
    }
}
