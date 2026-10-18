//! next-queue 5: release -> final text, per STT path, on the e2e fixtures streamed at real-time pace (100 ms of
//! audio every 100 ms, as the mic delivers it). Median of 3. #[ignore]: real model, real money.
//!   SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-large-v3-turbo-q5_0.bin SUSURRO_WHISPER_THREADS=6 \
//!   OPENROUTER_API_KEY=.. ELEVENLABS_API_KEY=.. OPENAI_API_KEY=.. cargo test --release --test latency_bench -- --ignored --nocapture --test-threads=1
//! Any env missing = that row is skipped. Audio sent: ~16 s per run per cloud path (3 runs: ~47 s per path).
//! scribe_rt_probe: one short clip and the silence clip through Scribe Realtime (~5 s of audio).

use std::time::{Duration, Instant};
use susurro_lib::audio::Chunker;
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, elevenlabs::ElevenLabs, openrouter::OpenRouter, scribe_rt, whisper_local};

const CLIPS: [(&str, &str); 3] = [("en_snippet.wav", "en"), ("es_accents.wav", "es"), ("en_numbers.wav", "en")];

fn wav(name: &str) -> Vec<f32> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/e2e/fixtures").join(name);
    susurro_lib::history::read_wav(&p).unwrap()
}

fn median(mut v: Vec<u128>) -> u128 {
    v.sort();
    v[v.len() / 2]
}

/// Plays `audio` at real-time pace into `feed` (100 ms pieces), then returns the release instant.
fn play(audio: &[f32], mut feed: impl FnMut(&[f32])) -> Instant {
    let start = Instant::now();
    for (i, piece) in audio.chunks(1600).enumerate() {
        feed(piece);
        let due = start + Duration::from_millis(100 * (i as u64 + 1));
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    Instant::now()
}

fn row(name: &str, runs: &mut dyn FnMut(&[f32], &str) -> anyhow::Result<(u128, String)>) {
    for (clip, lang) in CLIPS {
        let audio = wav(clip);
        let mut ms = vec![];
        let mut text = String::new();
        for _ in 0..3 {
            match runs(&audio, lang) {
                Ok((t, s)) => {
                    ms.push(t);
                    text = s;
                }
                Err(e) => {
                    println!("{name} {clip}: FAILED {e:#}");
                    break;
                }
            }
        }
        if ms.len() == 3 {
            println!("| {name} | {clip} ({:.1} s) | {} | {ms:?} | {text:?} |", audio.len() as f64 / 16000.0, median(ms.clone()));
        }
    }
}

#[test]
#[ignore]
fn latency_bench() {
    let h = Hints::default();
    println!("| path | clip | median ms | runs ms | text |");
    if let Ok(model) = std::env::var("SUSURRO_TEST_MODEL") {
        let ctx = std::sync::Arc::new(whisper_local::load(std::path::Path::new(&model)).unwrap());
        // warm-up: the first Metal run compiles shaders
        whisper_local::Whisper::new(ctx.clone()).transcribe(&SttRequest { audio: &wav("en_snippet.wav"), language: Some("en"), hints: &h }).unwrap();
        row("local turbo streaming", &mut |audio, lang| {
            let st = whisper_local::Stream::spawn(whisper_local::Whisper::new(ctx.clone()), Some(lang.into()), Hints::default());
            let mut ch = Chunker::new(16000);
            let released = play(audio, |p| {
                if let Some(c) = ch.push(p) {
                    st.push(c);
                }
            });
            let clip = susurro_lib::audio::Clip { samples: audio.to_vec(), duration_ms: 0, peak_db: 0.0, device_names: vec![] };
            let o = st.finish(ch.tail(&clip))?;
            Ok((released.elapsed().as_millis(), o.text))
        });
    }
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        let mai = OpenRouter { base: susurro_lib::stt::OPENROUTER_BASE.into(), key, model: "microsoft/mai-transcribe-2".into(), clean: true };
        row("MAI batch", &mut |audio, lang| {
            let released = play(audio, |_| {});
            let o = mai.transcribe(&SttRequest { audio, language: Some(lang), hints: &Hints::default() })?;
            Ok((released.elapsed().as_millis(), o.text))
        });
    }
    if let Ok(key) = std::env::var("ELEVENLABS_API_KEY") {
        let el = ElevenLabs { base: susurro_lib::stt::ELEVENLABS_BASE.into(), key: key.clone(), opts: Default::default() };
        row("Scribe v2 batch", &mut |audio, lang| {
            let released = play(audio, |_| {});
            let o = el.transcribe(&SttRequest { audio, language: Some(lang), hints: &Hints::default() })?;
            Ok((released.elapsed().as_millis(), o.text))
        });
        row("Scribe v2 Realtime", &mut |audio, lang| {
            let o = scribe_rt::Opts { proto: scribe_rt::Proto::Scribe, url: scribe_rt::URL.into(), key: key.clone(), rate: 16000, language: Some(lang.into()), keyterms: vec![] };
            let (text, after) = scribe_rt::transcribe(o, audio, true, Duration::from_secs(30))?;
            Ok((after.as_millis(), text))
        });
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        row("OpenAI Realtime (gpt-live-transcribe)", &mut |audio, lang| {
            let o = scribe_rt::Opts { proto: scribe_rt::Proto::OpenAi, url: scribe_rt::OPENAI_URL.into(), key: key.clone(), rate: 16000, language: Some(lang.into()), keyterms: vec![] };
            let (text, after) = scribe_rt::transcribe(o, audio, true, Duration::from_secs(30))?;
            Ok((after.as_millis(), text))
        });
    }
}

/// Voice control: the COMMAND cuts (300 ms pause) on two clips at real-time pace. Per cut: cut -> its text.
/// Scribe Realtime commits at the cut; MAI gets each cut chunk as one request (the batch path of live.rs).
#[test]
#[ignore]
fn command_chunks() {
    use susurro_lib::audio::COMMAND;
    for clip in ["en_custom.wav", "en_numbers.wav"] {
        let audio = wav(clip);
        if let Ok(key) = std::env::var("ELEVENLABS_API_KEY") {
            let (tx, rx) = std::sync::mpsc::channel();
            let o = scribe_rt::Opts { proto: scribe_rt::Proto::Scribe, url: scribe_rt::URL.into(), key, rate: 16000, language: Some("en".into()), keyterms: vec![] };
            let live = scribe_rt::Live::start(o, move |e| drop(tx.send((Instant::now(), e))));
            let mut ch = Chunker::with_cuts(16000, COMMAND);
            let mut cuts = vec![];
            play(&audio, |p| {
                let cut = ch.push(p).is_some();
                live.send(p, cut);
                if cut {
                    cuts.push(Instant::now());
                }
            });
            cuts.push(Instant::now());
            live.finish();
            let mut got = vec![];
            while let Ok((at, e)) = rx.recv_timeout(Duration::from_secs(10)) {
                match e {
                    scribe_rt::Ev::Committed(t) => got.push((at, t)),
                    scribe_rt::Ev::Closed | scribe_rt::Ev::Failed(_) => break,
                    _ => {}
                }
            }
            let ms: Vec<u128> = got.iter().zip(&cuts).map(|((at, _), c)| at.duration_since(*c).as_millis()).collect();
            println!("| Scribe Realtime per cut | {clip} | {ms:?} | {:?} |", got.iter().map(|g| g.1.as_str()).collect::<Vec<_>>());
        }
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            let mai = OpenRouter { base: susurro_lib::stt::OPENROUTER_BASE.into(), key, model: "microsoft/mai-transcribe-2".into(), clean: true };
            let mut ch = Chunker::with_cuts(16000, COMMAND);
            let mut chunks = vec![];
            play(&audio, |p| chunks.extend(ch.push(p)));
            let clip_ = susurro_lib::audio::Clip { samples: audio.clone(), duration_ms: 0, peak_db: 0.0, device_names: vec![] };
            chunks.extend(ch.tail(&clip_));
            let mut out = vec![];
            for c in &chunks {
                let t = Instant::now();
                let r = mai.transcribe(&SttRequest { audio: c, language: Some("en"), hints: &Hints::default() });
                out.push((t.elapsed().as_millis(), r.map(|o| o.text).unwrap_or_else(|e| format!("ERR {e:#}"))));
            }
            println!("| MAI per chunk | {clip} | {:?} | {:?} |", out.iter().map(|o| o.0).collect::<Vec<_>>(), out.iter().map(|o| o.1.as_str()).collect::<Vec<_>>());
        }
    }
}

/// Commit latency for a short command and what a silent commit answers (the finish must not hang on it).
#[test]
#[ignore]
fn scribe_rt_probe() {
    let key = std::env::var("ELEVENLABS_API_KEY").expect("set ELEVENLABS_API_KEY");
    for clip in ["en_snippet.wav", "silence.wav"] {
        let o = scribe_rt::Opts { proto: scribe_rt::Proto::Scribe, url: scribe_rt::URL.into(), key: key.clone(), rate: 16000, language: None, keyterms: vec!["Contaflow".into()] };
        let r = scribe_rt::transcribe(o, &wav(clip), true, Duration::from_secs(15));
        println!("{clip}: {r:?}");
        assert!(r.is_ok(), "{r:?}");
    }
}
