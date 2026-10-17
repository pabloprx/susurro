//! WP5: tray menu (Hold <key> to talk, Open susurro, Settings…, Paste / Copy last transcript, Microphone >,
//! Pause hotkey, Quit) + `navigate` event. Rebuilt on every `mic_state` and `hotkey_status` event.

use crate::AppState;
use crate::contract::{MicSelection, Navigate};
use crate::session::SessionCmd;
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuEvent, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Listener, Manager, Wry};

const TRAY: &str = "susurro";

fn paused(app: &AppHandle) -> bool {
    app.try_state::<crate::hotkey::HotkeyHandle>().is_some_and(|h| h.paused())
}

/// Pure: "F1" -> "Hold F1 to talk", "RightOption" -> "Hold Right Option to talk".
fn talk_hint(ptt: &str) -> String {
    let mut name = String::new();
    for (i, c) in ptt.trim().chars().enumerate() {
        let prev_lower = i > 0 && ptt.trim().chars().nth(i - 1).is_some_and(|p| p.is_lowercase());
        if c.is_uppercase() && prev_lower {
            name.push(' ');
        }
        name.push(c);
    }
    format!("Hold {name} to talk")
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    // The live bindings: hotkey_status fires before save_settings stores the new settings.
    let h = match app.try_state::<crate::hotkey::HotkeyHandle>() {
        Some(h) => h.bindings(),
        None => app.state::<AppState>().settings.read().unwrap().hotkeys.clone(),
    };
    let (ptt, paste_last, copy_last) = (h.ptt, h.paste_last, h.copy_last);
    let hint = tauri::menu::MenuItem::with_id(app, "hint", talk_hint(&ptt), false, None::<&str>)?;
    let item = |id: &str, label: &str, accel: &str| {
        // A bad accelerator string only loses the shortcut label, not the item.
        tauri::menu::MenuItem::with_id(app, id, label, true, Some(accel))
            .or_else(|_| tauri::menu::MenuItem::with_id(app, id, label, true, None::<&str>))
    };
    let ms = crate::audio::mic_state(app.state::<AppState>().inner());
    let auto = ms.selection == MicSelection::Auto;
    let mut mic = SubmenuBuilder::new(app, "Microphone").item(&CheckMenuItem::with_id(app, "mic:auto", "Auto", true, auto, None::<&str>)?);
    if !ms.devices.is_empty() {
        mic = mic.separator();
    }
    for d in &ms.devices {
        let on = matches!(&ms.selection, MicSelection::Device { id } if *id == d.id);
        mic = mic.item(&CheckMenuItem::with_id(app, format!("mic:{}", d.id), &d.name, true, on, None::<&str>)?);
    }
    let mic = mic.separator().text("audio-settings", "Open audio settings…").build()?;
    MenuBuilder::new(app)
        .item(&hint)
        .separator()
        .text("open", "Open susurro")
        .text("settings", "Settings…")
        .separator()
        .item(&item("paste-last", "Paste last transcript", &paste_last)?)
        .item(&item("copy-last", "Paste + copy last transcript", &copy_last)?)
        .separator()
        .item(&mic)
        .item(&CheckMenuItem::with_id(app, "pause", "Pause hotkey", true, paused(app), None::<&str>)?)
        .separator()
        .text("quit", "Quit susurro")
        .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn talk_hints() {
        assert_eq!(super::talk_hint("F1"), "Hold F1 to talk");
        assert_eq!(super::talk_hint("RightOption"), "Hold Right Option to talk");
        assert_eq!(super::talk_hint("Alt+Space"), "Hold Alt+Space to talk");
    }
}

fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY) else { return };
    match menu(app) {
        Ok(m) => {
            let _ = tray.set_menu(Some(m));
        }
        Err(e) => log::warn!("tray menu: {e}"),
    }
}

fn on_menu(app: &AppHandle, e: MenuEvent) {
    let id = e.id().as_ref();
    let state = app.state::<AppState>();
    match id {
        "open" => crate::show_main(app),
        "audio-settings" => {
            crate::show_main(app);
            let _ = app.emit_to("main", "navigate", Navigate { screen: "audio".into() });
        }
        "settings" => {
            crate::show_main(app);
            let _ = app.emit_to("main", "navigate", Navigate { screen: "settings".into() });
        }
        "paste-last" => {
            let _ = state.session_tx.send(SessionCmd::PasteLast);
        }
        "copy-last" => {
            let _ = state.session_tx.send(SessionCmd::CopyLast);
        }
        "pause" => {
            if let Some(h) = app.try_state::<crate::hotkey::HotkeyHandle>() {
                h.set_paused(!h.paused());
            }
            refresh(app);
        }
        "quit" => {
            let _ = state.session_tx.send(SessionCmd::Shutdown);
            app.exit(0);
        }
        _ => {
            if let Some(dev) = id.strip_prefix("mic:") {
                let sel = if dev == "auto" { MicSelection::Auto } else { MicSelection::Device { id: dev.into() } };
                if let Err(e) = crate::audio::set_mic_selection(app.clone(), sel) {
                    log::warn!("tray mic selection: {e}");
                }
                // A click toggles the check mark on its own; redraw from the real state.
                refresh(app);
            }
        }
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    // ponytail: the app icon in the menu bar; a monochrome template icon is a design follow-up.
    let mut b = TrayIconBuilder::with_id(TRAY).tooltip("susurro").menu(&menu(app)?).show_menu_on_left_click(true).on_menu_event(on_menu);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    let a = app.clone();
    app.listen("mic_state", move |_| refresh(&a));
    // A new talk key (save_settings) or a pause re-emits hotkey_status: redraw the hint and the check mark.
    let a = app.clone();
    app.listen("hotkey_status", move |_| refresh(&a));
    Ok(())
}
