//! API keys in the macOS Keychain (service `app.susurro.desktop`). Keys never go to the webview.

use crate::contract::{KeyName, KeyStatus};

const SERVICE: &str = "app.susurro.desktop";

fn account(k: KeyName) -> &'static str {
    match k {
        KeyName::Elevenlabs => "elevenlabs",
        KeyName::Openrouter => "openrouter",
        KeyName::LocalStt => "local_stt",
        KeyName::LocalLlm => "local_llm",
    }
}

fn entry(k: KeyName) -> anyhow::Result<keyring::Entry> {
    // A test instance (debug + isolated data dir) gets its own service: it never reads the real keys
    // and never triggers a Keychain access prompt for them.
    let test_instance = cfg!(debug_assertions) && std::env::var_os("SUSURRO_DATA_DIR").is_some();
    Ok(keyring::Entry::new(if test_instance { "app.susurro.desktop.e2e" } else { SERVICE }, account(k))?)
}

/// None if missing or the Keychain is unavailable.
pub fn get(k: KeyName) -> Option<String> {
    #[cfg(debug_assertions)]
    if let Some(v) = crate::test_hooks::key(account(k)) {
        return Some(v).filter(|s| !s.is_empty());
    }
    entry(k).ok()?.get_password().ok().filter(|s| !s.is_empty())
}

pub fn set(k: KeyName, value: &str) -> anyhow::Result<()> {
    Ok(entry(k)?.set_password(value)?)
}

pub fn delete(k: KeyName) -> anyhow::Result<()> {
    match entry(k)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn status() -> KeyStatus {
    KeyStatus {
        elevenlabs: get(KeyName::Elevenlabs).is_some(),
        openrouter: get(KeyName::Openrouter).is_some(),
        local_stt: get(KeyName::LocalStt).is_some(),
        local_llm: get(KeyName::LocalLlm).is_some(),
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
    .map_err(|e| format!("Keychain: {e}"))?;
    Ok(status())
}
