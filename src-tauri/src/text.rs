//! WP3: spoken punctuation, rule-based backtrack fallback, style rules, hallucination filter (PLAN 2.6). Pure logic.
//! Also the normalizer and tokenizer that `dictionary.rs` shares (PLAN 2.2).

use crate::contract::Style;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// NFD, drop combining marks, lowercase, `’` -> `'`. "Pérez" -> "perez".
pub fn norm(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c == '’' { '\'' } else { c })
        .collect()
}

/// A word: a maximal run of letters/digits with inner apostrophes. Offsets are bytes in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub norm: String,
    /// Byte offset where a trailing `'s` / `’s` starts ("john's" -> the `'s`).
    pub suffix_at: Option<usize>,
}

impl Token {
    /// The norm without the `'s` suffix.
    pub fn base(&self) -> &str {
        if self.suffix_at.is_some() { &self.norm[..self.norm.len() - 2] } else { &self.norm }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || is_combining_mark(c)
}

/// Placeholders `⟦n⟧` are skipped (never tokens) and act as barriers (see `sep_ok`).
pub fn tokenize(s: &str) -> Vec<Token> {
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let at = |j: usize| if j < cs.len() { cs[j].0 } else { s.len() };
    let mut toks = Vec::new();
    let mut k = 0;
    while k < cs.len() {
        let c = cs[k].1;
        if c == '⟦' {
            while k < cs.len() && cs[k].1 != '⟧' {
                k += 1;
            }
            k += 1;
            continue;
        }
        if !is_word(c) {
            k += 1;
            continue;
        }
        let mut j = k;
        while j < cs.len() {
            let c = cs[j].1;
            let inner_apos = (c == '\'' || c == '’') && j > k && j + 1 < cs.len() && is_word(cs[j + 1].1);
            if is_word(c) || inner_apos {
                j += 1;
            } else {
                break;
            }
        }
        let (start, end) = (at(k), at(j));
        let raw = &s[start..end];
        let lower = raw.to_lowercase();
        let suffix_at = [("'s", 2usize), ("’s", 4)]
            .iter()
            .find(|(suf, n)| lower.ends_with(suf) && raw.len() > *n)
            .map(|(_, n)| end - n);
        toks.push(Token { start, end, norm: norm(raw), suffix_at });
        k = j;
    }
    toks
}

/// May two tokens with this text between them belong to one phrase? Commas, hyphens and spaces
/// are fine (whisper inserts "conta, flow"); sentence ends, newlines, brackets and placeholders are not.
/// An inner dot with no space ("node.js", "amara.org") is fine.
pub fn sep_ok(sep: &str) -> bool {
    !sep.chars().any(|c| "\n\r⟦⟧!?¡¿;:()[]{}\"“”«»".contains(c))
        && !(sep.contains('.') && sep.chars().any(char::is_whitespace))
}

/// The window is part of an email, URL, code identifier or glued word: "pablo@x.app", "github.com/x",
/// "non-linear", "SUPABASE_URL", "--flag", "$var", "mod::Type".
/// Inner separators of a multi-token window ("node.js") are inside the window, so they still match,
/// except "_" ("pablo_perez" is an identifier, not a name).
pub fn glued(s: &str, start: usize, end: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut before = s[..start].chars().rev();
    let mut after = s[end..].chars();
    let (b1, b2, a1, a2) = (before.next(), before.next(), after.next(), after.next());
    s[start..end].contains('_')
        || matches!(b1, Some('@' | '/' | '_' | '$'))
        || matches!(a1, Some('@' | '/' | '_'))
        || (matches!(b1, Some('.' | '-')) && (word(b2) || b2 == Some('-')))
        || (matches!(a1, Some('.' | '-')) && word(a2))
        || (b1 == Some(':') && b2 == Some(':'))
        || (a1 == Some(':') && a2 == Some(':'))
}

// ---------- spoken punctuation ----------

/// (normalized phrase, mark, conditional). Conditional commands apply only at the end of the text,
/// right before punctuation whisper inserted, or before another command ("the period of time" stays).
const CMDS: &[(&str, &str, bool)] = &[
    ("signo de interrogacion", "?", false),
    ("signo de exclamacion", "!", false),
    ("punto y aparte", ".\n", false),
    ("punto y seguido", ".", false),
    ("punto y coma", ";", false),
    ("new paragraph", "\n\n", false),
    ("new line", "\n", false),
    ("question mark", "?", false),
    ("exclamation mark", "!", false),
    ("exclamation point", "!", false),
    ("full stop", ".", false),
    ("nueva linea", "\n", false),
    ("nuevo parrafo", "\n\n", false),
    ("dos puntos", ":", true),
    ("comma", ",", true),
    ("period", ".", true),
    ("colon", ":", true),
    ("semicolon", ";", true),
    ("coma", ",", true),
    ("punto", ".", true),
];

/// A conditional command right after one of these is a noun ("a comma", "en punto", "en estado de coma").
const DETERMINERS: &[&str] = &[
    "the", "a", "an", "this", "that", "my", "your", "his", "her", "its", "our", "their", "one", "each", "every", "per",
    "no", "el", "la", "los", "las", "un", "una", "unos", "unas", "en", "al", "del", "de", "este", "esta", "ese", "esa",
    "aquel", "mi", "tu", "su", "buen", "otro", "otra", "cada",
];

const WHISPER_PUNCT: &str = ",.;:!?\n";

/// (token count, mark, conditional) of the command starting at token i.
fn command_at(s: &str, toks: &[Token], i: usize) -> Option<(usize, &'static str, bool)> {
    CMDS.iter().find_map(|(phrase, mark, cond)| {
        let words: Vec<&str> = phrase.split(' ').collect();
        let n = words.len();
        let ok = i + n <= toks.len()
            && (0..n).all(|k| toks[i + k].norm == words[k])
            && (1..n).all(|k| sep_ok(&s[toks[i + k - 1].end..toks[i + k].start]));
        ok.then_some((n, *mark, *cond))
    })
}

fn conditional_ok(s: &str, toks: &[Token], i: usize, n: usize) -> bool {
    // Only a plain space: "this, comma, and that" is a command, "a comma" is a noun.
    if i > 0
        && DETERMINERS.contains(&toks[i - 1].norm.as_str())
        && s[toks[i - 1].end..toks[i].start].chars().all(char::is_whitespace)
    {
        return false;
    }
    let j = i + n;
    j == toks.len()
        || s[toks[j - 1].end..toks[j].start].chars().any(|c| WHISPER_PUNCT.contains(c))
        || command_at(s, toks, j).is_some()
}

fn capitalize_first(s: &str) -> String {
    match s.char_indices().find(|(_, c)| c.is_alphabetic()) {
        Some((i, c)) => format!("{}{}{}", &s[..i], c.to_uppercase(), &s[i + c.len_utf8()..]),
        None => s.to_string(),
    }
}

/// "hello comma" -> "hello,"; "new line" -> "\n"; "hello comma world" stays. en + es.
pub fn spoken_punctuation(s: &str) -> String {
    let toks = tokenize(s);
    let mut out = String::new();
    let mut pos = 0;
    let mut cap_next = false;
    let mut floor = 0; // never trim a mark we inserted
    let push = |out: &mut String, chunk: &str, cap: &mut bool| {
        if *cap && chunk.chars().any(char::is_alphabetic) {
            out.push_str(&capitalize_first(chunk));
            *cap = false;
        } else {
            out.push_str(chunk);
        }
    };
    let mut i = 0;
    while i < toks.len() {
        let Some((n, mark, _)) = command_at(s, &toks, i)
            .filter(|&(n, _, c)| !glued(s, toks[i].start, toks[i + n - 1].end) && (!c || conditional_ok(s, &toks, i, n)))
        else {
            i += 1;
            continue;
        };
        push(&mut out, &s[pos..toks[i].start], &mut cap_next);
        // Drop the punctuation whisper put before the command; a newline keeps a sentence end.
        let drop: &[char] = if mark.starts_with('\n') { &[',', ';', ':'] } else { &[',', '.', ';', ':', '?', '!'] };
        let kept = out.trim_end().trim_end_matches(drop).trim_end().len();
        out.truncate(kept.max(floor));
        out.push_str(mark);
        floor = out.len();
        // Skip whisper's punctuation and spaces after the command.
        pos = toks[i + n - 1].end;
        pos += s[pos..].len() - s[pos..].trim_start_matches(|c: char| c.is_whitespace() || WHISPER_PUNCT.contains(c)).len();
        if pos < s.len() && !mark.ends_with('\n') {
            out.push(' ');
        }
        cap_next |= mark.ends_with(['.', '?', '!', '\n']);
        i += n;
    }
    push(&mut out, &s[pos..], &mut cap_next);
    out
}

// ---------- backtrack fallback ----------

const BACKTRACK: &[&[&str]] = &[&["never", "mind", "that"], &["scratch", "that"], &["borra", "eso"]];

/// "scratch that" / "borra eso" / "never mind that" deletes the sentence before it.
/// ponytail: rule-based, only for the no-LLM path; the LLM does the real backtrack.
pub fn backtrack_rules(text: &str) -> String {
    let mut s = text.to_string();
    loop {
        let toks = tokenize(&s);
        let hit = (0..toks.len()).find_map(|i| {
            BACKTRACK.iter().find_map(|p| {
                let n = p.len();
                let ok = i + n <= toks.len()
                    && (0..n).all(|k| toks[i + k].norm == p[k])
                    && (1..n).all(|k| sep_ok(&s[toks[i + k - 1].end..toks[i + k].start]));
                ok.then_some((i, n))
            })
        });
        let Some((i, n)) = hit else { break };
        let mut end = toks[i + n - 1].end;
        end += s[end..].len() - s[end..].trim_start_matches(|c: char| c.is_whitespace() || ",.;:!?".contains(c)).len();
        let before = s[..toks[i].start].trim_end_matches(|c: char| c.is_whitespace() || ",;:".contains(c));
        // If the phrase starts a sentence, the sentence before it is the previous one.
        let core = before.strip_suffix(['.', '!', '?', '\n']).unwrap_or(before);
        let cut = last_sentence_end(core);
        let head = s[..cut].trim_end();
        let tail = capitalize_first(&s[end..]);
        s = match (head.is_empty(), tail.is_empty()) {
            (true, _) => tail,
            (false, true) => head.to_string(),
            (false, false) if head.ends_with('\n') => format!("{head}{tail}"),
            _ => format!("{head} {tail}"),
        };
    }
    s
}

/// Never a sentence end: "Dr. Smith", "Sra. Pérez".
const ABBREVIATIONS: &[&str] = &["dr", "dra", "mr", "mrs", "ms", "sr", "sra", "srta", "prof", "st", "vs", "ud", "uds"];

/// Byte offset just after the last sentence end in `s` (0 if none). A sentence end is a newline, or
/// `.!?` followed by whitespace, so "1.5", "v1.2", "example.com" and "Dr. Smith" do not count.
fn last_sentence_end(s: &str) -> usize {
    s.char_indices()
        .filter(|&(i, c)| {
            let next_space = s[i + c.len_utf8()..].starts_with(char::is_whitespace);
            let abbr = || {
                let w = s[..i].rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
                ABBREVIATIONS.contains(&norm(w).as_str())
            };
            c == '\n' || ("!?".contains(c) && next_space) || (c == '.' && next_space && !abbr())
        })
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8())
}

// ---------- style rules ----------

fn one_sentence(t: &str) -> bool {
    !t.contains('\n') && !t.char_indices().any(|(i, c)| ".!?".contains(c) && t[i + 1..].starts_with(char::is_whitespace))
}

/// Casual: drop the trailing period of a one-sentence text. Very casual: also lowercase the first
/// letter, unless the text starts with a dictionary term or an all-caps/mixed-case word ("API", "McDonald").
/// Formal and Excited: unchanged. Raw apps skip this (caller).
pub fn style_rules(text: &str, style: Style, terms: &[String]) -> String {
    let mut t = text.to_string();
    if matches!(style, Style::Casual | Style::VeryCasual) {
        let trimmed = t.trim_end();
        if let Some(body) = trimmed.strip_suffix('.') {
            if !body.ends_with('.') && one_sentence(body) {
                t = body.to_string();
            }
        }
    }
    if style == Style::VeryCasual {
        let first_word: &str = t.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        let rest_lower = first_word.chars().skip(1).all(|c| !c.is_uppercase());
        let is_term = terms.iter().any(|term| {
            t.starts_with(term.as_str()) && !t[term.len()..].starts_with(char::is_alphanumeric)
        });
        if rest_lower && !is_term {
            if let Some(c) = t.chars().next() {
                t = format!("{}{}", c.to_lowercase(), &t[c.len_utf8()..]);
            }
        }
    }
    t
}

// ---------- hallucination filter ----------

/// Normalized: diacritics gone, punctuation as spaces ("Amara.org" -> "amara org").
const HALLUCINATIONS: &[&str] = &[
    "thank you",
    "thank you very much",
    "thanks for watching",
    "thank you for watching",
    "you",
    "gracias",
    "muchas gracias",
    "gracias por ver",
    "gracias por ver el video",
    "subtitulos realizados por la comunidad de amara org",
    "subtitulos por la comunidad de amara org",
    "subtitulado por la comunidad de amara org",
    "blank audio",
    "music",
    "musica",
];

/// STT output as one line of text: whisper-server joins its segments with "\n " and writes
/// "[BLANK_AUDIO]" / "[MUSIC]" for non-speech. Spoken "new line" is handled later, from the words.
pub fn tidy_stt(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(a) = rest.find('[') {
        let tag = rest[a + 1..].find(']').map(|b| &rest[a + 1..a + 1 + b]);
        match tag.filter(|t| !t.is_empty() && t.chars().all(|c| c.is_alphabetic() || c == '_' || c == ' ')) {
            Some(t) => {
                out.push_str(&rest[..a]);
                rest = &rest[a + t.len() + 2..];
            }
            None => {
                out.push_str(&rest[..=a]);
                rest = &rest[a + 1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when the whole result is a phrase whisper invents on silence ("Thank you.", "Gracias.").
pub fn is_hallucination(text: &str) -> bool {
    let key: String = norm(text).chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let key = key.split_whitespace().collect::<Vec<_>>().join(" ");
    HALLUCINATIONS.contains(&key.as_str())
}

// ---------- press enter ----------

const PRESS_ENTER: &[[&str; 2]] = &[["press", "enter"], ["presiona", "enter"], ["pulsa", "enter"]];

/// Some(text without the phrase) when the text ends with "press enter" / "presiona enter".
pub fn strip_press_enter(text: &str) -> Option<String> {
    let toks = tokenize(text);
    let n = toks.len();
    if n < 2 || !PRESS_ENTER.iter().any(|p| toks[n - 2].norm == p[0] && toks[n - 1].norm == p[1]) {
        return None;
    }
    Some(text[..toks[n - 2].start].trim_end_matches(|c: char| c.is_whitespace() || c == ',').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_and_tokens() {
        assert_eq!(norm("Pérez’s ÑANDÚ"), "perez's nandu");
        let t = tokenize("John's don't conta-flow ⟦1⟧ node.js");
        let n: Vec<&str> = t.iter().map(|t| t.norm.as_str()).collect();
        assert_eq!(n, ["john's", "don't", "conta", "flow", "node", "js"]);
        assert_eq!(t[0].base(), "john");
        assert_eq!(t[1].base(), "don't");
        assert_eq!(tokenize("John’s")[0].base(), "john");
        assert!(sep_ok(", ") && sep_ok("-") && sep_ok(".") && !sep_ok(". ") && !sep_ok(" ⟦1⟧ ") && !sep_ok("\n"));
    }

    #[test]
    fn spoken_punctuation_table() {
        let cases = [
            ("hello comma world", "hello comma world"),
            ("hello comma", "hello,"),
            ("Hello comma.", "Hello,"),
            ("Hello, comma, world", "Hello, world"),
            ("the period of time", "the period of time"),
            ("Is it done question mark", "Is it done?"),
            ("hello new line world", "hello\nWorld"),
            ("new line", "\n"),
            ("Contaflow uses Tauri, new line, thanks", "Contaflow uses Tauri\nThanks"),
            ("Hello. New line. Thanks.", "Hello.\nThanks."),
            ("one new paragraph two", "one\n\nTwo"),
            ("I am done period. How are you", "I am done. How are you"),
            ("wait comma new line ok", "wait,\nOk"),
            ("I put a comma.", "I put a comma."),
            ("Write the period.", "Write the period."),
            ("great exclamation mark", "great!"),
            ("full stop", "."),
            // Spanish
            ("hola punto y aparte qué tal", "hola.\nQué tal"),
            ("hola coma", "hola,"),
            ("hola coma qué tal", "hola coma qué tal"),
            ("Llegamos a las tres en punto.", "Llegamos a las tres en punto."),
            ("Estaba en estado de coma.", "Estaba en estado de coma."),
            ("qué tal signo de interrogación", "qué tal?"),
            // whisper already wrote the mark the user spoke: one mark, not two
            ("Hola, ¿cómo estás? Signo de interrogación nueva línea hasta mañana.", "Hola, ¿cómo estás?\nHasta mañana."),
            ("Is it done? Question mark", "Is it done?"),
            ("primero punto y coma segundo", "primero; segundo"),
            ("la lista dos puntos", "la lista:"),
            ("tengo dos puntos en el examen", "tengo dos puntos en el examen"),
            ("hola nueva línea adiós", "hola\nAdiós"),
            ("fin punto", "fin."),
            ("vale punto nueva línea", "vale.\n"),
        ];
        for (input, want) in cases {
            assert_eq!(spoken_punctuation(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn backtrack_table() {
        let cases = [
            ("Let's meet at 3. Scratch that. Let's meet at 4.", "Let's meet at 4."),
            ("I like apples. Send it at 3, scratch that, at 4.", "I like apples. At 4."),
            ("Nos vemos el lunes. Borra eso. Nos vemos el martes.", "Nos vemos el martes."),
            ("Buy milk. Call mom. Never mind that.", "Buy milk."),
            ("Nothing to undo here.", "Nothing to undo here."),
        ];
        for (input, want) in cases {
            assert_eq!(backtrack_rules(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn style_table() {
        let terms = vec!["Contaflow".to_string()];
        let cases = [
            ("See you soon.", Style::Casual, "See you soon"),
            ("See you soon. Bye.", Style::Casual, "See you soon. Bye."),
            ("Wait...", Style::Casual, "Wait..."),
            ("Version 1.5 is out.", Style::Casual, "Version 1.5 is out"),
            ("See you soon.", Style::VeryCasual, "see you soon"),
            ("Contaflow is live.", Style::VeryCasual, "Contaflow is live"),
            ("API is down.", Style::VeryCasual, "API is down"),
            ("See you soon.", Style::Formal, "See you soon."),
            ("See you soon!", Style::Excited, "See you soon!"),
            ("Nos vemos.", Style::Casual, "Nos vemos"),
        ];
        for (input, style, want) in cases {
            assert_eq!(style_rules(input, style.clone(), &terms), want, "input: {input:?} {style:?}");
        }
    }

    #[test]
    fn hallucination_table() {
        for t in ["Thank you.", " you ", "Gracias.", "Subtítulos realizados por la comunidad de Amara.org", "[BLANK_AUDIO]"] {
            assert!(is_hallucination(t), "{t:?}");
        }
        for t in ["Thank you for the report.", "Gracias por todo, Pablo.", "You are right.", ""] {
            assert!(!is_hallucination(t), "{t:?}");
        }
    }

    #[test]
    fn tidy_stt_table() {
        let cases = [
            (" One sentence.\n Two, and\n three.\n [BLANK_AUDIO]", "One sentence. Two, and three."),
            ("[MUSIC] hello [Music]", "hello"),
            ("[BLANK_AUDIO]", ""),
            ("keep [1] and [a-b] and [x", "keep [1] and [a-b] and [x"),
            ("  plain  ", "plain"),
        ];
        for (input, want) in cases {
            assert_eq!(tidy_stt(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn press_enter_table() {
        assert_eq!(strip_press_enter("Send it, press enter."), Some("Send it".into()));
        assert_eq!(strip_press_enter("ok presiona enter"), Some("ok".into()));
        assert_eq!(strip_press_enter("press enter to continue"), None);
        assert_eq!(strip_press_enter("hello"), None);
    }
}
