// Base components (DESIGN 4). Plain elements + app.css; native <dialog> for sheets, <select> for menus of values.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { X } from "lucide-react";
import { api, on, type Toast } from "./lib/api";
import { hasDeadline } from "./lib/format";

/** Serif title; odd parts are italic: ["Your ", "vocabulary."] */
export function Serif({ parts, as: Tag = "span" }: { parts: string[]; as?: "h1" | "h2" | "span" | "p" }) {
  return <Tag>{parts.map((p, i) => (i % 2 ? <em key={i}>{p}</em> : p))}</Tag>;
}

export function Title({ parts, right }: { parts: string[]; right?: ReactNode }) {
  return (
    <header className="screen-head" data-tauri-drag-region>
      <div className="screen-title">
        <Serif as="h1" parts={parts} />
        {right && <div className="screen-right">{right}</div>}
      </div>
      <div className="ruler" aria-hidden />
    </header>
  );
}

export const Label = ({ children, right }: { children: ReactNode; right?: ReactNode }) => (
  <div className="label-row">
    <span className="label">{children}</span>
    {right}
  </div>
);

export function Toggle({ on: value, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button role="switch" aria-checked={value} aria-label={label} className="toggle" onClick={() => onChange(!value)}>
      <span />
    </button>
  );
}

/** Arrow keys in a radiogroup (DESIGN 9.4): focus and select the next radio. Pair with tabIndex = checked ? 0 : -1. */
export function radioKeys(e: React.KeyboardEvent<HTMLElement>) {
  const d = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[e.key];
  if (!d || (e.target as HTMLElement).getAttribute("role") !== "radio") return;
  const radios = [...e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')];
  const i = radios.findIndex((r) => r.getAttribute("aria-checked") === "true");
  const next = radios[(i + d + radios.length) % radios.length];
  e.preventDefault();
  next.focus();
  next.click();
}

export function Segmented<T extends string>(p: {
  value: T;
  options: { value: T; label: ReactNode }[];
  onChange: (v: T) => void;
  label: string;
}) {
  const i = Math.max(0, p.options.findIndex((o) => o.value === p.value));
  return (
    <div
      className="seg"
      role="radiogroup"
      aria-label={p.label}
      style={{ ["--n" as string]: p.options.length, ["--i" as string]: i }}
      onKeyDown={radioKeys}
    >
      <span className="seg-thumb" aria-hidden />
      {p.options.map((o, j) => (
        <button
          key={o.value}
          role="radio"
          aria-checked={j === i}
          tabIndex={j === i ? 0 : -1}
          onClick={() => p.onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export const Kbd = ({ keys }: { keys: string[] }) => (
  <span className="kbd-group">
    {keys.map((k, i) => (
      <kbd key={i}>{k}</kbd>
    ))}
  </span>
);

export const Spinner = () => <span className="spinner" aria-label="Working" />;

export const Dot = ({ tone }: { tone: "ok" | "idle" | "live" | "danger" | "warn" }) => <span className={`dot ${tone}`} aria-hidden />;

/** 12-bar static glyph: the only ornament on empty states (DESIGN 7). */
export function Empty({ line, help, action }: { line: string[]; help: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty">
      <span className="glyph" aria-hidden>
        {[3, 6, 10, 7, 12, 8, 5, 9, 11, 6, 4, 3].map((h, i) => (
          <i key={i} style={{ height: h * 1.6 }} />
        ))}
      </span>
      <Serif as="h2" parts={line} />
      <p>{help}</p>
      {action}
    </div>
  );
}

/** Small anchored menu. Closes on outside click and Esc. */
export function Popover(p: { trigger: (open: () => void, isOpen: boolean) => ReactNode; children: (close: () => void) => ReactNode; align?: "left" | "right"; up?: boolean }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const key = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("pointerdown", down);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("pointerdown", down);
      document.removeEventListener("keydown", key);
    };
  }, [open]);
  return (
    <div className="pop-anchor" ref={ref}>
      {p.trigger(() => setOpen((o) => !o), open)}
      {open && (
        <div className={`popover ${p.align ?? "right"}${p.up ? " up" : ""}`}>
          {p.children(() => setOpen(false))}
        </div>
      )}
    </div>
  );
}

/** Modal sheet on a native <dialog>: Esc, focus trap and backdrop come free. */
export function Sheet({ open, onClose, title, children }: { open: boolean; onClose: () => void; title: ReactNode; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current!;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog ref={ref} className="sheet" onClose={onClose} onClick={(e) => e.target === ref.current && onClose()}>
      {open && (
        <>
          <div className="sheet-head">
            <h2>{title}</h2>
            <button className="icon-btn" aria-label="Close" onClick={onClose}>
              <X size={16} strokeWidth={1.5} />
            </button>
          </div>
          {children}
        </>
      )}
    </dialog>
  );
}

// ---------------- level meter ----------------

/** dBFS -> smoothed display value with VU attack/release and a 1 s peak hold (DESIGN 6.6). */
export function useLevel(active: boolean) {
  const [s, set] = useState({ db: -90, peak: -90 });
  const target = useRef(-90);
  useEffect(() => {
    if (!active) {
      set({ db: -90, peak: -90 });
      return;
    }
    const off = on("audio_level", (l) => (target.current = l.db));
    let raf = 0;
    let disp = -90;
    let peak = -90;
    let peakAt = 0;
    const frame = (now: number) => {
      const t = target.current;
      disp += (t - disp) * (t > disp ? 0.55 : 0.18);
      if (disp >= peak) (peak = disp), (peakAt = now);
      else if (now - peakAt > 1000) peak = Math.max(disp, peak - 0.6);
      set({ db: disp, peak });
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      off.then((f) => f());
      target.current = -90;
    };
  }, [active]);
  return s;
}

/** Segmented VU meter, -60..0 dB. Colors switch at -20 and -6 dB. */
export function Meter({ db, peak, segments = 32, mini, label = "Input level" }: { db: number; peak: number; segments?: number; mini?: boolean; label?: string }) {
  const at = (d: number) => Math.round(((Math.min(0, Math.max(-60, d)) + 60) / 60) * segments);
  const lit = at(db);
  const pk = at(peak);
  const zone = (i: number) => {
    const d = -60 + ((i + 1) / segments) * 60;
    return d > -6 ? "hot" : d > -20 ? "mid" : "low";
  };
  // aria-valuenow at most 4x per second (DESIGN 9.5)
  const [aria, setAria] = useState(-60);
  const dbRef = useRef(db);
  dbRef.current = db;
  useEffect(() => {
    const iv = setInterval(() => setAria(Math.max(-60, Math.round(dbRef.current))), 250);
    return () => clearInterval(iv);
  }, []);
  return (
    <div className={`meter${mini ? " mini" : ""}`} role="meter" aria-label={label} aria-valuemin={-60} aria-valuemax={0} aria-valuenow={aria} aria-valuetext={`${aria} dB`}>
      {Array.from({ length: segments }, (_, i) => (
        <i key={i} className={i < lit ? zone(i) : i === pk - 1 && pk > 1 ? `${zone(i)} peak` : ""} />
      ))}
    </div>
  );
}

// ---------------- toasts ----------------

type LocalToast = Omit<Toast, "actions"> & { actions: { id: string; label: string; run?: () => void }[]; local?: boolean };
let pushToast: (t: LocalToast) => void = () => {};

/** Show a toast in the main window. Actions with `run` are local; others go to toast_action. */
export function toast(text: string, opts: { kind?: Toast["kind"]; actions?: { label: string; run: () => void }[]; ttl?: number } = {}) {
  pushToast({
    id: `l${Math.random()}`,
    kind: opts.kind ?? "info",
    text,
    ttl_ms: opts.ttl ?? 5000,
    actions: (opts.actions ?? []).map((a, i) => ({ id: String(i), ...a })),
    local: true,
  });
}

export function Toasts() {
  const [list, setList] = useState<(LocalToast & { leaving?: boolean })[]>([]);
  const dismiss = (id: string) => {
    setList((l) => l.map((t) => (t.id === id ? { ...t, leaving: true } : t)));
    setTimeout(() => setList((l) => l.filter((t) => t.id !== id)), 150);
  };
  useEffect(() => {
    pushToast = (t) => setList((l) => [...l.filter((x) => x.id !== t.id), t].slice(-3));
    const off = on("toast", (t) => pushToast(t));
    return () => void off.then((f) => f());
  }, []);
  return (
    <div className="toasts" aria-live="polite">
      {list.map((t) => (
        <ToastCard key={t.id} t={t} onDone={() => dismiss(t.id)} />
      ))}
    </div>
  );
}

function ToastCard({ t, onDone }: { t: LocalToast & { leaving?: boolean }; onDone: () => void }) {
  const [shown, setShown] = useState(false);
  const left = useRef(t.ttl_ms || 5000);
  const paused = useRef(false);
  useEffect(() => {
    requestAnimationFrame(() => setShown(true));
    // auto-dismiss; pauses on hover and while the window is hidden, except when Rust holds a deadline
    let last = performance.now();
    const iv = setInterval(() => {
      const now = performance.now();
      if (hasDeadline(t) || (!paused.current && !document.hidden)) left.current -= now - last;
      last = now;
      if (left.current <= 0) onDone();
    }, 100);
    return () => clearInterval(iv);
  }, []);
  return (
    <div
      className={`toast ${t.kind}${shown && !t.leaving ? " shown" : ""}`}
      role={t.kind === "error" ? "alert" : "status"}
      onMouseEnter={() => (paused.current = true)}
      onMouseLeave={() => (paused.current = false)}
    >
      <span className={`toast-tick ${t.kind}`} aria-hidden />
      <p>{t.text}</p>
      <div className="toast-actions">
        {t.actions.map((a, i) => (
          <button
            key={a.id}
            className={`btn ${i === t.actions.length - 1 ? "secondary" : "ghost"} sm`}
            onClick={() => {
              if (t.local) a.run?.();
              else api.toastAction(t.id, a.id);
              onDone();
            }}
          >
            {a.label}
          </button>
        ))}
        <button className="icon-btn sm" aria-label="Dismiss" onClick={onDone}>
          <X size={14} strokeWidth={1.5} />
        </button>
      </div>
    </div>
  );
}

/** Copy text; tells the user where it went. */
export async function copy(text: string) {
  await navigator.clipboard.writeText(text);
  toast("Copied to clipboard", { kind: "success", ttl: 2000 });
}
