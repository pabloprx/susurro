// The overlay pill (DESIGN 5). Its own window: no router, no store, only the event listeners + the canvas.
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Clipboard, Mic, Square, X } from "lucide-react";
import { api, isTauri, on, type SessionState, type Settings, type Toast } from "../lib/api";
import { hasDeadline, keysOf, mmss } from "../lib/format";
import { Waveform } from "./waveform";
import "../styles/tokens.css";
import "../styles/overlay.css";

type View = "idle" | "listening" | "handsfree" | "processing" | "done" | "copied" | "error" | "notice";
const IDLE: SessionState = { phase: "idle", started_at: null, device: null, app: null, message: null };
const HOLD: Partial<Record<View, number>> = { done: 700, copied: 2500, error: 4000 };
const SIZE: Record<View, [number, number]> = {
  idle: [44, 8],
  listening: [200, 36],
  handsfree: [232, 36],
  processing: [120, 36],
  done: [120, 36],
  copied: [260, 36],
  error: [0, 36], // auto, measured
  notice: [0, 36],
};
const WAVE_VIEWS: View[] = ["listening", "handsfree", "processing"];
const reduced = matchMedia("(prefers-reduced-motion: reduce)");

function viewOf(s: SessionState): View {
  switch (s.phase) {
    case "recording":
      return "listening";
    case "locked":
      return "handsfree";
    case "processing":
      return "processing";
    case "done":
      return s.message === "copied" ? "copied" : "done";
    case "error":
      return "error";
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
  const [hover, setHover] = useState(false);
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
        setSess(s);
        if (s.phase === "recording" || s.phase === "locked") api.getSettings().then(setSettings);
      }),
      on("audio_level", (l) => wave.current?.push(l.db)),
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

  // notice lifetime: ttl, capped at 4 s unless it asks for a decision. Undo keeps the exact Rust deadline.
  const deadline = notice !== null && hasDeadline(notice);
  useEffect(() => {
    if (!notice) return;
    const ttl = deadline ? notice.ttl_ms : notice.actions.length ? Math.max(notice.ttl_ms, 6000) : Math.min(notice.ttl_ms || 2500, 4000);
    const t = setTimeout(() => (deadline || !hover) && setNotice(null), ttl);
    return () => clearTimeout(t);
  }, [notice, deadline || hover]);

  // session -> shown view, with the holds for done / copied / error
  useEffect(() => {
    const target = viewOf(sess);
    const hold = HOLD[shown.view];
    if (target === "idle" && hold && shown.view !== "idle") {
      const left = shown.since + hold - Date.now();
      if (left > 0 || (shown.view === "error" && hover)) {
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
    wave.current ??= new Waveform(canvas.current);
    if (waveOn) wave.current.start(view === "processing");
    else wave.current.stop();
  }, [waveOn, view]);

  // measure the incoming content: auto width for error / notice, and where the waveform slot sits
  useLayoutEffect(() => {
    const c = pill.current?.querySelector<HTMLElement>(".layer.in .content");
    if (!c) return;
    if (view === "error" || view === "notice") setAutoW(c.offsetWidth + 28);
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
  if (view === "idle" && hover) [w, h] = [120, 24];
  const hidden = view === "idle" && settings && !settings.overlay.show_idle;

  const timer = (
    <span className={`timer${warning ? " warn" : ""}`}>{warning ? `${mmss(Math.max(0, max - elapsed))} left` : mmss(elapsed)}</span>
  );

  const body: Record<View, ReactNode> = {
    idle: <span className="hint">{keysOf(settings?.hotkeys.ptt ?? "Fn").join(" ")} to talk</span>,
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
    done: (
      <>
        <Check size={14} strokeWidth={2} className="ok" />
        <span className="msg">Pasted</span>
      </>
    ),
    copied: (
      <>
        <Clipboard size={13} strokeWidth={1.75} className="dim" />
        <span className="msg">Copied - grant Accessibility</span>
      </>
    ),
    error: (
      <>
        <AlertTriangle size={13} strokeWidth={1.75} className="danger" />
        <span className="msg">{shown.msg || "Dictation failed"}</span>
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
              api.toastAction(notice.id, a.id);
              setNotice(null);
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
    const [{ Menu, CheckMenuItem, MenuItem, PredefinedMenuItem }, { Window }, { emitTo }] = await Promise.all([
      import("@tauri-apps/api/menu"),
      import("@tauri-apps/api/window"),
      import("@tauri-apps/api/event"),
    ]);
    const mic = await api.getMicState();
    const auto = mic.selection === "auto";
    const items = [
      await CheckMenuItem.new({ text: "Auto", checked: auto, action: () => void api.setMicSelection("auto") }),
      await PredefinedMenuItem.new({ item: "Separator" }),
      ...(await Promise.all(
        mic.devices.map((d) =>
          CheckMenuItem.new({
            text: d.name,
            checked: !auto && mic.active === d.id,
            action: () => void api.setMicSelection({ device: { id: d.id } }),
          }),
        ),
      )),
      await PredefinedMenuItem.new({ item: "Separator" }),
      await MenuItem.new({
        text: "Open audio settings…",
        action: async () => {
          const main = await Window.getByLabel("main");
          await main?.show();
          await main?.setFocus();
          await emitTo("main", "navigate", { screen: "audio" });
        },
      }),
    ];
    (await Menu.new({ items })).popup();
  }

  const key = view === "notice" ? `n${notice?.id}` : `${view}${view === "error" ? shown.msg : ""}`;
  const shake = view === "idle" && shown.from === "error" && !reduced.matches;

  return (
    <div className="pill-root">
      <div
        className={`pill v-${view}${hover ? " hover" : ""}${hidden ? " hidden" : ""}${warning ? " warning" : ""}${shake ? " shake" : ""}`}
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
