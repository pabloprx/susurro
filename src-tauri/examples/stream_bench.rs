//! Streaming vs single-shot local whisper: time from release to the final text, and WER.
//!   cargo run --release --example stream_bench -- <model.bin> [fixtures dir]
//! The audio is fed in real time (100 ms pieces, like the session tick) to the app's Chunker + Stream.
//! Clips: long.wav with its 14 s padding trimmed (the release 300 ms after the last word), and that x3 (about 2.3 min).
//! Set SUSURRO_WHISPER_THREADS=6 on a shared Mac.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use susurro_lib::audio::{Chunker, Clip, RATE};
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, whisper_local};

fn norm(s: &str) -> Vec<String> {
    let n: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    n.split_whitespace().map(str::to_string).collect()
}

/// Same WER as tests/providers_real.rs.
fn wer(reference: &str, hyp: &str) -> f64 {
    let (r, h) = (norm(reference), norm(hyp));
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for i in 1..=r.len() {
        let mut cur = vec![i; h.len() + 1];
        for j in 1..=h.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(r[i - 1] != h[j - 1]));
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len().max(1) as f64
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = Path::new(&args[0]);
    let dir = Path::new(args.get(1).map(String::as_str).unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/e2e/fixtures")));
    let expect: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap()).unwrap();
    let text = expect["files"].as_array().unwrap().iter().find(|f| f["file"] == "long.wav").unwrap()["text"].as_str().unwrap().to_string();
    let long = susurro_lib::history::read_wav(&dir.join("long.wav")).unwrap();
    // trim the 60 s padding: keep up to 300 ms after the last sample above -50 dBFS
    let last = long.iter().rposition(|x| x.abs() > 0.003).unwrap_or(long.len());
    let speech = &long[..(last + 4800).min(long.len())];
    let gap = vec![0f32; RATE as usize * 6 / 10];
    let triple = [speech, &gap, speech, &gap, speech].concat();
    let clips = [("long.wav", speech.to_vec(), text.clone()), ("long x3", triple, [text.as_str(); 3].join(" "))];

    let ctx = Arc::new(whisper_local::load(model).expect("load"));
    let w = || whisper_local::Whisper::new(ctx.clone());
    let hints = Hints::default();
    // warm up Metal (the app's preload + first dictation do this)
    let _ = w().transcribe(&SttRequest { audio: speech, language: Some("en"), hints: &hints });
    println!("clip\taudio_s\tmode\tchunks\trelease_to_text_ms\twer\ttext");
    for (name, audio, reference) in &clips {
        let secs = audio.len() as f32 / RATE as f32;
        // single-shot: everything after the release
        let t = Instant::now();
        let single = w().transcribe(&SttRequest { audio, language: Some("en"), hints: &hints }).unwrap().text;
        let single_ms = t.elapsed().as_millis();
        println!("{name}\t{secs:.1}\tsingle\t1\t{single_ms}\t{:.3}\t{single}", wer(reference, &single));
        // streaming: real-time feed, chunks transcribed on the worker meanwhile
        let mut ch = Chunker::new(RATE);
        let st = whisper_local::Stream::spawn(w(), Some("en".into()), hints.clone());
        let (t0, mut n) = (Instant::now(), 0);
        for (i, piece) in audio.chunks(RATE as usize / 10).enumerate() {
            if let Some(c) = ch.push(piece) {
                n += 1;
                st.push(c);
            }
            let due = Duration::from_millis(100 * (i as u64 + 1));
            std::thread::sleep(due.saturating_sub(t0.elapsed()));
        }
        let clip = Clip { samples: audio.clone(), ..Default::default() };
        let t = Instant::now();
        let tail = ch.tail(&clip);
        let out = st.finish(tail.clone()).unwrap().text;
        let ms = t.elapsed().as_millis();
        println!("{name}\t{secs:.1}\tstream\t{}\t{ms}\t{:.3}\t{out}", n + tail.is_some() as usize, wer(reference, &out));
    }
}
