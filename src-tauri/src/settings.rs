//! `{app_data_dir}/settings.json`: load with defaults, atomic save, version migration.

use crate::contract::{MicSettings, SETTINGS_VERSION, Settings};
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

/// Add `if s.version < N { .. }` steps here when a field changes meaning.
fn migrate(mut s: Settings) -> Settings {
    // v1 stored mic.priority[].last_seen in seconds; every timestamp is ms now. Idempotent, so no version bump.
    for k in &mut s.mic.priority {
        if k.last_seen < 100_000_000_000 {
            k.last_seen *= 1000;
        }
    }
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

/// The page's copy of Settings with the backend-owned parts taken from `cur`. The mic commands and the
/// device monitor own `mic` (selection, priority, healed ids); the page only sets `show_virtual`.
fn merge(cur: &Settings, mut new: Settings) -> Settings {
    new.version = SETTINGS_VERSION;
    new.mic = MicSettings { show_virtual: new.mic.show_virtual, ..cur.mic.clone() };
    new
}

/// The whisper context is needed now and may not be loaded: the model, the provider or the fallback changed.
fn whisper_changed(old: &Settings, new: &Settings) -> bool {
    let (o, n) = (&old.stt, &new.stt);
    o.whisper_model != n.whisper_model || o.provider != n.provider || o.local_fallback != n.local_fallback
}

/// Validates hotkeys, applies hotkeys and autostart, preloads whisper if it is newly needed, then persists.
#[tauri::command]
pub fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    use tauri::Manager;
    use tauri_plugin_autostart::ManagerExt;

    crate::hotkey::validate(&settings.hotkeys).map_err(|e| e.join("; "))?;
    let old = state.settings.read().unwrap().clone();
    if old.hotkeys != settings.hotkeys {
        if let Some(h) = app.try_state::<crate::hotkey::HotkeyHandle>() {
            h.update(&settings.hotkeys).map_err(|e| e.to_string())?;
        }
    }
    // A test instance (isolated data dir) must not register itself as a login item.
    let test_instance = cfg!(debug_assertions) && std::env::var_os("SUSURRO_DATA_DIR").is_some();
    if old.autostart != settings.autostart && !test_instance {
        let al = app.autolaunch();
        if settings.autostart { al.enable() } else { al.disable() }.map_err(|e| format!("autostart: {e}"))?;
    }
    // One write lock for merge + save + store: the mic monitor writes settings.mic from its own thread.
    let new = {
        let mut cur = state.settings.write().unwrap();
        let new = merge(&cur, settings);
        save(&state.settings_path(), &new).map_err(|e| format!("save settings: {e}"))?;
        *cur = new.clone();
        new
    };
    if whisper_changed(&old, &new) {
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
    fn save_keeps_the_backend_mic_choice() {
        let mut cur = Settings::default();
        cur.mic.selection = MicSelection::Device { id: "usb-1".into() };
        cur.mic.priority = vec![KnownDevice { id: "usb-1".into(), name: "USB".into(), kind: DeviceKind::Usb, last_seen: 1 }];
        // The page loaded mic before the user picked usb-1; it only changes show_virtual and sounds.
        let mut page = Settings::default();
        page.mic.show_virtual = true;
        page.recording.sounds = false;
        let m = merge(&cur, page);
        assert_eq!(m.mic.selection, cur.mic.selection);
        assert_eq!(m.mic.priority, cur.mic.priority);
        assert!(m.mic.show_virtual && !m.recording.sounds);
    }

    #[test]
    fn fallback_or_provider_change_preloads_whisper() {
        let old = Settings::default();
        let mut new = old.clone();
        assert!(!whisper_changed(&old, &new));
        new.stt.local_fallback = !old.stt.local_fallback;
        assert!(whisper_changed(&old, &new));
        let mut new = old.clone();
        new.stt.provider = SttProvider::ElevenLabs;
        assert!(whisper_changed(&old, &new));
    }

    #[test]
    fn last_seen_seconds_become_ms() {
        let p = tmp_dir("lastseen").join("settings.json");
        let mut s = Settings::default();
        s.mic.priority = vec![
            KnownDevice { id: "a".into(), name: "A".into(), kind: DeviceKind::Usb, last_seen: 1_760_000_000 },
            KnownDevice { id: "b".into(), name: "B".into(), kind: DeviceKind::Usb, last_seen: 1_760_000_000_123 },
        ];
        save(&p, &s).unwrap();
        let l = load(&p);
        assert_eq!(l.mic.priority.iter().map(|k| k.last_seen).collect::<Vec<_>>(), [1_760_000_000_000, 1_760_000_000_123]);
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
