//! Property/fuzz tests for the dictionary + text engine (dictionary.rs, text.rs, the pipeline placeholder path).
//!   cargo test --test dictionary_fuzz
//!   PROPTEST_CASES=20000 cargo test --test dictionary_fuzz   # longer fuzz run
//! The realistic table uses the real whisper ggml-base output of scripts/e2e/fixtures/*.wav (2026-10-17).

use proptest::prelude::*;
use susurro_lib::audio::Clip;
use susurro_lib::contract::{DictEntry, DictKind, DictOpts, Style};
use susurro_lib::dictionary::{Dictionary, Hints};
use susurro_lib::pipeline::{self, Opts, Outcome};
use susurro_lib::stt::{SttOutput, SttRequest};
use susurro_lib::text;

// ---------- fixtures ----------

fn entry(id: i64, kind: DictKind, text: &str, aliases: &[&str], exp: Option<&str>) -> DictEntry {
    DictEntry {
        id,
        kind,
        text: text.into(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        expansion: exp.map(Into::into),
        exact_only: false,
        starred: false,
        uses: 0,
        created_at: id,
    }
}

/// scripts/e2e/fixtures/dict.json: the entries the e2e fixtures need.
fn fixture_entries() -> Vec<DictEntry> {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/e2e/fixtures/dict.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let kind = if e["kind"] == "snippet" { DictKind::Snippet } else { DictKind::Word };
            let aliases: Vec<&str> = e["aliases"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
            let mut d = entry(i as i64 + 1, kind, e["text"].as_str().unwrap(), &aliases, e["expansion"].as_str());
            d.exact_only = e["exact_only"].as_bool().unwrap();
            d.starred = e["starred"].as_bool().unwrap();
            d
        })
        .collect()
}

const CAL: &str = "https://cal.example.com/pablo";
/// An expansion full of triggers and sloppy dictionary forms: it must be pasted exactly as typed.
const SIG: &str = "Saludos, pablo perez - contaflo, my calendar link, mi firma, kube control. ⟦9⟧";
const ADDR: &str = "Av. Siempre Viva 742, Springfield";

fn words_only() -> Vec<DictEntry> {
    let mut v: Vec<DictEntry> = fixture_entries().into_iter().filter(|e| e.kind == DictKind::Word).collect();
    v.extend([
        entry(50, DictKind::Word, "Node.js", &[], None),
        entry(51, DictKind::Word, "José Martínez", &["jose martines"], None),
        entry(52, DictKind::Word, "iPhone 15", &[], None),
        entry(53, DictKind::Word, "Python 3.12", &[], None),
        entry(54, DictKind::Word, "Fernández", &[], None),
        entry(55, DictKind::Word, "Tauri", &[], None),
        entry(56, DictKind::Word, "Ñandú Labs", &[], None),
    ]);
    v
}

fn full() -> Vec<DictEntry> {
    let mut v = words_only();
    v.extend(fixture_entries().into_iter().filter(|e| e.kind == DictKind::Snippet));
    v.push(entry(60, DictKind::Snippet, "mi firma", &[], Some(SIG)));
    v.push(entry(61, DictKind::Snippet, "mi dirección", &[], Some(ADDR)));
    v
}

fn dict(e: Vec<DictEntry>) -> Dictionary {
    Dictionary::new(e, DictOpts { fuzzy: true })
}

// ---------- generators ----------

/// Common EN/ES words. None of them, alone or glued to a neighbour, is a dictionary form or a
/// spoken-punctuation / backtrack / press-enter command.
const FILLER: &[&str] = &[
    "the", "and", "we", "fix", "today", "team", "ship", "it", "is", "ready", "for", "review", "please", "check",
    "invoice", "bank", "feed", "client", "report", "monday", "tomorrow", "meeting", "Hello", "Thanks", "la", "el",
    "con", "para", "hoy", "mañana", "reunión", "revisar", "informe", "facturación", "equipo", "cliente", "también",
    "después", "pero", "Hola", "gracias", "nos", "vemos", "lunes", "martes", "I", "you", "can", "did", "not", "yes",
];

/// Things a dictation can contain that must come out byte for byte, even when they hold a dictionary word.
const PROTECTED: &[&str] = &[
    "https://github.com/contaflow/supabase",
    "github.com/pablo/contaflow",
    "pablo@contaflow.app",
    "francisca.ugarte@supabase.io",
    "contaflow.rs",
    "supabase.co",
    "kubectl-apply",
    "contaflow-api",
    "~/projects/contaflow",
    "1,250",
    "3.12",
    "555-0199",
    "v2.0.1",
    "10:30",
    "$1,250",
    "5pm",
    "2026-10-17",
    "supabaseClient",
    "ContaflowAPI",
    "contaflows",
];

/// Ways whisper writes a dictionary term (casing, splits, hyphens, aliases, one-letter mistakes).
const FORMS: &[(&str, &str)] = &[
    ("Contaflow", "Contaflow"),
    ("contaflow", "Contaflow"),
    ("CONTAFLOW", "Contaflow"),
    ("Conta flow", "Contaflow"),
    ("conta-flow", "Contaflow"),
    ("Contaflo", "Contaflow"),
    ("cotnaflow", "Contaflow"),
    ("Pablo Pérez", "Pablo Pérez"),
    ("pablo perez", "Pablo Pérez"),
    ("Pablo Peres", "Pablo Pérez"),
    ("Kubectl", "kubectl"),
    ("kube control", "kubectl"),
    ("cube cuddle", "kubectl"),
    ("SuperBase", "Supabase"),
    ("super base", "Supabase"),
    ("Supa Base", "Supabase"),
    ("supabace", "Supabase"),
    ("Francisca Ugarde", "Francisca Ugarte"),
    ("francisca ugarte", "Francisca Ugarte"),
    ("node js", "Node.js"),
    ("jose martinez", "José Martínez"),
    ("Jose Martines", "José Martínez"),
    ("iphone 15", "iPhone 15"),
    ("python 3.12", "Python 3.12"),
    ("fernandez", "Fernández"),
    ("tauri", "Tauri"),
    ("nandu labs", "Ñandú Labs"),
];

const SEPS: &[&str] = &[" ", " ", " ", ", ", ". ", "? ", "! ", "\n", "; ", ": "];

#[derive(Debug, Clone)]
enum Piece {
    Filler(&'static str),
    Protected(&'static str),
    Form(&'static str, &'static str),
}

impl Piece {
    fn src(&self) -> &'static str {
        match self {
            Piece::Filler(s) | Piece::Protected(s) | Piece::Form(s, _) => s,
        }
    }
}

fn piece(forms: bool) -> BoxedStrategy<Piece> {
    let f = prop::sample::select(FILLER).prop_map(Piece::Filler);
    let p = prop::sample::select(PROTECTED).prop_map(Piece::Protected);
    if forms {
        let w = prop::sample::select(FORMS).prop_map(|(a, b)| Piece::Form(a, b));
        prop_oneof![4 => f, 1 => p, 2 => w].boxed()
    } else {
        prop_oneof![4 => f, 1 => p].boxed()
    }
}

/// (pieces, text). Pieces are joined by a random separator; the text may end with punctuation.
fn dictation(forms: bool) -> impl Strategy<Value = (Vec<Piece>, String)> {
    (prop::collection::vec((piece(forms), prop::sample::select(SEPS)), 1..12), prop::sample::select(&["", ".", "?", "!"][..]))
        .prop_map(|(v, end)| {
            let mut s = String::new();
            for (i, (p, sep)) in v.iter().enumerate() {
                if i > 0 {
                    s.push_str(sep);
                }
                s.push_str(p.src());
            }
            s.push_str(end);
            (v.into_iter().map(|(p, _)| p).collect(), s)
        })
}

/// Free text with the characters the engine treats specially.
fn nasty() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        any::<char>().prop_map(String::from),
        prop::sample::select(&[
            "⟦", "⟧", "⟦1⟧", "⟦2⟧", "⟦01⟧", "'s", "’s", "'", "’", "\u{301}", "\u{308}", "ß", "İ", "ﬁ", "@", "/", ".", "-",
            ", ", ". ", "\n", "\r\n", " ", "comma", "period", "punto", "coma", "new line", "punto y aparte",
            "question mark", "scratch that", "borra eso", "press enter", "presiona enter", "contaflow", "pablo",
            "pérez", "conta flow", "mi firma", "my calendar link", "Pablo Pérez", "kubectl", "0", "15", "3.12",
        ][..])
        .prop_map(String::from),
    ];
    prop::collection::vec(atom, 0..24).prop_map(|v| v.concat())
}

fn clip() -> Clip {
    Clip { samples: vec![0.5; 16_000], duration_ms: 1000, peak_db: -6.0, device_names: vec!["Test".into()] }
}

fn says(t: String) -> impl Fn(&SttRequest) -> anyhow::Result<SttOutput> {
    move |_| Ok(SttOutput { text: t.clone(), language: Some("en".into()), note: None, fell_back: false })
}

fn opts(style: Style) -> Opts<'static> {
    Opts { language: None, style, raw_app: false, press_enter_command: true }
}

fn style() -> impl Strategy<Value = Style> {
    prop::sample::select(vec![Style::Formal, Style::Casual, Style::VeryCasual, Style::Excited])
}

// ---------- properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1024), ..ProptestConfig::default() })]

    /// Nothing in the dictionary is said: the text comes back byte for byte, with no matches.
    #[test]
    fn no_hits_is_a_no_op((_, s) in dictation(false)) {
        let d = dict(full());
        let a = d.apply(&s);
        prop_assert_eq!(&a.text, &s);
        prop_assert!(a.matches.is_empty(), "{:?}", a.matches);
        let p = d.prepare(&s);
        prop_assert_eq!(&p.text, &s);
        prop_assert!(p.slots.is_empty() && p.used.is_empty());
    }

    /// Free text never panics anywhere in the text engine, and finish(prepare) never invents a bracket.
    #[test]
    fn never_panics(s in nasty(), st in style()) {
        let d = dict(full());
        let a = d.apply(&s);
        let p = d.prepare(&s);
        let _ = d.finish(&s, &p);
        let _ = d.finish(&p.text, &p);
        let _ = Dictionary::expand(&s, &p.slots);
        let _ = text::spoken_punctuation(&s);
        let _ = text::backtrack_rules(&s);
        let _ = text::style_rules(&s, st, &["Contaflow".into()]);
        let _ = text::strip_press_enter(&s);
        let _ = text::is_hallucination(&s);
        let _ = text::tokenize(&s);
        for m in &a.matches {
            prop_assert!(m.start <= m.end && m.end as usize <= a.text.chars().count(), "{:?} in {:?}", m, a.text);
        }
    }

    /// Applying the dictionary twice gives the same text as applying it once.
    #[test]
    fn idempotent((_, s) in dictation(true)) {
        let d = dict(words_only());
        let once = d.apply(&s).text;
        let twice = d.apply(&once).text;
        prop_assert_eq!(&twice, &once, "input: {:?}", s);
    }

    /// Every dictionary form in the dictation comes out as its entry text.
    #[test]
    fn forms_are_corrected((pieces, s) in dictation(true)) {
        let d = dict(words_only());
        let out = d.apply(&s).text;
        for p in &pieces {
            if let Piece::Form(_, want) = p {
                prop_assert!(out.contains(want), "{:?} missing from {:?} (input {:?})", want, out, s);
            }
        }
    }

    /// URLs, emails, numbers, versions, paths and code identifiers come out byte for byte.
    #[test]
    fn protected_tokens_survive((pieces, s) in dictation(true)) {
        let d = dict(full());
        let out = d.apply(&s).text;
        for p in &pieces {
            if let Piece::Protected(t) = p {
                let want = s.matches(t).count();
                prop_assert_eq!(out.matches(t).count(), want, "{:?} corrupted: {:?} -> {:?}", t, s, out);
            }
        }
    }

    /// An LLM that keeps the placeholder text as is: finish(prepare(x)) == apply(x).
    #[test]
    fn placeholders_round_trip((_, s) in dictation(true), trig in prop::sample::select(&["my calendar link", "mi firma", "mi dirección"][..]), at in 0usize..12) {
        let d = dict(full());
        let words: Vec<&str> = s.split(' ').collect();
        let at = at.min(words.len());
        let s = [&words[..at], &[trig], &words[at..]].concat().join(" ");
        let p = d.prepare(&s);
        prop_assert!(!p.slots.is_empty(), "no slot for {:?} in {:?}", trig, s);
        let fin = d.finish(&p.text, &p);
        let want = d.apply(&s).text;
        prop_assert_eq!(fin.as_deref(), Some(want.as_str()), "input {:?}, prepared {:?}", s, p.text);
    }

    /// An LLM that mangles placeholders: finish is None unless each slot appears exactly once, and a
    /// Some never holds a bracket the expansions did not bring.
    #[test]
    fn mangled_placeholders_fall_back(
        ops in prop::collection::vec((0u8..6, 0usize..4), 0..6),
        s in prop::sample::select(&["send my calendar link and mi firma to pablo perez", "mi dirección, mi firma", "my calendar link"][..]),
    ) {
        let d = dict(full());
        let p = d.prepare(s);
        let mut t = p.text.clone();
        for (op, n) in ops {
            let n = n % (p.slots.len() + 2);
            match op {
                0 => t = t.replacen(&format!("⟦{n}⟧"), "", 1),
                1 => t.push_str(&format!(" ⟦{n}⟧")),
                2 => t = t.replace('⟦', "[").replace('⟧', "]"),
                3 => t = t.replacen(&format!("⟦{n}⟧"), &format!("⟦ {n} ⟧"), 1),
                4 => t = t.to_uppercase(),
                _ => t = format!("⟦{n}⟧ {t}"),
            }
        }
        let valid = (1..=p.slots.len()).all(|k| t.matches(&format!("⟦{k}⟧")).count() == 1)
            && t.matches('⟦').count() == p.slots.len()
            && t.matches('⟧').count() == p.slots.len();
        match d.finish(&t, &p) {
            None => prop_assert!(!valid, "valid llm text {:?} rejected", t),
            Some(out) => {
                prop_assert!(valid, "invalid llm text {:?} accepted: {:?}", t, out);
                let own = p.slots.iter().map(|x| x.matches('⟦').count()).sum::<usize>();
                prop_assert_eq!(out.matches('⟦').count(), own, "{:?}", out);
            }
        }
    }

    /// The real pipeline with any LLM output: a snippet is never lost or duplicated, its expansion is
    /// pasted as typed, and no placeholder leaks into the pasted text.
    #[test]
    fn pipeline_never_leaks_or_loses_a_snippet(
        (_, s) in dictation(true),
        trig in prop::sample::select(&["my calendar link", "mi firma", "mi dirección"][..]),
        llm in prop_oneof![Just(None), nasty().prop_map(Some), Just(Some("KEEP".to_string()))],
        st in style(),
    ) {
        let raw = format!("{s} {trig} {s}");
        let exp = match trig { "my calendar link" => CAL, "mi firma" => SIG, _ => ADDR };
        let d = dict(full());
        let clean = |t: &str, _: &Hints| -> anyhow::Result<String> {
            Ok(match &llm { Some(x) if x == "KEEP" => t.to_string(), Some(x) => x.clone(), None => anyhow::bail!("down") })
        };
        let o = pipeline::run(&clip(), &opts(st), &d, &says(raw.clone()), Some(&clean));
        let Outcome::Text(done) = o else { return Err(TestCaseError::fail(format!("{o:?}"))) };
        let f = &done.final_text;
        prop_assert_eq!(f.matches(exp).count(), 1, "raw {:?} llm {:?} -> {:?}", raw, llm, f);
        let without = f.replacen(exp, "", 1);
        prop_assert!(!without.contains('⟦') && !without.contains('⟧'), "leak: {:?}", f);
    }

    /// A snippet expansion is never scanned again: its triggers and sloppy words stay as typed.
    #[test]
    fn expansion_never_retriggers((_, s) in dictation(true)) {
        let d = dict(full());
        for raw in [format!("{s} mi firma"), format!("mi firma, {s}"), "mi firma".to_string()] {
            let a = d.apply(&raw).text;
            prop_assert!(a.contains(SIG), "apply {:?} -> {:?}", raw, a);
            prop_assert!(!a.contains(CAL), "re-triggered: {:?}", a);
            let p = d.prepare(&raw);
            let fin = d.finish(&p.text, &p).unwrap();
            prop_assert!(fin.contains(SIG) && !fin.contains(CAL), "finish {:?} -> {:?}", raw, fin);
        }
    }

    /// Spoken punctuation is idempotent on its own output.
    #[test]
    fn spoken_punctuation_idempotent(s in nasty()) {
        let once = text::spoken_punctuation(&s);
        prop_assert_eq!(text::spoken_punctuation(&once), once.clone(), "input {:?}", s);
    }
}

// ---------- realistic dictation table ----------

/// (whisper raw text, expected Dictionary::apply output). Rows marked "whisper" are the real
/// ggml-base output for the e2e fixtures; the rest are the same mistakes in new sentences.
#[test]
fn realistic_dictation_table() {
    let d = dict(full());
    let cases: &[(&str, &str)] = &[
        // whisper, en_custom.wav, no prompt / with the dictionary prompt
        (
            "I pushed the Contaflo fix to SuperBase, then ran Cubectal Apply, Ping Pablo Paris and Francesca Uguard.",
            "I pushed the Contaflow fix to Supabase, then ran Cubectal Apply, Ping Pablo Paris and Francesca Uguard.",
        ),
        (
            "I pushed the Contaflow fix to Supabase, then ran Kubectl apply, ping Pablo Pérez and Francisca Ugarte.",
            "I pushed the Contaflow fix to Supabase, then ran kubectl apply, ping Pablo Pérez and Francisca Ugarte.",
        ),
        // whisper, en_snippet.wav: a trigger inside a sentence
        ("Sure, here is my calendar link.", "Sure, here is https://cal.example.com/pablo."),
        // whisper, en_numbers.wav: nothing to touch
        (
            "The invoice is $1,250, due on March 3rd, call 555-0199, before 5pm.",
            "The invoice is $1,250, due on March 3rd, call 555-0199, before 5pm.",
        ),
        // whisper, en_punct.wav: "comma" with no pause is a word (PLAN 2.6 conditional rule)
        (
            "Hello comma how are you question mark new line see you tomorrow period.",
            "Hello comma how are you?\nSee you tomorrow.",
        ),
        ("Hello Kamahawar you question mark new line see you tomorrow period.", "Hello Kamahawar you?\nSee you tomorrow."),
        // whisper, es_custom.wav
        (
            "Francisca Ugarte revisó Contaflow con Pablo Pérez en su pavase.",
            "Francisca Ugarte revisó Contaflow con Pablo Pérez en su pavase.",
        ),
        (
            "Francisco Agarte revisó con Tafló con Pablo Pérez en su pavase.",
            "Francisco Agarte revisó Contaflow con Pablo Pérez en su pavase.",
        ),
        // whisper, es_punct.wav
        (
            "Hola, ¿cómo estás signo de interrogación nueva línea de hasta mañana punto?",
            "Hola, ¿cómo estás?\nDe hasta mañana.",
        ),
        ("Hola como estás signo de interrogación nueva línea de hasta mañana punto.", "Hola como estás?\nDe hasta mañana."),
        // whisper, es_accents.wav / en_backtrack.wav: nothing to touch
        (
            "La reunión es mañana a las tres. ¿Puedes revisar el informe de facturación?",
            "La reunión es mañana a las tres. ¿Puedes revisar el informe de facturación?",
        ),
        ("Let's meet at 3, no wait, actually let's meet at 4.", "Let's meet at 3, no wait, actually let's meet at 4."),
        // same mistakes, new sentences (EN)
        ("Deploy conta flow to super base tonight.", "Deploy Contaflow to Supabase tonight."),
        ("Run kube control get pods, then ping pablo perez.", "Run kubectl get pods, then ping Pablo Pérez."),
        ("The Conta-Flow API talks to Supa Base.", "The Contaflow API talks to Supabase."),
        ("Ask Francisca Ugarde about the cube cuddle error.", "Ask Francisca Ugarte about the kubectl error."),
        ("contaflow's importer is slow", "Contaflow's importer is slow"),
        ("Email pablo@contaflow.app the contaflow report.", "Email pablo@contaflow.app the Contaflow report."),
        ("Open github.com/pablo/contaflow and check contaflow.", "Open github.com/pablo/contaflow and check Contaflow."),
        ("We moved from node js 18 to node.js 20.", "We moved from Node.js 18 to Node.js 20."),
        ("My iphone 15 and my iPhone 16.", "My iPhone 15 and my iPhone 16."),
        ("Upgrade python 3.12, not Python 3.11.", "Upgrade Python 3.12, not Python 3.11."),
        ("contaflow comma supabase comma and kubectl period", "Contaflow comma Supabase comma and kubectl."),
        ("Contaflow, comma, Supabase, comma, and kubectl. Period.", "Contaflow, Supabase, and kubectl."),
        ("I need a comma here.", "I need a comma here."),
        ("Send it to Pablo Pérez press enter", "Send it to Pablo Pérez press enter"),
        // same mistakes, new sentences (ES)
        ("Mañana revisamos conta flow con jose martinez.", "Mañana revisamos Contaflow con José Martínez."),
        ("Le escribí a Fernandez y a Pablo Peres.", "Le escribí a Fernández y a Pablo Pérez."),
        ("La base de super base está caída punto", "La base de Supabase está caída."),
        ("Hola coma qué tal punto y aparte nos vemos", "Hola coma qué tal.\nNos vemos"),
        ("Te mando mi dirección y mi firma.", &format!("Te mando {ADDR} y {SIG}.")),
        ("Mi firma.", SIG),
        ("nandu labs y ñandú labs", "Ñandú Labs y Ñandú Labs"),
        ("Llegamos a las tres en punto.", "Llegamos a las tres en punto."),
        ("Pablo pero no vino.", "Pablo pero no vino."),
    ];
    let mut bad = vec![];
    for (raw, want) in cases {
        let got = d.apply(raw).text;
        if got != *want {
            bad.push(format!("  {raw:?}\n    want {want:?}\n    got  {got:?}"));
        }
    }
    assert!(bad.is_empty(), "{} rows differ:\n{}", bad.len(), bad.join("\n"));
}

/// The same table through the whole pipeline, no LLM, Formal style: press-enter and backtrack run too.
#[test]
fn realistic_pipeline_table() {
    let d = dict(full());
    let addr = format!("{ADDR}.");
    let cases: &[(&str, &str, bool)] = &[
        // whisper, en_scratch.wav: the backtrack rule cuts the clause before "scratch that"
        ("Let's meet at 3, scratch that, let's meet at 4.", "Let's meet at 4.", false),
        ("Let's meet at three. Scratch that. Let's meet at four.", "Let's meet at four.", false),
        ("Nos vemos el lunes. Borra eso. Nos vemos el martes.", "Nos vemos el martes.", false),
        ("Send it to pablo perez, press enter.", "Send it to Pablo Pérez", true),
        ("Mándalo a conta flow, presiona enter", "Mándalo a Contaflow", true),
        ("Sure, here is my calendar link.", "Sure, here is https://cal.example.com/pablo.", false),
        ("My calendar link. Scratch that. Mi dirección.", &addr, false),
    ];
    let mut bad = vec![];
    for &(raw, want, enter) in cases {
        match pipeline::run(&clip(), &opts(Style::Formal), &d, &says(raw.into()), None) {
            Outcome::Text(done) if done.final_text == want && done.press_enter == enter => {}
            o => bad.push(format!("  {raw:?}\n    want {want:?} enter={enter}\n    got  {o:?}")),
        }
    }
    assert!(bad.is_empty(), "{} rows differ:\n{}", bad.len(), bad.join("\n"));
}

// ---------- regressions (bugs found by this suite on 2026-10-17, now fixed) ----------

fn table(f: impl Fn(&str) -> String, cases: &[(&str, &str)]) {
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|(i, want)| {
            let got = f(i);
            (got != *want).then(|| format!("  {i:?}\n    want {want:?}\n    got  {got:?}"))
        })
        .collect();
    assert!(bad.is_empty(), "{} rows differ:\n{}", bad.len(), bad.join("\n"));
}

/// Spoken punctuation fires inside URLs and emails: "pablo@punto.cl" -> "pablo@. Cl".
#[test]
fn bug_spoken_punctuation_inside_urls_and_emails() {
    let d = dict(full());
    table(|s| d.apply(s).text, &[
        ("Escríbeme a pablo@punto.cl mañana", "Escríbeme a pablo@punto.cl mañana"),
        ("write to pablo@coma.cl", "write to pablo@coma.cl"),
        ("Abre punto.cl y revisa", "Abre punto.cl y revisa"),
        ("see https://example.com/period", "see https://example.com/period"),
        ("the docs are at example.com/comma", "the docs are at example.com/comma"),
    ]);
}

/// Whisper writes the mark and the speaker says it: "How are you? Question mark." -> "How are you??".
#[test]
fn bug_spoken_mark_doubles_whisper_mark() {
    table(text::spoken_punctuation, &[
        ("How are you? Question mark.", "How are you?"),
        ("Great! Exclamation mark!", "Great!"),
        ("¿Qué tal? Signo de interrogación.", "¿Qué tal?"),
        ("Hello, comma, how are you? Question mark, new line, see you tomorrow. Period.", "Hello, how are you?\nSee you tomorrow."),
    ]);
}

/// The no-LLM backtrack rule treats any "." as a sentence end: decimals, versions, domains and
/// abbreviations get cut in half ("v1.2 scratch that v1.3" -> "v1. V1.3").
#[test]
fn bug_backtrack_cuts_inside_numbers_and_domains() {
    table(text::backtrack_rules, &[
        ("Use v1.2 scratch that v1.3", "V1.3"),
        ("The rate is 1.5. Scratch that. The rate is 2.", "The rate is 2."),
        ("Thanks. Visit example.com. Scratch that.", "Thanks."),
        ("I met Dr. Smith today. Scratch that.", ""),
    ]);
}

/// "this, comma, and that": a determiner before a comma-separated command blocks it.
#[test]
fn bug_determiner_blocks_command_with_commas() {
    table(text::spoken_punctuation, &[
        ("I want this, comma, and that.", "I want this, and that."),
        ("Quiero esta, coma, y esa.", "Quiero esta, y esa."),
    ]);
}

/// Code identifiers: "_" and "$"/"--"/"::" are not treated as glue, and "_" is squashed away.
#[test]
fn bug_code_identifiers_are_corrected() {
    let d = dict(full());
    table(|s| d.apply(s).text, &[
        ("set SUPABASE_URL first", "set SUPABASE_URL first"),
        ("call contaflow_api now", "call contaflow_api now"),
        ("the pablo_perez column", "the pablo_perez column"),
        ("run with --contaflow", "run with --contaflow"),
        ("echo $contaflow", "echo $contaflow"),
        ("contaflow::Api is the type", "contaflow::Api is the type"),
    ]);
}

/// A possessive on a snippet trigger is glued to the expansion: "my calendar link's" -> "https://...pablo's".
#[test]
fn bug_snippet_possessive_glued_to_expansion() {
    let d = dict(full());
    table(|s| d.apply(s).text, &[("check my calendar link's slots", "check https://cal.example.com/pablo slots")]);
}
