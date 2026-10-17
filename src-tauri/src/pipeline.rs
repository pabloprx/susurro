//! WP5: the pure pipeline from PLAN section 0: gate -> STT -> hallucination filter -> prepare -> cleanup -> finish.
//! No Tauri, no paste, no DB: the session actor and history_retry wrap it. Tested with fakes.
//! Rule: a dictation is never lost. Each failed step falls back to the previous text.

use crate::audio::Clip;
use crate::contract::Style;
use crate::dictionary::{Dictionary, Hints};
use crate::stt::{SttOutput, SttRequest};
use crate::text;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// No history row, no provider call. Some = a message for the user (silence gate, nothing heard).
    Skip(Option<String>),
    /// STT failed: the caller writes a `failed` row and keeps the WAV.
    Failed(String),
    Text(Done),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Done {
    /// STT text (after the hallucination filter and the press-enter strip).
    pub raw: String,
    pub final_text: String,
    /// The text ended with "press enter": press Return after the paste.
    pub press_enter: bool,
    /// Matched dictionary entry ids, for `record_uses`.
    pub used: Vec<i64>,
    /// The LLM output was used.
    pub cleaned: bool,
    /// Small notes for the toast (local fallback used, cleanup skipped). UI text only.
    pub notes: Vec<String>,
    /// Local whisper produced the text after the cloud STT failed.
    pub fell_back: bool,
}

pub struct Opts<'a> {
    pub language: Option<&'a str>,
    pub style: Style,
    /// Raw app: no LLM, no style rules, corrections kept.
    pub raw_app: bool,
    pub press_enter_command: bool,
    /// Drop ¿ ¡ (Styles.strip_inverted, unless the app is in Styles.inverted_apps).
    pub strip_inverted: bool,
    /// Some = the app takes slash commands at the start of a dictation (terminals, Claude): the known names.
    pub slash: Option<&'a [String]>,
}

pub type Stt<'a> = &'a dyn Fn(&SttRequest) -> anyhow::Result<SttOutput>;
/// (prepared text, hints) -> cleaned text. None = cleanup is off or not configured.
pub type Cleaner<'a> = Option<&'a dyn Fn(&str, &Hints) -> anyhow::Result<String>>;

pub fn run(clip: &Clip, o: &Opts, d: &Dictionary, stt: Stt, clean: Cleaner) -> Outcome {
    if clip.too_short() {
        return Outcome::Skip(None);
    }
    if clip.silent() {
        let name = clip.device_names.last().map(String::as_str).unwrap_or("the mic");
        return Outcome::Skip(Some(format!("No audio from {name}. Is your mic muted?")));
    }
    let hints = d.hints();
    let out = match stt(&SttRequest { audio: &clip.samples, language: o.language, hints: &hints }) {
        Ok(o) => o,
        Err(e) => return Outcome::Failed(format!("{e:#}")),
    };
    let mut raw = text::tidy_stt(&out.text);
    if raw.is_empty() || text::is_hallucination(&raw) {
        return Outcome::Skip(Some("Heard nothing".into()));
    }
    let mut done = Done { notes: out.note.into_iter().collect(), fell_back: out.fell_back, ..Default::default() };
    if o.press_enter_command {
        if let Some(t) = text::strip_press_enter(&raw) {
            (raw, done.press_enter) = (t, true);
        }
    }
    // "slash compact keep the tests" -> "/compact" + the rest, which is cleaned like any dictation.
    let (cmd, mut body) = match o.slash.and_then(|names| crate::slash::split(&raw, names)) {
        Some((c, r)) => (Some(c), r),
        None => (None, raw.clone()),
    };
    if o.strip_inverted {
        body = text::strip_inverted(&body);
    }
    if body.trim().is_empty() {
        done.final_text = String::new();
    } else if o.raw_app {
        let a = d.apply(&body);
        done.used = a.matches.iter().map(|m| m.entry_id).collect();
        done.final_text = a.text;
    } else {
        let p = d.prepare(&body);
        // Backtrack and style rules run on the placeholder text: a snippet expansion is pasted as typed.
        let style = |t: &str| {
            let t = text::style_rules(t, o.style.clone(), &hints.terms);
            if o.strip_inverted { text::strip_inverted(&t) } else { t }
        };
        let no_llm = || Dictionary::expand(&style(&text::backtrack_rules(&p.text)), &p.slots).map_or_else(|| d.apply(&body).text, |(t, _)| t);
        let llm = if p.standalone { None } else { clean.map(|c| c(&p.text, &hints)) };
        done.used = p.used.clone();
        match llm {
            _ if p.standalone => done.final_text = p.slots.first().cloned().unwrap_or_default(),
            Some(Ok(t)) => match d.finish(&style(&t), &p) {
                Some(f) => {
                    done.final_text = f;
                    done.cleaned = true;
                }
                None => {
                    done.final_text = no_llm();
                    done.notes.push("Cleanup lost a snippet - used the plain text".into());
                }
            },
            Some(Err(e)) => {
                log::warn!("cleanup failed: {e:#}");
                done.final_text = no_llm();
                done.notes.push(format!("Cleanup skipped: {e:#}"));
            }
            None => done.final_text = no_llm(),
        }
    }
    if let Some(c) = cmd {
        done.final_text = crate::slash::join(&c, &done.final_text);
    }
    if done.final_text.trim().is_empty() && !done.press_enter {
        return Outcome::Skip(Some("Heard nothing".into()));
    }
    done.raw = raw;
    Outcome::Text(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{DictEntry, DictKind, DictOpts};
    use std::cell::Cell;

    fn clip(peak: f32, ms: u32) -> Clip {
        Clip {
            samples: vec![peak; (ms * 16) as usize],
            duration_ms: ms,
            peak_db: crate::audio::db(peak),
            device_names: vec!["USB Mic".into()],
        }
    }

    fn dict() -> Dictionary {
        let e = |id, kind, text: &str, aliases: &[&str], exp: Option<&str>| DictEntry {
            id,
            kind,
            text: text.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            expansion: exp.map(String::from),
            exact_only: false,
            starred: false,
            uses: 0,
            created_at: id,
        };
        Dictionary::new(
            vec![
                e(1, DictKind::Word, "Contaflow", &["conta flow"], None),
                e(2, DictKind::Snippet, "my calendly", &[], Some("https://calendly.com/pablo")),
            ],
            DictOpts::default(),
        )
    }

    fn opts() -> Opts<'static> {
        Opts { language: None, style: Style::Formal, raw_app: false, press_enter_command: true, strip_inverted: true, slash: None }
    }

    fn says(t: &'static str) -> impl Fn(&SttRequest) -> anyhow::Result<SttOutput> {
        move |_| Ok(SttOutput { text: t.into(), language: Some("en".into()), note: None, fell_back: false })
    }

    fn text_of(o: Outcome) -> Done {
        match o {
            Outcome::Text(d) => d,
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn silence_gate_and_short_clip_skip_the_stt() {
        let called = Cell::new(false);
        let stt = |_: &SttRequest| -> anyhow::Result<SttOutput> {
            called.set(true);
            anyhow::bail!("must not run")
        };
        assert_eq!(run(&clip(0.0001, 2000), &opts(), &dict(), &stt, None), Outcome::Skip(Some("No audio from USB Mic. Is your mic muted?".into())));
        assert_eq!(run(&clip(0.5, 100), &opts(), &dict(), &stt, None), Outcome::Skip(None));
        assert!(!called.get());
    }

    #[test]
    fn stt_failure_is_failed() {
        let stt = |_: &SttRequest| -> anyhow::Result<SttOutput> { anyhow::bail!("401 bad key") };
        assert_eq!(run(&clip(0.5, 1000), &opts(), &dict(), &stt, None), Outcome::Failed("401 bad key".into()));
    }

    #[test]
    fn hallucination_is_dropped() {
        assert_eq!(run(&clip(0.5, 1000), &opts(), &dict(), &says("Thank you."), None), Outcome::Skip(Some("Heard nothing".into())));
    }

    #[test]
    fn no_llm_path_applies_dictionary() {
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("I work at conta flow."), None));
        assert_eq!(d.final_text, "I work at Contaflow.");
        assert_eq!(d.used, vec![1]);
        assert!(!d.cleaned);
    }

    #[test]
    fn llm_output_is_finished() {
        let clean = |t: &str, _: &Hints| -> anyhow::Result<String> {
            assert!(t.contains("⟦1⟧"), "snippet goes to the LLM as a placeholder: {t}");
            Ok(t.replacen("Um, book", "Book", 1))
        };
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("Um, book me at my calendly."), Some(&clean)));
        assert_eq!(d.final_text, "Book me at https://calendly.com/pablo.");
        assert!(d.cleaned);
    }

    #[test]
    fn llm_error_falls_back_to_prepared_text() {
        let clean = |_: &str, _: &Hints| -> anyhow::Result<String> { anyhow::bail!("timeout") };
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("I work at conta flow."), Some(&clean)));
        assert_eq!(d.final_text, "I work at Contaflow.");
        assert!(!d.cleaned);
        assert_eq!(d.notes, vec!["Cleanup skipped: timeout".to_string()]);
    }

    #[test]
    fn lost_placeholder_uses_no_llm_path() {
        let clean = |_: &str, _: &Hints| -> anyhow::Result<String> { Ok("Book me at your link.".into()) };
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("Book me at my calendly."), Some(&clean)));
        assert_eq!(d.final_text, "Book me at https://calendly.com/pablo.");
        assert!(!d.cleaned);
    }

    #[test]
    fn raw_app_skips_llm_and_style() {
        let clean = |_: &str, _: &Hints| -> anyhow::Result<String> { panic!("raw apps never reach the LLM") };
        let o = Opts { raw_app: true, style: Style::Casual, ..opts() };
        let d = text_of(run(&clip(0.5, 1000), &o, &dict(), &says("git commit conta flow."), Some(&clean)));
        assert_eq!(d.final_text, "git commit Contaflow.", "corrections kept, casual period rule not applied");
    }

    #[test]
    fn standalone_snippet_skips_llm() {
        let clean = |_: &str, _: &Hints| -> anyhow::Result<String> { panic!("standalone snippet skips the LLM") };
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("My Calendly."), Some(&clean)));
        assert_eq!(d.final_text, "https://calendly.com/pablo");
    }

    #[test]
    fn press_enter_is_detected() {
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &says("Run the tests, press enter."), None));
        assert!(d.press_enter);
        assert_eq!(d.final_text, "Run the tests");
        let o = Opts { press_enter_command: false, ..opts() };
        assert!(!text_of(run(&clip(0.5, 1000), &o, &dict(), &says("Run the tests, press enter."), None)).press_enter);
    }

    #[test]
    fn local_fallback_note_reaches_the_toast() {
        let stt = |_: &SttRequest| Ok(SttOutput { text: "Hello there.".into(), language: None, note: Some("Used local whisper: 503".into()), fell_back: true });
        let d = text_of(run(&clip(0.5, 1000), &opts(), &dict(), &stt, None));
        assert_eq!(d.notes, vec!["Used local whisper: 503".to_string()]);
        assert!(d.fell_back, "the History label reads the flag, not the note text");
    }

    // ---------- next-queue 2, 3, 6, 7, 8: one table, EN/ES ----------

    fn seeded() -> Dictionary {
        let es = crate::dictionary::SEEDS
            .iter()
            .enumerate()
            .map(|(i, (t, a))| DictEntry { id: i as i64 + 1, kind: DictKind::Word, text: t.to_string(), aliases: a.iter().map(|x| x.to_string()).collect(), expansion: None, exact_only: false, starred: false, uses: 0, created_at: i as i64 })
            .collect();
        Dictionary::new(es, DictOpts::default())
    }

    #[test]
    fn text_rules_table() {
        let names: Vec<String> = ["compact", "review", "btw", "ponytail-review", "ponytail", "lights-out"].map(String::from).to_vec();
        let term = Some(names.as_slice());
        // (STT text, style, slash names, press enter, final text)
        let cases: &[(&str, Style, Option<&[String]>, bool, &str)] = &[
            // 3: press enter at the end only
            ("Run the tests, presenter.", Style::Formal, None, true, "Run the tests"),
            ("The TV presenter said hello.", Style::Formal, None, false, "The TV presenter said hello."),
            ("Nos vemos, dale enter.", Style::Formal, None, true, "Nos vemos"),
            // 6: ¿ ¡ dropped
            ("¿cómo estás?", Style::Formal, None, false, "cómo estás?"),
            ("Hola, ¡qué bueno verte!", Style::Formal, None, false, "Hola, qué bueno verte!"),
            // 7: slang and its spoken forms
            ("Jajaja equis de", Style::Formal, None, false, "jajaja xd"),
            ("Me salió seis siete", Style::Formal, None, false, "Me salió 67"),
            ("That is six seven lol", Style::Formal, None, false, "That is 67 lol"),
            ("Cachai bro, w t f", Style::Formal, None, false, "cachai bro, wtf"),
            // 2: lowercase keeps periods, commas and dictionary casing
            ("Hola. Le escribí a OpenRouter, a Jev y a Contaflow.", Style::Lowercase, None, false, "hola. le escribí a OpenRouter, a Jev y a Contaflow."),
            ("I use Ghostty. It is fast.", Style::Lowercase, None, false, "i use Ghostty. it is fast."),
            // 8: slash commands at the start, in a terminal only
            ("By the way, what does this do?", Style::Formal, term, false, "/btw what does this do?"),
            ("I said by the way that it works.", Style::Formal, term, false, "I said by the way that it works."),
            ("By the way, what does this do?", Style::Formal, None, false, "By the way, what does this do?"),
            ("Por cierto, ¿esto compila?", Style::Formal, term, false, "/btw esto compila?"),
            ("Slash ponytail review", Style::Formal, term, false, "/ponytail-review"),
            ("slash compact, keep the tests. press enter", Style::Formal, term, true, "/compact keep the tests."),
            ("Slash deploy staging", Style::Formal, term, false, "/deploy-staging"),
            ("Slash review the diff.", Style::Lowercase, term, false, "/review the diff."),
        ];
        for (input, style, slash, enter, want) in cases {
            let o = Opts { style: style.clone(), slash: *slash, ..opts() };
            let d = text_of(run(&clip(0.5, 1000), &o, &seeded(), &says(input), None));
            assert_eq!((d.final_text.as_str(), d.press_enter), (*want, *enter), "input: {input:?}");
        }
        // inverted marks kept when asked (an exception app)
        let o = Opts { strip_inverted: false, ..opts() };
        assert_eq!(text_of(run(&clip(0.5, 1000), &o, &seeded(), &says("¿cómo estás?"), None)).final_text, "¿cómo estás?");
        // the LLM sees only the words after the command, and its ¿ is dropped
        let clean = |t: &str, _: &Hints| -> anyhow::Result<String> {
            assert_eq!(t, "what changed?");
            Ok("¿What changed?".into())
        };
        let o = Opts { slash: term, ..opts() };
        let d = text_of(run(&clip(0.5, 1000), &o, &seeded(), &says("By the way what changed?"), Some(&clean)));
        assert_eq!(d.final_text, "/btw What changed?");
    }

    // ---------- adversarial (2026-10-17 review) ----------

    #[test]
    fn style_rules_do_not_rewrite_a_snippet_expansion() {
        let e = |id, kind, text: &str, exp: Option<&str>| DictEntry { id, kind, text: text.into(), aliases: vec![], expansion: exp.map(String::from), exact_only: false, starred: false, uses: 0, created_at: id };
        let d = Dictionary::new(vec![e(1, DictKind::Snippet, "mi firma", Some("Saludos,\nPablo."))], DictOpts::default());
        let echo = |t: &str, _: &Hints| -> anyhow::Result<String> { Ok(t.to_string()) };
        let o = Opts { style: Style::VeryCasual, ..opts() };
        for clean in [None, Some(&echo as &dyn Fn(&str, &Hints) -> anyhow::Result<String>)] {
            let got = text_of(run(&clip(0.5, 1000), &o, &d, &says("mi firma gracias"), clean)).final_text;
            assert!(got.starts_with("Saludos,\nPablo."), "{got:?}");
        }
    }

    #[test]
    fn stt_error_text_reaches_failed_row() {
        let stt = |_: &SttRequest| -> anyhow::Result<SttOutput> { Err(anyhow::anyhow!("HTTP 500").context("ElevenLabs")) };
        assert_eq!(run(&clip(0.5, 1000), &opts(), &dict(), &stt, None), Outcome::Failed("ElevenLabs: HTTP 500".into()));
    }

    #[test]
    fn whitespace_only_stt_is_nothing_heard() {
        assert_eq!(run(&clip(0.5, 1000), &opts(), &dict(), &says("  \n "), None), Outcome::Skip(Some("Heard nothing".into())));
        // exactly at the 250 ms bar is not a tap
        assert!(matches!(run(&clip(0.5, 250), &opts(), &dict(), &says("hi"), None), Outcome::Text(_)));
    }
}
