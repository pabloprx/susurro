//! Debug-only test hooks, read from env vars. lib.rs compiles this module only with debug_assertions.
//! - SUSURRO_DATA_DIR=<dir>: settings, db, audio, models and logs live here instead of the app data dir.
//! - SUSURRO_TEST_AUDIO=<a.wav>[:<b.wav>...]: each recorder open streams the next file (wraps around) instead of cpal.
//! - SUSURRO_TEST_PASTE_TARGET=<bundle id>: Cmd+V / Return only if that app is frontmost; else no paste and no clipboard write.
//! - SUSURRO_TEST_CONTROL=<port>: HTTP on 127.0.0.1 (GET /state, POST /settings /dict /audio /event/<name>
//!   /mic/select /mic/priority /mic/forget /toast_action /navigate/<screen|hide>, GET /permissions,
//!   POST /permissions/request/<microphone|accessibility|input_monitoring>).

use crate::AppState;
use crate::contract::{DictEntryInput, Settings};
use crate::session::SessionCmd;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Manager};

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("SUSURRO_DATA_DIR").map(PathBuf::from)
}

// ---------- test audio ----------

/// (files, index of the next one).
static AUDIO: LazyLock<Mutex<(Vec<PathBuf>, usize)>> =
    LazyLock::new(|| Mutex::new((split(&std::env::var("SUSURRO_TEST_AUDIO").unwrap_or_default()), 0)));

fn split(list: &str) -> Vec<PathBuf> {
    list.split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect()
}

/// The next test file, or None = use the real mic.
pub fn next_audio() -> Option<PathBuf> {
    let mut a = AUDIO.lock().unwrap();
    if a.0.is_empty() {
        return None;
    }
    let p = a.0[a.1 % a.0.len()].clone();
    a.1 += 1;
    Some(p)
}

/// Any WAV -> 16 kHz mono f32.
pub fn load_wav(path: &std::path::Path) -> anyhow::Result<Vec<f32>> {
    use anyhow::Context;
    let mut r = hound::WavReader::open(path).with_context(|| format!("test audio {}", path.display()))?;
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = raw.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    Ok(crate::audio::resample(&mono, spec.sample_rate))
}

// ---------- in-memory keys ----------

static KEYS: LazyLock<Mutex<std::collections::HashMap<String, String>>> = LazyLock::new(Default::default);

/// A key set through POST /keys (Keychain account name). Only a test instance sets any.
pub fn key(account: &str) -> Option<String> {
    KEYS.lock().unwrap().get(account).cloned()
}

// ---------- paste guard ----------

/// Some(reason) = do not post keys: the frontmost app is not the test target.
pub fn paste_blocked() -> Option<String> {
    let target = std::env::var("SUSURRO_TEST_PASTE_TARGET").ok()?;
    let front = crate::paste::frontmost_app().and_then(|a| a.bundle_id);
    (front.as_deref() != Some(target.as_str())).then(|| format!("frontmost is {front:?}, not {target}"))
}

// ---------- control endpoint ----------

pub fn start_control(app: &AppHandle) {
    let Some(port) = std::env::var("SUSURRO_TEST_CONTROL").ok().and_then(|p| p.parse::<u16>().ok()) else { return };
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => return log::error!("test control: bind 127.0.0.1:{port}: {e}"),
    };
    log::info!("test control: http://127.0.0.1:{port}");
    let app = app.clone();
    std::thread::Builder::new()
        .name("test-control".into())
        .spawn(move || {
            for s in listener.incoming().flatten() {
                if let Err(e) = serve(&app, s) {
                    log::warn!("test control: {e:#}");
                }
            }
        })
        .expect("spawn test control");
}

fn serve(app: &AppHandle, mut s: std::net::TcpStream) -> anyhow::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut len = 0;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((_, v)) = h.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")) {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let (code, out) = match route(app, &method, &path, &body) {
        Ok(v) => (200, v),
        Err(e) => (400, json!({ "error": e })),
    };
    let out = out.to_string();
    write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len())?;
    Ok(())
}

fn route(app: &AppHandle, method: &str, path: &str, body: &[u8]) -> Result<Value, String> {
    let state = app.state::<AppState>();
    let parse = || serde_json::from_slice::<Value>(body).map_err(|e| format!("body: {e}"));
    let to_json = |v: Result<Value, serde_json::Error>| v.map_err(|e| e.to_string());
    match (method, path) {
        ("GET", "/state") => {
            let last = crate::history::list(&state.db.lock().unwrap(), None, None, 1).map_err(|e| e.to_string())?.into_iter().next();
            Ok(json!({
                "session": crate::session::get_session_state(),
                "last_history": last,
                "stats": crate::history::get_stats(app.state()).ok(),
                "mic": crate::audio::mic_state(&state),
            }))
        }
        ("POST", "/settings") => {
            let mut cur = serde_json::to_value(state.settings.read().unwrap().clone()).map_err(|e| e.to_string())?;
            merge(&mut cur, parse()?);
            let s: Settings = serde_json::from_value(cur).map_err(|e| format!("settings: {e}"))?;
            let saved = crate::settings::save_settings(app.clone(), state, s)?;
            to_json(serde_json::to_value(saved))
        }
        ("POST", "/dict") => {
            let v = parse()?;
            let list = if v.is_array() { v } else { Value::Array(vec![v]) };
            let entries: Vec<DictEntryInput> = serde_json::from_value(list).map_err(|e| format!("dict: {e}"))?;
            let db = state.db.lock().unwrap();
            let out = entries.iter().map(|e| crate::dictionary::upsert(&db, e)).collect::<Result<Vec<_>, _>>()?;
            to_json(serde_json::to_value(out))
        }
        ("POST", "/audio") => {
            let list = String::from_utf8_lossy(body).trim().to_string();
            *AUDIO.lock().unwrap() = (split(&list), 0);
            Ok(json!({ "audio": list }))
        }
        // The real mic commands (POST /settings keeps settings.mic except show_virtual).
        ("POST", "/mic/select") => {
            let sel = serde_json::from_slice(body).map_err(|e| format!("selection: {e}"))?;
            to_json(serde_json::to_value(crate::audio::set_mic_selection(app.clone(), sel)?))
        }
        ("POST", "/mic/priority") => {
            let ids = serde_json::from_slice(body).map_err(|e| format!("ids: {e}"))?;
            to_json(serde_json::to_value(crate::audio::set_mic_priority(app.clone(), app.state(), ids)?))
        }
        ("POST", "/mic/forget") => {
            let id = serde_json::from_slice(body).map_err(|e| format!("id: {e}"))?;
            to_json(serde_json::to_value(crate::audio::forget_device(app.clone(), app.state(), id)?))
        }
        ("POST", "/toast_action") => {
            let v = parse()?;
            let (toast_id, action_id) = (v["toast_id"].as_str().unwrap_or_default().into(), v["action_id"].as_str().unwrap_or_default().into());
            state.session_tx.send(SessionCmd::ToastAction { toast_id, action_id }).map_err(|_| "session is not running".to_string())?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/keys") => {
            let v: std::collections::HashMap<String, String> = serde_json::from_slice(body).map_err(|e| format!("keys: {e}"))?;
            let mut k = KEYS.lock().unwrap();
            k.extend(v);
            Ok(json!({ "keys": k.iter().map(|(n, v)| (n.clone(), !v.is_empty())).collect::<std::collections::HashMap<_, _>>() }))
        }
        ("POST", p) if p.starts_with("/test_provider/") => {
            let r = tauri::async_runtime::block_on(crate::stt::test_provider(app.clone(), p["/test_provider/".len()..].to_string()))?;
            to_json(serde_json::to_value(r))
        }
        ("GET", p) if p.starts_with("/models/") => {
            let r = tauri::async_runtime::block_on(crate::stt::openrouter_models(p["/models/".len()..].to_string()));
            match r {
                Ok(m) => Ok(json!({ "count": m.len(), "first": m.iter().take(5).map(|m| &m.id).collect::<Vec<_>>(), "ids": m.iter().map(|m| &m.id).collect::<Vec<_>>() })),
                Err(e) => Err(e),
            }
        }
        // Shows the main window on a screen (as the tray does): POST /navigate/<screen>; /navigate/hide hides it.
        ("POST", p) if p.starts_with("/navigate/") => {
            use tauri::Emitter;
            let screen = &p["/navigate/".len()..];
            if screen == "hide" {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            } else {
                crate::show_main(app);
                let _ = app.emit_to("main", "navigate", crate::contract::Navigate { screen: screen.into() });
            }
            Ok(json!({ "ok": true }))
        }
        ("GET", "/permissions") => to_json(serde_json::to_value(crate::permissions::permissions_status()?)),
        ("POST", p) if p.starts_with("/permissions/request/") => {
            let kind = serde_json::from_value(json!(&p["/permissions/request/".len()..])).map_err(|e| e.to_string())?;
            crate::permissions::request_permission(kind)?;
            Ok(json!({ "ok": true }))
        }
        ("POST", p) if p.starts_with("/event/") => {
            let cmd = match &p["/event/".len()..] {
                "hold_start" => SessionCmd::PttDown,
                "hold_end" => SessionCmd::PttUp,
                "toggle" => SessionCmd::Toggle,
                "stop" => SessionCmd::Stop,
                "cancel" => SessionCmd::Cancel,
                "paste_last" => SessionCmd::PasteLast,
                other => return Err(format!("unknown event {other}")),
            };
            state.session_tx.send(cmd).map_err(|_| "session is not running".to_string())?;
            Ok(json!({ "ok": true }))
        }
        _ => Err(format!("no route {method} {path}")),
    }
}

/// Object keys merge recursively; anything else replaces.
fn merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_deep() {
        let mut a = json!({"stt": {"provider": "openrouter", "whisper_model": "x"}, "language": "en"});
        merge(&mut a, json!({"stt": {"provider": "whisper_local"}, "language": "es"}));
        assert_eq!(a, json!({"stt": {"provider": "whisper_local", "whisper_model": "x"}, "language": "es"}));
    }

    #[test]
    fn wav_loads_as_16k_mono() {
        let p = std::env::temp_dir().join(format!("susurro-hook-{}.wav", std::process::id()));
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..48_000 {
            w.write_sample(8192i16).unwrap();
            w.write_sample(8192i16).unwrap();
        }
        w.finalize().unwrap();
        let s = load_wav(&p).unwrap();
        assert!((s.len() as i64 - 16_000).abs() < 200, "{}", s.len());
        assert!((s[8000] - 0.25).abs() < 0.01);
        let _ = std::fs::remove_file(p);
    }
}
