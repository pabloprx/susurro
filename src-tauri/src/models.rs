//! WP2: whisper model catalog, download with progress (`.part` + Content-Length check), delete.

use crate::AppState;
use crate::contract::{ModelInfo, ModelProgress};
use crate::stt::{http, whisper_local};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// (file, size in MB, note). Sizes: Content-Length from Hugging Face, measured 2026-10-17.
pub const CATALOG: &[(&str, u32, &str)] = &[
    ("ggml-large-v3-turbo-q5_0.bin", 574, "Recommended. Multilingual, best accuracy for its speed"),
    ("ggml-large-v3-turbo-q8_0.bin", 874, "Multilingual, a little more accurate than q5_0, bigger"),
    ("ggml-small-q5_1.bin", 190, "Multilingual, balanced"),
    ("ggml-base-q5_1.bin", 60, "Quick start. Multilingual, fast"),
    ("ggml-base.en-q5_1.bin", 60, "English only, fast"),
    ("ggml-tiny-q5_1.bin", 32, "Fastest, lowest accuracy"),
];

fn check(file: &str) -> Result<(), String> {
    CATALOG.iter().any(|(f, ..)| *f == file).then_some(()).ok_or_else(|| format!("Unknown model {file:?}"))
}

/// GET `url` into `dest` via `dest.part`. Bytes written must equal Content-Length, then rename.
/// `progress(done, total)` runs about every 1% and at the end.
pub(crate) fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
    // ponytail: one 2 h cap for the whole transfer, no resume; add Range resume if users ask.
    let mut resp = http().get(url).timeout(Duration::from_secs(7200)).send().map_err(|e| anyhow::anyhow!("Download failed: {e}"))?;
    anyhow::ensure!(resp.status().is_success(), "Download failed: HTTP {}", resp.status().as_u16());
    let total = resp.content_length().ok_or_else(|| anyhow::anyhow!("Download failed: no Content-Length"))?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = dest.with_file_name(format!("{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    let r = (|| -> anyhow::Result<u64> {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
        let mut buf = vec![0u8; 256 * 1024];
        let (mut done, mut last) = (0u64, 0u64);
        loop {
            let n = resp.read(&mut buf).map_err(|e| anyhow::anyhow!("Download interrupted: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            if done - last >= (total / 100).max(1) {
                last = done;
                progress(done, total);
            }
        }
        out.flush()?;
        Ok(done)
    })();
    match r {
        Ok(done) if done == total => {
            progress(done, total);
            std::fs::rename(&part, dest)?;
            Ok(())
        }
        Ok(done) => {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("Download incomplete: got {done} of {total} bytes")
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn list(data_dir: &Path, active: &str) -> Vec<ModelInfo> {
    CATALOG
        .iter()
        .map(|(f, mb, note)| ModelInfo {
            file: f.to_string(),
            size_mb: *mb,
            downloaded: whisper_local::model_path(data_dir, f).exists(),
            active: *f == active,
            note: note.to_string(),
        })
        .collect()
}

#[tauri::command]
pub fn whisper_models(state: State<'_, AppState>) -> Result<Vec<ModelInfo>, String> {
    let active = state.settings.read().unwrap().stt.whisper_model.clone();
    Ok(list(&state.data_dir, &active))
}

/// Emits `model_progress` to `main`. Reloads whisper when the file is the configured model.
#[tauri::command]
pub async fn whisper_download(app: AppHandle, file: String) -> Result<(), String> {
    check(&file)?;
    // ponytail: two clicks on the same model run two downloads into the same .part; the UI disables the button.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let dest = whisper_local::model_path(&state.data_dir, &file);
        download(&format!("{HF_BASE}/{file}"), &dest, |done, total| {
            let _ = app.emit_to("main", "model_progress", ModelProgress { file: file.clone(), done, total });
        })?;
        if state.settings.read().unwrap().stt.whisper_model == file {
            whisper_local::load_in_background(app.clone());
        }
        anyhow::Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn whisper_delete(state: State<'_, AppState>, file: String) -> Result<(), String> {
    check(&file)?;
    {
        let mut g = state.whisper.lock().unwrap();
        if g.as_ref().is_some_and(|(f, _)| *f == file) {
            *g = None;
        }
    }
    match std::fs::remove_file(whisper_local::model_path(&state.data_dir, &file)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Delete failed: {e}")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("susurro-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn download_writes_part_then_renames() {
        let mut srv = mockito::Server::new();
        let body = vec![7u8; 600_000];
        let m = srv.mock("GET", "/m.bin").with_body(&body).create();
        let dir = tmp("ok");
        let dest = dir.join("models/m.bin");
        let mut calls = vec![];
        download(&format!("{}/m.bin", srv.url()), &dest, |d, t| calls.push((d, t))).unwrap();
        m.assert();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!dir.join("models/m.bin.part").exists());
        assert!(calls.len() > 2);
        assert_eq!(*calls.last().unwrap(), (600_000, 600_000));
        assert!(calls.windows(2).all(|w| w[0].0 <= w[1].0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn download_http_error_leaves_nothing() {
        let mut srv = mockito::Server::new();
        srv.mock("GET", "/m.bin").with_status(404).create();
        let dir = tmp("404");
        let dest = dir.join("m.bin");
        let e = download(&format!("{}/m.bin", srv.url()), &dest, |_, _| {}).unwrap_err().to_string();
        assert!(e.contains("HTTP 404"), "{e}");
        assert!(!dest.exists() && !dir.join("m.bin.part").exists());
    }

    #[test]
    fn catalog_and_list() {
        assert!(check("ggml-base.en-q5_1.bin").is_ok());
        assert!(check("../../etc/passwd").is_err());
        assert!(CATALOG.iter().any(|(f, mb, n)| *f == "ggml-large-v3-turbo-q5_0.bin" && *mb == 574 && n.starts_with("Recommended")));
        let dir = tmp("list");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("models/ggml-tiny-q5_1.bin"), b"x").unwrap();
        let l = list(&dir, "ggml-tiny-q5_1.bin");
        let tiny = l.iter().find(|m| m.file == "ggml-tiny-q5_1.bin").unwrap();
        assert!(tiny.downloaded && tiny.active);
        assert_eq!(l.iter().filter(|m| m.downloaded || m.active).count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
