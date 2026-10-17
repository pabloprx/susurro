//! WP2 smoke test: jfk.wav through the real whisper_local Transcriber (metal).
//!   SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-base.en-q5_1.bin cargo test --release --test whisper_smoke -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, whisper_local};

#[test]
#[ignore]
fn jfk() {
    let model = std::env::var("SUSURRO_TEST_MODEL").expect("set SUSURRO_TEST_MODEL").replacen('~', &std::env::var("HOME").unwrap(), 1);
    let mut r = hound::WavReader::open("/opt/homebrew/share/whisper.cpp/jfk.wav").unwrap();
    let audio: Vec<f32> = r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();

    let t = Instant::now();
    let w = whisper_local::Whisper::new(Arc::new(whisper_local::load(model.as_ref()).unwrap()));
    println!("model load: {} ms", t.elapsed().as_millis());

    // Run 2 adds dictionary terms, so the initial prompt + real tokenizer path runs too.
    let terms = Hints { terms: vec!["Contaflow".into(), "Tauri".into()], ..Default::default() };
    for (run, hints) in [(1, Hints::default()), (2, terms)] {
        let t = Instant::now();
        let out = w.transcribe(&SttRequest { audio: &audio, language: None, hints: &hints }).unwrap();
        println!("transcribe #{run}: {} ms ({:.1} s clip) lang={:?} {:?}", t.elapsed().as_millis(), audio.len() as f32 / 16000.0, out.language, out.text);
        let norm: String = out.text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
        let norm = norm.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(norm.contains("ask not what your country can do for you"), "{norm}");
    }
}
