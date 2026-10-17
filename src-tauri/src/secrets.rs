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
    Ok(keyring::Entry::new(SERVICE, account(k))?)
}

/// None if missing or the Keychain is unavailable.
pub fn get(k: KeyName) -> Option<String> {
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
