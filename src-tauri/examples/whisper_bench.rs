//! Perf bench: whisper load + transcribe time per fixture, with the app's own Whisper transcriber.
//!   cargo run --release --example whisper_bench -- <model.bin> <a.wav> [b.wav ...]
//! Language: files named es_* use "es", others "en". Prints TSV: model file lang audio_ms cold_ms warm_median_ms warm_min_ms text.
//! "cold" = first transcribe in this process after the load (includes Metal pipeline setup).

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, whisper_local};

const WARM_RUNS: usize = 3;

fn wav(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("wav");
    let spec = r.spec();
    let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
    let raw: Vec<f32> = r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect();
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = raw.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    susurro_lib::audio::resample(&mono, spec.sample_rate)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Recorder::stop resamples the whole clip on the session thread: time it for a 48 kHz mic.
    for secs in [2usize, 46] {
        let s48 = vec![0.1f32; 48_000 * secs];
        let t = Instant::now();
        let out = susurro_lib::audio::resample(&s48, 48_000);
        println!("#resample\t48k->16k\t{secs}s\t{} ms\t{} samples", t.elapsed().as_millis(), out.len());
    }
    let model = Path::new(&args[0]);
    let t = Instant::now();
    let ctx = Arc::new(whisper_local::load(model).expect("load"));
    let load_ms = t.elapsed().as_millis();
    let t = Instant::now();
    drop(ctx);
    let ctx = Arc::new(whisper_local::load(model).expect("reload"));
    let reload_ms = t.elapsed().as_millis();
    let name = model.file_name().unwrap().to_string_lossy();
    println!("#load\t{name}\tfirst_ms={load_ms}\treload_ms={reload_ms}");
    let w = whisper_local::Whisper::new(ctx);
    let hints = Hints { terms: vec![], aliases: vec![], triggers: vec![] };
    for f in &args[1..] {
        let p = Path::new(f);
        let fname = p.file_name().unwrap().to_string_lossy().to_string();
        let lang = if fname.starts_with("es_") { "es" } else { "en" };
        let audio = wav(p);
        let req = SttRequest { audio: &audio, language: Some(lang), hints: &hints };
        let t = Instant::now();
        let out = w.transcribe(&req);
        let cold = t.elapsed().as_millis();
        let text = match &out {
            Ok(o) => o.text.clone(),
            Err(e) => format!("ERR {e}"),
        };
        let mut warm: Vec<u128> = (0..WARM_RUNS)
            .map(|_| {
                let t = Instant::now();
                let _ = w.transcribe(&req);
                t.elapsed().as_millis()
            })
            .collect();
        warm.sort();
        println!("{name}\t{fname}\t{lang}\t{}\t{cold}\t{}\t{}\t{text}", audio.len() / 16, warm[WARM_RUNS / 2], warm[0]);
    }
}
