//! WP4 paste check:  cargo run --example paste -- "hola desde susurro" 3
//! Focus a text field (TextEdit) within N seconds. The text is pasted, then the clipboard is restored.
//! Needs Accessibility for the terminal; without it the text stays on the clipboard (CopiedOnly).

use susurro_lib::contract::PasteOpts;
use susurro_lib::paste;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = args.first().cloned().unwrap_or_else(|| "hola desde susurro".into());
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);

    let before = paste::clipboard_text();
    println!("clipboard before: {before:?}");
    println!("focus a text field, pasting in {secs} s...");
    std::thread::sleep(std::time::Duration::from_secs(secs));

    let app = paste::frontmost_app();
    println!("frontmost app: {app:?}");
    let opts = PasteOpts::default();
    let t = std::time::Instant::now();
    let out = paste::paste(&text, &opts, None);
    println!("outcome: {out:?} in {} ms (includes restore_delay_ms = {})", t.elapsed().as_millis(), opts.restore_delay_ms);

    let after = paste::clipboard_text();
    println!("clipboard after: {after:?}");
    match out {
        paste::PasteOutcome::Pasted if after == before => println!("OK: pasted, clipboard restored"),
        paste::PasteOutcome::Pasted => {
            println!("FAIL: clipboard not restored");
            std::process::exit(1);
        }
        paste::PasteOutcome::CopiedOnly => {
            println!("COPIED ONLY: no Accessibility for this terminal; the text is on the clipboard");
            std::process::exit(2);
        }
        paste::PasteOutcome::NoField => {
            println!("NO FIELD: the focused element takes no text; the text is on the clipboard");
            std::process::exit(3);
        }
    }
}
