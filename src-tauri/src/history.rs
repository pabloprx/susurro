//! WP5: history + stats queries, WAV files, retention sweep (PLAN 6).
//! `created_at` is unix ms. WAVs live in `{data_dir}/audio/{id}.wav`; the `audio` column holds the file name.

use crate::AppState;
use crate::contract::{DayStat, HistoryItem, HistoryKind, HistoryStatus, Stats};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};

/// Fixed typing baseline for "time saved" (PLAN 6). The UI labels it as an assumption.
const TYPING_WPM: f32 = 45.0;
const DAY_MS: i64 = 86_400_000;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn status_str(s: &HistoryStatus) -> &'static str {
    match s {
        HistoryStatus::Pasted => "pasted",
        HistoryStatus::Copied => "copied",
        HistoryStatus::Cancelled => "cancelled",
        HistoryStatus::Failed => "failed",
        HistoryStatus::Done => "done",
    }
}

fn status_of(s: &str) -> HistoryStatus {
    match s {
        "pasted" => HistoryStatus::Pasted,
        "copied" => HistoryStatus::Copied,
        "cancelled" => HistoryStatus::Cancelled,
        "done" => HistoryStatus::Done,
        _ => HistoryStatus::Failed,
    }
}

pub fn audio_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("audio")
}

const COLS: &str = "id, created_at, app_bundle, app_name, raw, final_text, provider, cleanup, duration_ms, words, latency_ms, status, audio, error, kind";

fn row(r: &rusqlite::Row) -> rusqlite::Result<HistoryItem> {
    Ok(HistoryItem {
        id: r.get(0)?,
        created_at: r.get(1)?,
        app_bundle: r.get(2)?,
        app_name: r.get(3)?,
        raw: r.get(4)?,
        final_text: r.get(5)?,
        provider: r.get(6)?,
        cleanup: r.get(7)?,
        duration_ms: r.get(8)?,
        words: r.get(9)?,
        latency_ms: r.get(10)?,
        status: status_of(&r.get::<_, String>(11)?),
        has_audio: r.get::<_, Option<String>>(12)?.is_some(),
        error: r.get(13)?,
        kind: if r.get::<_, String>(14)? == "command" { HistoryKind::Command } else { HistoryKind::Dictation },
    })
}

pub fn words(t: &str) -> u32 {
    t.split_whitespace().count() as u32
}

/// Inserts `h` (its id and has_audio are ignored). With `wav` set, writes `{id}.wav` and links it.
pub fn insert(conn: &Connection, data_dir: &Path, h: &HistoryItem, wav: Option<&[f32]>) -> anyhow::Result<HistoryItem> {
    conn.execute(
        "INSERT INTO history(created_at, app_bundle, app_name, raw, final_text, provider, cleanup, duration_ms, words, latency_ms, status, error, kind)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![h.created_at, h.app_bundle, h.app_name, h.raw, h.final_text, h.provider, h.cleanup, h.duration_ms, h.words, h.latency_ms, status_str(&h.status), h.error,
            if h.kind == HistoryKind::Command { "command" } else { "dictation" }],
    )?;
    let id = conn.last_insert_rowid();
    if let Some(samples) = wav {
        let file = format!("{id}.wav");
        let dir = audio_dir(data_dir);
        match std::fs::create_dir_all(&dir).map_err(anyhow::Error::from).and_then(|_| crate::audio::write_wav(&dir.join(&file), samples)) {
            Ok(()) => {
                conn.execute("UPDATE history SET audio = ?1 WHERE id = ?2", params![file, id])?;
            }
            Err(e) => log::warn!("history: WAV for {id} not written: {e:#}"),
        }
    }
    get(conn, id)?.ok_or_else(|| anyhow::anyhow!("history row {id} vanished"))
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<HistoryItem>> {
    conn.query_row(&format!("SELECT {COLS} FROM history WHERE id = ?1"), [id], row).optional()
}

pub fn latest(conn: &Connection) -> rusqlite::Result<Option<HistoryItem>> {
    conn.query_row(&format!("SELECT {COLS} FROM history WHERE status IN ('pasted','copied') ORDER BY id DESC LIMIT 1"), [], row).optional()
}

pub fn list(conn: &Connection, query: Option<&str>, before_id: Option<i64>, limit: u32) -> rusqlite::Result<Vec<HistoryItem>> {
    let q = query.map(str::trim).filter(|q| !q.is_empty()).map(|q| format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM history
         WHERE (?1 IS NULL OR raw LIKE ?1 ESCAPE '\\' OR final_text LIKE ?1 ESCAPE '\\') AND (?2 IS NULL OR id < ?2)
         ORDER BY id DESC LIMIT ?3"
    ))?;
    let rows = st.query_map(params![q, before_id, limit.clamp(1, 500)], row)?;
    rows.collect()
}

fn audio_file(conn: &Connection, data_dir: &Path, id: i64) -> rusqlite::Result<Option<PathBuf>> {
    let f: Option<Option<String>> = conn.query_row("SELECT audio FROM history WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    Ok(f.flatten().map(|f| audio_dir(data_dir).join(f)))
}

pub fn delete(conn: &Connection, data_dir: &Path, ids: &[i64]) -> rusqlite::Result<()> {
    for &id in ids {
        if let Some(p) = audio_file(conn, data_dir, id)? {
            let _ = std::fs::remove_file(p);
        }
        conn.execute("DELETE FROM history WHERE id = ?1", [id])?;
    }
    Ok(())
}

pub fn clear(conn: &Connection, data_dir: &Path) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM history", [])?;
    let _ = std::fs::remove_dir_all(audio_dir(data_dir));
    Ok(())
}

/// Drop the WAV of a row (retention, or a retry succeeded with store_audio off).
pub fn drop_audio(conn: &Connection, data_dir: &Path, id: i64) -> rusqlite::Result<()> {
    if let Some(p) = audio_file(conn, data_dir, id)? {
        let _ = std::fs::remove_file(p);
    }
    conn.execute("UPDATE history SET audio = NULL WHERE id = ?1", [id])?;
    Ok(())
}

/// Deletes WAVs older than `days`. Failed rows keep theirs until a retry succeeds or the user deletes them.
pub fn sweep(conn: &Connection, data_dir: &Path, days: u32, now: i64) -> rusqlite::Result<usize> {
    let cutoff = now - days as i64 * DAY_MS;
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM history WHERE audio IS NOT NULL AND status != 'failed' AND created_at < ?1")?
        .query_map([cutoff], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for &id in &ids {
        drop_audio(conn, data_dir, id)?;
    }
    Ok(ids.len())
}

/// SQL over pasted/copied rows (PLAN 6). `daily` = the last 7 local days, oldest first.
pub fn stats(conn: &Connection, now: i64) -> rusqlite::Result<Stats> {
    let ok = "status IN ('pasted','copied')";
    let (words_total, dur_ms): (i64, i64) =
        conn.query_row(&format!("SELECT COALESCE(SUM(words),0), COALESCE(SUM(duration_ms),0) FROM history WHERE {ok}"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let week = now - 7 * DAY_MS;
    let (words_7d, sessions_7d): (i64, i64) =
        conn.query_row(&format!("SELECT COALESCE(SUM(words),0), COUNT(*) FROM history WHERE {ok} AND created_at >= ?1"), [week], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let speak_min = dur_ms as f32 / 60_000.0;
    let wpm = if speak_min > 0.0 { words_total as f32 / speak_min } else { 0.0 };
    let minutes_saved = if words_total > 0 { (words_total as f32 / TYPING_WPM - speak_min).max(0.0) } else { 0.0 };

    use chrono::{Duration, Local, TimeZone};
    let today = Local.timestamp_millis_opt(now).single().unwrap_or_else(Local::now).date_naive();
    let mut daily: Vec<DayStat> = (0..7).rev().map(|d| DayStat { day: (today - Duration::days(d)).format("%Y-%m-%d").to_string(), words: 0 }).collect();
    let mut st = conn.prepare(&format!("SELECT created_at, words FROM history WHERE {ok} AND created_at >= ?1"))?;
    let rows = st.query_map([now - 8 * DAY_MS], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?)))?;
    for r in rows {
        let (at, w) = r?;
        if let Some(t) = Local.timestamp_millis_opt(at).single() {
            let key = t.date_naive().format("%Y-%m-%d").to_string();
            if let Some(d) = daily.iter_mut().find(|d| d.day == key) {
                d.words += w;
            }
        }
    }
    Ok(Stats { words_total: words_total as u32, words_7d: words_7d as u32, sessions_7d: sessions_7d as u32, wpm, minutes_saved, daily })
}

/// Reads a 16 kHz mono s16 WAV (our own format).
/// The paste result of a retried row.
pub fn set_status(conn: &Connection, id: i64, status: &HistoryStatus) -> rusqlite::Result<()> {
    conn.execute("UPDATE history SET status = ?1 WHERE id = ?2", params![status_str(status), id])?;
    Ok(())
}

pub fn read_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
    decode_wav(&std::fs::read(path)?)
}

/// 16 kHz mono s16 WAV bytes -> f32 samples.
pub fn decode_wav(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    let r = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    anyhow::ensure!(r.spec().sample_rate == crate::audio::RATE && r.spec().channels == 1, "unexpected WAV format");
    Ok(r.into_samples::<i16>().collect::<Result<Vec<_>, _>>()?.into_iter().map(|v| v as f32 / 32768.0).collect())
}

/// Re-runs the stored WAV through the current settings and updates the row (no paste).
/// The status stays (a failed row stays "not pasted"); the error is cleared.
pub fn retry(app: &AppHandle, id: i64) -> anyhow::Result<HistoryItem> {
    let state = app.state::<AppState>();
    let (item, path) = {
        let db = state.db.lock().unwrap();
        let item = get(&db, id)?.ok_or_else(|| anyhow::anyhow!("No history item {id}"))?;
        anyhow::ensure!(item.kind == HistoryKind::Dictation, "A voice command has no audio to retry");
        (item, audio_file(&db, &state.data_dir, id)?)
    };
    let path = path.filter(|p| p.exists()).ok_or_else(|| anyhow::anyhow!("The audio of this dictation has expired"))?;
    let samples = read_wav(&path)?;
    let clip = crate::audio::Clip { duration_ms: (samples.len() as u64 * 1000 / crate::audio::RATE as u64) as u32, peak_db: 0.0, samples, device_names: vec![] };
    let t = std::time::Instant::now();
    let run = crate::session::run_pipeline(&state, &clip, item.app_bundle.as_deref());
    let latency_ms = t.elapsed().as_millis() as u32;
    let cleanup = match &run.outcome {
        crate::pipeline::Outcome::Text(d) => run.cleanup(d),
        _ => None,
    };
    let done = match run.outcome {
        crate::pipeline::Outcome::Text(d) => d,
        crate::pipeline::Outcome::Failed(e) => {
            let db = state.db.lock().unwrap();
            db.execute("UPDATE history SET error = ?1, provider = ?2 WHERE id = ?3", params![e, run.provider, id])?;
            anyhow::bail!("Transcription failed: {e}");
        }
        crate::pipeline::Outcome::Skip(m) => anyhow::bail!(m.unwrap_or_else(|| "Heard nothing".into())),
    };
    let store_audio = state.settings.read().unwrap().privacy.store_audio;
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE history SET raw=?1, final_text=?2, provider=?3, cleanup=?4, words=?5, latency_ms=?6, error=NULL WHERE id=?7",
        params![done.raw, done.final_text, run.provider, cleanup, words(&done.final_text), latency_ms, id],
    )?;
    if !store_audio {
        drop_audio(&db, &state.data_dir, id)?;
    }
    let _ = crate::dictionary::record_uses(&db, &done.used);
    get(&db, id)?.ok_or_else(|| anyhow::anyhow!("history row {id} vanished"))
}

// ---------- commands ----------

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn history_list(state: State<'_, AppState>, query: Option<String>, before_id: Option<i64>, limit: u32) -> Result<Vec<HistoryItem>, String> {
    list(&state.db.lock().unwrap(), query.as_deref(), before_id, limit).map_err(err)
}

#[tauri::command]
pub fn history_delete(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    delete(&state.db.lock().unwrap(), &state.data_dir, &ids).map_err(err)
}

#[tauri::command]
pub fn history_clear(state: State<'_, AppState>) -> Result<(), String> {
    clear(&state.db.lock().unwrap(), &state.data_dir).map_err(err)
}

/// Runs the stored WAV through the current settings (PLAN 1.2: blocking work off the main thread).
#[tauri::command]
pub async fn history_retry(app: AppHandle, id: i64) -> Result<HistoryItem, String> {
    tauri::async_runtime::spawn_blocking(move || retry(&app, id).map_err(|e| format!("{e:#}"))).await.map_err(err)?
}

/// Raw WAV bytes; JS builds a Blob URL.
#[tauri::command]
pub fn history_audio(state: State<'_, AppState>, id: i64) -> Result<tauri::ipc::Response, String> {
    let p = audio_file(&state.db.lock().unwrap(), &state.data_dir, id).map_err(err)?.ok_or("No audio for this dictation")?;
    Ok(tauri::ipc::Response::new(std::fs::read(p).map_err(|e| format!("audio: {e}"))?))
}

/// which: "raw" | "final". The session actor hides susurro and pastes into the app behind it.
#[tauri::command]
pub fn history_paste(state: State<'_, AppState>, id: i64, which: String) -> Result<(), String> {
    let h = get(&state.db.lock().unwrap(), id).map_err(err)?.ok_or("No such history item")?;
    let text = if which == "raw" { h.raw } else { h.final_text };
    state.session_tx.send(crate::session::SessionCmd::PasteText(text)).map_err(|_| "session is not running".to_string())
}

#[tauri::command]
pub fn get_stats(state: State<'_, AppState>) -> Result<Stats, String> {
    stats(&state.db.lock().unwrap(), now_ms()).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(at: i64, text: &str, status: HistoryStatus, dur: u32) -> HistoryItem {
        HistoryItem {
            id: 0,
            created_at: at,
            app_bundle: Some("com.apple.TextEdit".into()),
            app_name: Some("TextEdit".into()),
            raw: text.to_lowercase(),
            final_text: text.into(),
            provider: "whisper_local:ggml-base.bin".into(),
            cleanup: None,
            duration_ms: dur,
            words: words(text),
            latency_ms: 100,
            status,
            has_audio: false,
            error: None,
            kind: HistoryKind::Dictation,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("susurro-hist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn insert_list_search_page_delete() {
        let (c, dir) = (crate::db::open_memory().unwrap(), tmp("list"));
        let now = now_ms();
        let a = insert(&c, &dir, &item(now - 3000, "Hello Contaflow", HistoryStatus::Pasted, 2000), Some(&[0.1; 1600])).unwrap();
        let b = insert(&c, &dir, &item(now - 2000, "100% done_now", HistoryStatus::Copied, 1000), None).unwrap();
        let f = insert(&c, &dir, &item(now - 1000, "", HistoryStatus::Failed, 1000), Some(&[0.1; 1600])).unwrap();
        assert!(a.has_audio && !b.has_audio && f.has_audio);
        assert!(audio_dir(&dir).join(format!("{}.wav", a.id)).exists());
        assert_eq!(read_wav(&audio_dir(&dir).join(format!("{}.wav", a.id))).unwrap().len(), 1600);

        let all = list(&c, None, None, 50).unwrap();
        assert_eq!(all.iter().map(|h| h.id).collect::<Vec<_>>(), vec![f.id, b.id, a.id], "newest first");
        assert_eq!(list(&c, Some("contaflow"), None, 50).unwrap().len(), 1, "LIKE is case-insensitive");
        assert_eq!(list(&c, Some("%"), None, 50).unwrap().len(), 1, "% is literal");
        assert_eq!(list(&c, Some("_"), None, 50).unwrap().len(), 1, "_ is literal");
        assert_eq!(list(&c, None, Some(f.id), 1).unwrap()[0].id, b.id, "before_id pages");
        assert_eq!(latest(&c).unwrap().unwrap().id, b.id, "paste last skips failed rows");

        delete(&c, &dir, &[a.id]).unwrap();
        assert!(!audio_dir(&dir).join(format!("{}.wav", a.id)).exists());
        assert_eq!(list(&c, None, None, 50).unwrap().len(), 2);
        clear(&c, &dir).unwrap();
        assert!(list(&c, None, None, 50).unwrap().is_empty());
    }

    #[test]
    fn sweep_keeps_failed_and_recent_audio() {
        let (c, dir) = (crate::db::open_memory().unwrap(), tmp("sweep"));
        let now = now_ms();
        let old = insert(&c, &dir, &item(now - 20 * DAY_MS, "old one", HistoryStatus::Pasted, 1000), Some(&[0.0; 160])).unwrap();
        let failed = insert(&c, &dir, &item(now - 20 * DAY_MS, "", HistoryStatus::Failed, 1000), Some(&[0.0; 160])).unwrap();
        let recent = insert(&c, &dir, &item(now - DAY_MS, "new one", HistoryStatus::Pasted, 1000), Some(&[0.0; 160])).unwrap();
        assert_eq!(sweep(&c, &dir, 14, now).unwrap(), 1);
        assert!(!get(&c, old.id).unwrap().unwrap().has_audio);
        assert!(!audio_dir(&dir).join(format!("{}.wav", old.id)).exists());
        assert!(get(&c, failed.id).unwrap().unwrap().has_audio);
        assert!(get(&c, recent.id).unwrap().unwrap().has_audio);
    }

    #[test]
    fn stats_count_only_delivered_rows() {
        let (c, dir) = (crate::db::open_memory().unwrap(), tmp("stats"));
        let now = now_ms();
        // 90 words over 60 s of speech = 90 wpm; typing them takes 2 min, so 1 min saved.
        insert(&c, &dir, &item(now - 1000, &"word ".repeat(60), HistoryStatus::Pasted, 40_000), None).unwrap();
        insert(&c, &dir, &item(now - 2000, &"word ".repeat(30), HistoryStatus::Copied, 20_000), None).unwrap();
        insert(&c, &dir, &item(now - 10 * DAY_MS, &"word ".repeat(0), HistoryStatus::Failed, 5_000), None).unwrap();
        let s = stats(&c, now).unwrap();
        assert_eq!((s.words_total, s.words_7d, s.sessions_7d), (90, 90, 2));
        assert!((s.wpm - 90.0).abs() < 0.01);
        assert!((s.minutes_saved - 1.0).abs() < 0.01);
        assert_eq!(s.daily.len(), 7);
        assert_eq!(s.daily.last().unwrap().words, 90);
        let empty = stats(&crate::db::open_memory().unwrap(), now).unwrap();
        assert_eq!((empty.wpm, empty.minutes_saved), (0.0, 0.0));
    }
}
