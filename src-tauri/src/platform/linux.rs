//! Linux: dictation only. Clipboard through arboard (X11 and Wayland), the paste keystroke and Return through
//! the first helper that is installed (wtype or ydotool on Wayland, xdotool on X11), the frontmost window
//! through xdotool (X11 only; Wayland does not tell other apps which window has focus).
//! Voice control, agent mode and modifier-only talk keys stay macOS-only.

use crate::paste::AppRef;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Distance from the bottom of the screen to the overlay pill, in logical px.
const BOTTOM_GAP: f64 = 48.0;

/// arboard serves the X11 selection from a background thread while a Clipboard lives: keep one forever,
/// or the text vanishes when the dictation's Clipboard is dropped.
static CLIP: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn with_clip<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
    let mut g = CLIP.lock().unwrap();
    if g.is_none() {
        match arboard::Clipboard::new() {
            Ok(c) => *g = Some(c),
            Err(e) => {
                log::warn!("clipboard: {e}");
                return None;
            }
        }
    }
    f(g.as_mut()?)
}

pub fn clipboard_text() -> Option<String> {
    with_clip(|c| c.get_text().ok())
}

pub fn write_text(text: &str) -> bool {
    with_clip(|c| match c.set_text(text) {
        Ok(()) => Some(()),
        Err(e) => {
            log::warn!("clipboard write: {e}");
            None
        }
    })
    .is_some()
}

pub fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
}

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run(tool: &str, args: &[&str]) -> bool {
    match Command::new(tool).args(args).stdout(Stdio::null()).stderr(Stdio::piped()).output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            log::warn!("{tool} {args:?}: {}", String::from_utf8_lossy(&o.stderr).trim());
            false
        }
        Err(e) => {
            log::warn!("{tool}: {e}");
            false
        }
    }
}

/// A key the paste and Enter need. `shift` adds Shift to the Ctrl (terminals paste with Ctrl+Shift+V).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Paste { shift: bool },
    Return,
}

/// Pure: the helper invocation for a key, in the order we try them. Linux input keycodes for ydotool:
/// 29 LeftCtrl, 42 LeftShift, 47 V, 28 Enter.
pub fn key_commands(key: Key, wayland: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    let (wtype, ydo, xdo): (Vec<&str>, Vec<&str>, Vec<&str>) = match key {
        Key::Paste { shift: false } => (
            vec!["-M", "ctrl", "-k", "v", "-m", "ctrl"],
            vec!["key", "29:1", "47:1", "47:0", "29:0"],
            vec!["key", "--clearmodifiers", "ctrl+v"],
        ),
        Key::Paste { shift: true } => (
            vec!["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl"],
            vec!["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
            vec!["key", "--clearmodifiers", "ctrl+shift+v"],
        ),
        Key::Return => (vec!["-k", "Return"], vec!["key", "28:1", "28:0"], vec!["key", "--clearmodifiers", "Return"]),
    };
    if wayland {
        // xdotool still reaches XWayland windows, so it is the last resort there.
        vec![("wtype", wtype), ("ydotool", ydo), ("xdotool", xdo)]
    } else {
        vec![("xdotool", xdo)]
    }
}

/// Posts the key with the first helper that is installed and works. False = nothing could type.
pub fn key(k: Key) -> bool {
    for (tool, args) in key_commands(k, wayland()) {
        if has(tool) && run(tool, &args) {
            return true;
        }
    }
    false
}

/// Can susurro type into other apps here? (onboarding's "Type for you" step)
pub fn can_type() -> bool {
    key_commands(Key::Return, wayland()).iter().any(|(t, _)| has(t))
}

/// Window classes that paste with Ctrl+Shift+V.
const TERMINALS: [&str; 16] = [
    "gnome-terminal", "gnome-terminal-server", "org.gnome.console", "kgx", "konsole", "xterm", "uxterm",
    "alacritty", "kitty", "terminator", "tilix", "xfce4-terminal", "wezterm", "foot", "urxvt", "com.mitchellh.ghostty",
];

/// Pure: does this window class paste with Ctrl+Shift+V?
pub fn is_terminal(class: Option<&str>) -> bool {
    class.is_some_and(|c| {
        let c = c.to_lowercase();
        TERMINALS.iter().any(|t| c == *t || c.ends_with(&format!(".{t}")))
    })
}

fn xdotool(args: &[&str]) -> Option<String> {
    let o = Command::new("xdotool").args(args).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// The focused window's app on X11 (WM class as the id). None on Wayland or without xdotool.
pub fn frontmost_app() -> Option<AppRef> {
    if wayland() && std::env::var_os("DISPLAY").is_none() {
        return None;
    }
    let id = xdotool(&["getactivewindow"])?;
    // Tk and some Java apps set no _NET_WM_PID: 0 then, which is never susurro itself
    let pid = xdotool(&["getwindowpid", &id]).and_then(|p| p.parse().ok()).unwrap_or(0);
    // getwindowclassname is new (xdotool 3.2021); Debian and Ubuntu ship 3.2016, so ask xprop
    let class = xdotool(&["getwindowclassname", &id]).or_else(|| wm_class(&id));
    let name = xdotool(&["getwindowname", &id]);
    Some(AppRef { bundle_id: class.map(|c| c.to_lowercase()), name, pid })
}

/// The instance name from `xprop -id <id> WM_CLASS`.
fn wm_class(id: &str) -> Option<String> {
    let o = Command::new("xprop").args(["-id", id, "WM_CLASS"]).stderr(Stdio::null()).output().ok()?;
    parse_wm_class(&String::from_utf8_lossy(&o.stdout))
}

/// Pure: `WM_CLASS(STRING) = "gnome-terminal-server", "Gnome-terminal"` -> "gnome-terminal-server".
pub fn parse_wm_class(out: &str) -> Option<String> {
    let (_, rest) = out.split_once('=')?;
    let first = rest.split(',').next()?.trim().trim_matches('"');
    (!first.is_empty()).then(|| first.to_string())
}

/// Restores `old` after `delay_ms` unless something else wrote to the clipboard since our own write.
pub fn restore_later(ours: String, old: String, delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        if clipboard_text().as_deref() == Some(ours.as_str()) {
            write_text(&old);
        }
    });
}

/// The overlay pill takes no clicks on Linux (no hit test): the window ignores the mouse, so the transparent
/// area never blocks the app under it. Bottom center of the monitor the window is on.
pub fn setup_overlay(app: &AppHandle) {
    let Some(w) = app.get_webview_window("overlay") else { return };
    let _ = w.set_ignore_cursor_events(true);
    let _ = w.set_visible_on_all_workspaces(true);
    if let (Ok(Some(m)), Ok(size)) = (w.current_monitor(), w.outer_size()) {
        let scale = m.scale_factor();
        let (mp, ms) = (m.position().to_logical::<f64>(scale), m.size().to_logical::<f64>(scale));
        let s = size.to_logical::<f64>(scale);
        let pos = tauri::LogicalPosition::new(mp.x + ((ms.width - s.width) / 2.0).round(), mp.y + ms.height - s.height - BOTTOM_GAP);
        let _ = w.set_position(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_uses_xdotool_only() {
        let c = key_commands(Key::Paste { shift: false }, false);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "xdotool");
        assert_eq!(c[0].1, ["key", "--clearmodifiers", "ctrl+v"]);
    }

    #[test]
    fn wayland_tries_wtype_then_ydotool_then_xdotool() {
        let tools: Vec<_> = key_commands(Key::Return, true).into_iter().map(|c| c.0).collect();
        assert_eq!(tools, ["wtype", "ydotool", "xdotool"]);
        let shift = key_commands(Key::Paste { shift: true }, true);
        assert_eq!(shift[0].1, ["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl"]);
        // every ydotool press has its release
        for (_, a) in key_commands(Key::Paste { shift: true }, true).into_iter().filter(|c| c.0 == "ydotool") {
            let down = a.iter().filter(|s| s.ends_with(":1")).count();
            let up = a.iter().filter(|s| s.ends_with(":0")).count();
            assert_eq!(down, up);
        }
    }

    #[test]
    fn wm_class_from_xprop() {
        assert_eq!(parse_wm_class("WM_CLASS(STRING) = \"gnome-terminal-server\", \"Gnome-terminal\"\n").as_deref(), Some("gnome-terminal-server"));
        assert_eq!(parse_wm_class("WM_CLASS:  not found.\n"), None);
        assert_eq!(parse_wm_class(""), None);
    }

    #[test]
    fn terminals_paste_with_shift() {
        assert!(is_terminal(Some("gnome-terminal-server")));
        assert!(is_terminal(Some("Alacritty")));
        assert!(is_terminal(Some("org.wezfurlong.wezterm")));
        assert!(!is_terminal(Some("firefox")));
        assert!(!is_terminal(Some("code")));
        assert!(!is_terminal(None));
    }
}
