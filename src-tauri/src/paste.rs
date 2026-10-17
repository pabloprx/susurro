//! WP4: frontmost app, full pasteboard save/restore, Cmd+V, Enter (PLAN 5). macOS via objc2 + core-graphics.
//! Runs on the session actor thread (it sleeps `restore_delay_ms`).

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

    pub fn general() -> Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }
}

pub fn frontmost_app() -> Option<AppRef> {
    #[cfg(target_os = "macos")]
    return mac::frontmost_app();
    #[cfg(not(target_os = "macos"))]
    None
}

pub fn paste(text: &str, o: &PasteOpts) -> PasteOutcome {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::CGEventFlags;
        let pb = mac::general();
        if !crate::permissions::accessibility_trusted() {
            // CGEvent cannot post without Accessibility: leave the text on the clipboard, no restore.
            mac::write_text(&pb, text, false);
            return PasteOutcome::CopiedOnly;
        }
        let snap = if o.restore_clipboard { mac::snapshot(&pb) } else { None };
        let ours = mac::write_text(&pb, text, true);
        mac::key(9, CGEventFlags::CGEventFlagCommand); // kVK_ANSI_V
        if let Some(snap) = snap {
            std::thread::sleep(std::time::Duration::from_millis(o.restore_delay_ms as u64));
            if should_restore(ours, pb.changeCount()) {
                mac::restore(&pb, &snap);
            }
        }
        PasteOutcome::Pasted
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (text, o);
        PasteOutcome::CopiedOnly
    }
}

pub fn press_enter() {
    #[cfg(target_os = "macos")]
    mac::key(36, core_graphics::event::CGEventFlags::empty()); // kVK_Return
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
    fn restore_only_if_untouched() {
        assert!(should_restore(42, 42));
        assert!(!should_restore(42, 43), "the user copied something after our write");
    }
}
