//! Adversarial review of the wispr-parity diff (2026-10-17). Each test states a bug; it fails while the bug exists.

use susurro_lib::contract::Hotkeys;
use susurro_lib::hotkey;
use susurro_lib::stt::{Transcriber, openrouter};

/// Rust is the trust boundary: validate() must reject a command key that equals push to talk.
#[test]
fn validate_rejects_command_key_equal_to_ptt() {
    let h = Hotkeys { ptt: "F2".into(), command: "F2".into(), ..Hotkeys::default() };
    assert!(hotkey::validate(&h).is_err(), "command == ptt is accepted: both actions fire on F2");
}

/// Same, but the command key is a modifier alone (contract: "never modifier-only").
#[test]
fn validate_rejects_modifier_only_command_key() {
    let h = Hotkeys { command: "RightOption".into(), ..Hotkeys::default() };
    assert!(hotkey::validate(&h).is_err(), "a modifier-only command key is accepted");
}

/// A v2 user who already holds F2 for push to talk gets command = F2 from serde defaults on migration.
#[test]
fn migration_does_not_give_f2_to_a_user_whose_ptt_is_f2() {
    let dir = std::env::temp_dir().join(format!("susurro-adv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("settings.json");
    std::fs::write(&p, r#"{"version":2,"hotkeys":{"ptt":"F2"}}"#).unwrap();
    let s = susurro_lib::settings::load(&p);
    assert_ne!(s.hotkeys.command, s.hotkeys.ptt, "migration created a hotkey clash");
}

/// OpenRouter: base64 over 25 MB is accepted only for OpenAI and Groq models (.lo/openrouter-stt.md:20).
/// max_seconds can be 600 s: such a clip must go as multipart (19.2 MB), not as a 25.6 MB base64 JSON body.
#[test]
fn max_length_clip_body_fits_openrouter_base64_limit() {
    let wav_len = |secs: usize| 44 + secs * 16000 * 2;
    assert!(openrouter::json_fits(wav_len(580)), "580 s still fits the JSON body (keeps the phrase list)");
    assert!(!openrouter::json_fits(wav_len(600)), "600 s is 25.6 MB of base64: over the limit for MAI");

    let mut srv = mockito::Server::new();
    let m = srv
        .mock("POST", "/api/v1/audio/transcriptions")
        .match_header("content-type", mockito::Matcher::Regex("^multipart/form-data".into()))
        .with_body(r#"{"text":"ok"}"#)
        .create();
    let c = openrouter::OpenRouter { base: format!("{}/api/v1", srv.url()), key: "k".into(), model: "microsoft/mai-transcribe-2".into(), clean: true };
    let audio = vec![0.0f32; 600 * 16000];
    let hints = susurro_lib::dictionary::Hints::default();
    let out = c.transcribe(&susurro_lib::stt::SttRequest { audio: &audio, language: None, hints: &hints }).unwrap();
    m.assert();
    assert_eq!(out.text, "ok");
}
