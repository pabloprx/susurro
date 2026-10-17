//! API keys in `keys.json` in the app data dir (mode 0600). Keys never go to the webview.
//! ponytail: plain file, not the Keychain - Keychain access prompts on every read were unusable
//! (Pablo, 2026-10-17). Move back to the Keychain once builds ship notarized.

use crate::contract::{KeyName, KeyStatus};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// Called once at startup with the app data dir.
pub fn init(data_dir: &Path) {
    let _ = FILE.set(data_dir.join("keys.json"));
}

fn account(k: KeyName) -> &'static str {
    match k {
        KeyName::Elevenlabs => "elevenlabs",
        KeyName::Openrouter => "openrouter",
        KeyName::LocalStt => "local_stt",
        KeyName::LocalLlm => "local_llm",
    }
}

fn read(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write(path: &Path, keys: &BTreeMap<String, String>) -> anyhow::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(keys)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn path() -> anyhow::Result<&'static PathBuf> {
    FILE.get().ok_or_else(|| anyhow::anyhow!("key store not initialised"))
}

/// None if missing.
pub fn get(k: KeyName) -> Option<String> {
    #[cfg(debug_assertions)]
    if let Some(v) = crate::test_hooks::key(account(k)) {
        return Some(v).filter(|s| !s.is_empty());
    }
    read(path().ok()?).remove(account(k)).filter(|s| !s.is_empty())
}

fn update(k: KeyName, value: Option<&str>) -> anyhow::Result<()> {
    let p = path()?;
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = read(p);
    match value {
        Some(v) => keys.insert(account(k).into(), v.into()),
        None => keys.remove(account(k)),
    };
    write(p, &keys)
}

pub fn set(k: KeyName, value: &str) -> anyhow::Result<()> {
    update(k, Some(value))
}

pub fn delete(k: KeyName) -> anyhow::Result<()> {
    update(k, None)
}

pub fn status() -> KeyStatus {
    let keys = path().map(|p| read(p)).unwrap_or_default();
    let has = |k| keys.get(account(k)).is_some_and(|s: &String| !s.is_empty());
    KeyStatus {
        elevenlabs: has(KeyName::Elevenlabs),
        openrouter: has(KeyName::Openrouter),
        local_stt: has(KeyName::LocalStt),
        local_llm: has(KeyName::LocalLlm),
    }
}

#[tauri::command]
pub fn key_status() -> KeyStatus {
    status()
}

#[tauri::command]
pub fn set_api_key(name: KeyName, value: Option<String>) -> Result<KeyStatus, String> {
    match value.as_deref().map(str::trim) {
        Some(v) if !v.is_empty() => set(name, v),
        _ => delete(name),
    }
    .map_err(|e| format!("Saving the key failed: {e}"))?;
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip_is_private() {
        let dir = std::env::temp_dir().join(format!("susurro-keys-{}", std::process::id()));
        let p = dir.join("keys.json");
        let mut m = BTreeMap::new();
        m.insert("openrouter".to_string(), "sk-test".to_string());
        write(&p, &m).unwrap();
        assert_eq!(read(&p).get("openrouter").map(String::as_str), Some("sk-test"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
