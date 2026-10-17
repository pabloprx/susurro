//! WP4 hotkey check: runs the real hotkey module inside a windowless tauri app and prints what it sends.
//!
//!   cargo run --example keys [-- "Alt+Space"]     # optional PTT binding; Ctrl+C exits
//!
//! PTT = RightOption (CGEventTap, needs Input Monitoring for the terminal). Chords via global-shortcut:
//! hands-free Ctrl+Alt+Space, paste last Ctrl+Cmd+V. "fn double-tap" uses the session's rule
//! (PLAN 4.2: a hold < 250 ms, then a 2nd press within 500 ms), computed here only for display.
//! It also emits a fake `session_state` (Recording while PTT is held), so Esc cancel is registered then.

use std::sync::mpsc;
use std::time::{Duration, Instant};
use susurro_lib::contract::{Hotkeys, Phase, SessionState};
use tauri::Emitter;
use susurro_lib::session::SessionCmd;

fn main() {
    let ptt = std::env::args().nth(1).unwrap_or_else(|| "RightOption".into());
    let name = ptt.clone();
    let b = Hotkeys { ptt, hands_free: Some("Ctrl+Alt+Space".into()), ..Default::default() };
    let (tx, rx) = mpsc::channel();
    let print = move |app: tauri::AppHandle| {
        let phase = |p| {
            let _ = app.emit("session_state", SessionState { phase: p, ..Default::default() });
        };
        let (mut down_at, mut short_tap_at): (Option<Instant>, Option<Instant>) = (None, None);
        for cmd in rx {
            let now = Instant::now();
            match cmd {
                SessionCmd::PttDown => {
                    let double = short_tap_at.is_some_and(|t| now - t < Duration::from_millis(500));
                    println!("{name} {}", if double { "double-tap" } else { "down" });
                    down_at = Some(now);
                    phase(Phase::Recording);
                }
                SessionCmd::PttUp => {
                    let held = down_at.take().map(|t| now - t).unwrap_or_default();
                    println!("{name} up (held {} ms)", held.as_millis());
                    short_tap_at = (held < Duration::from_millis(250)).then_some(now);
                    phase(Phase::Idle);
                }
                SessionCmd::Cancel => {
                    println!("{}", if down_at.is_some_and(|t| now - t < Duration::from_millis(300)) { "ptt cancelled (chord)" } else { "cancel (Esc)" });
                    down_at = None;
                    phase(Phase::Idle);
                }
                SessionCmd::Toggle => println!("chord: hands-free (Ctrl+Alt+Space)"),
                SessionCmd::PasteLast => println!("chord: paste last (Ctrl+Cmd+V)"),
                other => println!("{other:?}"),
            }
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let p = susurro_lib::permissions::permissions_status()?;
            println!(
                "input monitoring: {} (needs relaunch: {}) | accessibility: {} | fn key free: {:?}",
                p.input_monitoring, p.needs_relaunch, p.accessibility, p.fn_key_free
            );
            if !p.input_monitoring {
                println!("the key tap waits for Input Monitoring (grant it to the terminal); chords work now");
            }
            let h = susurro_lib::hotkey::start(app.handle(), &b, tx.clone())?;
            std::mem::forget(h); // keep it alive for the whole run
            let handle = app.handle().clone();
            std::thread::spawn(move || print(handle));
            println!("listening. Ctrl+C exits");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri");
}
