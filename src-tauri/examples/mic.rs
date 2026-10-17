//! WP1 check: device listing, recording with live device-loss recovery, latency numbers.
//!   cargo run --example mic -- list
//!   cargo run --example mic -- record 3 /tmp/susurro-mic.wav [device-id]
//! Unplug the mic during `record 10`: the log says "switched to <name>" and the WAV keeps both parts.

use std::sync::Arc;
use std::time::{Duration, Instant};
use susurro_lib::audio::{self, Recorder};
use susurro_lib::contract::MicSelection;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => list(),
        Some("record") => record(
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3),
            args.get(2).map_or("/tmp/susurro-mic.wav", String::as_str),
            args.get(3).cloned(),
        ),
        _ => anyhow::bail!("usage: mic list | mic record SECS PATH [device-id]"),
    }
}

fn list() -> anyhow::Result<()> {
    let t = Instant::now();
    let devs = audio::list_devices();
    let first = t.elapsed();
    for d in &devs {
        println!("{}{:<36} {:<10} {:<28} {}", if d.is_default { "* " } else { "  " }, d.name, format!("{:?}", d.kind), d.manufacturer.as_deref().unwrap_or("-"), d.id);
    }
    const N: u32 = 20;
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(audio::list_devices());
    }
    println!("poll cost: first {:.2} ms, avg {:.2} ms over {N} (one monitor tick every 1.5 s)", first.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3 / N as f64);
    let def = devs.iter().find(|d| d.is_default).map(|d| d.id.as_str());
    match audio::resolve(&MicSelection::Auto, &[], &devs, def) {
        Some(r) => println!("auto -> {} ({:?})", r.info.name, r.reason),
        None => println!("auto -> No microphone"),
    }
    Ok(())
}

fn record(secs: u64, path: &str, device: Option<String>) -> anyhow::Result<()> {
    let devs = audio::list_devices();
    let def = devs.iter().find(|d| d.is_default).map(|d| d.id.clone());
    let sel = device.map_or(MicSelection::Auto, |id| MicSelection::Device { id });
    let r = audio::resolve(&sel, &[], &devs, def.as_deref()).ok_or_else(|| anyhow::anyhow!("No microphone"))?;
    println!("recording {secs} s from {} ({:?})", r.info.name, r.reason);
    let last_print = std::sync::Mutex::new(Instant::now());
    let mut rec = Recorder::open(
        &r.info,
        Arc::new(move |l| {
            let mut t = last_print.lock().unwrap();
            if t.elapsed() >= Duration::from_millis(500) {
                println!("  level {:>6.1} dB  peak {:>6.1} dB", l.db, l.peak_db);
                *t = Instant::now();
            }
        }),
        Arc::new(|| eprintln!("  stream: device lost")),
    )?;
    let t0 = Instant::now();
    let mut last_check = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(100));
        // Same two signals as the app: the stream error flag, or the device missing from a poll.
        if rec.lost() || last_check.elapsed() >= Duration::from_millis(1500) {
            last_check = Instant::now();
            match rec.recover(&sel, &[]) {
                Ok(Some(r)) => println!("switched to {}", r.info.name),
                Ok(None) => {}
                Err(e) => {
                    println!("no device left ({e}); stopping");
                    break;
                }
            }
        }
    }
    let lat = rec.first_sample_latency();
    let t = Instant::now();
    let clip = rec.stop();
    println!(
        "press-to-first-sample {:.1} ms; stop+resample {:.1} ms; {} samples, {} ms, peak {:.1} dBFS, devices {:?}{}",
        lat.map_or(f64::NAN, |d| d.as_secs_f64() * 1e3),
        t.elapsed().as_secs_f64() * 1e3,
        clip.samples.len(),
        clip.duration_ms,
        clip.peak_db,
        clip.device_names,
        if clip.silent() { "  (below the silence gate)" } else { "" }
    );
    audio::write_wav(std::path::Path::new(path), &clip.samples)?;
    println!("wrote {path}");
    Ok(())
}
