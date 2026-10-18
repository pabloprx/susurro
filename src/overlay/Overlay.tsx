// The overlay pill (DESIGN 5). Its own window: no router, no store, only the event listeners + the canvas.
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Clipboard, Hand, Lock, Mic, Square, X } from "lucide-react";
import { api, isTauri, on, type CommandStep, type LiveState, type Recents, type SessionState, type Settings, type Toast } from "../lib/api";
import { firstLine, hasDeadline, isPick, keysOf, mmss, opOf, pendingQuestion, VERB } from "../lib/format";
import { Waveform } from "./waveform";
import "../styles/tokens.css";
import "../styles/overlay.css";

type CmdView = "cmd-listening" | "cmd-live" | "cmd-paused" | "cmd-thinking" | "cmd-acting" | "cmd-pick" | "cmd-confirm" | "cmd-done" | "cmd-stopped" | "cmd-unsure" | "cmd-blocked" | "cmd-setup";
type View = "idle" | "listening" | "grace" | "handsfree" | "processing" | "done" | "copied" | "error" | "notice" | CmdView;
/** Phases that own the pill: a notice from before them is cleared (vc-feedback 19). */
const ACTIVE: SessionState["phase"][] = ["recording", "locked", "processing", "acting"];
const HOVER_CAP_MS = 8000;
const IDLE: SessionState = { phase: "idle", started_at: null, device: null, app: null, message: null, command: false };
const HOLD: Partial<Record<View, number>> = {
  done: 700,
  copied: 2500,
  error: 4000,
  "cmd-done": 1200,
  "cmd-stopped": 1200,
  "cmd-unsure": 3000,
  "cmd-blocked": 3000,
  "cmd-setup": 6000,
};
/** Voice control (F2) results that outlive the run: shown for their HOLD after the last command_step. */
const CMD_END: Partial<Record<CommandStep["status"], CmdView>> = { done: "cmd-done", stopped: "cmd-stopped", unsure: "cmd-unsure", blocked: "cmd-blocked", setup: "cmd-setup", error: "cmd-unsure" };
const CMD_LIVE: Partial<Record<CommandStep["status"], CmdView>> = { thinking: "cmd-thinking", marks: "cmd-thinking", aim: "cmd-acting", pick: "cmd-pick", confirm: "cmd-confirm" };
const CONFIRM_MS = 10_000;
/** 0 = auto width, measured from the content (max 300 by .content max-width). Compact on purpose (DESIGN 5). */
const SIZE: Record<View, [number, number]> = {
  idle: [44, 8],
  listening: [200, 36],
  grace: [0, 36],
  handsfree: [232, 36],
  processing: [120, 36],
  done: [120, 36],
  copied: [168, 36],
  error: [0, 36], // auto, measured
  notice: [0, 36],
  "cmd-listening": [176, 36],
  "cmd-live": [272, 36], // fixed: the heard text grows per chunk, the pill must not jitter
  "cmd-paused": [0, 36],
  "cmd-thinking": [0, 36],
  "cmd-acting": [0, 36],
  "cmd-pick": [0, 36],
  "cmd-confirm": [0, 36],
  "cmd-done": [0, 36],
  "cmd-stopped": [0, 36],
  "cmd-unsure": [0, 36],
  "cmd-blocked": [0, 36],
  "cmd-setup": [0, 36],
};
const WAVE_VIEWS: View[] = ["listening", "handsfree", "processing", "cmd-listening"];
const reduced = matchMedia("(prefers-reduced-motion: reduce)");
const SNOOZE_MS = 60 * 60_000;

/** Show the main window on one screen (the bar and its menu have no router of their own). */
async function openMain(screen: string) {
  if (!isTauri) return;
  const [{ Window }, { emitTo }] = await Promise.all([import("@tauri-apps/api/window"), import("@tauri-apps/api/event")]);
  const main = await Window.getByLabel("main");
  await main?.show();
  await main?.setFocus();
  await emitTo("main", "navigate", { screen });
}

/** The one-click fix for an error, when the message names a cause the user can change. */
function fixFor(msg: string | null): { label: string; run: () => unknown } | null {
  if (!msg) return null;
  if (/accessibility/i.test(msg)) return { label: "Turn on", run: () => api.requestPermission("accessibility") };
  if (/microphone|mic\b|no audio|muted/i.test(msg)) return { label: "Check mic", run: () => openMain("audio") };
  if (/\bkey\b|401|403|openrouter|elevenlabs|provider|server|model/i.test(msg)) return { label: "Fix", run: () => openMain("providers") };
  return null;
}

type Step = CommandStep & { at: number };

/** A live session (VOICE_CONTROL_V2 1): on, or still draining its queue after the key went up. */
const isLive = (l: LiveState | null): l is LiveState => !!l && (l.on || l.queue.length > 0);

function viewOf(s: SessionState, step: Step | null, live: LiveState | null): View {
  if (isLive(live)) {
    if (isPick(live.pending) || step?.status === "pick") return "cmd-pick";
    if (live.pending || step?.status === "confirm") return "cmd-confirm";
    if (live.paused) return "cmd-paused";
    if (live.buffer.trim()) return "cmd-live";
    if (step && CMD_LIVE[step.status]) return CMD_LIVE[step.status]!;
    return "cmd-listening";
  }
  if (s.command && (s.phase === "recording" || s.phase === "locked")) return "cmd-listening";
  if (s.command && s.phase === "processing") return "cmd-thinking";
  if (step) {
    const end = CMD_END[step.status];
    if (s.phase === "acting") return CMD_LIVE[step.status] ?? end ?? "cmd-thinking";
    // the result lands as the phase goes idle: show it, then the HOLD logic keeps it up
    if (end && s.phase === "idle" && Date.now() - step.at < 400) return end;
  }
  switch (s.phase) {
    case "recording":
      return s.grace_ms ? "grace" : "listening";
    case "locked":
      return "handsfree";
    case "processing":
      return "processing";
    case "done":
      return s.message === "copied" ? "copied" : "done";
    case "error":
      return "error";
    case "acting":
      return "cmd-thinking";
    default:
      return "idle";
  }
}


export default function Overlay() {
  const [sess, setSess] = useState<SessionState>(IDLE);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [shown, setShown] = useState<{ view: View; msg: string | null; since: number; from: View }>({
    view: "idle",
    msg: null,
    since: 0,
    from: "idle",
  });
  const [notice, setNotice] = useState<Toast | null>(null);
  const [step, setStep] = useState<Step | null>(null);
  const [live, setLive] = useState<LiveState | null>(null);
  const [recents, setRecents] = useState<Recents>({ items: [], picked: null });
  const [hover, setHover] = useState(false);
  const phaseRef = useRef<SessionState["phase"]>("idle");
  const [snoozed, setSnoozed] = useState(false);
  const [, tick] = useState(0);
  const wave = useRef<Waveform | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const pill = useRef<HTMLDivElement>(null);
  const [autoW, setAutoW] = useState(200);
  const [waveX, setWaveX] = useState(0);

  // events
  useEffect(() => {
    const offs = [
      on("session_state", (s) => {
        // vc-feedback 19: a new recording (or its processing / acting) always beats a notice left from idle.
        const active = ACTIVE.includes(s.phase);
        if (active && !ACTIVE.includes(phaseRef.current)) setNotice(null);
        phaseRef.current = s.phase;
        setSess(s);
        if (s.phase === "recording" || s.phase === "locked") api.getSettings().then(setSettings);
      }),
      on("audio_level", (l) => wave.current?.push(l.db)),
      // step 0 = a marks board refresh (live.rs draw_board), not a step: the pill keeps what it shows
      on("command_step", (c) => c.step !== 0 && setStep({ ...c, at: Date.now() })),
      on("command_live", (l) => {
        // the session just ended: its last step is the result to show (viewOf shows a fresh end step)
        setLive((prev) => (isLive(prev) && !isLive(l) && setStep((st) => st && { ...st, at: Date.now() }), l));
      }),
      on("recents", setRecents),
      on("toast", async (t) => {
        // The main window keeps the toast stack when it is visible (DESIGN 6.6); the pill shows one notice otherwise.
        if (isTauri) {
          const { Window } = await import("@tauri-apps/api/window");
          const main = await Window.getByLabel("main");
          if (main && (await main.isVisible()) && (await main.isFocused())) return;
        }
        setNotice(t);
      }),
    ];
    api.getSettings().then(setSettings);
    api.getSessionState().then((s) => s && setSess(s));
    if (!isTauri) {
      document.body.classList.add("mock-desk");
      const state = new URLSearchParams(location.search).get("state");
      if (state === "hover") setHover(true);
      import("../lib/mock").then((m) => m.overlayDemo(state));
    }
    return () => offs.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (!snoozed) return;
    const t = setTimeout(() => setSnoozed(false), SNOOZE_MS);
    return () => clearTimeout(t);
  }, [snoozed]);

  // notice lifetime: ttl, capped at 4 s unless it asks for a decision. Undo keeps the exact Rust deadline.
  const deadline = notice !== null && hasDeadline(notice);
  const ttl = !notice ? 0 : deadline ? notice.ttl_ms : notice.actions.length ? Math.max(notice.ttl_ms, 6000) : Math.min(notice.ttl_ms || 2500, 4000);
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => (deadline || !hover) && setNotice(null), ttl);
    return () => clearTimeout(t);
  }, [notice, deadline || hover]);
  // a stuck hover (the window turned click-through before mouseleave fired) never holds a notice past 8 s
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice((n) => (n === notice ? null : n)), Math.max(ttl, HOVER_CAP_MS));
    return () => clearTimeout(t);
  }, [notice]);
  useEffect(() => {
    const off = () => setHover(false);
    window.addEventListener("blur", off);
    return () => window.removeEventListener("blur", off);
  }, []);

  // a command result stops being the target 400 ms after it lands (viewOf): re-render then, so the HOLD logic takes over
  useEffect(() => {
    if (!step || !CMD_END[step.status]) return;
    const t = setTimeout(() => tick((n) => n + 1), 450);
    return () => clearTimeout(t);
  }, [step]);

  // session -> shown view, with the holds for done / copied / error
  useEffect(() => {
    const target = viewOf(sess, step, live);
    const hold = HOLD[shown.view];
    if (target === "idle" && hold && shown.view !== "idle") {
      const left = shown.since + hold - Date.now();
      if (left > 0 || ((shown.view === "error" || shown.view === "cmd-setup") && hover)) {
        const t = setTimeout(() => tick((n) => n + 1), Math.max(left, 100));
        return () => clearTimeout(t);
      }
    }
    if (target !== shown.view || sess.message !== shown.msg)
      setShown({ view: target, msg: sess.message, since: Date.now(), from: shown.view });
  });

  const view: View = notice ? "notice" : shown.view;
  const recording = view === "listening" || view === "handsfree" || (view === "notice" && WAVE_VIEWS.includes(shown.view));

  // timer + warning
  useEffect(() => {
    if (!recording) return;
    const t = setInterval(() => tick((n) => n + 1), 250);
    return () => clearInterval(t);
  }, [recording]);
  const elapsed = sess.started_at ? (Date.now() - sess.started_at) / 1000 : 0;
  const max = settings?.recording.max_seconds ?? 600;
  const warning = (view === "listening" || view === "handsfree") && elapsed >= max - 60;

  // waveform: alive only while it is on screen (DESIGN 5: do not draw at rest)
  const waveOn = WAVE_VIEWS.includes(view) && !(view === "processing" && reduced.matches);
  useEffect(() => {
    if (!canvas.current) return;
    // The error shake remounts the pill (its `key`), so the canvas element can be a new one.
    if (wave.current?.canvas !== canvas.current) {
      wave.current?.stop();
      wave.current = new Waveform(canvas.current);
    }
    if (waveOn) wave.current.start(view === "processing");
    else wave.current.stop();
  }, [waveOn, view]);

  // measure the incoming content: auto width for error / notice, and where the waveform slot sits
  useLayoutEffect(() => {
    const c = pill.current?.querySelector<HTMLElement>(".layer.in .content");
    if (!c) return;
    if (!SIZE[view][0] || (view === "idle" && hover)) setAutoW(c.offsetWidth + 28);
    const slot = c.querySelector(".wave-slot");
    if (slot) {
      const a = slot.getBoundingClientRect();
      const b = c.getBoundingClientRect();
      setWaveX(a.left + a.width / 2 - (b.left + b.width / 2));
    } else if (view === "processing") setWaveX(0);
  });

  let [w, h] = SIZE[view];
  if (!w) w = autoW;
  if (warning) w += view === "listening" ? 70 : 34; // "0:59 left" is wider than "0:42"
  if (view === "idle" && hover) [w, h] = [autoW, 24];
  const hidden = view === "idle" && settings && !settings.overlay.show_idle;
  const ptt = keysOf(settings?.hotkeys.ptt ?? "F1").join(" ");
  const cmdKey = keysOf(settings?.hotkeys.command ?? "F2");
  const fix = view === "error" ? fixFor(shown.msg) : null;
  const target = step?.marks.find((m) => m.id === step.target) ?? null;

  // The window ignores the mouse outside this rect (platform/macos.rs hit test, which adds its own slack), so apps
  // under the window stay clickable. The pill sits bottom-center, 18 px up (overlay.css .pill-root). The live size,
  // frame by frame during the 260 ms morph (a ResizeObserver sees each width/height step): the hit zone never
  // runs ahead of the visible pill when it grows, nor lags it when it shrinks.
  const lastRect = useRef("");
  const gone = snoozed && view === "idle";
  useEffect(() => {
    const root = document.getElementById("root")!;
    const el = pill.current;
    const report = () => {
      const [pw, ph] = el && !gone ? [el.offsetWidth, el.offsetHeight] : [0, 0];
      const r = pw ? { x: Math.round((root.clientWidth - pw) / 2), y: root.clientHeight - 18 - ph, w: pw, h: ph } : { x: 0, y: 0, w: 0, h: 0 };
      const k = `${r.x},${r.y},${r.w},${r.h}`;
      if (k === lastRect.current) return;
      lastRect.current = k;
      api.setPillRect(r).catch(() => {});
    };
    report();
    const ro = new ResizeObserver(report);
    if (el) ro.observe(el);
    ro.observe(root);
    return () => ro.disconnect();
  }, [gone]);

  const timer = (
    <span className={`timer${warning ? " warn" : ""}`}>{warning ? `${mmss(Math.max(0, max - elapsed))} left` : mmss(elapsed)}</span>
  );

  const body: Record<View, ReactNode> = {
    idle: <span className="hint">Click or hold {ptt}</span>,
    listening: (
      <>
        <span className="tally-dot pulse" />
        <PillButton label="Cancel" onClick={api.dictationCancel}>
          <X size={13} strokeWidth={2} />
        </PillButton>
        <span className="wave-slot" />
        {warning && timer}
        <PillButton label="Stop and paste" onClick={api.dictationStop}>
          <Square size={9} fill="currentColor" strokeWidth={0} />
        </PillButton>
      </>
    ),
    // a hold was released: the recording goes on for grace_ms; a tap now locks hands-free (session.rs Grace)
    grace: (
      <>
        <span className="tally-dot pulse" />
        <span className="msg dim-text">Tap {ptt} again for hands-free</span>
        <span className="drain soft" key={sess.started_at ?? 0} style={{ animationDuration: `${sess.grace_ms ?? 0}ms` }} aria-hidden />
      </>
    ),
    handsfree: (
      <>
        <span className="tally-dot" />
        <PillButton label="Cancel" onClick={api.dictationCancel}>
          <X size={13} strokeWidth={2} />
        </PillButton>
        <span className="wave-slot" />
        {timer}
        <PillButton label="Stop and paste" onClick={api.dictationStop}>
          <Square size={9} fill="currentColor" strokeWidth={0} />
        </PillButton>
      </>
    ),
    processing: reduced.matches ? <span className="dots-static">…</span> : <span />,
    "cmd-listening": (
      <>
        <Reticle live />
        <span className="wave-slot" />
        <span className="cmd-tag">CMD</span>
      </>
    ),
    "cmd-live": (
      <>
        <Reticle live />
        <span className="heard" title={live?.buffer}>
          <span dir="ltr">{live?.buffer.slice(-120)}</span>
        </span>
        <Queue n={live?.queue.length ?? 0} />
      </>
    ),
    "cmd-paused": (
      <>
        <Hand size={13} strokeWidth={1.75} className="dim" />
        <span className="msg dim-text">Paused, you took over</span>
        <Queue n={live?.queue.length ?? 0} />
      </>
    ),
    "cmd-thinking": (
      <>
        {isLive(live) ? <Queue n={live.queue.length} /> : step && <Ticks n={step.max} done={step.step - 1} />}
        <span className="msg ellipsis instr">{step?.instruction || "Listening to the command"}</span>
        <Dots />
      </>
    ),
    "cmd-acting": (
      <>
        {isLive(live) ? <Queue n={live.queue.length} /> : step && <Ticks n={step.max} done={step.step - 1} />}
        <span className="cmd-verb">{VERB[step ? opOf(step) : ""] ?? "ACT"}</span>
        {(target || step?.detail) && <span className="msg ellipsis instr">{target?.name ?? step?.detail}</span>}
      </>
    ),
    "cmd-pick": (
      <>
        <Reticle live />
        <span className="msg">Which one?</span>
        <span className="small-dim">say its number</span>
      </>
    ),
    "cmd-confirm": (
      <>
        <AlertTriangle size={13} strokeWidth={1.75} className="warn" />
        <span className="msg ellipsis instr">{pendingQuestion(live?.pending ?? null) ?? confirmText(step, target?.name)}</span>
        <span className="small-dim">say yes ·</span>
        <Keys k={cmdKey} />
        <span className="small-dim">·</span>
        <Keys k={["esc"]} />
        <span className="drain" style={{ animationDuration: `${CONFIRM_MS}ms` }} aria-hidden />
      </>
    ),
    "cmd-done": (
      <>
        <Check size={14} strokeWidth={2} className="ok" />
        <span className="msg">
          Done{step?.step ? <span className="mono-dim"> · {step.step} {step.step === 1 ? "step" : "steps"}</span> : null}
        </span>
      </>
    ),
    "cmd-stopped": <span className="msg dim-text">Stopped</span>,
    "cmd-unsure": <span className="msg dim-text ellipsis">{step?.message || "Not sure where. Say it again."}</span>,
    "cmd-blocked": (
      <>
        <Lock size={13} strokeWidth={1.75} className="dim" />
        <span className="msg ellipsis">{step?.message || "Password field. susurro won't touch it."}</span>
      </>
    ),
    "cmd-setup": (
      <>
        <span className="msg ellipsis">{step?.message === "key" ? "Voice control needs an OpenRouter key" : "Voice control needs Screen Recording"}</span>
        <button
          className="link"
          onClick={(e) => {
            e.stopPropagation();
            void openMain(step?.message === "key" ? "providers" : "voice");
          }}
        >
          Set up
        </button>
      </>
    ),
    done: (
      <>
        <Check size={14} strokeWidth={2} className="ok" />
        <span className="msg">Pasted</span>
      </>
    ),
    copied: (
      <>
        <Clipboard size={13} strokeWidth={1.75} className="dim" />
        <span className="msg">Copied to clipboard</span>
      </>
    ),
    error: (
      <>
        <AlertTriangle size={13} strokeWidth={1.75} className="danger" />
        <span className="msg ellipsis" title={shown.msg || undefined}>{shown.msg || "Dictation failed"}</span>
        {fix && (
          <button
            className="link"
            onClick={(e) => {
              e.stopPropagation();
              void fix.run();
            }}
          >
            {fix.label}
          </button>
        )}
      </>
    ),
    notice: notice && (
      <>
        {notice.kind === "info" ? (
          <Mic size={13} strokeWidth={1.75} className="dim" />
        ) : notice.kind === "success" ? (
          <Check size={13} strokeWidth={2} className="ok" />
        ) : (
          <AlertTriangle size={13} strokeWidth={1.75} className={notice.kind === "error" ? "danger" : "warn"} />
        )}
        <span className="msg ellipsis">{notice.text}</span>
        {notice.actions.map((a) => (
          <button
            key={a.id}
            className="link"
            onClick={(e) => {
              e.stopPropagation();
              setNotice(null);
              api.toastAction(notice.id, a.id).catch((err) => setNotice({ id: `err${Date.now()}`, kind: "error", text: String(err), actions: [], ttl_ms: 4000 }));
            }}
          >
            {a.label}
          </button>
        ))}
      </>
    ),
  };

  async function menu(e: React.MouseEvent) {
    e.preventDefault();
    if (!isTauri) return;
    const { Menu, CheckMenuItem, MenuItem, PredefinedMenuItem, Submenu } = await import("@tauri-apps/api/menu");
    const mic = await api.getMicState();
    const auto = mic.selection === "auto";
    const sep = () => PredefinedMenuItem.new({ item: "Separator" });
    const mics = await Submenu.new({
      text: "Microphone",
      items: [
        await CheckMenuItem.new({ text: "Auto", checked: auto, action: () => void api.setMicSelection("auto") }),
        await sep(),
        ...(await Promise.all(
          mic.devices.map((d) =>
            CheckMenuItem.new({
              text: d.name,
              checked: !auto && mic.active === d.id,
              action: () => void api.setMicSelection({ device: { id: d.id } }),
            }),
          ),
        )),
        await sep(),
        await MenuItem.new({ text: "Audio settings…", action: () => void openMain("audio") }),
      ],
    });
    const items = [
      await MenuItem.new({ text: "Paste last transcript", action: () => void api.pasteLast() }),
      mics,
      await sep(),
      await MenuItem.new({ text: "Hide for 1 hour", action: () => setSnoozed(true) }),
      await MenuItem.new({ text: "Open susurro", action: () => void openMain("home") }),
    ];
    (await Menu.new({ items })).popup();
  }

  // a new step re-crossfades the thinking / acting content (the instruction or the verb changes)
  const key = view === "notice" ? `n${notice?.id}` : `${view}${view === "error" ? shown.msg : ""}${view.startsWith("cmd-") && view !== "cmd-live" && view !== "cmd-paused" && step ? `:${step.step}` : ""}`;
  const shake = view === "idle" && shown.from === "error" && !reduced.matches;

  return (
    <div className="pill-root">
      {recents.items.length > 0 && <RecentsList r={recents} />}
      <div
        className={`pill v-${view}${hover ? " hover" : ""}${hidden ? " hidden" : ""}${snoozed && view === "idle" ? " snoozed" : ""}${warning ? " warning" : ""}${shake ? " shake" : ""}`}
        ref={pill}
        style={{ width: w, height: h }}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
        onClick={() => view === "idle" && api.dictationToggle()}
        onContextMenu={menu}
        key={shake ? `s${shown.since}` : "pill"}
      >
        <canvas
          ref={canvas}
          aria-hidden
          className={`wave${waveOn ? " on" : ""}`}
          style={{ transform: `translate(calc(-50% + ${waveX}px), -50%)` }}
        />
        <Crossfade id={key}>
          <div className="content">
            {body[view]}
          </div>
        </Crossfade>
      </div>
    </div>
  );
}

/** "Click “Send now”?" / "Delete this?": the confirm question names the action and its target. */
function confirmText(step: Step | null, name?: string) {
  const verb = step?.action === "press_key" ? "Press the key" : step?.action === "focus_and_type" ? "Type in" : "Click";
  return name ? `${verb} “${name}”?` : "Do this?";
}

/** 10 px reticle (circle + 4 ticks): F2 is live, not F1 (the dictation dot). */
function Reticle({ live }: { live?: boolean }) {
  return (
    <svg className={`reticle${live ? " pulse" : ""}`} width="12" height="12" viewBox="0 0 12 12" aria-hidden>
      <circle cx="6" cy="6" r="3" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M6 0v2M6 10v2M0 6h2M10 6h2" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

/** Ctrl+ptt: the last 9 dictations above the pill. Rust owns the keys (1-9 paste, Cmd+1-9 copy, Esc) as temporary
 *  global shortcuts, so the app in front keeps the focus. ponytail: no clicks on rows (a click would take the focus
 *  from the app the paste must land in); add them when the overlay can take a click without activating. */
function RecentsList({ r }: { r: Recents }) {
  return (
    <div className="recents" role="listbox" aria-label="Recent dictations">
      {r.items.slice(0, 9).map((h, i) => {
        const n = i + 1;
        const flag = h.status === "copied" ? "not pasted" : h.status === "failed" ? "failed" : null;
        return (
          <div key={h.id} className={`rc-row${r.picked === n ? " picked" : ""}`} role="option" aria-selected={r.picked === n} aria-label={`${n}: ${firstLine(h.final_text || h.raw)}`}>
            <kbd className="rc-n">{n}</kbd>
            <span className="rc-text">{firstLine(h.final_text || h.raw) || "(empty)"}</span>
            {flag && (
              <span className={`rc-flag ${h.status}`} title={flag}>
                {h.status === "copied" ? <Clipboard size={11} strokeWidth={1.75} /> : <AlertTriangle size={11} strokeWidth={1.75} />}
                {flag}
              </span>
            )}
            <span className="rc-app">{h.app_name ?? ""}</span>
          </div>
        );
      })}
      <div className="rc-foot">
        <span>
          <kbd>1</kbd>-<kbd>9</kbd> paste
        </span>
        <span>
          <kbd>⌘</kbd>
          <kbd>1</kbd>-<kbd>9</kbd> copy
        </span>
        <span>
          <kbd>esc</kbd> close
        </span>
      </div>
    </div>
  );
}

/** One dot per action waiting in the live queue (max 6 dots, then +n). */
function Queue({ n }: { n: number }) {
  if (!n) return null;
  return (
    <span className="queue" aria-label={`${n} queued`}>
      {Array.from({ length: Math.min(n, 6) }, (_, i) => (
        <i key={i} />
      ))}
      {n > 6 && <b>+{n - 6}</b>}
    </span>
  );
}

/** One tick per allowed step: done = bright, the rest = faint. */
function Ticks({ n, done }: { n: number; done: number }) {
  return (
    <span className="ticks" aria-label={`Step ${done + 1} of ${n}`}>
      {Array.from({ length: n }, (_, i) => (
        <i key={i} className={i < done ? "on" : i === done ? "now" : ""} />
      ))}
    </span>
  );
}

const Dots = () =>
  reduced.matches ? (
    <span className="dots-static">…</span>
  ) : (
    <span className="dots" aria-hidden>
      {[0, 1, 2, 3, 4].map((i) => (
        <i key={i} style={{ animationDelay: `${i * 90}ms` }} />
      ))}
    </span>
  );

const Keys = ({ k }: { k: string[] }) => (
  <span className="pill-kbd">
    {k.map((x, i) => (
      <kbd key={i}>{x}</kbd>
    ))}
  </span>
);

function PillButton(p: { label: string; onClick: () => unknown; children: ReactNode }) {
  return (
    <button
      className="pill-btn"
      aria-label={p.label}
      title={p.label}
      onClick={(e) => {
        e.stopPropagation();
        p.onClick();
      }}
    >
      {p.children}
    </button>
  );
}

/** Outgoing content blurs out in 120 ms; incoming fades in after 60 ms (DESIGN 5 morph). */
function Crossfade({ id, children }: { id: string; children: ReactNode }) {
  const [leaving, setLeaving] = useState<{ id: string; node: ReactNode } | null>(null);
  const last = useRef({ id, node: children });
  if (last.current.id !== id) {
    const prev = last.current;
    last.current = { id, node: children };
    queueMicrotask(() => setLeaving(prev));
  } else last.current.node = children;
  useEffect(() => {
    if (!leaving) return;
    const t = setTimeout(() => setLeaving(null), 130);
    return () => clearTimeout(t);
  }, [leaving]);
  return (
    <>
      {leaving && leaving.id !== id && (
        <div className="layer out" key={`out-${leaving.id}`}>
          {leaving.node}
        </div>
      )}
      <div className="layer in" key={id}>
        {children}
      </div>
    </>
  );
}
