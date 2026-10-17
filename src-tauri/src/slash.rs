//! Slash commands for Claude Code in terminals and the Claude app (next-queue 8). Only at the very start of a
//! dictation: "by the way, ..." -> "/btw ...", "slash compact" -> "/compact", "slash ponytail review" ->
//! "/ponytail-review". Mid-sentence words stay text. Names: Claude Code built-ins + ~/.claude skills, commands and
//! plugin skills, matched fuzzily; an unknown name keeps "/<words-joined-with-dashes>".

use crate::dictionary::dl;
use crate::text::{Token, norm, tokenize};
use std::path::Path;

/// Claude Code built-in commands (docs.claude.com/en/docs/claude-code/slash-commands, 2026-10).
const BUILT_INS: &[&str] = &[
    "add-dir", "agents", "bashes", "btw", "bug", "clear", "compact", "config", "context", "cost", "doctor", "exit",
    "export", "help", "hooks", "ide", "init", "install-github-app", "login", "logout", "mcp", "memory", "model",
    "output-style", "permissions", "plan", "plugin", "pr-comments", "privacy-settings", "release-notes", "resume",
    "review", "rewind", "security-review", "status", "statusline", "terminal-setup", "todos", "upgrade", "usage", "vim",
];

/// Terminals and the Claude app: the apps where a dictation can start with a slash command.
const APPS: &[&str] = &[
    "com.mitchellh.ghostty", "com.apple.Terminal", "com.googlecode.iterm2", "dev.warp.Warp-Stable", "com.github.wez.wezterm",
    "net.kovidgoyal.kitty", "org.alacritty", "com.anthropic.claudefordesktop",
];

pub fn app_takes_slash(bundle: &str) -> bool {
    APPS.iter().any(|a| a.eq_ignore_ascii_case(bundle))
}

/// Built-ins + skills and commands under `~/.claude` (user and installed plugins). Read on each dictation in a
/// terminal: a few directory listings.
pub fn names(claude_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = BUILT_INS.iter().map(|s| s.to_string()).collect();
    let mut roots = vec![claude_dir.to_path_buf()];
    if let Ok(raw) = std::fs::read_to_string(claude_dir.join("plugins/installed_plugins.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            for installs in v["plugins"].as_object().into_iter().flat_map(|m| m.values()) {
                for i in installs.as_array().into_iter().flatten() {
                    if let Some(p) = i["installPath"].as_str() {
                        roots.push(p.into());
                    }
                }
            }
        }
    }
    for root in roots {
        for (sub, file) in [("skills", false), ("commands", true)] {
            let Ok(rd) = std::fs::read_dir(root.join(sub)) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = if file { p.file_stem() } else { p.is_dir().then(|| p.file_name()).flatten() };
                if let Some(n) = name.and_then(|n| n.to_str()) {
                    if !n.starts_with('.') && !out.iter().any(|o| o == n) {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Most words a spoken command name takes ("install github app").
const MAX_WORDS: usize = 4;
/// Most words an unknown name takes.
const UNKNOWN_MAX_WORDS: usize = 2;

/// The closest known name to the spoken words (squashed: "pony tail review" = "ponytailreview").
fn find(words: &[&Token], names: &[String]) -> Option<(usize, String)> {
    let sq = |s: &str| norm(s).chars().filter(|c| c.is_alphanumeric()).collect::<Vec<char>>();
    let named: Vec<(Vec<char>, &String)> = names.iter().map(|n| (sq(n), n)).collect();
    for k in (1..=words.len().min(MAX_WORDS)).rev() {
        let said: Vec<char> = words[..k].iter().flat_map(|t| t.norm.chars().filter(|c| c.is_alphanumeric())).collect();
        let budget = match said.len() {
            0..=4 => 0,
            5..=8 => 1,
            _ => 2,
        };
        let best = named.iter().map(|(n, name)| (dl(&said, n), *name)).filter(|(d, _)| *d <= budget).min_by_key(|(d, _)| *d);
        if let Some((_, name)) = best {
            return Some((k, name.clone()));
        }
    }
    None
}

/// Some(rewritten text) when the dictation starts with a slash command; None = leave the text alone.
/// Returns (command, rest): the caller cleans `rest` like any dictation and joins them.
pub fn split(text: &str, names: &[String]) -> Option<(String, String)> {
    let toks = tokenize(text);
    let t0 = toks.first()?;
    // Only words before it: "Hi, by the way" is not a start.
    if text[..t0.start].chars().any(char::is_alphanumeric) {
        return None;
    }
    // The words after token i-1: "By the way, what is X?" -> "what is X?".
    let rest_from = |i: usize| -> String {
        let r = text[toks[i - 1].end..].trim_start_matches(|c: char| c.is_whitespace() || ",.;:-".contains(c)).trim_end();
        if r.chars().any(char::is_alphanumeric) { r.to_string() } else { String::new() }
    };
    let w = |i: usize| toks.get(i).map(|t| t.norm.as_str());
    let starts = |ph: &[&str]| ph.iter().enumerate().all(|(i, p)| w(i) == Some(p));
    for ph in [&["by", "the", "way"][..], &["btw"], &["por", "cierto"]] {
        if starts(ph) {
            return Some(("/btw".into(), rest_from(ph.len())));
        }
    }
    // "/compact" already written by the STT.
    // Only a bare "/name" counts: "/usr/bin", "./run.sh", "~/x" and "/etc/hosts" are paths and stay text.
    let typed_slash = text[..t0.start].trim() == "/"
        && text.trim_start()[1..].split_whitespace().next().is_some_and(|w| {
            w.trim_end_matches([',', '.', ';', ':', '!', '?']).chars().all(|c| c.is_alphanumeric() || c == '-')
        });
    if !typed_slash && text[..t0.start].contains('/') {
        return None;
    }
    let skip = if typed_slash { 0 } else if matches!(w(0), Some("slash" | "barra")) { 1 } else { return None };
    // The name runs up to the first punctuation.
    let mut name_toks: Vec<&Token> = vec![];
    for (i, t) in toks.iter().enumerate().skip(skip) {
        if i > skip && text[toks[i - 1].end..t.start].chars().any(|c| !c.is_whitespace() && c != '-') {
            break;
        }
        name_toks.push(t);
    }
    if name_toks.is_empty() {
        return None;
    }
    let (k, name) = match find(&name_toks, names) {
        Some(f) => f,
        // Unknown: every word up to the first punctuation, joined with dashes. More words = a sentence that starts
        // with "slash" / "barra" ("Barra lateral no funciona"): text.
        None if name_toks.len() <= UNKNOWN_MAX_WORDS => (name_toks.len(), name_toks.iter().map(|t| t.norm.as_str()).collect::<Vec<_>>().join("-")),
        None => return None,
    };
    Some((format!("/{name}"), rest_from(skip + k)))
}

/// "/compact" + "keep the tests" -> "/compact keep the tests".
pub fn join(cmd: &str, rest: &str) -> String {
    if rest.is_empty() { cmd.to_string() } else { format!("{cmd} {rest}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        BUILT_INS.iter().map(|s| s.to_string()).chain(["ponytail-review", "ponytail", "lights-out", "bro", "agent-browser"].map(String::from)).collect()
    }

    fn run(t: &str) -> Option<String> {
        split(t, &known()).map(|(c, r)| join(&c, &r))
    }

    #[test]
    fn slash_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("By the way, what does this function do?", Some("/btw what does this function do?")),
            ("by the way what is a monad", Some("/btw what is a monad")),
            ("Por cierto, ¿qué hace esta función?", Some("/btw ¿qué hace esta función?")),
            ("btw is the build green", Some("/btw is the build green")),
            ("Slash compact.", Some("/compact")),
            ("slash compact", Some("/compact")),
            ("Slash review, the auth module.", Some("/review the auth module.")),
            ("slash ponytail review", Some("/ponytail-review")),
            ("Slash pony tail review.", Some("/ponytail-review")),
            ("slash ponytail", Some("/ponytail")),
            ("Slash lights out. Fix the login bug.", Some("/lights-out Fix the login bug.")),
            ("slash light out", Some("/lights-out")),
            ("barra compact", Some("/compact")),
            ("slash security review", Some("/security-review")),
            ("Slash install GitHub app", Some("/install-github-app")),
            ("/compact", Some("/compact")),
            ("/compact.", Some("/compact")),
            // typed paths stay text
            ("/Users/pablo/projects is the path", None),
            ("/usr/bin/env python", None),
            ("/etc/hosts", None),
            ("./run.sh", None),
            ("~/projects", None),
            ("/review.md is the file", None),
            // unknown names keep the words, dashed
            ("slash deploy staging", Some("/deploy-staging")),
            ("Slash frobnicate, now please.", Some("/frobnicate now please.")),
            // mid-sentence stays text
            ("I said by the way that it works", None),
            ("Hola, por cierto, mañana no puedo", None),
            ("The path is slash usr slash bin", None),
            ("Use the slash command", None),
            ("Slash and burn is bad", None),
            ("Barra lateral no funciona", None),
            ("", None),
        ];
        for (input, want) in cases {
            assert_eq!(run(input).as_deref(), *want, "input: {input:?}");
        }
    }

    #[test]
    fn apps_and_names_from_disk() {
        assert!(app_takes_slash("com.mitchellh.ghostty") && app_takes_slash("com.anthropic.claudefordesktop"));
        assert!(!app_takes_slash("com.apple.TextEdit"));
        let d = std::env::temp_dir().join(format!("susurro-slash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("skills/lights-out")).unwrap();
        std::fs::create_dir_all(d.join("commands")).unwrap();
        std::fs::write(d.join("commands/roast.md"), "x").unwrap();
        let plug = d.join("plug");
        std::fs::create_dir_all(plug.join("skills/ponytail-review")).unwrap();
        std::fs::create_dir_all(d.join("plugins")).unwrap();
        std::fs::write(d.join("plugins/installed_plugins.json"), serde_json::json!({"plugins": {"p@m": [{"installPath": plug}]}}).to_string()).unwrap();
        let n = names(&d);
        for want in ["compact", "btw", "lights-out", "roast", "ponytail-review"] {
            assert!(n.iter().any(|x| x == want), "{want} in {n:?}");
        }
    }
}
