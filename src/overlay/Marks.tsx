// Voice control: the full-screen, click-through marks window (one per command, on the display it acts on).
// It draws what Jev chooses from: a numbered badge on every actionable AX element, a faint tint per region, then the
// chosen target (registration brackets + crosshair), the "Which one?" top 3, or the risky-action confirm.
// No router, no store: one command_step listener, like the pill.
import { useEffect, useState, type CSSProperties } from "react";
import { api, isTauri, on, type CommandStep, type Mark, type PtRect, type Settings } from "../lib/api";
import { keysOf, VERB } from "../lib/format";
import "../styles/tokens.css";
import "../styles/marks.css";

/** Statuses that show marks. Anything else (thinking before the AX read, done, stopped ...) = empty window. */
const SHOWN: CommandStep["status"][] = ["marks", "aim", "pick", "confirm"];
const CONFIRM_MS = 10_000;

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
  const [settings, setSettings] = useState<Settings | null>(null);
  useEffect(() => {
    const offs = [on("command_step", setStep), on("session_state", (s) => s.phase === "idle" && setStep(null))];
    api.getSettings().then(setSettings);
    if (!isTauri) import("../lib/mock").then((m) => m.marksDemo(new URLSearchParams(location.search).get("state")));
    return () => offs.forEach((p) => p.then((f) => f()));
  }, []);

  if (!step || !SHOWN.includes(step.status) || !step.marks.length) return <div className="marks-root" />;
  const d = step.display;
  const target = step.marks.find((m) => m.id === step.target) ?? null;
  const picks = step.status === "pick" ? step.choices.map(([id]) => id) : [];
  const focus = step.status === "pick" ? picks : target && step.status !== "marks" ? [target.id] : [];
  const key = keysOf(settings?.hotkeys.command ?? "F2");

  return (
    <div className={`marks-root s-${step.status}`}>
      {regionsOf(step.marks).map(([name, r]) => (
        <div key={name} className="region" style={place(r, d)}>
          {r.w >= 120 && r.h >= 60 && <span className="region-name">{name}</span>}
        </div>
      ))}
      {step.marks.map((m, i) => {
        const pick = picks.indexOf(m.id);
        const cls = focus.length ? (focus.includes(m.id) ? (pick >= 0 ? " pick" : " chosen") : " faded") : "";
        return (
          <div key={m.id} className={`mark${cls}`} style={{ ...place(m.rect, d), ["--i" as string]: Math.min(i, 100) }}>
            <span className="badge">{m.id}</span>
          </div>
        );
      })}
      {target && (step.status === "aim" || step.status === "confirm") && <Brackets key={`${step.step}:${target.id}`} r={target.rect} d={d} warn={step.status === "confirm"} />}
      <Hud step={step} target={target} cmdKey={key} />
    </div>
  );
}

/** Registration marks on the target: 4 corner brackets close in over the 300 ms aim, a crosshair at the click point. */
function Brackets({ r, d, warn }: { r: PtRect; d: PtRect; warn: boolean }) {
  const pad = 12;
  const w = Math.max(48, r.w + 2 * pad);
  const h = Math.max(48, r.h + 2 * pad);
  const style: CSSProperties = { left: r.x - d.x + r.w / 2 - w / 2, top: r.y - d.y + r.h / 2 - h / 2, width: w, height: h };
  return (
    <div className={`brackets${warn ? " warn" : ""}`} style={style} aria-hidden>
      <i className="c tl" />
      <i className="c tr" />
      <i className="c bl" />
      <i className="c br" />
      <i className="cross" />
    </div>
  );
}

/** Top-center card: step counter + verb + instruction + Esc, and below it the pick list or the confirm question. */
function Hud({ step, target, cmdKey }: { step: CommandStep; target: Mark | null; cmdKey: string[] }) {
  const byId = new Map(step.marks.map((m) => [m.id, m]));
  const verb = step.status === "marks" ? "LOOKING" : step.status === "pick" ? "WHICH ONE?" : (VERB[step.action ?? ""] ?? "ACT");
  return (
    <div className={`hud${step.status === "confirm" ? " warn" : step.status === "pick" ? " ask" : ""}`} role="status" aria-live="polite">
      <div className="hud-row">
        <span className="hud-step">
          STEP {step.step}/{step.max}
        </span>
        <span className="hud-verb">{verb}</span>
        <span className="hud-instr">{step.instruction}</span>
        <span className="hud-esc">
          <kbd>esc</kbd> stops
        </span>
      </div>
      {step.status === "pick" && (
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
            Not sure enough to act. Hold <Kbds k={cmdKey} /> and say the number.
          </p>
        </>
      )}
      {step.status === "confirm" && (
        <>
          <p className="hud-q">
            {step.action === "focus_and_type" ? "Type in" : "Click"} <b>“{target?.name ?? "this"}”</b>? It may send or delete something.
          </p>
          <p className="hud-foot">
            Press <Kbds k={cmdKey} /> to confirm · <kbd>esc</kbd> cancels
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
