// Voice control (hold or double-tap the command key, say what to do). v2 flow (.lo/VOICE_CONTROL_V2.md): the planner LLM
// turns the spoken words into actions while Pablo talks, Jev grounds each click on numbered AX marks, a screenshot only
// when it is needed. This screen: readiness, keys, the planner model (ranked by the bench), the memory, the last run.
import { useEffect, useState } from "react";
import { AlertTriangle, Check, MonitorUp, RotateCw } from "lucide-react";
import { api, on, type CommandStep, type MemKind, type MemoryEntry } from "../lib/api";
import { clock, keyLabel, keysOf, keyStore, opOf, permStore, save, saveSettings, settingsStore, VERB } from "../lib/store";
import { S } from "../lib/strings";
import { Kbd, Label, radioKeys, Title, toast, Toggle } from "../ui";
import { Diagram, usePermWatch } from "./Onboarding";
import { DEFAULTS, PICKS, useRecorder } from "./Shortcuts";
import type { Nav } from "../App";
import type { SttProvider } from "../lib/contract.gen";

const PINNED_MODEL = "typesafe/jev-1.14";
const ENDED: CommandStep["status"][] = ["done", "stopped", "unsure", "blocked", "setup", "error"];

type Planner = { id: string; name: string; via: string; same: number; strict: number; p50: number; p90: number; usd: number; tag?: string };
/** .lo/planner-bench.md, 2026-10-17: v2 prompt, 20 scenarios from Pablo's log x 3 runs = 60 calls per model, one request
 *  at a time from this Mac. same = the plan had the right end effect; p50 / p90 = wall clock of one request; usd = usage.cost.
 *  Ranked by same-effect score, then p50. ponytail: a fixed table; re-run the bench (and edit it here) to update it. */
export const PLANNERS: Planner[] = [
  { id: "openai/gpt-oss-120b", name: "gpt-oss-120b", via: "Cerebras · reasoning low", same: 60, strict: 57, p50: 0.78, p90: 1.11, usd: 0.00045, tag: "FASTEST" },
  { id: "openai/gpt-6-luna", name: "GPT-6 Luna", via: "reasoning minimal", same: 60, strict: 56, p50: 1.67, p90: 3.15, usd: 0.00012, tag: "CHEAPEST" },
  { id: "google/gemini-3.1-flash-lite", name: "Gemini 3.1 Flash Lite", via: "reasoning minimal", same: 60, strict: 57, p50: 1.8, p90: 1.99, usd: 0.000349 },
  { id: "anthropic/claude-haiku-5.5", name: "Claude Haiku 5.5", via: "Anthropic", same: 60, strict: 60, p50: 2.05, p90: 2.86, usd: 0.000233, tag: "MOST EXACT" },
  { id: "google/gemini-2.5-flash-lite", name: "Gemini 2.5 Flash Lite", via: "misses 7 of 60", same: 53, strict: 53, p50: 0.98, p90: 1.22, usd: 0.000105 },
];
const SLOWEST = Math.max(...PLANNERS.map((p) => p.p90));

const MEM_LABEL: Record<MemKind, string> = { said: "SAID", typed: "TYPED", deleted: "DELETED", copied: "COPIED", pasted: "PASTED", clicked: "CLICKED", key: "KEY", opened: "OPENED", failed: "FAILED" };

export default function VoiceControl({ nav }: { nav: Nav }) {
  const s = settingsStore.use();
  const perms = permStore.use();
  const keys = keyStore.use();
  const [log, setLog] = useState<CommandStep[] | null>(null);
  const [rec, setRec] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [asked, setAsked] = useState(false);
  const [memory, setMemory] = useState<MemoryEntry[] | null>(null);

  useEffect(() => {
    const load = () => api.commandLog().then(setLog, () => setLog([]));
    const loadMem = () => api.commandMemory().then((m) => setMemory(m ?? []), () => setMemory([]));
    load();
    loadMem();
    const offs = [on("command_step", (c) => ENDED.includes(c.status) && (load(), loadMem())), on("command_live", (l) => !l.on && !l.queue.length && loadMem())];
    return () => offs.forEach((o) => void o.then((f) => f()));
  }, []);
  const clearMemory = () =>
    api.commandMemoryClear().then(
      () => setMemory([]),
      (e) => toast(`Not cleared: ${e}`, { kind: "error" }),
    );

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
        Hold <Kbd keys={key} /> and say what to do: <em>“new tab and run ls”</em>, <em>“copy the link”</em>, <em>“remove what you typed”</em>. Double-tap{" "}
        <Kbd keys={key} /> for a live session: it acts while you keep talking, and a tap ends it.
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
            <div className="label">Command key</div>
            <div className="sub">Hold, or double-tap for live</div>
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
            <div className="label">Target model</div>
            <div className="sub">Picks the button you named</div>
          </div>
          <span className="inline">
            <span className="mono">{s.command.model || PINNED_MODEL}</span>
            <span className="tag">PINNED</span>
          </span>
          <span />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Speech to text</div>
            <div className="sub">Hears your commands. Scribe Realtime answers in ~0.2 s, MAI in ~0.7 s</div>
          </div>
          <select
            className="input"
            aria-label="Speech to text for commands"
            value={s.command.stt ?? "auto"}
            onChange={(e) => save((x) => void (x.command.stt = e.target.value === "auto" ? null : (e.target.value as SttProvider)))}
          >
            <option value="auto">Auto · {keys?.elevenlabs ? "Scribe Realtime" : "same as dictation"}</option>
            <option value="elevenlabs_realtime">Scribe Realtime (live)</option>
            <option value="openrouter">OpenRouter</option>
            <option value="elevenlabs">ElevenLabs Scribe</option>
            <option value="whisper_local">Local Whisper</option>
          </select>
          <span />
        </div>
      </div>

      <Label right={<span className="small">60 calls per model, measured</span>}>Planner</Label>
      <PlannerPicker value={s.command.planner_model} onPick={(id) => save((x) => void (x.command.planner_model = id))} />
      <p className="small vc-bench-note">
        Measured on Oct 17 from this Mac: 20 orders from your log, 3 runs each, one request at a time. Real screens send more text, so real
        times run higher. Same effect = the plan did the right thing, even with other steps.
      </p>

      <Label>How it works</Label>
      <ol className="vc-how">
        <li>
          <span className="label">Plan</span>
          <span>The planner turns your words into keys, typing, clicks and copies. It plans each order as soon as you finish it.</span>
        </li>
        <li>
          <span className="label">Marks</span>
          <span>Before each click it reads the screen again, numbers the buttons and picks the one you named.</span>
        </li>
        <li>
          <span className="label">Look</span>
          <span>Only for “the red one” or “next to Send”: one screenshot of one display. Nothing is saved.</span>
        </li>
        <li>
          <span className="label">Ask</span>
          <span>Under 80% sure, it asks “Which one?”: say the number. Before Return or a send, it asks: say “yes”.</span>
        </li>
      </ol>
      <p className="small vc-safety">
        <kbd>esc</kbd> or <Kbd keys={key} /> stops at once. susurro never types in or clicks a password field.
      </p>

      <Label
        right={
          memory && memory.length > 0 && (
            <span className="inline">
              <span className="small mono">{memory.length} / 50</span>
              <button className="btn ghost sm" onClick={clearMemory}>
                Clear memory
              </button>
            </span>
          )
        }
      >
        Memory
      </Label>
      {!memory || !memory.length ? (
        <p className="small vc-empty">Nothing yet. susurro remembers what it typed, copied and clicked, so “remove that” knows what “that” is. RAM only.</p>
      ) : (
        <div className="card rows vc-mem" aria-label="Memory">
          {[...memory].reverse().map((m, i) => (
            <div className={`row k-${m.kind}`} key={`${m.at}:${i}`}>
              <span className="mono dim">{clock(m.at)}</span>
              <span className="mono vc-verb">{MEM_LABEL[m.kind] ?? m.kind.toUpperCase()}</span>
              <span className="ellipsis vc-mem-text">{m.kind === "said" ? `“${m.text}”` : m.text}</span>
              <span className="ellipsis small">{[m.app, m.field].filter(Boolean).join(" · ")}</span>
            </div>
          ))}
        </div>
      )}

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
              <span className="mono vc-verb">{c.status === "done" ? "DONE" : (VERB[opOf(c)] ?? c.status.toUpperCase())}</span>
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

/** The bench table as a radio list: same-effect score, p50 with a p90 whisker, cost per 1000 orders. An id that is
 *  not in the table (set by hand) shows as its own row, unmeasured. */
function PlannerPicker({ value, onPick }: { value: string; onPick: (id: string) => void }) {
  const rows: (Planner | { id: string; name: string; via: string; custom: true })[] = PLANNERS.some((p) => p.id === value)
    ? PLANNERS
    : [...PLANNERS, { id: value, name: value, via: "not measured", custom: true }];
  return (
    <div className="card rows vc-planner" role="radiogroup" aria-label="Planner model" onKeyDown={radioKeys}>
      <div className="row vc-pl-head" aria-hidden>
        <span />
        <span>Model</span>
        <span>Same effect</span>
        <span>Latency p50 · p90</span>
        <span>per 1000 orders</span>
      </div>
      {rows.map((p) => {
        const sel = p.id === value;
        return (
          <div
            key={p.id}
            className={`row hoverable vc-pl${sel ? " sel" : ""}`}
            role="radio"
            aria-checked={sel}
            aria-label={p.name}
            tabIndex={sel ? 0 : -1}
            onClick={() => !sel && onPick(p.id)}
          >
            <span className="vc-radio" aria-hidden />
            <span className="vc-pl-name">
              <span className="n">
                {p.name}
                {"tag" in p && p.tag && <span className="tag">{p.tag}</span>}
              </span>
              <span className="small">{p.via}</span>
            </span>
            {"custom" in p ? (
              <>
                <span className="mono dim">-</span>
                <span className="mono dim">-</span>
                <span className="mono dim">-</span>
              </>
            ) : (
              <>
                <span className={`mono vc-pl-score${p.same < 60 ? " miss" : ""}`}>{p.same}/60</span>
                <span className="vc-pl-lat" title={`p50 ${p.p50.toFixed(2)} s, p90 ${p.p90.toFixed(2)} s`}>
                  <span className="bar" aria-hidden>
                    <i className="p90" style={{ width: `${(p.p90 / SLOWEST) * 100}%` }} />
                    <i className="p50" style={{ width: `${(p.p50 / SLOWEST) * 100}%` }} />
                  </span>
                  <span className="mono num">{p.p50.toFixed(2)} s</span>
                </span>
                <span className="mono num">${(p.usd * 1000).toFixed(2)}</span>
              </>
            )}
          </div>
        );
      })}
    </div>
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
