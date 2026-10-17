//! `{app_data_dir}/settings.json`: load with defaults, atomic save, version migration.

use crate::contract::{CleanupLevel, DEFAULT_OPENROUTER_CLEANUP, DEFAULT_OPENROUTER_STT, DEFAULT_PLANNER, Hotkeys, JEV_MODEL, MicSettings, PasteOpts, SETTINGS_VERSION, Settings, SttProvider};
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
    // v2: fn never worked for most users (system-owned, missing on many keyboards): F1 is the default now.
    // The 350 ms restore delay was the old default and too short for Electron apps. Later choices are kept.
    if s.version < 2 {
        if s.hotkeys.ptt.trim().eq_ignore_ascii_case("fn") {
            s.hotkeys.ptt = "F1".into();
        }
        if s.paste.restore_delay_ms == 350 {
            s.paste.restore_delay_ms = PasteOpts::default().restore_delay_ms;
        }
    }
    // v3: voice control. A missing hotkeys.command gets "F2" from serde; an empty OpenRouter STT model
    // (the old default) becomes MAI-Transcribe 2.
    // A user who already holds F2 for another action keeps it: the command key starts unbound.
    if s.version < 3 {
        if s.stt.openrouter_model.is_empty() {
            s.stt.openrouter_model = DEFAULT_OPENROUTER_STT.into();
        }
        if crate::hotkey::validate(&s.hotkeys).is_err() && crate::hotkey::validate(&Hotkeys { command: String::new(), ..s.hotkeys.clone() }).is_ok() {
            s.hotkeys.command.clear();
        }
    }
    // v4: an empty cleanup model left cleanup off with no sign of it.
    if s.version < 4 && s.cleanup.openrouter_model.trim().is_empty() {
        s.cleanup.openrouter_model = DEFAULT_OPENROUTER_CLEANUP.into();
    }
    // v5 (vc-feedback 12, 17; VOICE_CONTROL_V2 4.1, 4.3): the recents key follows an F-key ptt (F12 -> Ctrl+F12);
    // the old 400 ms grace was too short; MAI cleans by itself, so the LLM cleanup goes off once (he can turn it on).
    if s.version < 5 {
        let ptt = s.hotkeys.ptt.trim();
        if ptt.len() >= 2 && ptt.len() <= 3 && ptt.starts_with(['F', 'f']) && ptt[1..].parse::<u8>().is_ok() {
            s.hotkeys.recents = format!("Ctrl+{}", ptt.to_uppercase());
        }
        if crate::hotkey::validate(&s.hotkeys).is_err() && crate::hotkey::validate(&Hotkeys { recents: String::new(), ..s.hotkeys.clone() }).is_ok() {
            s.hotkeys.recents.clear();
        }
        if s.hotkeys.hands_free_grace_ms == 400 {
            s.hotkeys.hands_free_grace_ms = Hotkeys::default().hands_free_grace_ms;
        }
        if is_mai(&s) {
            s.cleanup.level = CleanupLevel::None;
        }
    }
    s.version = SETTINGS_VERSION;
    s
}

/// The STT is MAI-Transcribe on OpenRouter.
pub fn is_mai(s: &Settings) -> bool {
    s.stt.provider == SttProvider::OpenRouter && s.stt.openrouter_model.starts_with("microsoft/mai-transcribe")
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
    // The Jev model is pinned (never ~latest): the page cannot change it.
    new.command.model = JEV_MODEL.into();
    if new.command.planner_model.trim().is_empty() {
        new.command.planner_model = DEFAULT_PLANNER.into();
    }
    new.hotkeys.hands_free_grace_ms = new.hotkeys.hands_free_grace_ms.min(1500);
    new
}

/// The whisper context is needed now and may not be loaded: the model, the provider or the fallback changed.
fn whisper_changed(old: &Settings, new: &Settings) -> bool {
    let (o, n) = (&old.stt, &new.stt);
    o.whisper_model != n.whisper_model || o.provider != n.provider || o.local_fallback != n.local_fallback
}

/// The bindings that are live: voice control off = its key is not registered, so it cannot clash.
/// The bindings to register: the command key only while voice control is on.
pub fn active_hotkeys(s: &Settings) -> Hotkeys {
    let mut h = s.hotkeys.clone();
    if !s.command.enabled {
        h.command.clear();
    }
    h
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

    let active = active_hotkeys(&settings);
    crate::hotkey::validate(&active).map_err(|e| e.join("; "))?;
    let old = state.settings.read().unwrap().clone();
    if active_hotkeys(&old) != active {
        if let Some(h) = app.try_state::<crate::hotkey::HotkeyHandle>() {
            h.update(&active).map_err(|e| e.to_string())?;
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
    fn v1_fn_becomes_f1_once() {
        let p = tmp_dir("fnmig").join("settings.json");
        std::fs::write(&p, r#"{"version":1,"hotkeys":{"ptt":"Fn"},"paste":{"restore_delay_ms":350}}"#).unwrap();
        let s = load(&p);
        assert_eq!(s.hotkeys.ptt, "F1");
        assert_eq!(s.hotkeys.copy_last, "Ctrl+Cmd+C", "new field gets its default");
        assert_eq!(s.paste.restore_delay_ms, 500);
        assert_eq!(s.version, SETTINGS_VERSION);
        // A v2 user who picks Fn again keeps it.
        let mut s2 = s.clone();
        s2.hotkeys.ptt = "Fn".into();
        save(&p, &s2).unwrap();
        assert_eq!(load(&p).hotkeys.ptt, "Fn");
        // Other v1 choices are kept.
        std::fs::write(&p, r#"{"version":1,"hotkeys":{"ptt":"Alt+Space"}}"#).unwrap();
        assert_eq!(load(&p).hotkeys.ptt, "Alt+Space");
        // A v1 user's own delay is kept; the default ptt is F1.
        std::fs::write(&p, r#"{"version":1,"paste":{"restore_delay_ms":900}}"#).unwrap();
        assert_eq!(load(&p).paste.restore_delay_ms, 900);
        assert_eq!(Settings::default().hotkeys.ptt, "F1");
    }

    #[test]
    fn v2_gets_voice_control_and_the_mai_default() {
        let p = tmp_dir("v3mig").join("settings.json");
        std::fs::write(&p, r#"{"version":2,"hotkeys":{"ptt":"F1"},"stt":{"provider":"openrouter","openrouter_model":""}}"#).unwrap();
        let s = load(&p);
        assert_eq!(s.hotkeys.command, "F2");
        assert_eq!(s.stt.openrouter_model, "microsoft/mai-transcribe-2");
        assert_eq!(s.command, CommandSettings { enabled: false, confirm_risky: true, model: "typesafe/jev-1.14".into(), planner_model: DEFAULT_PLANNER.into() }, "opt-in, also for migrated users");
        assert_eq!(s.version, SETTINGS_VERSION);
        // A picked model is kept; a v3 user may clear it on purpose.
        std::fs::write(&p, r#"{"version":2,"stt":{"openrouter_model":"openai/whisper-1"}}"#).unwrap();
        assert_eq!(load(&p).stt.openrouter_model, "openai/whisper-1");
        std::fs::write(&p, r#"{"version":3,"stt":{"openrouter_model":""}}"#).unwrap();
        assert_eq!(load(&p).stt.openrouter_model, "");
    }

    #[test]
    fn v3_gets_a_cleanup_model() {
        let p = tmp_dir("v4mig").join("settings.json");
        std::fs::write(&p, r#"{"version":3,"cleanup":{"provider":"openrouter","openrouter_model":""}}"#).unwrap();
        assert_eq!(load(&p).cleanup.openrouter_model, DEFAULT_OPENROUTER_CLEANUP);
        std::fs::write(&p, r#"{"version":3,"cleanup":{"openrouter_model":"openai/gpt-4.1-mini"}}"#).unwrap();
        assert_eq!(load(&p).cleanup.openrouter_model, "openai/gpt-4.1-mini", "a picked model is kept");
        assert_eq!(Settings::default().cleanup.openrouter_model, DEFAULT_OPENROUTER_CLEANUP);
    }

    #[test]
    fn migration_leaves_f2_to_the_action_that_holds_it() {
        let p = tmp_dir("v3f2").join("settings.json");
        for hk in [r#"{"ptt":"F2"}"#, r#"{"cancel":"f2"}"#, r#"{"paste_last":"F2"}"#, r#"{"hands_free":"F2"}"#] {
            std::fs::write(&p, format!(r#"{{"version":2,"hotkeys":{hk}}}"#)).unwrap();
            let s = load(&p);
            assert_eq!(s.hotkeys.command, "", "{hk}");
            assert_eq!(crate::hotkey::validate(&s.hotkeys), Ok(()), "{hk}");
        }
    }

    #[test]
    fn a_disabled_command_key_does_not_clash_and_the_model_stays_pinned() {
        let mut s = Settings::default();
        s.hotkeys.ptt = "F2".into();
        assert_eq!(crate::hotkey::validate(&active_hotkeys(&s)), Ok(()));
        s.command.enabled = true;
        assert!(crate::hotkey::validate(&active_hotkeys(&s)).is_err());
        s.command.model = "typesafe/jev~latest".into();
        assert_eq!(merge(&Settings::default(), s).command.model, JEV_MODEL);
    }

    #[test]
    fn v5_recents_grace_and_mai_cleanup() {
        let p = tmp_dir("v5mig").join("settings.json");
        // Pablo's v4 file (2026-10-17): F12 ptt, 400 ms grace, MAI + light cleanup.
        std::fs::write(&p, r#"{"version":4,"hotkeys":{"ptt":"F12","hands_free_grace_ms":400,"copy_last":"Shift+F12","command":"F11"},
            "stt":{"provider":"openrouter","openrouter_model":"microsoft/mai-transcribe-2"},"cleanup":{"level":"light"},
            "command":{"enabled":true}}"#).unwrap();
        let s = load(&p);
        assert_eq!(s.hotkeys.recents, "Ctrl+F12");
        assert_eq!(s.hotkeys.hands_free_grace_ms, 1000, "v5_moves_400_to_the_new_default");
        assert_eq!(s.cleanup.level, CleanupLevel::None, "v5_turns_cleanup_off_for_mai_once");
        assert!(s.stt.mai_clean, "MAI clean stays on");
        assert_eq!((s.hotkeys.ptt.as_str(), s.hotkeys.copy_last.as_str(), s.hotkeys.command.as_str()), ("F12", "Shift+F12", "F11"), "his keys stay");
        assert!(s.command.enabled && s.command.planner_model == DEFAULT_PLANNER);
        assert_eq!(crate::hotkey::validate(&s.hotkeys), Ok(()));
        // once: a v5 user who turns cleanup back on keeps it
        let mut s2 = s.clone();
        s2.cleanup.level = CleanupLevel::Light;
        save(&p, &s2).unwrap();
        assert_eq!(load(&p).cleanup.level, CleanupLevel::Light);
        // other values stay; a non-F ptt keeps the default recents; not MAI = cleanup kept
        std::fs::write(&p, r#"{"version":4,"hotkeys":{"ptt":"RightOption","hands_free_grace_ms":700},"stt":{"provider":"elevenlabs"},"cleanup":{"level":"medium"}}"#).unwrap();
        let s = load(&p);
        assert_eq!((s.hotkeys.hands_free_grace_ms, s.hotkeys.recents.as_str(), s.cleanup.level.clone()), (700, "Ctrl+F1", CleanupLevel::Medium));
        // a recents key already used elsewhere starts unbound
        std::fs::write(&p, r#"{"version":4,"hotkeys":{"ptt":"F5","paste_last":"Ctrl+F5"}}"#).unwrap();
        assert_eq!(load(&p).hotkeys.recents, "");
    }

    #[test]
    fn settings_planner_model_roundtrip_and_grace_clamp() {
        let mut page = Settings::default();
        page.command.planner_model = "anthropic/claude-haiku-5.5".into();
        page.hotkeys.hands_free_grace_ms = 9000;
        let m = merge(&Settings::default(), page);
        assert_eq!(m.command.planner_model, "anthropic/claude-haiku-5.5");
        assert_eq!(m.hotkeys.hands_free_grace_ms, 1500);
        let mut page = Settings::default();
        page.command.planner_model = " ".into();
        assert_eq!(merge(&Settings::default(), page).command.planner_model, DEFAULT_PLANNER);
        let p = tmp_dir("planner").join("settings.json");
        let mut s = Settings::default();
        s.command.planner_model = "openai/gpt-6-luna".into();
        save(&p, &s).unwrap();
        assert_eq!(load(&p).command.planner_model, "openai/gpt-6-luna");
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

