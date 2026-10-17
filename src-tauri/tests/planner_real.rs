//! Real-API planner scenarios (VOICE_CONTROL_V2 section 8): the production `planner::PROMPT`, request and parse on
//! `tests/fixtures/planner_cases.json` (every planner-side case of `.lo/vc-feedback.md`). Sequential calls.
//! `OPENROUTER_API_KEY=... cargo test --test planner_real -- --ignored --nocapture --test-threads=1`
//! Env: PLANNER_MODEL (default contract::DEFAULT_PLANNER), RUNS (default 2).

use serde_json::Value;
use susurro_lib::ax::Field;
use susurro_lib::planner::{self, Action, Input, Plan};

const MODS: [&str; 4] = ["cmd", "ctrl", "opt", "shift"];

fn norm_keys(k: &str) -> String {
    let alias = |p: &str| match p {
        "enter" => "return",
        "backspace" => "delete",
        "command" | "meta" => "cmd",
        "option" | "alt" => "opt",
        "control" => "ctrl",
        p => p,
    }
    .to_string();
    let parts: Vec<String> = k.to_lowercase().replace(' ', "").split('+').map(alias).collect();
    let mut mods: Vec<String> = parts.iter().filter(|p| MODS.contains(&p.as_str())).cloned().collect();
    mods.sort_by_key(|m| MODS.iter().position(|x| x == m));
    mods.extend(parts.into_iter().filter(|p| !MODS.contains(&p.as_str())));
    mods.join("+")
}

/// Same effect: key cmd+c / cmd+v = copy / paste of the focused element.
fn norm(a: &Action) -> Action {
    let mut a = a.clone();
    if a.op == "key" {
        match a.keys.as_deref().map(norm_keys).as_deref() {
            Some("cmd+c") => return Action { op: "copy".into(), ..Default::default() },
            Some("cmd+v") => return Action { op: "paste".into(), ..Default::default() },
            _ => {}
        }
    }
    a.target = a.target.filter(|t| !t.trim().is_empty());
    a
}

fn matches(a: &Action, p: &Value) -> bool {
    let a = norm(a);
    if a.op != p["op"].as_str().unwrap() {
        return false;
    }
    if let Some(k) = p["keys"].as_str() {
        if a.keys.as_deref().map(norm_keys) != Some(norm_keys(k)) {
            return false;
        }
    }
    let text = a.text.as_deref().unwrap_or("").trim().trim_end_matches('.');
    if let Some(t) = p["text"].as_str() {
        if text != t {
            return false;
        }
    }
    if let Some(t) = p["text_ci"].as_str() {
        if !text.eq_ignore_ascii_case(t) {
            return false;
        }
    }
    if let Some(app) = p["app"].as_str() {
        if !a.app.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(app)) {
            return false;
        }
    }
    match p["target"].as_array() {
        Some(groups) => {
            let t = a.target.as_deref().unwrap_or("").to_lowercase();
            groups.iter().all(|g| g.as_str().unwrap().split('|').any(|alt| t.contains(alt)))
        }
        // No target in the pattern: type / paste / copy go to the focused element. Clicks always name one.
        None => a.op == "click" || a.target.is_none() || !["type", "paste", "copy"].contains(&a.op.as_str()),
    }
}

fn accepted(p: &Plan, accept: &[Value]) -> bool {
    accept.iter().any(|want| {
        let acts = want["actions"].as_array().unwrap();
        want["complete"].as_bool() == Some(p.complete)
            && acts.len() == p.actions.len()
            && p.actions.iter().zip(acts).all(|(a, w)| matches(a, w))
            && want["answer"].as_str().is_none_or(|ans| p.answer.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(ans)))
    })
}

fn input(c: &Value, screens: &Value) -> Input {
    let sc = &screens[c["screen"].as_str().unwrap()];
    let f = &sc["focused"];
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    Input {
        utterance: s(&c["utterance"]),
        last: false,
        app: s(&sc["app"]),
        focused: Field {
            app: s(&sc["app"]),
            role: c["focused_role"].as_str().map_or_else(|| s(&f["role"]), String::from),
            name: s(&f["name"]),
            value: Some(c["focused_value"].as_str().map_or_else(|| s(&f["value"]), String::from)),
            ..Default::default()
        },
        memory: c["memory"].as_array().map(|m| m.iter().map(s).collect()).unwrap_or_default(),
        marks: sc["marks"].as_array().unwrap().iter().map(s).collect(),
        pending: c["pending"].as_str().map(String::from),
    }
}

#[test]
#[ignore]
fn planner_cases() {
    let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
    let model = std::env::var("PLANNER_MODEL").unwrap_or_else(|_| susurro_lib::contract::DEFAULT_PLANNER.into());
    let runs: usize = std::env::var("RUNS").ok().and_then(|r| r.parse().ok()).unwrap_or(2);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/planner_cases.json");
    let fx: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases = fx["cases"].as_array().unwrap();
    let (mut pass, mut total, mut cost, mut lat) = (0, 0, 0.0, vec![]);
    let mut misses = vec![];
    for run in 1..=runs {
        for c in cases {
            let id = c["id"].as_str().unwrap();
            total += 1;
            match planner::plan_openrouter(&key, &model, &input(c, &fx["screens"])) {
                Ok((p, m)) => {
                    let ok = accepted(&p, c["accept"].as_array().unwrap());
                    pass += ok as u32;
                    cost += m.cost;
                    lat.push(m.latency_ms);
                    let acts: Vec<String> = p.actions.iter().map(|a| format!("{}{}", planner::label(a), a.target.as_ref().map(|t| format!(" @{t}")).unwrap_or_default())).collect();
                    println!("run {run} {id:20} case {:2} {} {:5} ms complete={} answer={:?} {acts:?}", c["case"], if ok { "OK  " } else { "MISS" }, m.latency_ms, p.complete, p.answer);
                    if !ok {
                        misses.push(format!("{id} (run {run}): {acts:?} complete={}", p.complete));
                    }
                }
                Err(e) => {
                    println!("run {run} {id:20} ERROR {e:#}");
                    misses.push(format!("{id} (run {run}): error {e:#}"));
                }
            }
        }
    }
    lat.sort();
    let pct = |q: f64| lat.get(((lat.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0);
    println!("{model}: {pass}/{total} pass, p50 {} ms, p90 {} ms, usage.cost ${cost:.4}", pct(0.5), pct(0.9));
    assert!(misses.is_empty(), "misses:\n{}", misses.join("\n"));
}

/// Streaming (case 5 + 10): chunks cut at a 300 ms pause go through the real `live::Core` and the real planner.
/// The typed text never holds the target words, and no action runs twice. Sequential calls, one run.
#[test]
#[ignore]
fn planner_stream_cases() {
    use susurro_lib::live::{Core, Heard};
    let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
    let model = std::env::var("PLANNER_MODEL").unwrap_or_else(|_| susurro_lib::contract::DEFAULT_PLANNER.into());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/planner_cases.json");
    let fx: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    // (screen, chunks, words the typed text must not hold, the typed texts wanted in order, the field they go to:
    // "" = the focused one; a field click before the type counts too)
    let seqs: [(&str, &[&str], &[&str], &[&str], &str); 4] = [
        ("chrome", &["Type hello", "onto the search input."], &["onto", "search", "input"], &["hello"], "search"),
        ("chrome", &["Type onto the input search,", "math coding."], &["onto", "input", "search"], &["math coding"], "search"),
        ("ghostty", &["Create a new tab and", "run the command ls."], &["run", "command"], &["ls"], ""),
        ("whatsapp", &["Write", "see you at five"], &["write"], &["see you at five"], ""),
    ];
    let mut misses = vec![];
    for (screen, chunks, banned, want, field) in seqs {
        let base = serde_json::json!({"screen": screen, "utterance": ""});
        let mut core = Core::default();
        let mut all: Vec<Action> = vec![];
        let mut plan = |text: &str, last: bool, pending: Option<&str>| {
            let mut i = input(&base, &fx["screens"]);
            (i.utterance, i.last, i.pending) = (text.to_string(), last, pending.map(String::from));
            planner::plan_openrouter(&key, &model, &i).map(|(p, m)| {
                println!("  {text:?} final={last} -> complete={} rest={:?} {:?} {} ms", p.complete, p.rest, p.actions.iter().map(planner::label).collect::<Vec<_>>(), m.latency_ms);
                p
            })
        };
        for c in chunks {
            if let Heard::Order(_, a, _) = core.hear(c, false, None, &mut plan) {
                all.extend(a);
            }
        }
        if !core.buffer.is_empty() {
            if let Heard::Order(_, a, _) = core.hear("", true, None, &mut plan) {
                all.extend(a);
            }
        }
        let typed: Vec<String> = all.iter().filter(|a| a.op == "type").map(|a| a.text.clone().unwrap_or_default().trim().trim_end_matches('.').to_lowercase()).collect();
        let leak = typed.iter().any(|t| banned.iter().any(|b| t.split(|c: char| !c.is_alphanumeric()).any(|w| w == *b)));
        let aimed = field.is_empty()
            || all.iter().any(|a| ["type", "click"].contains(&a.op.as_str()) && a.target.as_deref().is_some_and(|t| t.to_lowercase().contains(field)));
        let ok = !leak && aimed && typed == want.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        println!("{screen:9} {chunks:?} -> {} {:?}", if ok { "OK  " } else { "MISS" }, all.iter().map(|a| format!("{}{}", planner::label(a), a.target.as_ref().map(|t| format!(" @{t}")).unwrap_or_default())).collect::<Vec<_>>());
        if !ok {
            misses.push(format!("{chunks:?}: typed {typed:?}"));
        }
    }
    assert!(misses.is_empty(), "misses:\n{}", misses.join("\n"));
}

/// next-queue 4: voice correction of a mis-heard word -> correct_word{target: heard, text: word}; spelled letters win.
#[test]
#[ignore]
fn planner_correct_word() {
    let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
    let model = std::env::var("PLANNER_MODEL").unwrap_or_else(|_| susurro_lib::contract::DEFAULT_PLANNER.into());
    // (utterance, focused value, Some((heard, word)) or None = no correct_word)
    let cases: [(&str, &str, Option<(&str, &str)>); 5] = [
        ("Corrige: Jeff es Jev, J E V.", "Le pregunté a Jeff por el modelo.", Some(("Jeff", "Jev"))),
        ("The last thing you wrote, GAB must be Jev.", "I asked GAB about it.", Some(("GAB", "Jev"))),
        ("No es Jeff, es jota e ve.", "Hablé con Jeff ayer.", Some(("Jeff", "Jev"))),
        ("Fix the word type safe, it's TypeSafe, T Y P E S A F E.", "We use type safe for that.", Some(("type safe", "TypeSafe"))),
        ("Type hello there.", "", None),
    ];
    let mut misses = vec![];
    for (utt, value, want) in cases {
        let i = Input { utterance: utt.into(), last: true, app: "Slack".into(), focused: Field { app: "Slack".into(), role: "AXTextArea".into(), name: "Message".into(), value: Some(value.into()), ..Default::default() }, ..Default::default() };
        let (p, m) = planner::plan_openrouter(&key, &model, &i).expect("planner call");
        let got = p.actions.iter().find(|a| a.op == "correct_word").map(|a| (a.target.clone().unwrap_or_default(), susurro_lib::command::spelled(a.text.as_deref().unwrap_or(""))));
        let ok = match want {
            Some((h, w)) => got.as_ref().is_some_and(|(gh, gw)| gh.eq_ignore_ascii_case(h) && gw == w),
            None => got.is_none(),
        };
        println!("{} {utt:?} -> {got:?} {:?} {} ms ${:.5}", if ok { "OK  " } else { "MISS" }, p.actions.iter().map(planner::label).collect::<Vec<_>>(), m.latency_ms, m.cost);
        if !ok {
            misses.push(utt);
        }
    }
    assert!(misses.is_empty(), "misses: {misses:?}");
}
