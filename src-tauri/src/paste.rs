//! WP4: frontmost app, full pasteboard save/restore, Cmd+V, Enter (PLAN 5). macOS via objc2 + core-graphics.
//! Before Cmd+V it reads the focused element over Accessibility: a non-text focus (a Finder list) gets
//! copy-only, and the char before the caret decides a leading space (Wispr's smart spacing).
//! Runs on the session actor thread. The clipboard restore runs `restore_delay_ms` later on its own
//! thread, so the overlay, History latency and the next hotkey press do not wait for it.

use crate::contract::PasteOpts;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppRef {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub pid: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PasteOutcome {
    Pasted,
    /// No Accessibility: text left on the clipboard.
    CopiedOnly,
    /// The focused element takes no text: text left on the clipboard, no Cmd+V.
    NoField,
}

/// What the focused element says about a paste.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Editable,
    NotEditable,
    /// No answer (Electron apps expose no AX tree by default): paste anyway, as before.
    Unknown,
}

const TEXT_ROLES: [&str; 4] = ["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];
/// Roles that never take a paste. Narrow on purpose: AXWindow, AXGroup and AXWebArea can wrap a
/// web text field, so they stay Unknown and get the Cmd+V. No AXTable/AXRow/AXCell: Cmd+V into a
/// selected Numbers or Excel cell is how you type into it.
const NON_TEXT_ROLES: [&str; 11] = [
    "AXList", "AXOutline", "AXBrowser", "AXButton", "AXCheckBox",
    "AXRadioButton", "AXPopUpButton", "AXMenuButton", "AXSlider", "AXImage", "AXMenuItem",
];

/// Pure: role + "is AXValue settable" -> verdict.
fn field_verdict(role: Option<&str>, value_settable: bool) -> Field {
    match role {
        None => Field::Unknown,
        Some(r) if TEXT_ROLES.contains(&r) || value_settable => Field::Editable,
        Some(r) if NON_TEXT_ROLES.contains(&r) => Field::NotEditable,
        Some(_) => Field::Unknown,
    }
}

/// Pure: prepend a space when the caret sits right after a word or a sentence end ("Hi.|" + "Thanks").
/// `before`: up to 2 chars before the caret ("" = start of the field, or no AX answer).
fn smart_space(text: &str, before: &str) -> String {
    let mut back = before.chars().rev();
    let (b, prev) = (back.next(), back.next());
    // A straight quote is an opener only at the start of the field or after a space or bracket:
    // `"|` + "hi" stays glued, `He said "hi"|` + "Then" gets its space.
    let opener = |c: Option<char>| c.is_none_or(|p| p.is_whitespace() || "([{".contains(p));
    let joins = |b: char| b.is_whitespace() || "([{“‘¿¡/-@#_".contains(b) || ("\"'".contains(b) && opener(prev));
    let starts_word = text.chars().next().is_some_and(|f| f.is_alphanumeric() || "¿¡(\"“".contains(f));
    match b {
        Some(b) if !joins(b) && starts_word => format!(" {text}"),
        _ => text.to_string(),
    }
}

/// Electron / Chromium apps read the pasteboard late: an early restore pastes the old clipboard.
/// ponytail: bundle-id list, 1500 ms not measured per app; measure and tune (PLAN 12).
const SLOW_APPS: [&str; 10] = [
    "com.tinyspeck.slackmacgap",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.microsoft.VSCode",
    "com.hnc.Discord",
    "notion.id",
    "com.microsoft.teams2",
    "com.anthropic.claudefordesktop",
    "com.openai.chat",
    "net.whatsapp.WhatsApp",
    "com.figma.Desktop",
];
const SLOW_DELAY_MS: u64 = 1500;

/// Pure: restore delay for the app that takes the paste.
fn restore_delay(bundle: Option<&str>, base_ms: u32) -> u64 {
    let slow = bundle.is_some_and(|b| SLOW_APPS.contains(&b));
    if slow { (base_ms as u64).max(SLOW_DELAY_MS) } else { base_ms as u64 }
}

const MAX_SNAPSHOT: usize = 50 * 1024 * 1024;
const FILE_URL: &str = "public.file-url";

/// Pure: keep collecting the snapshot? Skip it above 50 MB or when files are on the clipboard.
fn snapshot_ok(ty: &str, total_bytes: usize) -> bool {
    ty != FILE_URL && total_bytes <= MAX_SNAPSHOT
}

/// Pure: restore only if nobody wrote to the pasteboard after our own write.
fn should_restore(ours: isize, now: isize) -> bool {
    ours == now
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSData, NSString};

    /// Items, each a list of (type, bytes).
    pub type Snapshot = Vec<Vec<(String, Vec<u8>)>>;

    pub fn frontmost_app() -> Option<AppRef> {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        Some(AppRef {
            bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
            name: app.localizedName().map(|s| s.to_string()),
            pid: app.processIdentifier(),
        })
    }

    /// Remember every app that comes to the front, except susurro: a click on the overlay bar
    /// (a plain NSWindow) activates susurro, and the dictation still belongs to the app behind it.
    pub fn watch_activations() {
        use objc2_app_kit::NSWorkspaceDidActivateApplicationNotification;
        let block = block2::RcBlock::new(|_n: std::ptr::NonNull<objc2_foundation::NSNotification>| note_front(frontmost_app()));
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: a nil queue runs the block on the posting thread; the block only reads NSWorkspace.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceDidActivateApplicationNotification), None, None, &block)
        };
        std::mem::forget(token); // observe for the life of the app
        note_front(frontmost_app());
    }

    pub fn snapshot(pb: &NSPasteboard) -> Option<Snapshot> {
        let mut total = 0;
        let mut snap = vec![];
        for item in pb.pasteboardItems()?.iter() {
            let mut entry = vec![];
            for ty in item.types().iter() {
                let ty_s = ty.to_string();
                let data = item.dataForType(&ty).map(|d| d.to_vec()).unwrap_or_default();
                total += data.len();
                if !snapshot_ok(&ty_s, total) {
                    return None;
                }
                entry.push((ty_s, data));
            }
            snap.push(entry);
        }
        Some(snap)
    }

    pub fn restore(pb: &NSPasteboard, snap: &Snapshot) {
        pb.clearContents();
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = snap
            .iter()
            .map(|types| {
                let item = NSPasteboardItem::new();
                for (ty, data) in types {
                    item.setData_forType(&NSData::with_bytes(data), &NSString::from_str(ty));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        if !items.is_empty() {
            pb.writeObjects(&NSArray::from_retained_slice(&items));
        }
    }

    /// Clears the pasteboard, writes the text, returns the changeCount of our write.
    pub fn write_text(pb: &NSPasteboard, text: &str, concealed: bool) -> isize {
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        if concealed {
            // nspasteboard.org convention: clipboard managers skip this item.
            pb.setData_forType(Some(&NSData::new()), &NSString::from_str("org.nspasteboard.ConcealedType"));
        }
        pb.changeCount()
    }

    pub fn key(code: u16, flags: CGEventFlags) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else { return };
        for down in [true, false] {
            if let Ok(e) = CGEvent::new_keyboard_event(src.clone(), code, down) {
                // Explicit flags: drops a still-held fn (or any other modifier) from the event.
                e.set_flags(flags);
                e.post(CGEventTapLocation::HID);
            }
        }
    }

    /// Bring `pid` to the front (susurro's own window took focus) and wait up to 300 ms for it.
    pub fn activate(pid: i32) {
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else { return };
        app.activateWithOptions(NSApplicationActivationOptions::empty());
        for _ in 0..30 {
            if frontmost_app().is_some_and(|a| a.pid == pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Focused element over AX: (role, AXValue settable, char before the caret). Never blocks long:
    /// a 250 ms messaging timeout covers a hung app.
    pub fn focus_info() -> (Option<String>, bool, String) {
        use core_foundation::base::{CFType, CFTypeRef, TCFType};
        use core_foundation::string::{CFString, CFStringRef};
        use std::ffi::c_void;

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CFRange {
            location: isize,
            length: isize,
        }
        const K_AX_VALUE_CFRANGE: u32 = 4;

        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn AXUIElementCreateSystemWide() -> CFTypeRef;
            fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
            fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
            fn AXUIElementIsAttributeSettable(el: CFTypeRef, attr: CFStringRef, out: *mut u8) -> i32;
            fn AXUIElementCopyParameterizedAttributeValue(el: CFTypeRef, attr: CFStringRef, param: CFTypeRef, out: *mut CFTypeRef) -> i32;
            fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> u8;
            fn AXValueCreate(ty: u32, ptr: *const c_void) -> CFTypeRef;
        }
        /// Copy one attribute; the result is owned (create rule).
        fn attr(el: &CFType, name: &str) -> Option<CFType> {
            let mut out: CFTypeRef = std::ptr::null();
            let err = unsafe { AXUIElementCopyAttributeValue(el.as_CFTypeRef(), CFString::new(name).as_concrete_TypeRef(), &mut out) };
            (err == 0 && !out.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(out) })
        }

        let sys = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        unsafe { AXUIElementSetMessagingTimeout(sys.as_CFTypeRef(), 0.25) };
        let Some(el) = attr(&sys, "AXFocusedUIElement") else { return (None, false, String::new()) };
        let role = attr(&el, "AXRole").and_then(|r| r.downcast::<CFString>()).map(|s| s.to_string());
        let mut settable = 0u8;
        unsafe { AXUIElementIsAttributeSettable(el.as_CFTypeRef(), CFString::new("AXValue").as_concrete_TypeRef(), &mut settable) };
        let before = attr(&el, "AXSelectedTextRange").and_then(|v| {
            let mut r = CFRange { location: 0, length: 0 };
            let ok = unsafe { AXValueGetValue(v.as_CFTypeRef(), K_AX_VALUE_CFRANGE, &mut r as *mut _ as *mut c_void) } != 0;
            if !ok || r.location <= 0 {
                return None;
            }
            let n = r.location.min(2); // 2 chars: a straight quote needs the char before it (smart_space)
            let prev = CFRange { location: r.location - n, length: n };
            let param = unsafe { CFType::wrap_under_create_rule(AXValueCreate(K_AX_VALUE_CFRANGE, &prev as *const _ as *const c_void)) };
            let mut out: CFTypeRef = std::ptr::null();
            let attr = CFString::new("AXStringForRange");
            let err = unsafe { AXUIElementCopyParameterizedAttributeValue(el.as_CFTypeRef(), attr.as_concrete_TypeRef(), param.as_CFTypeRef(), &mut out) };
            if err != 0 || out.is_null() {
                return None;
            }
            unsafe { CFType::wrap_under_create_rule(out) }.downcast::<CFString>().map(|s| s.to_string())
        })
        .unwrap_or_default();
        (role, settable != 0, before)
    }

    pub fn general() -> Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }

    /// (changeCount of our write, what to put back). One at a time: a new paste first restores the
    /// previous one, so its snapshot is the user's clipboard, not our last dictation.
    static PENDING: std::sync::Mutex<Option<(isize, Snapshot)>> = std::sync::Mutex::new(None);

    fn restore_pending(only: Option<isize>) {
        let mut p = PENDING.lock().unwrap();
        if p.as_ref().is_some_and(|(ours, _)| only.is_none_or(|o| o == *ours)) {
            let (ours, snap) = p.take().unwrap();
            let pb = general();
            if should_restore(ours, pb.changeCount()) {
                restore(&pb, &snap);
            }
        }
    }

    /// Finish any earlier restore now, so the snapshot taken next is the user's clipboard.
    pub fn flush_restore() {
        restore_pending(None);
    }

    pub fn restore_later(ours: isize, snap: Snapshot, delay_ms: u64) {
        *PENDING.lock().unwrap() = Some((ours, snap));
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            restore_pending(Some(ours));
        });
    }
}

static LAST_EXTERNAL: std::sync::Mutex<Option<AppRef>> = std::sync::Mutex::new(None);

fn note_front(front: Option<AppRef>) {
    if let Some(f) = front.filter(|f| f.pid != std::process::id() as i32) {
        *LAST_EXTERNAL.lock().unwrap() = Some(f);
    }
}

/// The frontmost app, or the last other app when susurro itself is in front.
fn pick_target(front: Option<AppRef>, last: Option<AppRef>, me: i32) -> Option<AppRef> {
    match front {
        Some(f) if f.pid == me => last.or(Some(f)),
        f => f,
    }
}

/// Where a dictation or a "paste last" belongs: the app in front, never susurro if another app was
/// in front before (a click on the overlay or a toast activates susurro).
pub fn target_app() -> Option<AppRef> {
    let front = frontmost_app();
    note_front(front.clone());
    pick_target(front, LAST_EXTERNAL.lock().unwrap().clone(), std::process::id() as i32)
}

/// Start remembering the last frontmost app that is not susurro (see `target_app`). Main thread.
pub fn watch_activations() {
    #[cfg(target_os = "macos")]
    mac::watch_activations();
}

pub fn frontmost_app() -> Option<AppRef> {
    #[cfg(target_os = "macos")]
    return mac::frontmost_app();
    #[cfg(not(target_os = "macos"))]
    None
}

/// `target`: the app that was frontmost when the dictation started. If susurro's own window is in front
/// now (a click in History or a toast), that app comes back to the front before the Cmd+V.
pub fn paste(text: &str, o: &PasteOpts, target: Option<&AppRef>) -> PasteOutcome {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::CGEventFlags;
        #[cfg(debug_assertions)]
        if let Some(why) = crate::test_hooks::paste_blocked() {
            // Pablo uses this Mac during tests: no keys and no clipboard write outside the test target.
            log::warn!("test paste guard: {why}; not pasting {} chars", text.chars().count());
            return PasteOutcome::CopiedOnly;
        }
        mac::flush_restore();
        let pb = mac::general();
        if !crate::permissions::accessibility_trusted() {
            // CGEvent cannot post without Accessibility: leave the text on the clipboard, no restore.
            mac::write_text(&pb, text, false);
            return PasteOutcome::CopiedOnly;
        }
        let me = std::process::id() as i32;
        let mut front = mac::frontmost_app();
        if let Some(t) = target.filter(|t| t.pid != me && front.as_ref().is_some_and(|f| f.pid == me)) {
            mac::activate(t.pid);
            front = mac::frontmost_app();
        }
        let (role, settable, before) = mac::focus_info();
        let verdict = field_verdict(role.as_deref(), settable);
        log::info!("paste: app {:?}, focus {role:?} settable={settable} -> {verdict:?}", front.as_ref().and_then(|f| f.bundle_id.as_deref()));
        if verdict == Field::NotEditable {
            mac::write_text(&pb, text, false);
            return PasteOutcome::NoField;
        }
        let text = smart_space(text, &before);
        let snap = if o.restore_clipboard { mac::snapshot(&pb) } else { None };
        let ours = mac::write_text(&pb, &text, true);
        mac::key(9, CGEventFlags::CGEventFlagCommand); // kVK_ANSI_V
        if let Some(snap) = snap {
            let bundle = front.as_ref().and_then(|f| f.bundle_id.as_deref());
            mac::restore_later(ours, snap, restore_delay(bundle, o.restore_delay_ms));
        }
        PasteOutcome::Pasted
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (text, o, target);
        PasteOutcome::CopiedOnly
    }
}

pub fn press_enter() {
    #[cfg(debug_assertions)]
    if let Some(why) = crate::test_hooks::paste_blocked() {
        return log::warn!("test paste guard: {why}; not pressing Return");
    }
    #[cfg(target_os = "macos")]
    mac::key(36, core_graphics::event::CGEventFlags::empty()); // kVK_Return
}

/// One key press (voice control): `cmd` adds Command. Same test guard as the paste.
pub fn key_press(code: u16, cmd: bool) {
    #[cfg(debug_assertions)]
    if let Some(why) = crate::test_hooks::paste_blocked() {
        return log::warn!("test paste guard: {why}; not pressing key {code}");
    }
    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::CGEventFlags;
        mac::key(code, if cmd { CGEventFlags::CGEventFlagCommand } else { CGEventFlags::empty() });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (code, cmd);
}

/// Put text on the clipboard and leave it there (copy last transcript).
pub fn copy(text: &str) {
    #[cfg(target_os = "macos")]
    mac::write_text(&mac::general(), text, false);
    #[cfg(not(target_os = "macos"))]
    let _ = text;
}

/// Current plain-text clipboard, for examples and checks.
pub fn clipboard_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    return mac::general()
        .stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString })
        .map(|s| s.to_string());
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_skips_files_and_huge_clipboards() {
        assert!(snapshot_ok("public.utf8-plain-text", 10));
        assert!(snapshot_ok("public.png", MAX_SNAPSHOT));
        assert!(!snapshot_ok("public.png", MAX_SNAPSHOT + 1));
        assert!(!snapshot_ok(FILE_URL, 0));
    }

    #[test]
    fn field_verdicts() {
        assert_eq!(field_verdict(Some("AXTextArea"), false), Field::Editable);
        assert_eq!(field_verdict(Some("AXGroup"), true), Field::Editable, "contenteditable with a settable value");
        assert_eq!(field_verdict(Some("AXOutline"), false), Field::NotEditable, "Finder list");
        assert_eq!(field_verdict(Some("AXList"), false), Field::NotEditable);
        for r in ["AXWindow", "AXWebArea", "AXGroup"] {
            assert_eq!(field_verdict(Some(r), false), Field::Unknown, "{r} can hold a web text field");
        }
        assert_eq!(field_verdict(None, false), Field::Unknown, "Electron without an AX tree");
        for r in ["AXTable", "AXRow", "AXCell"] {
            assert_eq!(field_verdict(Some(r), false), Field::Unknown, "{r}: Cmd+V into a spreadsheet cell types into it");
        }
    }

    #[test]
    fn target_is_never_susurro_when_another_app_was_in_front() {
        let app = |pid| Some(AppRef { pid, ..Default::default() });
        assert_eq!(pick_target(app(1), app(2), 9), app(1), "another app in front: that one");
        assert_eq!(pick_target(app(9), app(2), 9), app(2), "a click on the overlay activated susurro");
        assert_eq!(pick_target(app(9), None, 9), app(9), "nothing else seen yet");
        assert_eq!(pick_target(None, app(2), 9), None);
    }

    #[test]
    fn smart_spacing() {
        assert_eq!(smart_space("Thanks", "."), " Thanks");
        assert_eq!(smart_space("world", "wo"), " world");
        assert_eq!(smart_space("¿Qué tal?", "!"), " ¿Qué tal?");
        assert_eq!(smart_space("Thanks", " "), "Thanks");
        assert_eq!(smart_space("Thanks", "\n"), "Thanks");
        assert_eq!(smart_space("Thanks", "("), "Thanks");
        assert_eq!(smart_space("Thanks", "@"), "Thanks");
        assert_eq!(smart_space(", and more", "o"), ", and more");
        assert_eq!(smart_space("Thanks", ""), "Thanks", "start of field or no AX");
        // straight quotes: a closer gets the space, an opener stays glued
        assert_eq!(smart_space("Then", "i\""), " Then", "after a closing quote");
        assert_eq!(smart_space("Then", "s'"), " Then", "after dogs'");
        assert_eq!(smart_space("hi", " \""), "hi", "after an opening quote");
        assert_eq!(smart_space("hi", "\""), "hi", "a quote at the start of the field");
        assert_eq!(smart_space("hi", "(\""), "hi");
        assert_eq!(smart_space("hi", "“"), "hi");
    }

    #[test]
    fn slow_apps_wait_longer() {
        assert_eq!(restore_delay(Some("com.tinyspeck.slackmacgap"), 500), 1500);
        assert_eq!(restore_delay(Some("com.todesktop.230313mzl4w4u92"), 2000), 2000, "a longer user value wins");
        assert_eq!(restore_delay(Some("com.apple.TextEdit"), 500), 500);
        assert_eq!(restore_delay(None, 500), 500);
    }

    #[test]
    fn restore_only_if_untouched() {
        assert!(should_restore(42, 42));
        assert!(!should_restore(42, 43), "the user copied something after our write");
    }
}
