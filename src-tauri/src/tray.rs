//! WP5: tray menu (Open susurro, Microphone >, Paste last transcript, Pause hotkey, Quit) + `navigate` event.
//! The Microphone submenu is rebuilt on every `mic_state` event.

use crate::AppState;
use crate::contract::{MicSelection, Navigate};
use crate::session::SessionCmd;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuEvent, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Listener, Manager, Wry};

const TRAY: &str = "susurro";
static PAUSED: AtomicBool = AtomicBool::new(false);

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
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
        .text("open", "Open susurro")
        .item(&mic)
        .text("paste-last", "Paste last transcript")
        .item(&CheckMenuItem::with_id(app, "pause", "Pause hotkey", true, PAUSED.load(Ordering::Relaxed), None::<&str>)?)
        .separator()
        .text("quit", "Quit susurro")
        .build()
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
        "paste-last" => {
            let _ = state.session_tx.send(SessionCmd::PasteLast);
        }
        "pause" => {
            let p = !PAUSED.load(Ordering::Relaxed);
            PAUSED.store(p, Ordering::Relaxed);
            if let Some(h) = app.try_state::<crate::hotkey::HotkeyHandle>() {
                h.set_paused(p);
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
    Ok(())
}
