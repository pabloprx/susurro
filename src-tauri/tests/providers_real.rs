//! Real-provider checks (no mocks). Every test is #[ignore] and needs env; scripts/e2e/providers-real.sh runs them.
//!   whisper_wer:             SUSURRO_TEST_MODELS=<a.bin>:<b.bin>   (all fixtures in scripts/e2e/fixtures/expect.json)
//!   openai_compat_server:    SUSURRO_TEST_OAI_BASE=http://127.0.0.1:<port>/v1   (whisper-server)
//!   elevenlabs_real:         ELEVENLABS_API_KEY
//!   openrouter_real:         OPENROUTER_API_KEY
//!   local_llm_down:          none (talks to a closed port and to Ollama if it runs)
//! Cloud tests make few calls and use short fixtures. A recording proxy sits between the client and the real
//! server, so the tests check what was sent (prompt, keyterms) and what came back.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use susurro_lib::contract::{CleanupLevel, ElevenOpts, LlmProvider, Settings, Style};
use susurro_lib::dictionary::Hints;
use susurro_lib::stt::{SttRequest, Transcriber, elevenlabs::ElevenLabs, openai_compat::OpenAiCompat, whisper_local};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/e2e/fixtures")
}

fn wav(name: &str) -> Vec<f32> {
    susurro_lib::history::read_wav(&fixtures().join(name)).unwrap()
}

/// The hints the app builds from fixtures/dict.json (starred first, then file order).
fn hints() -> Hints {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(fixtures().join("dict.json")).unwrap()).unwrap();
    let mut words: Vec<&Value> = v.as_array().unwrap().iter().filter(|e| e["kind"] == "word").collect();
    words.sort_by_key(|e| !e["starred"].as_bool().unwrap_or(false));
    let s = |e: &Value| e["text"].as_str().unwrap().to_string();
    Hints {
        terms: words.iter().map(|e| s(e)).collect(),
        aliases: words.iter().flat_map(|e| e["aliases"].as_array().unwrap().iter().map(|a| (a.as_str().unwrap().to_string(), s(e)))).collect(),
        triggers: vec![],
    }
}

fn norm(s: &str) -> Vec<String> {
    let n: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    n.split_whitespace().map(str::to_string).collect()
}

/// Word error rate: word-level Levenshtein / reference length.
fn wer(reference: &str, hyp: &str) -> f64 {
    let (r, h) = (norm(reference), norm(hyp));
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for i in 1..=r.len() {
        let mut cur = vec![i; h.len() + 1];
        for j in 1..=h.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(r[i - 1] != h[j - 1]));
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

#[test]
fn wer_math() {
    assert_eq!(wer("a b c", "a b c"), 0.0);
    assert_eq!(wer("a b c d", "a x c"), 0.5);
    assert_eq!(wer("Hello, world!", "hello world"), 0.0);
    assert_eq!(wer("", ""), 0.0);
}

// ---------- recording proxy ----------

struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    status: u16,
}

impl Seen {
    fn body(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    /// Values of multipart text fields called `name`.
    fn fields(&self, name: &str) -> Vec<String> {
        let b = self.body();
        let tag = format!("name=\"{name}\"\r\n\r\n");
        b.match_indices(&tag).map(|(i, _)| b[i + tag.len()..].split("\r\n").next().unwrap_or("").to_string()).collect()
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// HTTP on 127.0.0.1 that records each request and forwards it to `upstream` (http or https).
/// Returns the base URL to give the client (same path prefix as upstream) and the record.
fn proxy(upstream: &str) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let up = url_origin(upstream);
    let prefix = upstream[up.len()..].to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(90)).build().unwrap();
        for s in l.incoming().flatten() {
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let mut it = line.split_whitespace();
            let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.trim_end().split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).map_or(0, |(_, v)| v.parse().unwrap());
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let mut rb = client.request(method.parse().unwrap(), format!("{up}{path}")).body(body.clone());
            for (k, v) in &headers {
                if !["host", "content-length", "connection"].contains(&k.to_lowercase().as_str()) {
                    rb = rb.header(k, v);
                }
            }
            let (status, out) = match rb.send() {
                Ok(resp) => (resp.status().as_u16(), resp.bytes().map(|b| b.to_vec()).unwrap_or_default()),
                Err(e) => (599, format!("proxy: {e}").into_bytes()),
            };
            rec.lock().unwrap().push(Seen { path, headers, body, status });
            let _ = write!(w, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            let _ = w.write_all(&out);
        }
    });
    (format!("http://127.0.0.1:{port}{prefix}"), seen)
}

fn url_origin(u: &str) -> String {
    let after = u.find("://").unwrap() + 3;
    match u[after..].find('/') {
        Some(i) => u[..after + i].to_string(),
        None => u.to_string(),
    }
}

// ---------- local whisper, in process ----------

#[test]
#[ignore]
fn whisper_wer() {
    let models = std::env::var("SUSURRO_TEST_MODELS").expect("set SUSURRO_TEST_MODELS");
    let expect: Value = serde_json::from_str(&std::fs::read_to_string(fixtures().join("expect.json")).unwrap()).unwrap();
    let h = hints();
    let mut fails = Vec::new();
    for m in models.split(':').filter(|m| !m.is_empty()) {
        let t = Instant::now();
        let ctx = Arc::new(whisper_local::load(m.as_ref()).unwrap());
        let multilingual = ctx.is_multilingual();
        let w = whisper_local::Whisper::new(ctx);
        let name = m.rsplit('/').next().unwrap();
        println!("\n== {name} (load {} ms, multilingual={multilingual})", t.elapsed().as_millis());
        println!("{:<17} {:<5} {:>6} {:>7} {:>7}  text", "file", "lang", "WER", "ms", "audio_s");
        let mut tot = (0.0, 0usize);
        for f in expect["files"].as_array().unwrap() {
            let (file, lang, text) = (f["file"].as_str().unwrap(), f["lang"].as_str().unwrap(), f["text"].as_str().unwrap());
            let audio = wav(file);
            let langs: Vec<Option<&str>> = match (lang, multilingual) {
                ("none", true) => vec![None],
                ("none", false) => vec![Some("en")],
                (l, true) => vec![Some(l), None],
                ("en", false) => vec![Some("en")],
                (_, false) => vec![], // English-only model refuses other languages (checked below)
            };
            if langs.is_empty() {
                let e = w.transcribe(&SttRequest { audio: &audio, language: Some(lang), hints: &h }).unwrap_err().to_string();
                assert!(e.contains("only English"), "{file}: {e}");
                println!("{file:<17} {lang:<5} refused (English-only model) - ok");
                continue;
            }
            for l in langs {
                let t = Instant::now();
                let r = w.transcribe(&SttRequest { audio: &audio, language: l, hints: &h });
                let ms = t.elapsed().as_millis();
                let out = match r {
                    Ok(o) => o,
                    Err(e) => {
                        fails.push(format!("{name} {file} {l:?}: {e:#}"));
                        continue;
                    }
                };
                let filtered = if out.text.is_empty() || susurro_lib::text::is_hallucination(&out.text) { "" } else { out.text.as_str() };
                let e = wer(text, filtered);
                if !text.is_empty() && l.is_some() && file != "short.wav" {
                    tot = (tot.0 + e, tot.1 + 1);
                }
                println!("{file:<17} {:<5} {e:>6.3} {ms:>7} {:>7.1}  {:?} (detected {:?})", l.unwrap_or("auto"), audio.len() as f32 / 16000.0, out.text, out.language);
                if text.is_empty() && !filtered.is_empty() {
                    fails.push(format!("{name} {file}: silence gave text {:?}", out.text));
                }
                // ggml-base wrote a trailing "you" over long.wav's 14 s of padding (2026-10-17).
                // `filtered`: a whole-text "Thank you." on silence is dropped by the pipeline's hallucination filter.
                if filtered.trim_end_matches(['.', '!']).to_lowercase().ends_with(" you") {
                    fails.push(format!("{name} {file}: trailing hallucination {:?}", out.text));
                }
            }
        }
        println!("mean WER ({} speech files, fixed language): {:.3}", tot.1, tot.0 / tot.1.max(1) as f64);
    }
    assert!(fails.is_empty(), "{fails:#?}");
}

// ---------- OpenAI-compatible server (whisper-server) ----------

#[test]
#[ignore]
fn openai_compat_server() {
    let base = std::env::var("SUSURRO_TEST_OAI_BASE").expect("set SUSURRO_TEST_OAI_BASE");
    let (pbase, seen) = proxy(&base);
    let c = OpenAiCompat { base: pbase, model: "whisper-1".into(), key: Some("local-key".into()), hotwords: true, label: "local STT server" };
    let h = hints();
    let audio = wav("en_custom.wav");
    let t = Instant::now();
    let out = c.transcribe(&SttRequest { audio: &audio, language: Some("en"), hints: &h }).unwrap();
    println!("whisper-server en_custom: {} ms {:?} WER {:.3}", t.elapsed().as_millis(), out.text, wer("I pushed the Contaflow fix to Supabase, then ran kubectl apply. Ping Pablo Pérez and Francisca Ugarte.", &out.text));
    let s = seen.lock().unwrap();
    let r = &s[0];
    assert_eq!(r.path, "/v1/audio/transcriptions");
    assert_eq!(r.header("authorization"), Some("Bearer local-key"));
    let prompt = r.fields("prompt");
    println!("prompt sent: {prompt:?}\nhotwords sent: {:?}", r.fields("hotwords"));
    assert!(prompt.len() == 1 && prompt[0].starts_with("Hi, today we talk about Contaflow") && prompt[0].contains("Francisca Ugarte"), "{prompt:?}");
    assert_eq!(r.fields("language"), ["en"]);
    assert!(!out.text.is_empty());
    assert!(norm(&out.text).contains(&"supabase".to_string()), "{}", out.text);
    drop(s);

    // Spanish, auto language: no language field, no seed in the prompt. whisper-server ignores the request's
    // language and uses its own -l (default en), so start it with -l auto for this check.
    let out = c.transcribe(&SttRequest { audio: &wav("es_accents.wav"), language: None, hints: &h }).unwrap();
    let s = seen.lock().unwrap();
    println!("whisper-server es_accents auto: {:?} lang={:?} WER {:.3}", out.text, out.language, wer("La reunión es mañana a las tres. ¿Puedes revisar el informe de facturación?", &out.text));
    assert!(s[1].fields("language").is_empty());
    assert!(s[1].fields("prompt")[0].starts_with("Contaflow"), "{:?}", s[1].fields("prompt"));
    drop(s);

    // Wrong path -> the server's 404 is a clear "rejected" error, not a panic.
    let bad = OpenAiCompat { base: format!("{}/nope", base.trim_end_matches("/v1")), model: "whisper-1".into(), key: None, hotwords: false, label: "local STT server" };
    let e = bad.transcribe(&SttRequest { audio: &audio, language: Some("en"), hints: &Hints::default() }).unwrap_err().to_string();
    println!("wrong path -> {e}");
    assert!(e.contains("local STT server"), "{e}");
}

/// A stopped STT server must say "Cannot connect", like the GET path does (stt::tests::http_errors_are_clear).
#[test]
#[ignore]
fn stt_server_down_message() {
    let audio = wav("en_snippet.wav");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // closed once dropped
    let down = OpenAiCompat { base: format!("http://127.0.0.1:{port}/v1"), model: "m".into(), key: None, hotwords: false, label: "local STT server" };
    let e = down.transcribe(&SttRequest { audio: &audio, language: Some("en"), hints: &Hints::default() }).unwrap_err().to_string();
    println!("closed port, multipart POST -> {e}");
    assert!(e.contains("Cannot connect to local STT server"), "{e}");
}

// ---------- ElevenLabs Scribe ----------

#[test]
#[ignore]
fn elevenlabs_real() {
    let key = std::env::var("ELEVENLABS_API_KEY").expect("set ELEVENLABS_API_KEY");
    let (base, seen) = proxy(susurro_lib::stt::ELEVENLABS_BASE);
    let h = hints();
    let opts = ElevenOpts { model_id: "scribe_v2".into(), send_keyterms: true, max_keyterms: 100 };
    let c = ElevenLabs { base: base.clone(), key: key.clone(), opts: opts.clone() };
    let text = "I pushed the Contaflow fix to Supabase, then ran kubectl apply. Ping Pablo Pérez and Francisca Ugarte.";
    let t = Instant::now();
    let out = c.transcribe(&SttRequest { audio: &wav("en_custom.wav"), language: Some("en"), hints: &h }).unwrap();
    println!("elevenlabs en_custom keyterms: {} ms {:?} lang={:?} WER {:.3}", t.elapsed().as_millis(), out.text, out.language, wer(text, &out.text));
    {
        let s = seen.lock().unwrap();
        assert_eq!(s[0].status, 200);
        assert_eq!(s[0].path, "/v1/speech-to-text");
        let kt = s[0].fields("keyterms");
        println!("keyterms sent: {kt:?}");
        assert_eq!(kt, h.terms);
        assert_eq!(s[0].fields("language_code"), ["en"]);
        assert_eq!(s[0].fields("file_format"), ["pcm_s16le_16"]);
    }
    for w in ["contaflow", "supabase", "kubectl"] {
        assert!(norm(&out.text).contains(&w.to_string()), "{w} missing: {}", out.text);
    }

    // Bad key -> clear message, and the proxy saw ElevenLabs answer 401.
    let bad = ElevenLabs { base, key: "sk_bad_key_for_test".into(), opts };
    let e = bad.transcribe(&SttRequest { audio: &wav("en_snippet.wav"), language: Some("en"), hints: &Hints::default() }).unwrap_err().to_string();
    println!("bad key -> {e}");
    assert!(e.contains("ElevenLabs: invalid or missing API key"), "{e}");
}

// ---------- OpenRouter: STT, model lists, cleanup ----------

#[test]
#[ignore]
fn openrouter_real() {
    let key = std::env::var("OPENROUTER_API_KEY").expect("set OPENROUTER_API_KEY");
    let stt_model = std::env::var("SUSURRO_TEST_OR_STT").unwrap_or("openai/whisper-1".into());
    let chat_model = std::env::var("SUSURRO_TEST_OR_CHAT").unwrap_or("openai/gpt-4o-mini".into());
    let (base, seen) = proxy(susurro_lib::stt::OPENROUTER_BASE);
    let h = hints();

    let c = OpenAiCompat { base: base.clone(), model: stt_model.clone(), key: Some(key.clone()), hotwords: false, label: "OpenRouter" };
    let t = Instant::now();
    let out = c.transcribe(&SttRequest { audio: &wav("en_custom.wav"), language: Some("en"), hints: &h }).unwrap();
    println!("openrouter {stt_model} en_custom: {} ms {:?} WER {:.3}", t.elapsed().as_millis(), out.text, wer("I pushed the Contaflow fix to Supabase, then ran kubectl apply. Ping Pablo Pérez and Francisca Ugarte.", &out.text));
    {
        let s = seen.lock().unwrap();
        assert_eq!(s[0].path, "/api/v1/audio/transcriptions");
        assert!(s[0].fields("prompt")[0].contains("Contaflow"));
        assert!(s[0].fields("hotwords").is_empty());
    }
    assert!(norm(&out.text).contains(&"supabase".to_string()), "{}", out.text);

    // Cleanup with a snippet placeholder: the LLM must keep ⟦1⟧ (pipeline::finish rejects the output otherwise).
    let mut s = Settings::default();
    s.cleanup.openrouter_model = chat_model.clone();
    s.styles.style = Style::Casual;
    let text = "um so here is ⟦1⟧ and uh I think we should like ship it today";
    let t = Instant::now();
    let out = susurro_lib::cleanup::cleanup(&s, Some(&key), &CleanupLevel::Light, text, &h).unwrap();
    println!("openrouter cleanup {chat_model}: {} ms {:?}", t.elapsed().as_millis(), out);
    assert!(out.contains("⟦1⟧"), "placeholder lost: {out}");
    assert!(!norm(&out).contains(&"um".to_string()) && !norm(&out).contains(&"uh".to_string()), "{out}");
    assert!(!out.contains('\u{2014}'), "em dash: {out}");

    // The Local provider path pointed at an OpenAI-compatible server (OpenRouter here: no local LLM runs).
    let mut l = Settings::default();
    l.cleanup.provider = LlmProvider::Local;
    l.cleanup.local_base_url = susurro_lib::stt::OPENROUTER_BASE.into();
    l.cleanup.local_model = chat_model.clone();
    let out = susurro_lib::cleanup::cleanup(&l, Some(&key), &CleanupLevel::Light, "Pablo Perez said, uh, conta flow is ready", &h).unwrap();
    println!("local-provider path via OpenRouter: {out:?}");
    assert!(out.contains("Contaflow"), "{out}");

    // Bad keys -> clear messages.
    let e = susurro_lib::cleanup::cleanup(&s, Some("sk-or-bad"), &CleanupLevel::Light, "hi there", &h).unwrap_err().to_string();
    println!("cleanup bad key -> {e}");
    assert!(e.contains("OpenRouter: invalid or missing API key"), "{e}");
    let bad = OpenAiCompat { base, model: stt_model, key: Some("sk-or-bad".into()), hotwords: false, label: "OpenRouter" };
    let e = bad.transcribe(&SttRequest { audio: &wav("en_snippet.wav"), language: Some("en"), hints: &Hints::default() }).unwrap_err().to_string();
    println!("stt bad key -> {e}");
    assert!(e.contains("OpenRouter: invalid or missing API key"), "{e}");
}

// ---------- local LLM: nothing loaded ----------

#[test]
#[ignore]
fn local_llm_down() {
    let mut s = Settings::default();
    s.cleanup.provider = LlmProvider::Local;
    s.cleanup.local_model = "qwen3-4b".into();
    s.cleanup.local_base_url = "http://127.0.0.1:9/v1".into();
    let e = susurro_lib::cleanup::cleanup(&s, None, &CleanupLevel::Light, "hi there", &Hints::default()).unwrap_err().to_string();
    println!("closed port -> {e}");
    assert!(e.contains("Cannot connect to local LLM"), "{e}");
    // Ollama on 11434 with no such model: a clear "rejected" message with the server's reason.
    if std::net::TcpStream::connect("127.0.0.1:11434").is_ok() {
        s.cleanup.local_base_url = "http://127.0.0.1:11434/v1".into();
        s.cleanup.local_model = "susurro-no-such-model".into();
        let e = susurro_lib::cleanup::cleanup(&s, None, &CleanupLevel::Light, "hi there", &Hints::default()).unwrap_err().to_string();
        println!("ollama, missing model -> {e}");
        assert!(e.contains("local LLM") && e.contains("404"), "{e}");
    }
}
