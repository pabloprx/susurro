//! Adversarial whisper checks: both models, empty/short/silent audio, Spanish.
//!   SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-base.bin cargo test --release --test whisper_edge -- --ignored --nocapture --test-threads=1
//! Optional: SUSURRO_TEST_ES_WAV=<16 kHz mono s16 wav of Spanish speech>.

use std::sync::Arc;
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, whisper_local};

fn model() -> whisper_local::Whisper {
    let m = std::env::var("SUSURRO_TEST_MODEL").expect("set SUSURRO_TEST_MODEL").replacen('~', &std::env::var("HOME").unwrap(), 1);
    whisper_local::Whisper::new(Arc::new(whisper_local::load(m.as_ref()).unwrap()))
}

fn wav(path: &str) -> Vec<f32> {
    susurro_lib::history::read_wav(path.as_ref()).unwrap()
}

fn norm(s: &str) -> String {
    let n: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
#[ignore]
fn edges() {
    let w = model();
    let h = Hints::default();
    let jfk = susurro_lib::stt::jfk();
    // English-only models refuse any language but "en" (they would invent English from other speech).
    let multilingual = !std::env::var("SUSURRO_TEST_MODEL").unwrap().contains(".en");
    let langs: &[Option<&str>] = if multilingual { &[None, Some("auto"), Some("en")] } else { &[Some("en")] };
    if !multilingual {
        for lang in [None, Some("auto"), Some("es")] {
            assert!(w.transcribe(&SttRequest { audio: &jfk, language: lang, hints: &h }).is_err(), "{lang:?}");
        }
    }
    let any = langs[0];
    for &lang in langs {
        let o = w.transcribe(&SttRequest { audio: &jfk, language: lang, hints: &h }).unwrap();
        println!("jfk lang={lang:?} -> {:?} {:?}", o.language, o.text);
        assert!(norm(&o.text).contains("ask not what your country can do for you"), "{lang:?}: {}", o.text);
    }
    // Empty, tiny, silent and near-silent audio must not error and must not invent words
    // the pipeline does not filter.
    let noise: Vec<f32> = (0..32000u32).map(|i| ((i.wrapping_mul(2654435761) >> 16) as f32 / 65536.0 - 0.5) * 0.002).collect();
    // Empty audio is an error (pipeline::run never sends it: Clip::too_short gates it first).
    assert!(w.transcribe(&SttRequest { audio: &[], language: any, hints: &h }).is_err());
    for (name, a) in [("100ms", vec![0.0; 1600]), ("1s silence", vec![0.0; 16000]), ("2s noise", noise)] {
        let r = w.transcribe(&SttRequest { audio: &a, language: any, hints: &h });
        println!("{name} -> {r:?}");
        let o = r.unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert!(o.text.is_empty() || susurro_lib::text::is_hallucination(&o.text), "{name}: {:?}", o.text);
    }
    if let Some(p) = std::env::var("SUSURRO_TEST_ES_WAV").ok().filter(|_| multilingual) {
        let es = wav(&p);
        let terms = Hints { terms: vec!["Contaflow".into(), "Pablo".into()], ..Default::default() };
        for lang in [None, Some("es")] {
            let o = w.transcribe(&SttRequest { audio: &es, language: lang, hints: &terms }).unwrap();
            println!("es lang={lang:?} -> {:?} {:?}", o.language, o.text);
            assert!(norm(&o.text).contains("reunion") || norm(&o.text).contains("reunión"), "{}", o.text);
        }
    }
}
