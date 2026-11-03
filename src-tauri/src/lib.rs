//! Tauri builder, plugins, AppState, windows, invoke_handler. Written once in WP0 (PLAN 1.1).
//! Later WPs edit only the bodies of their own modules. WP5 adds its setup calls (marked below).

pub mod agent;
pub mod audio;
pub mod ax;
pub mod cleanup;
pub mod command;
pub mod contract;
pub mod db;
pub mod dictionary;
pub mod glance;
pub mod history;
pub mod hotkey;
pub mod jev;
pub mod learn;
pub mod live;
pub mod models;
pub mod ocr;
pub mod paste;
pub mod permissions;
pub mod pipeline;
pub mod planner;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod slash;
pub mod stt;
#[cfg(debug_assertions)]
pub mod test_hooks;
pub mod text;
pub mod tray;
pub mod web;
pub mod platform {
    #[cfg(target_os = "linux")]
    pub mod linux;
    #[cfg(target_os = "macos")]
    pub mod macos;

    /// The overlay page reports the pill's rect; only the macOS hit test uses it (Linux's overlay is click-through).
    #[tauri::command]
    pub fn set_pill_rect(rect: crate::contract::PtRect) {
        #[cfg(target_os = "macos")]
        macos::set_pill_rect(rect);
        #[cfg(not(target_os = "macos"))]
        let _ = rect;
    }
}

use contract::Settings;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, mpsc};
use tauri::{AppHandle, Manager};
use whisper_rs::WhisperContext;

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub db: Mutex<rusqlite::Connection>,
    /// hotkey, overlay, commands -> session actor
    pub session_tx: mpsc::Sender<session::SessionCmd>,
    pub mic: Mutex<audio::MicRuntime>,
    /// model file + loaded ctx
    pub whisper: Mutex<Option<(String, Arc<WhisperContext>)>>,
    /// {app_data_dir}: settings.json, susurro.db, audio/, models/
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn settings_path(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut log = tauri_plugin_log::Builder::new().level(log::LevelFilter::Info);
    #[cfg(debug_assertions)]
    if let Some(d) = test_hooks::data_dir() {
        use tauri_plugin_log::{Target, TargetKind};
        log = log.clear_targets().targets([
            Target::new(TargetKind::Stdout),
            Target::new(TargetKind::Folder { path: d.join("logs"), file_name: Some("susurro".into()) }),
        ]);
    }
    let app = tauri::Builder::default()
        .plugin(log.build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            paste::watch_activations(); // setup runs on the main thread

            let data_dir = app.path().app_data_dir()?;
            #[cfg(debug_assertions)]
            let data_dir = test_hooks::data_dir().unwrap_or(data_dir);
            secrets::init(&data_dir);
            let settings = settings::load(&data_dir.join("settings.json"));
            let db = db::open(&data_dir.join("susurro.db"))?;
            // relaunch_app leaves a marker: reopen the main window on the next launch.
            let marker = data_dir.join(permissions::SHOW_MAIN_MARKER);
            let relaunched = std::fs::remove_file(&marker).is_ok();
            let first_run = !settings.onboarded || relaunched;
            let hotkeys = settings::active_hotkeys(&settings);
            let (tx, rx) = mpsc::channel();
            let hotkey_tx = tx.clone();
            app.manage(AppState {
                settings: RwLock::new(settings),
                db: Mutex::new(db),
                session_tx: tx,
                mic: Mutex::new(audio::MicRuntime::default()),
                whisper: Mutex::new(None),
                data_dir,
            });
            let h = app.handle();
            learn::init(h);
            session::spawn(h.clone(), rx);
            audio::spawn_monitor(h.clone());
            app.manage(hotkey::start(h, &hotkeys, hotkey_tx)?);
            tray::build(h)?;
            permissions::spawn_watcher(h.clone());
            #[cfg(debug_assertions)]
            test_hooks::start_control(h);
            stt::whisper_local::load_in_background(h.clone());
            // The overlay window is always shown; the pill hides itself when idle and `show_idle` is off.
            #[cfg(target_os = "macos")]
            platform::macos::setup_overlay(h);
            if let Some(w) = app.get_webview_window("overlay") {
                let _ = w.show();
            }
            // after show: GTK has no GdkWindow for a hidden window, and the input shape needs one
            #[cfg(target_os = "linux")]
            platform::linux::setup_overlay(h);
            // The transparent window area must not take clicks: only the pill does (Pablo's bug, 2026-10-17).
            #[cfg(target_os = "macos")]
            platform::macos::spawn_hit_test(h);
            if first_run {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|w, e| {
            // Closing main hides it: the app lives in the menu bar.
            if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                if w.label() == "main" {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            settings::get_settings,
            settings::save_settings,
            secrets::key_status,
            secrets::set_api_key,
            audio::get_mic_state,
            audio::set_mic_selection,
            audio::set_mic_priority,
            audio::forget_device,
            audio::mic_test_start,
            audio::mic_test_stop,
            session::dictation_toggle,
            session::dictation_stop,
            session::dictation_cancel,
            session::paste_last,
            session::get_session_state,
            session::toast_action,
            dictionary::dict_list,
            dictionary::dict_upsert,
            dictionary::dict_delete,
            dictionary::dict_preview,
            history::history_list,
            history::history_delete,
            history::history_clear,
            history::history_retry,
            history::history_audio,
            history::history_paste,
            history::get_stats,
            models::whisper_models,
            models::whisper_download,
            models::whisper_delete,
            stt::test_provider,
            stt::openrouter_models,
            permissions::permissions_status,
            permissions::request_permission,
            permissions::relaunch_app,
            hotkey::hotkey_capture,
            platform::set_pill_rect,
            command::command_log,
            live::command_memory,
            live::command_memory_clear,
        ])
        .build(tauri::generate_context!())
        .expect("error while building susurro");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main(app);
        }
        // the agent's warm claude process never outlives the app
        if let tauri::RunEvent::Exit = event {
            agent::kill();
        }
        let _ = (app, event);
    });
}
