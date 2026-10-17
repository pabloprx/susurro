//! Voice control: contract plumbing only (the last run's log + the `command_step` event).
//! ponytail: the command runner itself (AX read, marks, Jev decision, actions) is not built yet; see the recap.

use crate::contract::CommandStep;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

static LOG: Mutex<Vec<CommandStep>> = Mutex::new(Vec::new());

/// Sends one step to every window and keeps it in the in-memory log. step 1 / fast path (0) starts a new run.
pub fn publish(app: &AppHandle, s: CommandStep) {
    record(&mut LOG.lock().unwrap(), s.clone());
    let _ = app.emit("command_step", s);
}

/// Step 0 (fast path) or 1 starts a new run: the log holds one run, even when the same instruction repeats.
fn record(log: &mut Vec<CommandStep>, s: CommandStep) {
    if s.step <= 1 {
        log.clear();
    }
    log.push(s);
}

/// The last voice-control run, oldest step first.
#[tauri::command]
pub fn command_log() -> Vec<CommandStep> {
    LOG.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_instruction_starts_a_new_run() {
        let st = |step, i: &str| CommandStep { step, instruction: i.into(), ..Default::default() };
        let mut log = vec![];
        for s in [st(1, "scroll down"), st(2, "scroll down"), st(1, "scroll down")] {
            record(&mut log, s);
        }
        assert_eq!(log.len(), 1, "the second 'scroll down' run is merged into the first");
        record(&mut log, st(0, "open Slack"));
        assert_eq!(log, vec![st(0, "open Slack")]);
    }
}
