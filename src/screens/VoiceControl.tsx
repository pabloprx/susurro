// Voice control (hold F2, say what to do). Plan: .lo/VOICE_CONTROL.md E.3, flow per .lo/som-experiment/RESULTS.md:
// fixed intents first (no screenshot), then numbered AX marks + a Jev choice, a screenshot only when it is needed.
import { useEffect, useState } from "react";
import { AlertTriangle, Check, MonitorUp, RotateCw } from "lucide-react";
import { api, on, type CommandStep } from "../lib/api";
import { keyLabel, keysOf, keyStore, permStore, saveSettings, settingsStore, VERB } from "../lib/store";
import { S } from "../lib/strings";
import { Kbd, Label, Title, Toggle, toast } from "../ui";
import { Diagram, usePermWatch } from "./Onboarding";
import { DEFAULTS, PICKS, useRecorder } from "./Shortcuts";
import type { Nav } from "../App";

const PINNED_MODEL = "typesafe/jev-1.14";
const save = (f: Parameters<typeof saveSettings>[0]) => saveSettings(f).catch((e) => toast(String(e), { kind: "error" }));
const ENDED: CommandStep["status"][] = ["done", "stopped", "unsure", "blocked", "setup", "error"];

export default function VoiceControl({ nav }: { nav: Nav }) {
  const s = settingsStore.use();
  const perms = permStore.use();
  const keys = keyStore.use();
  const [log, setLog] = useState<CommandStep[] | null>(null);
  const [rec, setRec] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [asked, setAsked] = useState(false);

  useEffect(() => {
    const load = () => api.commandLog().then(setLog, () => setLog([]));
    load();
    const off = on("command_step", (c) => ENDED.includes(c.status) && load());
    return () => void off.then((f) => f());
  }, []);

  const set = (binding: string) => {
    setRec(false);
    return saveSettings((x) => void (x.hotkeys = { ...x.hotkeys, command: binding }))
      .then(() => setMsg(null))
      .catch((e) => setMsg(String(e)));
  };
  const { held, err } = useRecorder(rec ? "command" : null, s?.hotkeys, set, () => (setRec(false), setMsg(null)));

  // Screen Recording is asked only once a step needed a screenshot (the last run stopped on it), or after a click on Allow.
  const screenOk = !!perms?.screen_recording && !perms.needs_relaunch;
  const neededScreen = !!log?.some((c) => c.status === "setup" && c.message === "screen_recording");
  usePermWatch(asked && !screenOk);

  if (!s || !perms || !keys) return <Title parts={S.title.voice} />;
  const on_ = s.command.enabled;
  const key = keysOf(s.hotkeys.command);
  const keyName = keyLabel(s.hotkeys.command);
  const relaunch = perms.screen_recording && perms.needs_relaunch;
  const askScreen = () => {
    setAsked(true);
    void api.requestPermission("screen_recording");
  };

  return (
    <>
      <Title
        parts={S.title.voice}
        right={
          <span className="inline vc-switch">
            <span className="label">{on_ ? "On" : "Off"}</span>
            <Toggle label="Voice control" on={on_} onChange={(v) => save((x) => void (x.command.enabled = v))} />
          </span>
        }
      />
      <p className="vc-lede">
        Hold <Kbd keys={key} /> and say what to do: <em>“open Slack”</em>, <em>“reply in thread”</em>, <em>“send the message”</em>. susurro numbers
        the buttons of the front app and acts on the one you mean.
      </p>

      {!screenOk && (neededScreen || relaunch) && (
        <div className="perm-card active vc-perm" role="region" aria-label="Screen Recording">
          <div className="pc-head">
            <span className="pc-ico" aria-hidden>
              <MonitorUp size={16} strokeWidth={1.5} />
            </span>
            <span className="pc-text">
              <span className="t">Let susurro look at the screen</span>
              <span className="d">Your last command needed a screenshot. susurro takes one only for steps like “the red one” or “next to Send”.</span>
            </span>
            {relaunch ? (
              <button className="btn primary sm" onClick={() => void api.relaunchApp()}>
                Restart susurro
              </button>
            ) : (
              <button className="btn primary sm" onClick={askScreen}>
                Open System Settings
              </button>
            )}
          </div>
          <div className="pc-body">
            {relaunch ? (
              <p className="ob-restart" role="status">
                <RotateCw size={14} strokeWidth={1.5} /> Allowed. macOS applies Screen Recording after a restart.
              </p>
            ) : (
              <>
                <Diagram pane="Screen & System Audio Recording" />
                <p className="small">{asked ? "Turn on susurro in the list. This page sees it by itself." : "Click the button, then turn on susurro."}</p>
              </>
            )}
          </div>
        </div>
      )}

      <Label>Ready</Label>
      <div className="card rows vc-ready">
        <Ready ok={keys.openrouter} name="OpenRouter key" what="Jev answers on OpenRouter">
          {!keys.openrouter && (
            <button className="btn secondary sm" onClick={() => nav.go("providers")}>
              Add key
            </button>
          )}
        </Ready>
        <Ready ok={perms.accessibility} name="Accessibility" what="reads the buttons, clicks and types">
          {!perms.accessibility && (
            <button className="btn secondary sm" onClick={() => void api.requestPermission("accessibility")}>
              Allow
            </button>
          )}
        </Ready>
        <Ready ok={screenOk} name="Screen Recording" what={screenOk ? "for steps that look at the screen" : "asked the first time a step needs to look"}>
          {!screenOk && !neededScreen && !relaunch && (
            <button className="btn ghost sm" onClick={askScreen}>
              Allow now
            </button>
          )}
        </Ready>
      </div>

      <Label>Command</Label>
      <div className="card">
        <div className="sc-row">
          <div>
            <div className="label">Hold to command</div>
            <div className="sub">Hold, say it, release</div>
          </div>
          <span className="inline">
            <button className={`bind${rec ? " recording" : ""}`} onClick={() => (setRec(!rec), setMsg(null))} aria-label={`Voice control key: ${keyName}. Press to record a new shortcut.`}>
              {rec ? held.length ? <Kbd keys={held} /> : <span className="rec-hint">Press a key or a combo… Esc to cancel</span> : <Kbd keys={key} />}
            </button>
            {rec && (
              <span className="chips picks" aria-label="Quick picks for Voice control">
                {PICKS.command.map((b) => (
                  <button key={b} className="chip" onClick={() => set(b)} aria-label={`Use ${keyLabel(b)}`} disabled={s.hotkeys.command === b}>
                    {keysOf(b).join(" ")}
                  </button>
                ))}
              </span>
            )}
          </span>
          <button className="btn ghost sm" onClick={() => set(DEFAULTS.command)} disabled={s.hotkeys.command === DEFAULTS.command}>
            Reset
          </button>
          {(rec ? err : msg) && (
            <p className={`recorder-msg ${rec ? "warn-text" : "danger-text"}`} role="alert">
              <AlertTriangle size={13} strokeWidth={1.5} /> {rec ? err : msg}
            </p>
          )}
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Ask before risky</div>
            <div className="sub">Send, delete, buy</div>
          </div>
          <span className="small">
            {s.command.confirm_risky ? (
              <>
                Highlights the target and waits for <Kbd keys={key} />.
              </>
            ) : (
              "Acts right away. Esc still stops it."
            )}
          </span>
          <Toggle label="Ask before risky actions" on={s.command.confirm_risky} onChange={(v) => save((x) => void (x.command.confirm_risky = v))} />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Model</div>
            <div className="sub">Picks the target</div>
          </div>
          <span className="inline">
            <span className="mono">{s.command.model || PINNED_MODEL}</span>
            <span className="tag">PINNED</span>
          </span>
          <span />
        </div>
      </div>

      <Label>How it picks</Label>
      <ol className="vc-how">
        <li>
          <span className="label">Fast</span>
          <span>“open Slack”, “new tab”, “paste the last one” run at once. No screenshot.</span>
        </li>
        <li>
          <span className="label">Marks</span>
          <span>It numbers the buttons of the front app and picks one by its name. Up to 6 steps.</span>
        </li>
        <li>
          <span className="label">Look</span>
          <span>Only for “the red one” or “next to Send”: one screenshot of one display. Nothing is saved.</span>
        </li>
        <li>
          <span className="label">Ask</span>
          <span>Under 80% sure, it shows the top 3 and asks “Which one?”. It never clicks a guess.</span>
        </li>
      </ol>
      <p className="small vc-safety">
        <kbd>esc</kbd> or <Kbd keys={key} /> stops at once. susurro never types in or clicks a password field.
      </p>

      <Label right={log && log.length > 0 && <span className="small mono">{total(log)}</span>}>Last run</Label>
      {!log || !log.length ? (
        <p className="small vc-empty">
          No commands yet. Hold {keyName} and say <em>open Slack</em>.
        </p>
      ) : (
        <div className="card rows vc-log">
          <div className="row vc-instr">
            <span className="label">Said</span>
            <span className="ellipsis">“{log[log.length - 1].instruction}”</span>
          </div>
          {log.map((c, i) => (
            <div className="row" key={i}>
              <span className="mono dim">{String(c.step).padStart(2, "0")}</span>
              <span className="mono vc-verb">{c.status === "done" ? "DONE" : (VERB[c.action ?? ""] ?? c.status.toUpperCase())}</span>
              <span className="ellipsis vc-what">{what(c)}</span>
              <span className="mono num">{(c.latency_ms / 1000).toFixed(2)} s</span>
              <span className="mono num">${c.cost_usd.toFixed(6)}</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

function Ready({ ok, name, what, children }: { ok: boolean; name: string; what: string; children?: React.ReactNode }) {
  return (
    <div className="row" aria-label={`${name}: ${ok ? "ready" : "not ready"}`}>
      <span className={`vc-mark${ok ? " ok" : ""}`} aria-hidden>
        {ok ? <Check size={12} strokeWidth={2.25} /> : null}
      </span>
      <span className="vc-name">{name}</span>
      <span className="small">{what}</span>
      <span className="vc-act">{children}</span>
    </div>
  );
}

/** What a step acted on: the mark name, or the message of a step that stopped. */
function what(c: CommandStep) {
  const m = c.marks.find((x) => x.id === c.target);
  if (m) return `${m.name} · ${m.role}`;
  if (c.detail) return c.detail;
  if (c.message && c.status !== "setup") return c.message;
  if (c.status === "setup") return c.message === "key" ? "needs an OpenRouter key" : "needs Screen Recording";
  return "";
}

/** Measured totals of the run (latency and usage.cost from Rust), never estimated. */
function total(log: CommandStep[]) {
  const ms = log.reduce((a, c) => a + c.latency_ms, 0);
  const usd = log.reduce((a, c) => a + c.cost_usd, 0);
  return `${log.length} steps · ${(ms / 1000).toFixed(2)} s · $${usd.toFixed(6)}`;
}
