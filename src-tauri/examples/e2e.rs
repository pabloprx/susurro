//! End-to-end check of the core loop with the real modules (WP5): jfk.wav (or `--mic N` seconds from the
//! resolved mic) -> audio::Clip -> pipeline::run (whisper local, dictionary, no LLM) -> paste::paste into a new
//! temp .txt opened in TextEdit -> Cmd+S -> read the file back -> assert. Also checks the clipboard is restored.
//!
//!   SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-base.en-q5_1.bin cargo run --release --example e2e [-- --mic 5]
//!
//! Needs Accessibility (Cmd+V, Cmd+S) for the terminal. No Apple Events, so no Automation grant.

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use susurro_lib::audio::{self, Clip};
use susurro_lib::contract::{DictEntry, DictKind, DictOpts, PasteOpts, Style};
use susurro_lib::dictionary::Dictionary;
use susurro_lib::pipeline::{self, Outcome};
use susurro_lib::stt::{SttRequest, Transcriber, whisper_local};
use susurro_lib::paste;

const EXPECTED: &str = "ask not what your country can do for you";

fn main() {
    if let Err(e) = run() {
        eprintln!("e2e FAILED: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mic_secs: Option<u64> = args.iter().position(|a| a == "--mic").map(|i| args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(5));
    let model = std::env::var("SUSURRO_TEST_MODEL").map_err(|_| anyhow::anyhow!("set SUSURRO_TEST_MODEL to a ggml model file"))?;
    let model = model.replacen('~', &std::env::var("HOME").unwrap_or_default(), 1);

    let t = Instant::now();
    let clip = match mic_secs {
        Some(n) => record(n)?,
        None => {
            let samples = susurro_lib::history::read_wav(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/jfk.wav")))?;
            let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
            Clip { duration_ms: (samples.len() as u64 * 1000 / audio::RATE as u64) as u32, peak_db: audio::db(peak), samples, device_names: vec!["jfk.wav".into()] }
        }
    };
    println!("audio:      {:>6} ms  ({:.1} s, peak {:.1} dBFS)", t.elapsed().as_millis(), clip.duration_ms as f32 / 1000.0, clip.peak_db);

    let t = Instant::now();
    let whisper = whisper_local::Whisper::new(Arc::new(whisper_local::load(std::path::Path::new(&model))?));
    println!("model load: {:>6} ms  ({model})", t.elapsed().as_millis());

    // One correction, so the dictionary pass is part of the check: "fellow Americans" -> "Fellow-Americans".
    let dict = Dictionary::new(
        vec![DictEntry {
            id: 1,
            kind: DictKind::Word,
            text: "Fellow-Americans".into(),
            aliases: vec![],
            expansion: None,
            exact_only: true,
            starred: false,
            uses: 0,
            created_at: 0,
        }],
        DictOpts::default(),
    );
    let t = Instant::now();
    let stt = |r: &SttRequest| whisper.transcribe(r);
    let opts = pipeline::Opts { language: Some("auto"), style: Style::Formal, raw_app: false, press_enter_command: false };
    let done = match pipeline::run(&clip, &opts, &dict, &stt, None) {
        Outcome::Text(d) => d,
        other => anyhow::bail!("pipeline gave {other:?}"),
    };
    println!("pipeline:   {:>6} ms  raw {:?}", t.elapsed().as_millis(), done.raw);
    println!("            final {:?} (dictionary ids {:?})", done.final_text, done.used);

    // The user's own clipboard (text) must survive the paste; it is never overwritten by this check.
    let before = paste::clipboard_text();
    // No Apple Events (they need an Automation grant): open a temp .txt, paste, Cmd+S, read the file.
    let doc = std::env::temp_dir().join(format!("susurro-e2e-{}.txt", std::process::id()));
    std::fs::write(&doc, "")?;
    let pids_before = textedit_pids();
    anyhow::ensure!(Command::new("open").args(["-a", "TextEdit"]).arg(&doc).status()?.success(), "open -a TextEdit failed");
    std::thread::sleep(Duration::from_millis(1500)); // ponytail: fixed wait for TextEdit focus; poll if it flakes.
    println!("frontmost:  {:?}", paste::frontmost_app().and_then(|a| a.bundle_id));
    let t = Instant::now();
    let out = paste::paste(&done.final_text, &PasteOpts::default());
    println!("paste:      {:>6} ms  {out:?} (includes restore_delay_ms = 350)", t.elapsed().as_millis());
    key(1, true); // Cmd+S
    std::thread::sleep(Duration::from_millis(800));
    let back = std::fs::read_to_string(&doc)?;
    key(13, true); // Cmd+W
    std::thread::sleep(Duration::from_millis(300));
    // Quit TextEdit only if this run started it.
    if pids_before.is_empty() {
        for pid in textedit_pids() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
    let _ = std::fs::remove_file(&doc);
    println!("read back:  {back:?}");
    let clip_now = paste::clipboard_text();

    anyhow::ensure!(out == paste::PasteOutcome::Pasted, "not pasted (does the terminal have Accessibility?)");
    anyhow::ensure!(back.trim() == done.final_text, "TextEdit holds {back:?}, expected {:?}", done.final_text);
    if before.is_some() {
        anyhow::ensure!(clip_now == before, "clipboard not restored: {clip_now:?}");
        println!("clipboard:  restored");
    }
    if mic_secs.is_none() {
        anyhow::ensure!(norm(&back).contains(EXPECTED), "transcript does not contain {EXPECTED:?}");
        anyhow::ensure!(back.contains("Fellow-Americans"), "dictionary correction missing");
    }
    println!("e2e OK");
    Ok(())
}

fn norm(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The real Recorder on the device a dictation would use (Auto rules).
fn record(secs: u64) -> anyhow::Result<Clip> {
    let devs = audio::list_devices();
    let def = devs.iter().find(|d| d.is_default).map(|d| d.id.as_str());
    let dev = audio::resolve(&susurro_lib::contract::MicSelection::Auto, &[], &devs, def).ok_or_else(|| anyhow::anyhow!("no microphone"))?;
    let rec = audio::Recorder::open(&dev.info, Arc::new(|_: susurro_lib::contract::AudioLevel| {}), Arc::new(|| eprintln!("mic lost")))?;
    println!("recording {secs} s from {} ...", dev.info.name);
    std::thread::sleep(Duration::from_secs(secs));
    Ok(rec.stop())
}

fn textedit_pids() -> Vec<u32> {
    let out = Command::new("pgrep").args(["-x", "TextEdit"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    out.split_whitespace().filter_map(|p| p.parse().ok()).collect()
}

/// keycode with Command only (the same flags rule as paste.rs).
fn key(code: u16, cmd: bool) {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    for down in [true, false] {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else { return };
        let Ok(e) = CGEvent::new_keyboard_event(src, code, down) else { return };
        e.set_flags(if cmd { CGEventFlags::CGEventFlagCommand } else { CGEventFlags::empty() });
        e.post(CGEventTapLocation::HID);
    }
}
