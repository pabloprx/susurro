//! WP3: entries, validation, matcher, hints(), prepare(), finish(), prompt builders (PLAN 2). Pure logic,
//! plus the `dict` table queries and the dict_* commands.

use crate::AppState;
use crate::contract::{Applied, DictEntry, DictEntryInput, DictKind, DictOpts, Match, MatchHow};
use crate::text::{self, Token, sep_ok, tokenize};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use tauri::State;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hints {
    /// Word texts, ranked: starred, then uses desc, then newest.
    pub terms: Vec<String>,
    /// (alias, text)
    pub aliases: Vec<(String, String)>,
    pub triggers: Vec<String>,
}

/// Text with snippet placeholders ⟦1⟧, ⟦2⟧ …; slots[i] is the expansion for ⟦i+1⟧.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prepared {
    pub text: String,
    pub slots: Vec<String>,
    /// The whole utterance was one snippet trigger: the output is exactly slots[0].
    /// The caller skips the LLM and the style rules.
    pub standalone: bool,
    /// Entry ids matched (one per match), for `record_uses` at the end of the session.
    pub used: Vec<i64>,
}

/// Normalized key of an entry text or alias: tokens joined by one space ("Conta-Flow" -> "conta flow").
/// The `dict.norm` column and the uniqueness check use it.
pub fn key(s: &str) -> String {
    tokenize(s).into_iter().map(|t| t.norm).collect::<Vec<_>>().join(" ")
}

/// Top 5000 words per language, from hermitdave/FrequencyWords (OpenSubtitles 2018, MIT).
fn is_common(w: &str) -> bool {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        [include_str!("../data/common_en.txt"), include_str!("../data/common_es.txt")]
            .iter()
            .flat_map(|f| f.lines())
            .map(text::norm)
            .collect()
    })
    .contains(w)
}

/// Damerau-Levenshtein (optimal string alignment).
fn dl(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

/// Fuzzy distance of a squashed window to a squashed pattern, if it is close enough (PLAN 2.3).
/// "contaflows" / "contaflowing" (pattern + an ending) never match: that is a different word.
fn fuzzy_d(w: &[char], pat: &[char]) -> Option<usize> {
    // The shorter side sets the budget: "fernanda" (8) is not 2 edits from "Fernández".
    let max = match pat.len().min(w.len()) {
        0..=5 => return None,
        6..=8 => 1,
        _ => 2,
    };
    if w.first() != pat.first() || w.len().abs_diff(pat.len()) > max || w.starts_with(pat) {
        return None;
    }
    let d = dl(w, pat);
    (1..=max).contains(&d).then_some(d)
}

struct Pat {
    entry: usize,
    sq: Vec<char>,
    sq_s: String,
    ntoks: usize,
    snippet: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Corrections + snippets expanded (apply).
    Expand,
    /// Corrections + snippets as ⟦n⟧ (prepare).
    Placeholder,
    /// Corrections only (finish: the LLM output).
    WordsOnly,
}

#[derive(Clone)]
struct Cand {
    pat: usize,
    how: MatchHow,
    dist: usize,
}

pub struct Dictionary {
    entries: Vec<DictEntry>,
    opts: DictOpts,
    pats: Vec<Pat>,
    exact: HashMap<String, Vec<usize>>,
    squash: HashMap<String, Vec<usize>>,
    max_len: usize,
    /// Run `text::spoken_punctuation` first in apply/prepare (Settings.spoken_punctuation).
    pub spoken_punctuation: bool,
}

impl Dictionary {
    pub fn new(entries: Vec<DictEntry>, opts: DictOpts) -> Self {
        let mut d = Dictionary {
            entries,
            opts,
            pats: vec![],
            exact: HashMap::new(),
            squash: HashMap::new(),
            max_len: 5,
            spoken_punctuation: true,
        };
        for (ei, e) in d.entries.iter().enumerate() {
            let snippet = e.kind == DictKind::Snippet;
            let forms = std::iter::once(&e.text).chain(if snippet { [].iter() } else { e.aliases.iter() });
            for form in forms {
                let toks: Vec<String> = tokenize(form).into_iter().map(|t| t.norm).collect();
                if toks.is_empty() {
                    continue;
                }
                let pi = d.pats.len();
                d.max_len = d.max_len.max(toks.len());
                d.exact.entry(toks.join(" ")).or_default().push(pi);
                if !snippet {
                    d.squash.entry(toks.concat()).or_default().push(pi);
                }
                d.pats.push(Pat { entry: ei, sq: toks.concat().chars().collect(), sq_s: toks.concat(), ntoks: toks.len(), snippet });
            }
        }
        d
    }

    pub fn hints(&self) -> Hints {
        let mut words: Vec<&DictEntry> = self.entries.iter().filter(|e| e.kind == DictKind::Word).collect();
        words.sort_by(|a, b| {
            (b.starred, b.uses, b.created_at, b.id).cmp(&(a.starred, a.uses, a.created_at, a.id))
        });
        Hints {
            terms: words.iter().map(|e| e.text.clone()).collect(),
            aliases: words.iter().flat_map(|e| e.aliases.iter().map(|a| (a.clone(), e.text.clone()))).collect(),
            triggers: self.entries.iter().filter(|e| e.kind == DictKind::Snippet).map(|e| e.text.clone()).collect(),
        }
    }

    /// Spoken punctuation + corrections + snippets expanded.
    pub fn apply(&self, text: &str) -> Applied {
        let t = self.punct(text);
        let (text, matches, _, _) = self.run(&t, Mode::Expand);
        Applied { text, matches }
    }

    /// Spoken punctuation + corrections + snippet placeholders.
    pub fn prepare(&self, raw: &str) -> Prepared {
        let t = self.punct(raw);
        let (text, matches, slots, standalone) = self.run(&t, Mode::Placeholder);
        let used = matches.iter().map(|m| m.entry_id).collect();
        Prepared { text, slots, standalone, used }
    }

    /// Corrections again (the LLM can misspell), then the placeholders expanded.
    /// None = a placeholder was lost, duplicated or invented: use the no-LLM path.
    pub fn finish(&self, llm_out: &str, p: &Prepared) -> Option<String> {
        let mut seen = vec![0u32; p.slots.len()];
        let mut rest = llm_out;
        while let Some(a) = rest.find('⟦') {
            let after = &rest[a + '⟦'.len_utf8()..];
            let b = after.find('⟧')?;
            let n: usize = after[..b].trim().parse().ok()?;
            *seen.get_mut(n.checked_sub(1)?)? += 1;
            rest = &after[b..];
        }
        if seen.iter().any(|&c| c != 1) {
            return None;
        }
        let (mut out, ..) = self.run(llm_out, Mode::WordsOnly);
        for (i, slot) in p.slots.iter().enumerate() {
            out = out.replace(&format!("⟦{}⟧", i + 1), slot);
        }
        Some(out)
    }

    fn punct(&self, t: &str) -> String {
        if self.spoken_punctuation { text::spoken_punctuation(t) } else { t.to_string() }
    }

    fn rank(&self, c: &Cand) -> (u8, usize, bool, std::cmp::Reverse<u32>) {
        let e = &self.entries[self.pats[c.pat].entry];
        let tier = match c.how {
            MatchHow::Exact | MatchHow::Snippet => 0,
            MatchHow::Squashed => 1,
            MatchHow::Fuzzy => 2,
        };
        (tier, c.dist, !e.starred, std::cmp::Reverse(e.uses))
    }

    /// Best match for one window of normalized words.
    fn best(&self, words: &[&str], mode: Mode) -> Option<Cand> {
        let mut cands = vec![];
        for &pi in self.exact.get(&words.join(" ")).into_iter().flatten() {
            let snippet = self.pats[pi].snippet;
            if !(snippet && mode == Mode::WordsOnly) {
                cands.push(Cand { pat: pi, how: if snippet { MatchHow::Snippet } else { MatchHow::Exact }, dist: 0 });
            }
        }
        let sq = words.concat();
        for &pi in self.squash.get(&sq).into_iter().flatten() {
            cands.push(Cand { pat: pi, how: MatchHow::Squashed, dist: 0 });
        }
        if cands.is_empty() && self.opts.fuzzy && !(words.len() == 1 && is_common(words[0])) {
            let wc: Vec<char> = sq.chars().collect();
            for (pi, p) in self.pats.iter().enumerate() {
                if p.snippet || self.entries[p.entry].exact_only {
                    continue;
                }
                let n = words.len();
                // Splitting a word is a mishearing ("conta flo"); gluing on a common word is not ("Fernando y").
                if n > p.ntoks && (is_common(words[0]) || is_common(words[n - 1])) {
                    continue;
                }
                let Some(d) = fuzzy_d(&wc, &p.sq) else { continue };
                // Any form of this entry + an ending ("contaflows") is another word, not a mishearing.
                if self.pats.iter().any(|q| q.entry == p.entry && sq.len() > q.sq_s.len() && sq.starts_with(&q.sq_s)) {
                    continue;
                }
                // A shorter window that matches as well wins: "contaflo is" must not eat "is".
                let shorter_ok = n > 1
                    && [&words[1..], &words[..n - 1]].iter().any(|w| {
                        let s = w.concat();
                        s == p.sq_s || fuzzy_d(&s.chars().collect::<Vec<_>>(), &p.sq).is_some_and(|d2| d2 <= d)
                    });
                if !shorter_ok {
                    cands.push(Cand { pat: pi, how: MatchHow::Fuzzy, dist: d });
                }
            }
        }
        cands.into_iter().min_by_key(|c| self.rank(c))
    }

    /// (len, suffix kept, cand) at token i: longest window first (PLAN 2.3).
    fn best_at(&self, s: &str, toks: &[Token], i: usize, mode: Mode) -> Option<(usize, bool, Cand)> {
        let mut maxl = 1;
        while maxl < self.max_len && i + maxl < toks.len() && sep_ok(&s[toks[i + maxl - 1].end..toks[i + maxl].start]) {
            maxl += 1;
        }
        for l in (1..=maxl).rev() {
            let w = &toks[i..i + l];
            // Middle tokens never carry a suffix; the last one may keep its 's ("john's" -> "John's").
            for base in [false, true] {
                if base && w[l - 1].suffix_at.is_none() {
                    continue;
                }
                let words: Vec<&str> =
                    w.iter().enumerate().map(|(k, t)| if base && k == l - 1 { t.base() } else { t.norm.as_str() }).collect();
                if let Some(c) = self.best(&words, mode) {
                    return Some((l, base, c));
                }
            }
        }
        None
    }

    fn expansion(&self, e: &DictEntry) -> String {
        expand_vars(e.expansion.as_deref().unwrap_or(""), chrono::Local::now())
    }

    /// (output, matches, slots, standalone)
    fn run(&self, s: &str, mode: Mode) -> (String, Vec<Match>, Vec<String>, bool) {
        let toks = tokenize(s);
        let mut slots = vec![];
        // Whole utterance == a trigger (punctuation ignored): exactly the expansion, no period.
        if mode != Mode::WordsOnly && !toks.is_empty() {
            let k = toks.iter().map(|t| t.norm.as_str()).collect::<Vec<_>>().join(" ");
            if let Some(&pi) = self.exact.get(&k).into_iter().flatten().find(|&&pi| self.pats[pi].snippet) {
                let e = &self.entries[self.pats[pi].entry];
                let exp = self.expansion(e);
                let out = if mode == Mode::Expand {
                    exp
                } else {
                    slots.push(exp);
                    "⟦1⟧".to_string()
                };
                let m = Match { start: 0, end: out.chars().count() as u32, entry_id: e.id, how: MatchHow::Snippet };
                return (out, vec![m], slots, true);
            }
        }
        let mut out = String::new();
        let mut chars = 0usize;
        let mut matches = vec![];
        let mut pos = 0;
        let mut i = 0;
        let mut push = |out: &mut String, piece: &str| {
            out.push_str(piece);
            chars += piece.chars().count();
            chars
        };
        while i < toks.len() {
            let Some((l, base, c)) = self.best_at(s, &toks, i, mode) else {
                i += 1;
                continue;
            };
            let last = &toks[i + l - 1];
            let start = push(&mut out, &s[pos..toks[i].start]);
            let e = &self.entries[self.pats[c.pat].entry];
            let repl = match (&c.how, mode) {
                (&MatchHow::Snippet, Mode::Placeholder) => {
                    slots.push(self.expansion(e));
                    format!("⟦{}⟧", slots.len())
                }
                (&MatchHow::Snippet, _) => self.expansion(e),
                _ => e.text.clone(),
            };
            let end = push(&mut out, &repl);
            if base {
                push(&mut out, &s[last.suffix_at.unwrap()..last.end]);
            }
            matches.push(Match { start: start as u32, end: end as u32, entry_id: e.id, how: c.how });
            pos = last.end;
            i += l;
        }
        push(&mut out, &s[pos..]);
        (out, matches, slots, false)
    }
}

/// `{date}` (YYYY-MM-DD), `{time}` (HH:MM), `{weekday}`, in local time.
/// ponytail: chrono gives English weekday names only; localize if Spanish users ask.
fn expand_vars<Tz: chrono::TimeZone>(s: &str, now: chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    s.replace("{date}", &now.format("%Y-%m-%d").to_string())
        .replace("{time}", &now.format("%H:%M").to_string())
        .replace("{weekday}", &now.format("%A").to_string())
}

/// whisper `initial_prompt` / OpenAI-compat `prompt`: natural preceding text, because whisper copies
/// its style. Terms are added in rank order while `count(prompt) <= max_tokens`. Empty if none fits.
/// Only "es"/"en" get a seed sentence; other languages and auto get the bare list, so an English seed
/// does not pull Spanish speech toward English.
pub fn whisper_prompt(h: &Hints, lang: Option<&str>, max_tokens: usize, count: &dyn Fn(&str) -> usize) -> String {
    let seed = match lang {
        Some("es") => "Hola, ¿qué tal? Hoy hablamos de ",
        Some("en") => "Hi, today we talk about ",
        _ => "",
    };
    let mut best = String::new();
    for k in 1..=h.terms.len() {
        let cand = format!("{seed}{}.", h.terms[..k].join(", "));
        if count(&cand) > max_tokens {
            break;
        }
        best = cand;
    }
    best
}

/// ElevenLabs keyterms: drop `<>{}[]\`, keep < 50 chars and <= 5 words, dedupe, cap at `max`.
pub fn keyterms(h: &Hints, max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    h.terms
        .iter()
        .filter_map(|t| {
            let c: String = t.chars().filter(|c| !"<>{}[]\\".contains(*c)).collect();
            let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
            let ok = !c.is_empty() && c.chars().count() < 50 && c.split(' ').count() <= 5;
            (ok && seen.insert(c.clone())).then_some(c)
        })
        .take(max)
        .collect()
}

// ---------- validation ----------

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_form(s: &str) -> Result<(), String> {
    let n = s.chars().count();
    if n == 0 {
        Err("Can't be empty".into())
    } else if n > 60 {
        Err(format!("Max 60 characters: \"{s}\""))
    } else if !s.chars().any(char::is_alphanumeric) {
        Err(format!("Needs at least one letter or number: \"{s}\""))
    } else {
        Ok(())
    }
}

fn tab(kind: &DictKind) -> &'static str {
    match kind {
        DictKind::Word => "Words",
        DictKind::Snippet => "Snippets",
    }
}

/// Cleans the input (trim, collapse spaces, dedupe aliases) and checks it against the other entries
/// (PLAN 2.1). Errors name the entry that conflicts.
pub fn validate(input: &DictEntryInput, existing: &[DictEntry]) -> Result<DictEntryInput, String> {
    let mut v = input.clone();
    v.text = clean(&v.text);
    check_form(&v.text)?;
    let text_key = key(&v.text);
    let mut aliases: Vec<String> = vec![];
    if v.kind == DictKind::Word {
        v.expansion = None;
        for a in v.aliases.iter().map(|a| clean(a)).filter(|a| !a.is_empty()) {
            check_form(&a)?;
            if key(&a) == text_key {
                return Err(format!("The correction \"{a}\" is the same as \"{}\"", v.text));
            }
            if !aliases.iter().any(|x| key(x) == key(&a)) {
                aliases.push(a);
            }
        }
    } else {
        let exp = v.expansion.as_deref().unwrap_or("");
        if exp.trim().is_empty() {
            return Err("A snippet needs an expansion".into());
        }
        if exp.chars().count() > 4000 {
            return Err("Max 4000 characters in the expansion".into());
        }
    }
    v.aliases = aliases;
    let mine: Vec<(String, &str)> =
        std::iter::once(&v.text).chain(&v.aliases).map(|s| (key(s), s.as_str())).collect();
    for o in existing.iter().filter(|o| Some(o.id) != v.id) {
        let theirs = std::iter::once((key(&o.text), tab(&o.kind).to_string()))
            .chain(o.aliases.iter().map(|a| (key(a), format!("Corrections (\"{a}\" → \"{}\")", o.text))));
        for (k, label) in theirs {
            if let Some((_, s)) = mine.iter().find(|(m, _)| *m == k) {
                return Err(if label.starts_with("Corrections") {
                    format!("\"{s}\" is already in {label}")
                } else {
                    format!("\"{s}\" is already in {label} (\"{}\")", o.text)
                });
            }
        }
    }
    Ok(v)
}

// ---------- storage (`dict` table) ----------

fn kind_str(k: &DictKind) -> &'static str {
    match k {
        DictKind::Word => "word",
        DictKind::Snippet => "snippet",
    }
}

pub fn load(conn: &Connection) -> rusqlite::Result<Vec<DictEntry>> {
    let mut st = conn.prepare(
        "SELECT id, kind, text, aliases, expansion, exact_only, starred, uses, created_at FROM dict ORDER BY id",
    )?;
    let rows = st.query_map([], |r| {
        let aliases: String = r.get(3)?;
        Ok(DictEntry {
            id: r.get(0)?,
            kind: if r.get::<_, String>(1)? == "snippet" { DictKind::Snippet } else { DictKind::Word },
            text: r.get(2)?,
            aliases: serde_json::from_str(&aliases).unwrap_or_default(),
            expansion: r.get(4)?,
            exact_only: r.get(5)?,
            starred: r.get(6)?,
            uses: r.get(7)?,
            created_at: r.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn upsert(conn: &Connection, input: &DictEntryInput) -> Result<DictEntry, String> {
    let existing = load(conn).map_err(|e| e.to_string())?;
    let v = validate(input, &existing)?;
    let aliases = serde_json::to_string(&v.aliases).map_err(|e| e.to_string())?;
    let (kind, norm) = (kind_str(&v.kind), key(&v.text));
    let res = match v.id {
        Some(id) => conn.execute(
            "UPDATE dict SET kind=?1, text=?2, norm=?3, aliases=?4, expansion=?5, exact_only=?6, starred=?7 WHERE id=?8",
            params![kind, v.text, norm, aliases, v.expansion, v.exact_only, v.starred, id],
        ),
        None => conn.execute(
            "INSERT INTO dict(kind, text, norm, aliases, expansion, exact_only, starred, created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![kind, v.text, norm, aliases, v.expansion, v.exact_only, v.starred, chrono::Utc::now().timestamp()],
        ),
    };
    if res.map_err(|e| e.to_string())? == 0 {
        return Err(format!("No entry with id {}", v.id.unwrap_or_default()));
    }
    let id = v.id.unwrap_or_else(|| conn.last_insert_rowid());
    load(conn).map_err(|e| e.to_string())?.into_iter().find(|e| e.id == id).ok_or_else(|| "entry vanished".into())
}

pub fn delete(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    for id in ids {
        conn.execute("DELETE FROM dict WHERE id = ?1", [id])?;
    }
    Ok(())
}

/// +1 `uses` per matched id (repeat an id to count it twice). WP5 calls it once at the end of a session
/// with `Prepared.used`.
pub fn record_uses(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    for id in ids {
        conn.execute("UPDATE dict SET uses = uses + 1 WHERE id = ?1", [id])?;
    }
    Ok(())
}

// ---------- commands ----------

#[tauri::command]
pub fn dict_list(state: State<'_, AppState>) -> Result<Vec<DictEntry>, String> {
    load(&*state.db.lock().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dict_upsert(state: State<'_, AppState>, entry: DictEntryInput) -> Result<DictEntry, String> {
    upsert(&*state.db.lock().map_err(|e| e.to_string())?, &entry)
}

#[tauri::command]
pub fn dict_delete(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), String> {
    delete(&*state.db.lock().map_err(|e| e.to_string())?, &ids).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dict_preview(state: State<'_, AppState>, text: String) -> Result<Applied, String> {
    let entries = load(&*state.db.lock().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let s = state.settings.read().map_err(|e| e.to_string())?;
    let mut d = Dictionary::new(entries, s.dictionary.clone());
    d.spoken_punctuation = s.spoken_punctuation;
    Ok(d.apply(&text))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, kind: DictKind, text: &str, aliases: &[&str], expansion: Option<&str>) -> DictEntry {
        DictEntry {
            id,
            kind,
            text: text.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            expansion: expansion.map(Into::into),
            exact_only: false,
            starred: false,
            uses: 0,
            created_at: id,
        }
    }
    fn w(id: i64, text: &str, aliases: &[&str]) -> DictEntry {
        entry(id, DictKind::Word, text, aliases, None)
    }
    fn sn(id: i64, trigger: &str, exp: &str) -> DictEntry {
        entry(id, DictKind::Snippet, trigger, &[], Some(exp))
    }
    const CAL: &str = "https://calendly.com/pablo";

    fn sample() -> Vec<DictEntry> {
        let mut kube = w(11, "Kubernetes", &[]);
        kube.exact_only = true;
        vec![
            w(1, "Contaflow", &["contra flow"]),
            w(2, "Pablo", &[]),
            w(3, "Pablo Pérez", &[]),
            w(4, "John", &[]),
            w(5, "Don", &[]),
            w(6, "Tauri", &[]),
            w(7, "kubectl", &[]),
            w(8, "iPhone", &[]),
            w(9, "Comforte", &[]),
            w(10, "Node.js", &[]),
            kube,
            w(12, "José Martínez", &[]),
            w(13, "Ñandú Labs", &[]),
            w(14, "Fernández", &[]),
            w(15, "Supabase", &[]),
            sn(20, "my calendly", CAL),
            sn(21, "mi firma", "Saludos,\nPablo"),
            sn(22, "fecha de hoy", "Hoy es {date}"),
        ]
    }
    fn dict() -> Dictionary {
        Dictionary::new(sample(), DictOpts { fuzzy: true })
    }

    #[test]
    fn corrections_table() {
        let d = dict();
        let cases = [
            // whole-word, case-insensitive
            ("contaflow is great", "Contaflow is great"),
            ("I use CONTAFLOW.", "I use Contaflow."),
            ("contaflowing is a verb", "contaflowing is a verb"),
            ("two contaflows", "two contaflows"),
            // squashed: spacing, hyphens, punctuation inside the match
            ("conta flow rocks", "Contaflow rocks"),
            ("Conta-Flow rocks", "Contaflow rocks"),
            ("conta, flow rocks", "Contaflow rocks"),
            ("Hola, conta flow funciona.", "Hola, Contaflow funciona."),
            // alias
            ("contra flow rocks", "Contaflow rocks"),
            ("Contra-flow rocks", "Contaflow rocks"),
            // fuzzy
            ("Contaflo is down", "Contaflow is down"),
            ("conta flo is down", "Contaflow is down"),
            ("cotnaflow", "Contaflow"),
            ("the supabace db", "the Supabase db"),
            ("hablé con fernandes ayer", "hablé con Fernández ayer"),
            ("I need comfort", "I need comfort"),
            ("kubernets cluster", "kubernets cluster"),
            ("kubernetes cluster", "Kubernetes cluster"),
            // diacritics, both directions
            ("pablo perez wrote it", "Pablo Pérez wrote it"),
            ("PABLO PÉREZ", "Pablo Pérez"),
            ("jose martinez llegó tarde", "José Martínez llegó tarde"),
            ("fernandez", "Fernández"),
            ("ñandu labs", "Ñandú Labs"),
            ("nandu labs", "Ñandú Labs"),
            // longest wins, fuzzy multi-token beats shorter exact
            ("pablo said hi", "Pablo said hi"),
            ("ask pablo pérez", "ask Pablo Pérez"),
            ("Pablo Peres says hi", "Pablo Pérez says hi"),
            // sentence end is a barrier
            ("pablo. perez", "Pablo. perez"),
            // possessive and contraction
            ("john's car", "John's car"),
            ("John’s car", "John’s car"),
            ("johns car", "johns car"),
            ("contaflow's api", "Contaflow's api"),
            ("I don't know", "I don't know"),
            ("don is here", "Don is here"),
            // stored casing, even at a sentence start
            ("Kubectl apply", "kubectl apply"),
            ("Iphone broke. my iphone too", "iPhone broke. my iPhone too"),
            ("node js is fast", "Node.js is fast"),
            ("node.js is fast", "Node.js is fast"),
            ("tauri app", "Tauri app"),
            // snippets inside a sentence
            ("here is my calendly, thanks", &format!("here is {CAL}, thanks")),
            ("te dejo mi firma", "te dejo Saludos,\nPablo"),
            // spoken punctuation runs first
            ("contaflow comma", "Contaflow,"),
            ("hola conta flow punto y aparte adiós", "hola Contaflow.\nAdiós"),
        ];
        for (input, want) in cases {
            assert_eq!(d.apply(input).text, want, "input: {input:?}");
        }
    }

    #[test]
    fn prose_has_no_false_positives() {
        let d = dict();
        for s in [
            "The conference is about comfortable shoes and contract law.",
            "Tomorrow I will call Paula and Pedro about the database.",
            "Necesito hablar con Fernando y con Fernanda mañana.",
            "Contamos con un contrato y un contacto en la base de datos.",
            "Don't worry, the flow of data is fine. Superb.",
            "The period of time was long, comma separated values are fine.",
            "Pablo's notes",
            "Ese es el punto, nos vemos en punto.",
            "Contact the kubernetes team about Tau Ceti.",
        ] {
            let want = s.replace("kubernetes", "Kubernetes");
            assert_eq!(d.apply(s).text, want, "input: {s:?}");
        }
    }

    #[test]
    fn match_offsets_and_tiers() {
        let d = dict();
        let m = |s: &str| d.apply(s).matches;
        assert_eq!(m("I use contaflow daily"), [Match { start: 6, end: 15, entry_id: 1, how: MatchHow::Exact }]);
        assert_eq!(m("conta flow")[0].how, MatchHow::Squashed);
        assert_eq!(m("contra flow")[0].how, MatchHow::Exact);
        assert_eq!(m("contaflo")[0].how, MatchHow::Fuzzy);
        // char offsets, not bytes
        assert_eq!(m("pablo perez está"), [Match { start: 0, end: 11, entry_id: 3, how: MatchHow::Exact }]);
        let a = d.apply("¿Ñandu labs? john's");
        assert_eq!(a.text, "¿Ñandú Labs? John's");
        assert_eq!((a.matches[0].start, a.matches[0].end), (1, 11));
        assert_eq!((a.matches[1].start, a.matches[1].end), (13, 17));
        assert_eq!(m("my calendly")[0].how, MatchHow::Snippet);
        assert!(m("nothing here").is_empty());
    }

    #[test]
    fn ties() {
        // exact beats squashed at the same window length
        let d = Dictionary::new(vec![w(1, "Contaflow", &[]), w(2, "Conta Flow", &[])], DictOpts { fuzzy: true });
        assert_eq!(d.apply("conta flow").text, "Conta Flow");
        // fuzzy tie: starred wins, then uses
        let mut a = w(1, "Contaflow", &[]);
        let mut b = w(2, "Contaflaw", &[]);
        b.starred = true;
        let d = Dictionary::new(vec![a.clone(), b.clone()], DictOpts { fuzzy: true });
        assert_eq!(d.apply("contaflew").text, "Contaflaw");
        b.starred = false;
        a.uses = 5;
        let d = Dictionary::new(vec![a, b], DictOpts { fuzzy: true });
        assert_eq!(d.apply("contaflew").text, "Contaflow");
    }

    #[test]
    fn fuzzy_off() {
        let d = Dictionary::new(sample(), DictOpts { fuzzy: false });
        assert_eq!(d.apply("Contaflo is down").text, "Contaflo is down");
        assert_eq!(d.apply("conta flow").text, "Contaflow");
    }

    #[test]
    fn spoken_punctuation_can_be_off() {
        let mut d = dict();
        d.spoken_punctuation = false;
        assert_eq!(d.apply("contaflow comma").text, "Contaflow comma");
    }

    #[test]
    fn snippets_standalone_and_date() {
        let d = dict();
        for s in ["my calendly", "My calendly.", "my calendly period", "  My, calendly! "] {
            assert_eq!(d.apply(s).text, CAL, "input: {s:?}");
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert_eq!(d.apply("Fecha de hoy.").text, format!("Hoy es {today}"));
        let t = chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap().and_hms_opt(9, 5, 0).unwrap().and_utc();
        assert_eq!(expand_vars("{weekday} {date} {time}", t), "Saturday 2026-10-17 09:05");
    }

    #[test]
    fn placeholders_round_trip() {
        let d = dict();
        let p = d.prepare("send my calendly to jose martinez");
        assert_eq!(p.text, "send ⟦1⟧ to José Martínez");
        assert_eq!(p.slots, [CAL]);
        assert!(!p.standalone);
        assert_eq!(p.used, [20, 12]);
        // the LLM rewrote and misspelled; corrections run again
        assert_eq!(d.finish("Send ⟦1⟧ to José Martines, contaflo.", &p).as_deref(), Some(&*format!("Send {CAL} to José Martínez, Contaflow.")));
        // lost, duplicated, invented, mangled
        for bad in ["Send it to José.", "⟦1⟧ ⟦1⟧", "⟦1⟧ ⟦2⟧", "Send [1] to José", "⟦x⟧ ⟦1⟧", "⟦0⟧ ⟦1⟧"] {
            assert_eq!(d.finish(bad, &p), None, "llm: {bad:?}");
        }
        let p2 = d.prepare("my calendly and mi firma");
        assert_eq!(p2.text, "⟦1⟧ and ⟦2⟧");
        assert_eq!(d.finish("⟦2⟧ y ⟦1⟧", &p2).unwrap(), format!("Saludos,\nPablo y {CAL}"));
        // no snippets: finish is just corrections
        let p3 = d.prepare("conta flow");
        assert_eq!(p3.slots.len(), 0);
        assert_eq!(d.finish("Contaflo.", &p3).as_deref(), Some("Contaflow."));
        assert_eq!(d.finish("⟦1⟧", &p3), None);
        let ps = d.prepare("My calendly.");
        assert!(ps.standalone);
        assert_eq!((ps.text.as_str(), ps.slots.as_slice()), ("⟦1⟧", [CAL.to_string()].as_slice()));
        assert_eq!(d.finish("⟦1⟧", &ps).as_deref(), Some(CAL));
    }

    fn input(kind: DictKind, text: &str, aliases: &[&str], expansion: Option<&str>) -> DictEntryInput {
        DictEntryInput {
            id: None,
            kind,
            text: text.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            expansion: expansion.map(Into::into),
            exact_only: false,
            starred: false,
        }
    }

    #[test]
    fn validation_table() {
        let ex = sample();
        let word = |t: &str, a: &[&str]| input(DictKind::Word, t, a, None);
        let bad = [
            (word("   ", &[]), "empty"),
            (word("!!! ...", &[]), "letter or number"),
            (word(&"a".repeat(61), &[]), "Max 60"),
            (word("Ok", &["x".repeat(61).as_str()]), "Max 60"),
            (word("Ok", &["¿?"]), "letter or number"),
            (input(DictKind::Snippet, "contaflow", &[], Some("x")), "already in Words (\"Contaflow\")"),
            (word("My Calendly", &[]), "already in Snippets"),
            (word("pablo perez", &[]), "Pablo Pérez"),
            (word("Conta-Flow-X", &["Contra  Flow"]), "already in Corrections (\"contra flow\" → \"Contaflow\")"),
            (word("Kontaflow", &["contaflow"]), "already in Words (\"Contaflow\")"),
            (word("Tauri App", &["tauri-app"]), "same as"),
            (input(DictKind::Snippet, "sig", &[], None), "needs an expansion"),
            (input(DictKind::Snippet, "sig", &[], Some(&"x".repeat(4001))), "4000"),
        ];
        for (inp, want) in bad {
            let err = validate(&inp, &ex).expect_err(&format!("should fail: {inp:?}"));
            assert!(err.contains(want), "{inp:?}: {err:?} should contain {want:?}");
        }
        // cleaned, deduped, empty aliases dropped; 60 chars is fine
        let ok = validate(&word("  Pablo   Gómez ", &["pablo  gomes", "Pablo Gomes", " "]), &ex).unwrap();
        assert_eq!((ok.text.as_str(), ok.aliases.as_slice()), ("Pablo Gómez", ["pablo gomes".to_string()].as_slice()));
        assert!(validate(&word(&"a".repeat(60), &[]), &ex).is_ok());
        // updating an entry does not conflict with itself
        let mut upd = word("Contaflow", &["contra flow", "conta flou"]);
        upd.id = Some(1);
        assert!(validate(&upd, &ex).is_ok());
        // a word never carries an expansion, a snippet never carries aliases
        let s = validate(&input(DictKind::Snippet, "firma pro", &["x"], Some("Hi")), &ex).unwrap();
        assert!(s.aliases.is_empty());
    }

    #[test]
    fn storage_round_trip() {
        let c = crate::db::open_memory().unwrap();
        let a = upsert(&c, &input(DictKind::Word, "Contaflow", &["contra flow"], None)).unwrap();
        assert_eq!((a.text.as_str(), a.aliases.len(), a.uses), ("Contaflow", 1, 0));
        let s = upsert(&c, &input(DictKind::Snippet, "my calendly", &[], Some(CAL))).unwrap();
        assert!(upsert(&c, &input(DictKind::Word, "Contra-Flow", &[], None)).unwrap_err().contains("Corrections"));
        let mut edit = input(DictKind::Word, "Contaflow", &["contra flow", "conta flou"], None);
        edit.id = Some(a.id);
        edit.starred = true;
        let a2 = upsert(&c, &edit).unwrap();
        assert!(a2.starred && a2.aliases.len() == 2 && a2.created_at == a.created_at);
        let mut ghost = input(DictKind::Word, "Ghost", &[], None);
        ghost.id = Some(999);
        assert!(upsert(&c, &ghost).is_err());
        record_uses(&c, &[a.id, a.id, s.id]).unwrap();
        let all = load(&c).unwrap();
        assert_eq!(all.iter().map(|e| e.uses).collect::<Vec<_>>(), [2, 1]);
        let norm: String = c.query_row("SELECT norm FROM dict WHERE id=?1", [s.id], |r| r.get(0)).unwrap();
        assert_eq!(norm, "my calendly");
        delete(&c, &[a.id]).unwrap();
        assert_eq!(load(&c).unwrap().len(), 1);
    }

    #[test]
    fn hints_ranking() {
        let mut e = sample();
        e[5].starred = true; // Tauri
        e[0].uses = 7; // Contaflow
        let h = Dictionary::new(e, DictOpts { fuzzy: true }).hints();
        assert_eq!(&h.terms[..3], ["Tauri", "Contaflow", "Supabase"]); // starred, uses, then newest
        assert!(!h.terms.iter().any(|t| t == "contra flow" || t == "my calendly"));
        assert_eq!(h.aliases, [("contra flow".to_string(), "Contaflow".to_string())]);
        assert_eq!(h.triggers, ["my calendly", "mi firma", "fecha de hoy"]);
    }

    #[test]
    fn whisper_prompt_builder() {
        let h = Hints {
            terms: vec!["Contaflow".into(), "Tauri".into(), "Pablo Pérez".into()],
            aliases: vec![("contra flow".into(), "Contaflow".into())],
            triggers: vec![],
        };
        let words = |s: &str| s.split_whitespace().count();
        assert_eq!(whisper_prompt(&h, Some("es"), 200, &words), "Hola, ¿qué tal? Hoy hablamos de Contaflow, Tauri, Pablo Pérez.");
        assert_eq!(whisper_prompt(&h, Some("en"), 200, &words), "Hi, today we talk about Contaflow, Tauri, Pablo Pérez.");
        assert_eq!(whisper_prompt(&h, None, 200, &words), "Contaflow, Tauri, Pablo Pérez.");
        // the cap is respected: 6 seed words + 2 terms
        let p = whisper_prompt(&h, Some("en"), 7, &words);
        assert_eq!(p, "Hi, today we talk about Contaflow, Tauri.");
        assert!(words(&p) <= 7);
        assert_eq!(whisper_prompt(&h, Some("en"), 3, &words), "");
        assert!(!whisper_prompt(&h, Some("en"), 200, &words).contains("contra"));
        // starred first, through hints()
        let mut e = vec![w(1, "Alpha", &["alfa"]), w(2, "Beta", &[])];
        e[1].starred = true;
        let h = Dictionary::new(e, DictOpts { fuzzy: true }).hints();
        assert_eq!(whisper_prompt(&h, None, 200, &words), "Beta, Alpha.");
    }

    #[test]
    fn keyterms_sanitized_and_capped() {
        let h = Hints {
            terms: vec![
                "Con<ta>flow".into(),
                "one two three four five six".into(),
                "x".repeat(50),
                "[]{}".into(),
                "Tauri".into(),
                "Tauri".into(),
                "  spaced   term ".into(),
            ],
            ..Default::default()
        };
        assert_eq!(keyterms(&h, 10), ["Contaflow", "Tauri", "spaced term"]);
        assert_eq!(keyterms(&h, 2), ["Contaflow", "Tauri"]);
        assert!(keyterms(&h, 0).is_empty());
    }

    #[test]
    fn dl_distance() {
        let d = |a: &str, b: &str| dl(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>());
        assert_eq!(d("contaflow", "contaflow"), 0);
        assert_eq!(d("contaflo", "contaflow"), 1);
        assert_eq!(d("cotnaflow", "contaflow"), 1); // transposition
        assert_eq!(d("", "abc"), 3);
        assert!(is_common("comfort") && is_common("tambien") && !is_common("contaflow"));
    }
}
