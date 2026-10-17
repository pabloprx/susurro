//! Dictionary that learns from corrections (Wispr's "Flow learns from edits").
//! After a paste, `watch` polls the focused field's AXValue once a second for 30 s. When the user retypes a
//! pasted word or phrase as a close spelling ("conta flow" -> "Contaflow"), a toast offers to add it, with the
//! misheard form as a correction. Never reads a secure field. The field text stays in this thread's memory
//! for the 30 s only; nothing but the changed span (word + heard) leaves it.

use crate::contract::{DictEntry, DictEntryInput, DictKind, ToastKind};
use crate::dictionary::{self, key};
use crate::text::norm;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// What the user wrote: the dictionary word.
    pub word: String,
    /// What susurro pasted: the correction (alias).
    pub heard: String,
}

/// Most words on one side of a replaced span. More is a rewrite, not a spelling fix.
const MAX_SPAN_WORDS: usize = 3;

/// Strip punctuation at both ends of a word ("flow," -> "flow").
fn bare(w: &str) -> &str {
    w.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Lowercase, accents folded, letters and digits only, then a rough EN/ES sound fold: v/b, c/k/qu, z/s, silent h,
/// ll/y/i, ph/f, doubled letters once. ponytail: hand rules, swap for a real phonetic code if misses show up.
fn fold(s: &str) -> Vec<char> {
    let mut t: String = norm(s).chars().filter(|c| c.is_alphanumeric()).collect();
    for (a, b) in [("ph", "f"), ("qu", "k"), ("ck", "k"), ("ll", "y"), ("sh", "x")] {
        t = t.replace(a, b);
    }
    let mut out: Vec<char> = vec![];
    for c in t.chars() {
        let c = match c {
            'v' => 'b',
            'c' | 'q' => 'k',
            'z' => 's',
            'w' => 'u',
            'y' => 'i',
            'h' => continue,
            c => c,
        };
        if out.last() != Some(&c) {
            out.push(c);
        }
    }
    out
}

/// Close enough to be the same word misheard: at most one sound edit per three letters.
fn sounds_close(a: &str, b: &str) -> bool {
    let (fa, fb) = (fold(a), fold(b));
    if fa.is_empty() || fb.is_empty() {
        return false;
    }
    dictionary::dl(&fa, &fb) * 3 <= fa.len().max(fb.len())
}

/// Word-level diff (LCS, words compared case-insensitively without end punctuation).
/// Returns the replaced hunks: (old words, new words), both sides non-empty.
fn hunks<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(Vec<&'a str>, Vec<&'a str>)> {
    let eq = |a: &str, b: &str| bare(a).to_lowercase() == bare(b).to_lowercase();
    let (n, m) = (old.len(), new.len());
    let mut l = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i][j] = if eq(old[i], new[j]) { l[i + 1][j + 1] + 1 } else { l[i + 1][j].max(l[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = vec![];
    let mut cur: (Vec<&str>, Vec<&str>) = (vec![], vec![]);
    let flush = |cur: &mut (Vec<&'a str>, Vec<&'a str>), out: &mut Vec<_>| {
        let c = std::mem::take(cur);
        if !c.0.is_empty() && !c.1.is_empty() {
            out.push(c);
        }
    };
    while i < n || j < m {
        if i < n && j < m && eq(old[i], new[j]) {
            flush(&mut cur, &mut out);
            (i, j) = (i + 1, j + 1);
        } else if j < m && (i == n || l[i][j + 1] >= l[i + 1][j]) {
            cur.1.push(new[j]);
            j += 1;
        } else {
            cur.0.push(old[i]);
            i += 1;
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Pure: what the pasted text became -> dictionary suggestions. Ignores case-only edits, deletions,
/// insertions, rewrites (far apart or longer than 3 words) and plain-word fixes (every new word is common).
pub fn suggest(pasted: &str, now: &str) -> Vec<Suggestion> {
    let (old, new): (Vec<&str>, Vec<&str>) = (pasted.split_whitespace().collect(), now.split_whitespace().collect());
    if old.len() > 400 || new.len() > 800 {
        return vec![];
    }
    let mut out: Vec<Suggestion> = vec![];
    for (o, n) in hunks(&old, &new) {
        if o.len() > MAX_SPAN_WORDS || n.len() > MAX_SPAN_WORDS {
            continue;
        }
        let join = |v: &[&str]| bare(&v.join(" ")).to_string();
        let (heard, word) = (join(&o), join(&n));
        // No letter = a number fix ("2025" -> "2026"), not a misheard word.
        if !word.chars().any(char::is_alphabetic) || heard.is_empty() || word.chars().count() > 60 || heard.to_lowercase() == word.to_lowercase() {
            continue;
        }
        if n.iter().all(|w| dictionary::is_common(&norm(bare(w)))) || !sounds_close(&heard, &word) {
            continue;
        }
        if !out.iter().any(|s| s.word == word) {
            out.push(Suggestion { word, heard });
        }
    }
    out
}

/// Pure: drop a suggestion whose heard form is already a word or a correction (the dictionary covers it).
pub fn is_new(s: &Suggestion, entries: &[DictEntry]) -> bool {
    let h = key(&s.heard);
    !entries.iter().any(|e| key(&e.text) == h || e.aliases.iter().any(|a| key(a) == h))
}

/// Pure: the entry to save for Add. An existing word gets the heard form as one more correction.
pub fn entry_for(s: &Suggestion, entries: &[DictEntry]) -> DictEntryInput {
    let wk = key(&s.word);
    let alias = (key(&s.heard) != wk).then(|| s.heard.clone());
    match entries.iter().find(|e| e.kind == DictKind::Word && key(&e.text) == wk) {
        Some(e) => DictEntryInput {
            id: Some(e.id),
            kind: DictKind::Word,
            text: e.text.clone(),
            aliases: e.aliases.iter().cloned().chain(alias).collect(),
            expansion: None,
            exact_only: e.exact_only,
            starred: e.starred,
        },
        None => DictEntryInput { id: None, kind: DictKind::Word, text: s.word.clone(), aliases: alias.into_iter().collect(), expansion: None, exact_only: false, starred: false },
    }
}

/// Pure: the entry that makes `heard` a correction of `word` (voice correct_word). None = the dictionary has it already.
/// Err = not learned: one of the 300 most frequent words as the heard form ("the" -> "Jev") would rewrite every dictation.
pub fn correction_for(word: &str, heard: &str, entries: &[DictEntry]) -> Result<Option<DictEntryInput>, String> {
    let (wk, hk) = (key(word), key(heard));
    let has = entries.iter().any(|e| e.kind == DictKind::Word && key(&e.text) == wk && (wk == hk || e.aliases.iter().any(|a| key(a) == hk)));
    if has {
        return Ok(None);
    }
    if heard.split_whitespace().all(|w| dictionary::is_top(&norm(bare(w)), 300)) {
        return Err(format!("\"{heard}\" is a common word, so it stays out of the dictionary"));
    }
    Ok(Some(entry_for(&Suggestion { word: word.into(), heard: heard.into() }, entries)))
}

/// Voice correction: saves `heard` as a correction of `word` (a new word if needed).
pub fn add_correction(app: &AppHandle, word: &str, heard: &str) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let entries = dictionary::load(&db).map_err(|e| e.to_string())?;
    match correction_for(word, heard, &entries)? {
        Some(e) => dictionary::upsert(&db, &e).map(|_| ()),
        None => Ok(()),
    }
}

/// Pure: the part of `now` that holds what was `base[start..end]` (the pasted span), by trimming the
/// common prefix and suffix. Edits outside the span widen it with extra words; the diff treats them as inserts.
pub fn current_span(base: &str, start: usize, end: usize, now: &str) -> String {
    let b: Vec<char> = base.chars().collect();
    let w: Vec<char> = now.chars().collect();
    let (s, e) = (base[..start].chars().count(), base[..end].chars().count());
    let mut p = b.iter().zip(&w).take_while(|(x, y)| x == y).count().min(s);
    let max_suf = (b.len() - e).min(w.len() - p);
    let q = b.iter().rev().zip(w.iter().rev()).take_while(|(x, y)| x == y).count().min(max_suf);
    p = p.min(w.len() - q);
    w[p..w.len() - q].iter().collect()
}

// ---------- live part ----------

static APP: OnceLock<AppHandle> = OnceLock::new();
static GEN: AtomicU64 = AtomicU64::new(0);
static PENDING: Mutex<Option<HashMap<String, Suggestion>>> = Mutex::new(None);

/// Setup: the watcher thread needs the handle for the toast and the db.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// After a successful Cmd+V into app `pid`. A newer paste stops the older watcher.
pub fn watch(pid: i32, pasted: &str) {
    let pasted = pasted.trim().to_string();
    if pasted.is_empty() || pasted.chars().count() > 4000 || APP.get().is_none() {
        return;
    }
    let ticket = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        if let Some(now) = mac::follow(pid, &pasted, ticket) {
            offer(suggest(&pasted, &now));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (pid, ticket);
    });
}

fn alive(ticket: u64) -> bool {
    GEN.load(Ordering::SeqCst) == ticket
}

fn offer(found: Vec<Suggestion>) {
    let Some(app) = APP.get() else { return };
    let state = app.state::<crate::AppState>();
    let entries = dictionary::load(&state.db.lock().unwrap()).unwrap_or_default();
    for s in found.into_iter().filter(|s| is_new(s, &entries)).take(2) {
        let id = format!("learn:{}", key(&s.word));
        let text = format!("Add {} to your dictionary? (heard: {})", s.word, s.heard);
        let mut p = PENDING.lock().unwrap();
        let m = p.get_or_insert_default();
        if m.len() > 20 {
            m.clear(); // toasts that timed out with no click
        }
        m.insert(id.clone(), s);
        drop(p);
        crate::session::toast(app, None, &id, ToastKind::Info, text, &[("add", "Add"), ("later", "Not now")], 12_000);
    }
}

/// Toast Add / Not now. true = the toast was ours.
pub fn handle_toast_action(app: &AppHandle, toast_id: &str, action_id: &str) -> bool {
    if !toast_id.starts_with("learn:") {
        return false;
    }
    let s = PENDING.lock().unwrap().as_mut().and_then(|m| m.remove(toast_id));
    if let (Some(s), "add") = (s, action_id) {
        let state = app.state::<crate::AppState>();
        let db = state.db.lock().unwrap();
        let res = dictionary::load(&db).map_err(|e| e.to_string()).and_then(|es| dictionary::upsert(&db, &entry_for(&s, &es)));
        drop(db);
        match res {
            Ok(_) => crate::session::toast(app, None, "learn-added", ToastKind::Success, format!("Added {} to your dictionary", s.word), &[], 3000),
            Err(e) => crate::session::toast(app, None, "learn-added", ToastKind::Error, format!("Not added: {e}"), &[], 5000),
        }
    }
    true
}

#[cfg(target_os = "macos")]
mod mac {
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use std::time::{Duration, Instant};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
    }

    const WINDOW: Duration = Duration::from_secs(30);
    const TICK: Duration = Duration::from_secs(1);
    /// A field bigger than this (a whole document) is not polled.
    const MAX_FIELD: usize = 200_000;

    fn attr(el: &CFType, name: &str) -> Option<CFType> {
        let mut out: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(el.as_CFTypeRef(), CFString::new(name).as_concrete_TypeRef(), &mut out) };
        (err == 0 && !out.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(out) })
    }
    fn string(el: &CFType, name: &str) -> String {
        attr(el, name).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string()).unwrap_or_default()
    }

    fn focused(app: &CFType) -> Option<CFType> {
        let el = attr(app, "AXFocusedUIElement")?;
        let secure = crate::ax::is_secure(&crate::ax::Node { role: string(&el, "AXRole"), subrole: string(&el, "AXSubrole"), ..Default::default() });
        (!secure).then_some(el)
    }

    fn value(el: &CFType) -> Option<String> {
        let v = attr(el, "AXValue")?.downcast::<CFString>()?.to_string();
        (v.len() <= MAX_FIELD).then_some(v)
    }

    /// Follow the field that took the paste. Returns the pasted span as it reads now, once it changed and then
    /// held still for 2 polls, or when focus leaves / the 30 s end. None = no AX text or the paste never showed.
    pub fn follow(pid: i32, pasted: &str, ticket: u64) -> Option<String> {
        let app = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) };
        unsafe { AXUIElementSetMessagingTimeout(app.as_CFTypeRef(), 0.25) };
        let t0 = Instant::now();
        std::thread::sleep(Duration::from_millis(300)); // let the app take the Cmd+V
        let el = focused(&app)?;
        // The pasted text in the field: the last copy of it (the caret sits after the paste).
        let (base, start) = (0..3).find_map(|i| {
            if i > 0 {
                std::thread::sleep(TICK);
            }
            let v = value(&el)?;
            v.rfind(pasted).map(|at| (v, at))
        })?;
        let end = start + pasted.len();
        let (mut last, mut still) = (pasted.to_string(), 0);
        while t0.elapsed() < WINDOW && super::alive(ticket) {
            std::thread::sleep(TICK);
            let front = crate::paste::frontmost_app().map(|a| a.pid);
            let same = front == Some(pid) && focused(&app).is_some_and(|f| f == el);
            let Some(now) = value(&el).filter(|_| same) else { break };
            let span = super::current_span(&base, start, end, &now);
            if span == last {
                still += 1;
            } else {
                (last, still) = (span, 0);
            }
            if still >= 2 && last != pasted && !super::suggest(pasted, &last).is_empty() {
                return Some(last);
            }
        }
        (last != pasted && super::alive(ticket)).then_some(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_for_adds_once() {
        let e = |id, text: &str, aliases: &[&str]| DictEntry { id, kind: DictKind::Word, text: text.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(), expansion: None, exact_only: false, starred: false, uses: 0, created_at: id };
        let es = vec![e(1, "Jev", &[]), e(2, "Contaflow", &["conta flow"])];
        let got = correction_for("Jev", "Jeff", &es).unwrap().unwrap();
        assert_eq!((got.id, got.aliases.as_slice()), (Some(1), &["Jeff".to_string()][..]), "the seeded word gets the correction");
        assert_eq!(correction_for("Contaflow", "conta flow", &es), Ok(None), "already known");
        assert!(correction_for("Jev", "the", &es).is_err() && correction_for("Jev", "es", &es).is_err(), "a common word is never learned");
        let new = correction_for("TypeSafe", "type safe", &es).unwrap().unwrap();
        assert_eq!((new.id, new.text.as_str()), (None, "TypeSafe"));
        assert!(dictionary::validate(&got, &es).is_ok());
    }

    fn s(word: &str, heard: &str) -> Suggestion {
        Suggestion { word: word.into(), heard: heard.into() }
    }

    #[test]
    fn suggestion_table() {
        let cases: &[(&str, &str, &str, &[(&str, &str)])] = &[
            ("joined brand", "We use conta flow for billing.", "We use Contaflow for billing.", &[("Contaflow", "conta flow")]),
            ("es split brand", "Lo guardo en su pavase hoy", "Lo guardo en Supabase hoy", &[("Supabase", "su pavase")]),
            ("name", "talk to jeff about it", "talk to Jev about it", &[("Jev", "jeff")]),
            ("es accent on a name", "habla con fernandez mañana", "habla con Fernández mañana", &[("Fernández", "fernandez")]),
            ("punctuation kept off", "ship it on versel, today", "ship it on Vercel, today", &[("Vercel", "versel")]),
            ("plus typing after", "open kuberneets now", "open Kubernetes now and then more text", &[("Kubernetes", "kuberneets")]),
            ("case-only en", "we use contaflow", "we use Contaflow", &[]),
            ("case-only es", "mi amigo pablo", "mi amigo Pablo", &[]),
            ("accent-only common word", "eso esta bien", "eso está bien", &[]),
            ("deletion", "we use conta flow for billing", "we use for billing", &[]),
            ("insertion", "we use flow", "we use Contaflow flow", &[]),
            ("unrelated rewrite", "I will send it tomorrow", "we can talk about it on Friday", &[]),
            ("far word swap", "call the dentist", "call the plumber", &[]),
            ("plain typo fix", "teh report is done", "the report is done", &[]),
            ("long rewrite", "a b c d e", "Zorblax Quentin Frabble Wuzzle Mox", &[]),
            ("no change", "hola mundo", "hola mundo", &[]),
            ("year fix", "due in 2025", "due in 2026", &[]),
            ("amount fix", "it costs 10 dollars", "it costs 100 dollars", &[]),
            ("time fix", "meet at 3:30", "meet at 3:45", &[]),
        ];
        for (name, pasted, now, want) in cases {
            let want: Vec<Suggestion> = want.iter().map(|(w, h)| s(w, h)).collect();
            assert_eq!(suggest(pasted, now), want, "{name}");
        }
    }

    #[test]
    fn span_follows_edits_inside_and_ignores_the_rest() {
        let base = "Hi. We use conta flow here. Bye";
        let (start, end) = (4, 4 + "We use conta flow here.".len());
        assert_eq!(current_span(base, start, end, "Hi. We use Contaflow here. Bye"), "We use Contaflow here.");
        assert_eq!(current_span(base, start, end, base), "We use conta flow here.");
        // typing after the field's old end widens the span; the diff sees inserts, no suggestion
        let wide = current_span(base, start, end, "Hi. We use conta flow here. Bye, see you");
        assert_eq!(wide, "We use conta flow here. Bye, see you");
        assert_eq!(suggest("We use conta flow here.", &wide), vec![]);
        // an edit before the span widens it; the diff sees an insert
        assert_eq!(current_span(base, start, end, "Hello. We use Contaflow here. Bye"), "ello. We use Contaflow here.");
        assert_eq!(suggest("We use conta flow here.", "ello. We use Contaflow here."), vec![s("Contaflow", "conta flow")]);
        // multibyte
        let base = "¿Qué? usa su pavase";
        let start = base.find("usa").unwrap();
        assert_eq!(current_span(base, start, base.len(), "¿Qué? usa Supabase"), "usa Supabase");
        // the whole field deleted
        assert_eq!(current_span(base, start, base.len(), ""), "");
    }

    fn word(id: i64, text: &str, aliases: &[&str]) -> DictEntry {
        DictEntry { id, kind: DictKind::Word, text: text.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(), expansion: None, exact_only: false, starred: false, uses: 0, created_at: 0 }
    }

    #[test]
    fn add_creates_or_extends() {
        let es = vec![word(1, "Contaflow", &["conta flu"])];
        let e = entry_for(&s("Contaflow", "conta flow"), &es);
        assert_eq!((e.id, e.aliases.clone()), (Some(1), vec!["conta flu".to_string(), "conta flow".to_string()]));
        let e = entry_for(&s("Supabase", "su pavase"), &es);
        assert_eq!((e.id, e.text.as_str(), e.aliases.clone()), (None, "Supabase", vec!["su pavase".to_string()]));
        let e = entry_for(&s("Fernández", "fernandez"), &es);
        assert!(e.aliases.is_empty(), "accent-only: the word alone (an equal alias is invalid)");
        assert!(dictionary::validate(&entry_for(&s("Supabase", "su pavase"), &es), &es).is_ok());
    }

    #[test]
    fn known_heard_forms_are_not_offered() {
        let es = vec![word(1, "Contaflow", &["conta flow"]), word(2, "Fernández", &[])];
        assert!(!is_new(&s("Contaflow", "conta flow"), &es));
        assert!(!is_new(&s("Fernández", "fernandez"), &es));
        assert!(is_new(&s("Supabase", "su pavase"), &es));
    }
}
