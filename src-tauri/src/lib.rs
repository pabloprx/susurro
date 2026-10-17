//! Tauri builder, plugins, AppState, windows, invoke_handler. Written once in WP0 (PLAN 1.1).
//! Later WPs edit only the bodies of their own modules. WP5 adds its setup calls (marked below).

pub mod audio;
pub mod cleanup;
pub mod contract;
pub mod db;
pub mod dictionary;
pub mod history;
pub mod hotkey;
pub mod models;
pub mod paste;
pub mod permissions;
pub mod pipeline;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod stt;
pub mod text;
pub mod tray;
pub mod platform {
    #[cfg(target_os = "macos")]
    pub mod macos;
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
    /// history id for "paste last"
    pub last_result: Mutex<Option<i64>>,
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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().level(log::LevelFilter::Info).build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app.path().app_data_dir()?;
            let settings = settings::load(&data_dir.join("settings.json"));
            let db = db::open(&data_dir.join("susurro.db"))?;
            let first_run = !settings.onboarded;
            let hotkeys = settings.hotkeys.clone();
            let (tx, rx) = mpsc::channel();
            let hotkey_tx = tx.clone();
            app.manage(AppState {
                settings: RwLock::new(settings),
                db: Mutex::new(db),
                session_tx: tx,
                mic: Mutex::new(audio::MicRuntime::default()),
                whisper: Mutex::new(None),
                last_result: Mutex::new(None),
                data_dir,
            });
            let h = app.handle();
            session::spawn(h.clone(), rx);
            audio::spawn_monitor(h.clone());
            app.manage(hotkey::start(h, &hotkeys, hotkey_tx)?);
            tray::build(h)?;
            stt::whisper_local::load_in_background(h.clone());
            // The overlay window is always shown; the pill hides itself when idle and `show_idle` is off.
            #[cfg(target_os = "macos")]
            platform::macos::setup_overlay(h);
            if let Some(w) = app.get_webview_window("overlay") {
                let _ = w.show();
            }
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building susurro");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = event {
            show_main(app);
        }
        let _ = (app, event);
    });
}
