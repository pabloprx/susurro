//! `{app_data_dir}/settings.json`: load with defaults, atomic save, version migration.

use crate::contract::{SETTINGS_VERSION, Settings};
use std::path::Path;

/// Missing file -> defaults. Corrupt file -> renamed to `settings.json.bad` (kept for the user), defaults.
pub fn load(path: &Path) -> Settings {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return Settings::default(),
    };
    match serde_json::from_str::<Settings>(&raw) {
        Ok(s) => migrate(s),
        Err(e) => {
            log::warn!("settings.json unreadable ({e}); keeping it as settings.json.bad, using defaults");
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Settings::default()
        }
    }
}

/// ponytail: no migrations exist yet (v1). Add `if s.version < N { .. }` steps here when a field changes meaning.
fn migrate(mut s: Settings) -> Settings {
    s.version = SETTINGS_VERSION;
    s
}

/// Write to `settings.json.tmp`, then rename: a crash never leaves a half-written file.
pub fn save(path: &Path, s: &Settings) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: tauri::State<'_, crate::AppState>) -> Settings {
    state.settings.read().unwrap().clone()
}

/// Validates hotkeys, applies hotkeys and autostart, reloads whisper if the model changed, then persists.
#[tauri::command]
pub fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    use tauri::Manager;
    use tauri_plugin_autostart::ManagerExt;

    crate::hotkey::validate(&settings.hotkeys).map_err(|e| e.join("; "))?;
    let mut new = settings;
    new.version = SETTINGS_VERSION;
    let old = state.settings.read().unwrap().clone();

    if old.hotkeys != new.hotkeys {
        if let Some(h) = app.try_state::<crate::hotkey::HotkeyHandle>() {
            h.update(&new.hotkeys).map_err(|e| e.to_string())?;
        }
    }
    if old.autostart != new.autostart {
        let al = app.autolaunch();
        if new.autostart { al.enable() } else { al.disable() }.map_err(|e| format!("autostart: {e}"))?;
    }
    save(&state.settings_path(), &new).map_err(|e| format!("save settings: {e}"))?;
    *state.settings.write().unwrap() = new.clone();
    if old.stt.whisper_model != new.stt.whisper_model {
        crate::stt::whisper_local::load_in_background(app.clone());
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("susurro-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let p = tmp_dir("rt").join("settings.json");
        let mut s = Settings::default();
        s.onboarded = true;
        s.mic.selection = MicSelection::Device { id: "usb-1".into() };
        s.stt.provider = SttProvider::ElevenLabs;
        s.styles.raw_apps.push("com.apple.Terminal".into());
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let p = tmp_dir("missing").join("settings.json");
        assert_eq!(load(&p), Settings::default());
    }

    #[test]
    fn missing_fields_get_defaults_and_unknown_are_ignored() {
        let p = tmp_dir("partial").join("settings.json");
        std::fs::write(&p, r#"{"version":0,"onboarded":true,"hotkeys":{"ptt":"Alt+Space"},"zzz_unknown":1}"#).unwrap();
        let s = load(&p);
        assert!(s.onboarded);
        assert_eq!(s.hotkeys.ptt, "Alt+Space");
        assert_eq!(s.hotkeys.cancel, "Escape");
        assert!(s.spoken_punctuation);
        assert_eq!(s.recording.max_seconds, 600);
        assert_eq!(s.version, SETTINGS_VERSION);
    }

    #[test]
    fn corrupt_file_is_kept_aside() {
        let p = tmp_dir("corrupt").join("settings.json");
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(load(&p), Settings::default());
        assert!(p.with_extension("json.bad").exists());
    }

    #[test]
    fn wire_names_are_snake_case() {
        let j = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(j["stt"]["provider"], "whisper_local");
        assert_eq!(j["cleanup"]["provider"], "openrouter");
        assert_eq!(j["mic"]["selection"], "auto");
        assert_eq!(j["styles"]["style"], "formal");
        let d = serde_json::to_value(MicSelection::Device { id: "x".into() }).unwrap();
        assert_eq!(d, serde_json::json!({"device": {"id": "x"}}));
    }
}
