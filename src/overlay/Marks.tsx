// Voice control: the full-screen, click-through marks window (one per command, on the display it acts on).
// It draws what Jev chooses from: a numbered badge on every actionable AX element, a faint tint per region, then the
// chosen target (registration brackets + crosshair), the "Which one?" top 3, or the risky-action confirm.
// Live session (VOICE_CONTROL_V2 3): the marks stay up for the whole session (Rust owns the window's visibility), the
// target keeps its brackets HIGHLIGHT_MS after the action, and Pablo's own mouse or keys (took_over) empty the window at
// once until the next order. The HUD shows what it heard, what it does and what is queued.
// No router, no store: the command_step + command_live listeners, like the pill.
import { useEffect, useState, type CSSProperties } from "react";
import { api, isTauri, on, type CommandStep, type LiveState, type Mark, type PtRect, type Settings } from "../lib/api";
import { isPick, keysOf, opOf, pendingQuestion, VERB } from "../lib/format";
import "../styles/tokens.css";
import "../styles/marks.css";

/** Statuses that show marks. Anything else (thinking before the AX read, done, stopped ...) = empty window. */
const SHOWN: CommandStep["status"][] = ["marks", "aim", "pick", "confirm"];
const CONFIRM_MS = 10_000;
/** vc-feedback 9: the target highlight stays ~1 s after the action. */
export const HIGHLIGHT_MS = 1000;
const isLive = (l: LiveState | null): l is LiveState => !!l && (l.on || l.queue.length > 0);

/** Global points -> this window's CSS px (the window covers `display` exactly). */
const place = (r: PtRect, d: PtRect): CSSProperties => ({ left: r.x - d.x, top: r.y - d.y, width: r.w, height: r.h });

/** Union rect per region, in first-seen order: the tint shows how the tree was grouped. */
export function regionsOf(marks: Mark[]): [string, PtRect][] {
  const out = new Map<string, PtRect>();
  for (const { region, rect: r } of marks) {
    const u = out.get(region);
    if (!u) out.set(region, { ...r });
    else {
      const x = Math.min(u.x, r.x);
      const y = Math.min(u.y, r.y);
      out.set(region, { x, y, w: Math.max(u.x + u.w, r.x + r.w) - x, h: Math.max(u.y + u.h, r.y + r.h) - y });
    }
  }
  return [...out];
}

export default function Marks() {
  const [step, setStep] = useState<CommandStep | null>(null);
  /** The last step that carried marks: in a live session they stay up while key / type steps run. */
  const [board, setBoard] = useState<CommandStep | null>(null);
  const [live, setLive] = useState<LiveState | null>(null);
  const [lit, setLit] = useState(false);
  const [settings, setSettings] = useState<Settings | null>(null);
  useEffect(() => {
    const clear = () => (setStep(null), setBoard(null));
    const offs = [
      on("command_step", (c) => (setStep(c), c.marks.length && setBoard(c))),
      on("command_live", (l) => (setLive(l), !isLive(l) && clear())),
      on("session_state", (s) => s.phase === "idle" && clear()),
    ];
    api.getSettings().then(setSettings);
    if (!isTauri) import("../lib/mock").then((m) => m.marksDemo(new URLSearchParams(location.search).get("state")));
    return () => offs.forEach((p) => p.then((f) => f()));
  }, []);
  // a done step keeps its target lit for HIGHLIGHT_MS, then only the badges stay
  useEffect(() => {
    if (step?.status !== "done" || step.target == null) return setLit(false);
    setLit(true);
    const t = setTimeout(() => setLit(false), HIGHLIGHT_MS);
    return () => clearTimeout(t);
  }, [step]);

  const liveOn = isLive(live);
  if (liveOn && live.took_over) return <div className="marks-root took-over" />; // Pablo took over: gone at once
  if (!liveOn && (!step || !SHOWN.includes(step.status) || !step.marks.length)) return <div className="marks-root" />;
  const b = liveOn ? board : step;
  const marks = b?.marks ?? [];
  const d = b?.display ?? { x: 0, y: 0, w: 0, h: 0 };
  // a step without marks (a key, a type into the focused field) has no target on this board
  const target = step && step === b ? (marks.find((m) => m.id === step.target) ?? null) : null;
  const status = step?.status ?? "thinking";
  const picks = status === "pick" && step ? step.choices.map(([id]) => id) : [];
  const done = status === "done";
  const focus = status === "pick" ? picks : target && status !== "marks" && (!done || lit) ? [target.id] : [];
  const key = keysOf(settings?.hotkeys.command ?? "F2");

  return (
    <div className={`marks-root s-${status}${liveOn ? " live" : ""}`}>
      {regionsOf(marks).map(([name, r]) => (
        <div key={name} className="region" style={place(r, d)}>
          {r.w >= 120 && r.h >= 60 && <span className="region-name">{name}</span>}
        </div>
      ))}
      {marks.map((m, i) => {
        const pick = picks.indexOf(m.id);
        // after an action the rest stay readable: the next order may name any of them
        const cls = focus.length ? (focus.includes(m.id) ? (pick >= 0 ? " pick" : " chosen") : done ? "" : " faded") : "";
        return (
          <div key={m.id} className={`mark${cls}`} style={{ ...place(m.rect, d), ["--i" as string]: Math.min(i, 100) }}>
            <span className="badge">{m.id}</span>
          </div>
        );
      })}
      {step && target && (status === "aim" || status === "confirm" || (done && lit)) && (
        <Brackets key={`${step.step}:${target.id}`} r={target.rect} d={d} warn={status === "confirm"} linger={done} />
      )}
      {step ? <Hud step={step} target={target} cmdKey={key} live={liveOn ? live : null} /> : liveOn && <LiveHud live={live} />}
    </div>
  );
}

/** Registration marks on the target: 4 corner brackets close in over the 300 ms aim, a crosshair at the click point. */
function Brackets({ r, d, warn, linger }: { r: PtRect; d: PtRect; warn: boolean; linger: boolean }) {
  const pad = 12;
  const w = Math.max(48, r.w + 2 * pad);
  const h = Math.max(48, r.h + 2 * pad);
  const style: CSSProperties = { left: r.x - d.x + r.w / 2 - w / 2, top: r.y - d.y + r.h / 2 - h / 2, width: w, height: h };
  return (
    <div className={`brackets${warn ? " warn" : ""}${linger ? " linger" : ""}`} style={{ ...style, ["--hl" as string]: `${HIGHLIGHT_MS}ms` }} aria-hidden>
      <i className="c tl" />
      <i className="c tr" />
      <i className="c bl" />
      <i className="c br" />
      <i className="cross" />
    </div>
  );
}

/** Top-center card: step counter + verb + instruction + Esc, and below it the pick list or the confirm question. */
const LIVE_VERB: Partial<Record<CommandStep["status"], string>> = { marks: "LOOKING", pick: "WHICH ONE?", thinking: "PLANNING", queued: "NEXT", done: "DONE" };

/** Live session, before its first step: what it hears and what is queued. */
function LiveHud({ live }: { live: LiveState }) {
  return (
    <div className="hud live" role="status" aria-live="polite">
      <div className="hud-row">
        <span className="hud-step">LIVE</span>
        <span className="hud-verb">LISTENING</span>
        <span className="hud-esc">
          <kbd>esc</kbd> stops
        </span>
      </div>
      <Stream live={live} />
    </div>
  );
}

/** HEARD: the words not planned yet (or the last chunk, dim). NEXT: the queued actions, next first. */
function Stream({ live }: { live: LiveState }) {
  const heard = live.buffer.trim() || live.last_said.trim();
  return (
    <>
      {heard && (
        <div className="hud-line heard">
          <span className="hud-k">HEARD</span>
          <span className={`hud-v${live.buffer.trim() ? "" : " old"}`}>
            <span dir="ltr">{heard}</span>
          </span>
        </div>
      )}
      {live.queue.length > 0 && (
        <div className="hud-line next">
          <span className="hud-k">NEXT</span>
          <span className="hud-q-list">
            {live.queue.map((q, i) => (
              <span key={i} className="q-chip">
                {q}
              </span>
            ))}
          </span>
        </div>
      )}
    </>
  );
}

function Hud({ step, target, cmdKey, live }: { step: CommandStep; target: Mark | null; cmdKey: string[]; live: LiveState | null }) {
  const byId = new Map(step.marks.map((m) => [m.id, m]));
  const verb = (live && LIVE_VERB[step.status]) || (step.status === "marks" ? "LOOKING" : step.status === "pick" ? "WHICH ONE?" : (VERB[opOf(step)] ?? "ACT"));
  const ask = step.status === "pick" || isPick(live?.pending ?? null);
  const confirm = !ask && (step.status === "confirm" || !!live?.pending);
  const what = target?.name ?? step.detail;
  return (
    <div className={`hud${confirm ? " warn" : ask ? " ask" : ""}${live ? " live" : ""}`} role="status" aria-live="polite">
      <div className="hud-row">
        <span className="hud-step">
          {live ? "LIVE" : `STEP ${step.step}/${step.max}`}
        </span>
        <span className="hud-verb">{verb}</span>
        {live && what && step.status !== "pick" ? <span className="hud-what">{what}</span> : step.instruction && <span className="hud-instr">{step.instruction}</span>}
        <span className="hud-esc">
          <kbd>esc</kbd> stops
        </span>
      </div>
      {live && <Stream live={live} />}
      {ask && step.choices.length > 0 && (
        <>
          <ol className="hud-picks">
            {step.choices.slice(0, 3).map(([id, p]) => {
              const m = byId.get(id);
              return (
                <li key={id}>
                  <span className="badge on">{id}</span>
                  <span className="pick-name">{m?.name || "(no name)"}</span>
                  <span className="pick-meta">
                    {m?.role} · {m?.region}
                  </span>
                  <span className="pick-p">{Math.round(p * 100)}%</span>
                </li>
              );
            })}
          </ol>
          <p className="hud-foot">
            {live ? (
              "Not sure enough to act. Say the number."
            ) : (
              <>
                Not sure enough to act. Hold <Kbds k={cmdKey} /> and say the number.
              </>
            )}
          </p>
        </>
      )}
      {confirm && (
        <>
          <p className="hud-q">
            {pendingQuestion(live?.pending ?? null) ?? (
              <>
                {step.action === "focus_and_type" ? "Type in" : "Click"} <b>“{target?.name ?? "this"}”</b>? It may send or delete something.
              </>
            )}
          </p>
          <p className="hud-foot">
            {live ? "Say “yes” or press " : "Press "}
            <Kbds k={cmdKey} /> to confirm · <kbd>esc</kbd> cancels
          </p>
          <span className="hud-drain" style={{ animationDuration: `${CONFIRM_MS}ms` }} aria-hidden />
        </>
      )}
    </div>
  );
}

const Kbds = ({ k }: { k: string[] }) => (
  <>
    {k.map((x, i) => (
      <kbd key={i}>{x}</kbd>
    ))}
  </>
);
