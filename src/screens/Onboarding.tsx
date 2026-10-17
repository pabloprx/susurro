// DESIGN 6.9 / PLAN 8, Wispr order: welcome -> permissions -> mic test -> talk key -> practice. Resumable.
// Permissions already granted are never asked again: the step is skipped, and a card that is done shows a check.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Keyboard, Mic, MousePointerClick, RotateCw } from "lucide-react";
import { api, on, type HistoryItem, type ModelInfo, type ModelProgress, type Permissions, type Settings } from "../lib/api";
import { hotkeyStore, isModOnly, keyLabel, keysOf, micStore, modelName, permStore, saveSettings, secs, settingsStore } from "../lib/store";
import { Kbd, Meter, radioKeys, Serif, Toggle, useLevel } from "../ui";
import { LANGUAGES } from "./Providers";
import { useRecorder } from "./Shortcuts";
import type { Nav } from "../App";

export const STEPS = 5;
export const PERM_STEP = 1;
/** The step to reopen on: after relaunch_app, or after a quit in the middle of onboarding (App reads it on boot). */
export const RESUME_KEY = "susurro.obStep";
const recheck = () => permStore.refresh();

type Need = "microphone" | "accessibility" | "input_monitoring";
/** The grants this setup needs right now, and which of them are done. Input Monitoring only for a held modifier. */
export function needs(p: Permissions, s: Settings): [Need, boolean][] {
  const out: [Need, boolean][] = [
    ["microphone", p.microphone === "granted"],
    ["accessibility", p.accessibility],
  ];
  if (isModOnly(s.hotkeys.ptt)) out.push(["input_monitoring", p.input_monitoring && !p.needs_relaunch]);
  return out;
}
export const allGranted = (p: Permissions, s: Settings) => needs(p, s).every(([, ok]) => ok);

/** macOS has no grant callback: poll every 1 s while `active`, re-check when the window comes back (the user returns
 *  from System Settings), and take a pushed permissions_changed when Rust sends one. */
function usePermWatch(active: boolean) {
  useEffect(() => {
    if (!active) return;
    const t = setInterval(recheck, 1000);
    const back = () => document.visibilityState !== "hidden" && void recheck();
    addEventListener("focus", back);
    document.addEventListener("visibilitychange", back);
    const off = on("permissions_changed", permStore.set);
    return () => {
      clearInterval(t);
      removeEventListener("focus", back);
      document.removeEventListener("visibilitychange", back);
      void off.then((f) => f());
    };
  }, [active]);
}

export default function Onboarding({ start, onDone, nav }: { start: number; onDone: () => void; nav: Nav }) {
  const perms = permStore.use();
  const s = settingsStore.use();
  // already onboarded: only the permissions page, opened because a grant went missing (or from a Grant link)
  const [repair] = useState(() => settingsStore.get()?.onboarded === true);
  const [step, setStep] = useState(() => Math.max(0, Math.min(STEPS - 1, repair ? PERM_STEP : start)));
  const [dir, setDir] = useState<"fwd" | "back">("fwd");

  useEffect(() => {
    if (repair) localStorage.removeItem(RESUME_KEY);
    else localStorage.setItem(RESUME_KEY, String(step));
  }, [step]);

  const finish = async () => {
    localStorage.removeItem(RESUME_KEY);
    sessionStorage.setItem("susurro.skipOnboarding", "1");
    // Wispr turns on open at login at first launch; a dictation app that is not running does nothing
    if (!repair) await saveSettings((x) => void ((x.onboarded = true), (x.autostart = true)));
    onDone();
  };
  const go = async (n: number) => {
    if (repair) return void finish();
    // never stop on a permissions page that has nothing left to ask (fresh read: a grant can land any time)
    if (n === PERM_STEP) {
      const p = await api.permissionsStatus().catch(() => null);
      const cur = settingsStore.get();
      if (p) permStore.set(p);
      if (p && cur && allGranted(p, cur)) n += n < step ? -1 : 1;
    }
    setDir(n < step ? "back" : "fwd");
    setStep(Math.max(0, Math.min(STEPS - 1, n)));
  };
  const next = () => void go(step + 1);
  const restart = () => {
    localStorage.setItem(RESUME_KEY, String(step));
    void api.relaunchApp();
  };

  if (!s || !perms) return <div className="onboarding" />;

  const steps: ReactNode[] = [
    <Welcome key="w" ptt={s.hotkeys.ptt} onNext={next} />,
    <PermsStep key="p" onNext={next} onRestart={restart} repair={repair} onUseF1={() => saveSettings((x) => void (x.hotkeys.ptt = "F1"))} />,
    <MicStep key="m" onNext={next} />,
    <TalkKeyStep key="k" onNext={next} onRestart={restart} />,
    <TryStep key="t" onFinish={finish} onCloud={async () => (await finish(), nav.go("providers"))} />,
  ];

  return (
    <div className="onboarding">
      <div className="ob-top" data-tauri-drag-region>
        {repair ? (
          <span />
        ) : (
          <div className="ticks" aria-label={`Step ${step + 1} of ${STEPS}`}>
            {Array.from({ length: STEPS }, (_, i) => (
              <i key={i} className={i < step ? "done" : i === step ? "cur" : ""} />
            ))}
            <span className="count">
              {step + 1}/{STEPS}
            </span>
          </div>
        )}
        <span className="inline">
          {step > 0 && !repair && (
            <button className="btn ghost sm" onClick={() => void go(step - 1)}>
              Back
            </button>
          )}
          <button className="btn ghost sm" onClick={finish}>
            {repair ? "Not now" : "Skip setup"}
          </button>
        </span>
      </div>
      <div className="ob-stage">
        <div className={`ob-step${dir === "back" ? " back" : ""}`} key={step}>
          {steps[step]}
        </div>
      </div>
    </div>
  );
}

const Head = ({ eyebrow, title, sub }: { eyebrow: string; title: string[]; sub?: ReactNode }) => (
  <>
    <div className="eyebrow">{eyebrow}</div>
    <Serif as="h1" parts={title} />
    {sub && <p className="why">{sub}</p>}
  </>
);

// ---------------- 1. welcome ----------------

function Welcome({ ptt, onNext }: { ptt: string; onNext: () => void }) {
  // the messy line: fillers strike and float FILLER, "friday no actually" collapses as BACKTRACK
  const W: [string, number?, string?][] = [["um", 200, "FILLER"], ["so", 450, "FILLER"], ["I"], ["think"], ["we"], ["should,"], ["uh,", 700, "FILLER"], ["ship"], ["it"], ["friday", 1000, "BACKTRACK"], ["no", 1000], ["actually", 1000], ["thursday"]];
  return (
    <>
      <div className="eyebrow">susurro</div>
      <h1>
        Talk. It <em>types.</em>
      </h1>
      <div className="demo" aria-label="um so I think we should, uh, ship it friday no actually thursday, becomes: I think we should ship it Thursday.">
        <div className="messy" aria-hidden>
          {W.map(([w, at, tag], i) => (
            <span key={i}>
              <span className={`w${at !== undefined ? " cut" : ""}`} style={{ ["--at" as string]: `${at ?? 0}ms` }}>
                {w}
                {tag && <span className="ftag">{tag}</span>}
              </span>{" "}
            </span>
          ))}
        </div>
        <div className="arrow" aria-hidden>
          ↓
        </div>
        <div className="clean" aria-hidden>
          I think we should ship it Thursday.
        </div>
      </div>
      <p className="why">
        Hold <Kbd keys={keysOf(ptt)} /> in any app, speak, let go. Clean text lands where you type.
      </p>
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onNext} autoFocus>
          Get started
        </button>
      </div>
      <p className="not">About a minute. Audio stays on this Mac with Local Whisper.</p>
    </>
  );
}

// ---------------- 2. permissions ----------------

const PANE: Record<Need, string> = { microphone: "Microphone", accessibility: "Accessibility", input_monitoring: "Input Monitoring" };

/** A picture of the exact row to turn on in System Settings. */
function Diagram({ pane }: { pane: string }) {
  return (
    <div className="diagram" aria-label={`In System Settings, Privacy and Security, ${pane}: turn on susurro`}>
      <div className="bar" aria-hidden>
        <i />
        <i />
        <i />
        <span>Privacy &amp; Security › {pane}</span>
      </div>
      <div className="item" aria-hidden>
        <span className="app-ico">T</span> Terminal
        <span className="toggle" aria-hidden>
          <span />
        </span>
      </div>
      <div className="item me" aria-hidden>
        <span className="app-ico">s</span> susurro
        <span className="hand" aria-hidden>
          <MousePointerClick size={14} strokeWidth={1.5} />
        </span>
        <span className="toggle on" aria-hidden>
          <span />
        </span>
      </div>
    </div>
  );
}

function PermsStep({ onNext, onRestart, repair, onUseF1 }: { onNext: () => void; onRestart: () => void; repair: boolean; onUseF1: () => void }) {
  const p = permStore.use()!;
  const s = settingsStore.use()!;
  const list = needs(p, s);
  const done = list.every(([, ok]) => ok);
  const active = list.find(([, ok]) => !ok)?.[0] ?? null;
  usePermWatch(!done);
  // the grant shows up: a short beat on the check marks, then move on by itself
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(onNext, 700);
    return () => clearTimeout(t);
  }, [done]);

  const label = keyLabel(s.hotkeys.ptt);
  const n = list.length === 2 ? "Two" : "Three";
  return (
    <>
      <Head
        eyebrow={repair ? "Permissions" : "Setup"}
        title={repair ? ["Turn it back ", "on."] : [`${n} `, "quick", " switches."]}
        sub={repair ? "macOS turned a permission off. Switch it back on and susurro sees it right away." : "macOS asks once. Anything you already allowed shows a check."}
      />
      <div className="perm-cards" role="list">
        {list.map(([k, ok]) => (
          <PermCard key={k} kind={k} ok={ok} active={k === active} p={p} label={label} onRestart={onRestart} />
        ))}
      </div>
      <div className="ob-links">
        {active === "input_monitoring" && (
          <button className="link small" onClick={onUseF1}>
            Use F1 instead (no permission)
          </button>
        )}
        {done && (
          <p className="ob-granted" role="status">
            <Check size={16} strokeWidth={2} /> All set
          </p>
        )}
      </div>
    </>
  );
}

const CARD: Record<Need, { icon: ReactNode; title: string; sub: (key: string) => string }> = {
  microphone: { icon: <Mic size={16} strokeWidth={1.5} />, title: "Use your microphone", sub: (k) => `Only while you hold ${k}. The menu bar dot shows it.` },
  accessibility: { icon: <MousePointerClick size={16} strokeWidth={1.5} />, title: "Type for you", sub: () => "Accessibility lets susurro paste into the app you are using. It does not read your screen." },
  input_monitoring: { icon: <Keyboard size={16} strokeWidth={1.5} />, title: "Hear your talk key", sub: (k) => `Input Monitoring lets susurro notice ${k} in any app. It does not record what you type.` },
};

function PermCard({ kind, ok, active, p, label, onRestart }: { kind: Need; ok: boolean; active: boolean; p: Permissions; label: string; onRestart: () => void }) {
  const [asked, setAsked] = useState(false);
  const [slow, setSlow] = useState(false);
  // asked and still nothing after 10 s: the old switch is probably stale (an older build). Say how to fix it.
  useEffect(() => {
    if (!asked || ok) return;
    const t = setTimeout(() => setSlow(true), 10_000);
    return () => clearTimeout(t);
  }, [asked, ok]);
  const ask = () => {
    setAsked(true);
    void api.requestPermission(kind);
  };
  // Input Monitoring is the one grant macOS applies only to a new process
  const relaunch = kind === "input_monitoring" && p.input_monitoring && p.needs_relaunch;
  const micPrompt = kind === "microphone" && p.microphone === "undetermined";
  const c = CARD[kind];
  return (
    <div className={`perm-card${ok ? " ok" : active ? " active" : ""}`} role="listitem" aria-label={`${c.title}: ${ok ? "allowed" : "not allowed yet"}`}>
      <div className="pc-head">
        <span className="pc-ico" aria-hidden>
          {ok ? <Check size={16} strokeWidth={2} /> : c.icon}
        </span>
        <span className="pc-text">
          <span className="t">{c.title}</span>
          <span className="d">{c.sub(label)}</span>
        </span>
        {ok ? (
          <span className="pc-state ok-text">Allowed</span>
        ) : active && !relaunch && micPrompt ? (
          <button className="btn primary sm" onClick={ask} autoFocus>
            Allow
          </button>
        ) : active && !relaunch ? (
          <button className="btn primary sm" onClick={ask} autoFocus>
            Open System Settings
          </button>
        ) : relaunch ? (
          <button className="btn primary sm" onClick={onRestart} autoFocus>
            Restart susurro
          </button>
        ) : null}
      </div>
      {active && !micPrompt && (
        <div className="pc-body">
          {relaunch ? (
            <p className="ob-restart" role="status">
              <RotateCw size={14} strokeWidth={1.5} /> Allowed. macOS applies it after a restart, and susurro reopens right here.
            </p>
          ) : (
            <>
              <Diagram pane={PANE[kind]} />
              <p className="small">{asked ? "Turn on susurro in the list. This page sees it by itself." : `Click the button, then turn on susurro in ${PANE[kind]}.`}</p>
              {slow && (
                <p className="note warn-text pc-slow" role="status">
                  <AlertTriangle size={14} strokeWidth={1.5} /> Already on? Turn susurro off and on again. macOS keeps an old switch from an earlier build.
                </p>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------- 3. microphone test ----------------

function MicStep({ onNext }: { onNext: () => void }) {
  const p = permStore.use()!;
  const mic = micStore.use();
  const granted = p.microphone === "granted";
  const [heard, setHeard] = useState(false);
  const lvl = useLevel(granted);
  usePermWatch(!granted);
  const sel = mic?.selection === "auto" ? "auto" : (mic?.selection.device.id ?? "auto");
  useEffect(() => {
    if (!granted) return;
    setHeard(false);
    api.micTestStart(null).catch(() => {});
    return () => void api.micTestStop();
  }, [granted, sel]);
  useEffect(() => {
    if (lvl.db > -40) setHeard(true);
  }, [lvl.db]);
  const active = mic?.devices.find((d) => d.id === mic.active);
  const pickMic = (v: string) => api.setMicSelection(v === "auto" ? "auto" : { device: { id: v } }).then(micStore.set);
  return (
    <>
      <Head eyebrow="Microphone" title={["Say ", "something."]} sub="Check that the meter moves when you talk." />
      {granted ? (
        <div className="ob-panel">
          <Meter db={lvl.db} peak={lvl.peak} segments={32} />
          <p className={`note ${heard ? "ok-text" : ""}`} style={{ justifyContent: "center" }} aria-live="polite">
            {heard ? (
              <>
                <Check size={14} strokeWidth={2} /> Sounds good.
              </>
            ) : (
              "Listening…"
            )}
          </p>
          <label className="mic-pick">
            <span className="label">Microphone</span>
            <select className="input" value={sel} onChange={(e) => void pickMic(e.target.value)} aria-label="Microphone">
              <option value="auto">Auto{active ? ` (${active.name})` : ""}</option>
              {mic?.devices.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name}
                </option>
              ))}
            </select>
          </label>
        </div>
      ) : (
        <div className="ob-panel">
          <p className="note warn-text" style={{ justifyContent: "center" }}>
            <AlertTriangle size={14} strokeWidth={1.5} /> susurro cannot use the microphone yet.
          </p>
        </div>
      )}
      <div className="ob-actions">
        {!granted && (
          <button className="btn secondary" onClick={() => api.requestPermission("microphone")}>
            {p.microphone === "denied" ? "Open System Settings" : "Allow microphone"}
          </button>
        )}
        <button className="btn primary ob-next" onClick={onNext} autoFocus={granted}>
          Continue
        </button>
      </div>
    </>
  );
}

// ---------------- 4. talk key ----------------

const KEY_PICKS: [string, string][] = [
  ["F1", "Recommended"],
  ["RightOption", "+1 permission"],
  ["RightCommand", "+1 permission"],
];

function TalkKeyStep({ onNext, onRestart }: { onNext: () => void; onRestart: () => void }) {
  const s = settingsStore.use()!;
  const perms = permStore.use()!;
  const st = hotkeyStore.use();
  const [rec, setRec] = useState(false);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const [heard, setHeard] = useState(false);
  const b = s.hotkeys.ptt;
  const label = keyLabel(b);
  const pick = (v: string) => {
    setRec(false);
    setHeard(false);
    saveSettings((x) => void (x.hotkeys.ptt = v))
      .then(() => setSaveErr(null))
      .catch((e) => setSaveErr(String(e)));
  };
  const r = useRecorder(rec ? "ptt" : null, s.hotkeys, pick, () => setRec(false));
  // the live check: the key works once a dictation starts from it
  useEffect(() => {
    const off = on("session_state", (x) => (x.phase === "recording" || x.phase === "locked") && setHeard(true));
    return () => void off.then((f) => f());
  }, []);

  const custom = !KEY_PICKS.some(([k]) => k === b);
  const status = st?.binding === b ? st : null;
  const needsIm = isModOnly(b) && (!perms.input_monitoring || perms.needs_relaunch);
  usePermWatch(needsIm);
  return (
    <>
      <Head eyebrow="Talk key" title={["Pick your ", "talk", " key."]} sub="Hold it to talk. Let go to paste." />
      <button className={`keycap${rec ? " recording" : ""}`} onClick={() => setRec(!rec)} aria-label={rec ? "Recording a talk key. Press Escape to cancel." : `Talk key: ${label}. Press to record your own.`}>
        {rec ? r.held.length ? r.held.map((k, i) => <kbd key={i}>{k}</kbd>) : <span className="rec-hint">Press any key or combo</span> : keysOf(b).map((k, i) => <kbd key={i}>{k}</kbd>)}
      </button>
      {rec && !r.err && <p className="small">Esc to cancel</p>}
      {rec && r.err && (
        <p className="note warn-text" role="alert" style={{ justifyContent: "center" }}>
          <AlertTriangle size={14} strokeWidth={1.5} /> {r.err}
        </p>
      )}
      <div className="keypicks">
        <div role="radiogroup" aria-label="Talk key" onKeyDown={radioKeys}>
          {KEY_PICKS.map(([k, tag]) => (
            <button key={k} role="radio" aria-checked={b === k} tabIndex={b === k || (custom && k === "F1") ? 0 : -1} onClick={() => pick(k)}>
              <span className="cap">{keysOf(k)[0]}</span>
              <span className="d">{tag}</span>
            </button>
          ))}
        </div>
        <button className={`own${custom && !rec ? " on" : ""}`} onClick={() => setRec(true)}>
          <span className="cap">{custom ? keysOf(b).join(" ") : "···"}</span>
          <span className="d">Record your own</span>
        </button>
      </div>
      <div className="ob-panel live-check" aria-live="polite">
        {saveErr ? (
          <p className="note danger-text">
            <AlertTriangle size={14} strokeWidth={1.5} /> {saveErr}
          </p>
        ) : needsIm ? (
          <p className="note warn-text">
            <AlertTriangle size={14} strokeWidth={1.5} /> {label} needs Input Monitoring.{" "}
            {perms.input_monitoring ? (
              <button className="link" onClick={onRestart}>
                Restart susurro
              </button>
            ) : (
              <button className="link" onClick={() => api.requestPermission("input_monitoring")}>
                Open System Settings
              </button>
            )}
          </p>
        ) : status && !status.active ? (
          <p className="note warn-text">
            <AlertTriangle size={14} strokeWidth={1.5} /> {label} is not working: {status.error ?? "unknown reason"}.
          </p>
        ) : heard ? (
          <p className="note ok-text">
            <Check size={14} strokeWidth={2} /> {label} works.
          </p>
        ) : (
          <p className="note">
            <span className="dot idle" aria-hidden /> Hold {label} now to check it.
          </p>
        )}
        {b === "Fn" && perms.fn_key_free === false && (
          <p className="note warn-text">
            <AlertTriangle size={14} strokeWidth={1.5} /> Set &lsquo;Press fn key to&rsquo; = Do Nothing.{" "}
            <button className="link" onClick={() => api.requestPermission("fn_key")}>
              Open Keyboard settings
            </button>
          </p>
        )}
      </div>
      <div className="ob-prefs">
        <div className="pref">
          <span>
            <span className="t">Hands-free</span>
            <span className="d">Double-tap {label} to talk without holding. Tap it again to stop.</span>
          </span>
          <Toggle label="Double-tap for hands-free" on={s.hotkeys.double_tap} onChange={(v) => saveSettings((x) => void (x.hotkeys.double_tap = v))} />
        </div>
        <div className="pref">
          <span>
            <span className="t">Language</span>
            <span className="d">One language is more accurate than auto.</span>
          </span>
          <select className="input" value={s.language} onChange={(e) => saveSettings((x) => void (x.language = e.target.value))} aria-label="Language">
            {LANGUAGES.slice(0, 3).map(([v, n]) => (
              <option key={v} value={v}>
                {n}
              </option>
            ))}
            <option disabled>──</option>
            {LANGUAGES.slice(3).map(([v, n]) => (
              <option key={v} value={v}>
                {n}
              </option>
            ))}
          </select>
        </div>
      </div>
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onNext}>
          Continue
        </button>
      </div>
    </>
  );
}

// ---------------- 5. practice ----------------

type Drill = { id: "hold" | "free"; title: (k: string[]) => ReactNode; say: string };
const DRILLS: Drill[] = [
  {
    id: "hold",
    title: (k) => (
      <>
        Hold <Kbd keys={k} />, say the line, let go.
      </>
    ),
    say: "This is my first dictation with susurro.",
  },
  {
    id: "free",
    title: (k) => (
      <>
        Double-tap <Kbd keys={k} />, talk hands-free, tap it once to stop.
      </>
    ),
    say: "Hands-free is great for longer messages.",
  },
];

function TryStep({ onFinish, onCloud }: { onFinish: () => void; onCloud: () => void }) {
  const s = settingsStore.use()!;
  const keys = keysOf(s.hotkeys.ptt);
  const drills = s.hotkeys.double_tap ? DRILLS : DRILLS.slice(0, 1);
  const [at, setAt] = useState(0);
  const [passed, setPassed] = useState<boolean[]>([]);
  const [result, setResult] = useState<HistoryItem | null>(null);
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [prog, setProg] = useState<ModelProgress | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tryErr, setTryErr] = useState<string | null>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const locked = useRef(false);
  const mine = useRef(false); // a dictation started on this step (one from the step before can land late)
  const atRef = useRef(at);
  atRef.current = at;
  const needModel = s.stt.provider === "whisper_local" && models !== null && !models.some((m) => m.downloaded);
  const allDone = at >= drills.length;

  const pass = () => {
    if (!mine.current) return;
    mine.current = false;
    const d = drills[atRef.current];
    // the hands-free drill passes only on a hands-free dictation
    if (d && (d.id === "hold" || locked.current)) {
      setPassed((p) => Object.assign([...p], { [atRef.current]: true }));
      setTimeout(() => setAt((a) => Math.max(a, atRef.current + 1)), 900);
    }
    locked.current = false;
  };

  useEffect(() => {
    const offs = [
      on("session_state", (x) => {
        if (x.phase === "recording" || x.phase === "locked") {
          mine.current = true;
          setTryErr(null);
        }
        if (x.phase === "locked") locked.current = true;
        // Done is the pass signal: history_added never comes with "store history" off.
        if (x.phase === "done" && (x.message === "pasted" || x.message === "copied")) pass();
        if (x.phase === "error" && mine.current) {
          mine.current = locked.current = false;
          setTryErr(x.message ?? "That did not work. Try again.");
        }
      }),
      on("history_added", (h) => {
        // A failed row (no model, bad cloud key) has empty text: it never passes a drill.
        if ((h.status !== "pasted" && h.status !== "copied") || !h.final_text.trim()) {
          if (mine.current) setTryErr(h.error ?? "Nothing was transcribed. Try again.");
          return;
        }
        setResult(h);
        pass();
      }),
      on("model_progress", setProg),
    ];
    api.whisperModels().then(setModels);
    return () => offs.forEach((p) => p.then((f) => f()));
  }, [drills.length]);

  // focus the field, so a real paste lands in it like it would in any app
  useEffect(() => void (!allDone && field.current?.focus()), [at, needModel]);

  // no model yet: base q5_1 first so the first try is fast, then turbo q5_0 in the background (DESIGN 6.9)
  useEffect(() => {
    if (!needModel) return;
    const base = s.language === "en" ? "ggml-base.en-q5_1.bin" : "ggml-base-q5_1.bin";
    api
      .whisperDownload(base)
      .then(async () => {
        await saveSettings((x) => void (x.stt.whisper_model = base));
        setModels(await api.whisperModels());
        setProg(null);
        void api.whisperDownload("ggml-large-v3-turbo-q5_0.bin").catch(() => {});
      })
      .catch((e) => setErr(String(e)));
  }, [needModel]);

  return (
    <>
      <Head
        eyebrow="Practice"
        title={allDone ? ["You are ", "set."] : ["Your first ", "dictation."]}
        sub={allDone ? (
          <>
            Now try it anywhere: Slack, Mail, your editor. Hold <Kbd keys={keys} /> and talk.
          </>
        ) : (
          "The text lands in the box below, like it will in any app."
        )}
      />
      {needModel && !err && (
        <div className="ob-panel">
          <div className="progress">
            <div className="track">
              <div className="fill" style={{ width: `${prog ? (prog.done / prog.total) * 100 : 0}%` }} />
            </div>
            <span className="mono">{prog ? `${Math.floor((prog.done / prog.total) * 100)}%` : "starting"}</span>
          </div>
          <p className="small" style={{ marginTop: 8 }}>
            Getting {modelName(s.language === "en" ? "ggml-base.en-q5_1.bin" : "ggml-base-q5_1.bin")} ready, the fast model.{" "}
            <button className="link" onClick={onCloud}>
              Use a cloud provider instead
            </button>
          </p>
        </div>
      )}
      {err && (
        <p className="field-error" role="alert">
          <AlertTriangle size={13} strokeWidth={1.5} /> {err}
        </p>
      )}
      <ol className="drills">
        {drills.map((d, i) => (
          <li key={d.id} className={passed[i] ? "ok" : i === at ? "active" : i < at ? "skipped" : ""} aria-current={i === at ? "step" : undefined}>
            <span className="n" aria-hidden>
              {passed[i] ? <Check size={13} strokeWidth={2.25} /> : i + 1}
            </span>
            <span className="body">
              <span className="t">{d.title(keys)}</span>
              {i === at && (
                <span className="say">
                  Say: <em>&ldquo;{d.say}&rdquo;</em>
                </span>
              )}
            </span>
            {i === at && !passed[i] && (
              <button className="link small" onClick={() => setAt(i + 1)}>
                Skip
              </button>
            )}
          </li>
        ))}
      </ol>
      <textarea ref={field} className="input practice" placeholder="Your words land here." defaultValue={result?.final_text} key={result?.id} aria-label="Practice field" />
      <p className="result-line" aria-live="polite">
        {tryErr ? (
          <span className="field-error" role="alert">
            <AlertTriangle size={13} strokeWidth={1.5} /> {tryErr}
          </span>
        ) : result ? (
          <>
            <span className="ok-text">✓ {result.words} words</span>
            <span>{secs(result.duration_ms)} spoken</span>
            <span>{result.latency_ms} ms to text</span>
          </>
        ) : (
          <span>&nbsp;</span>
        )}
      </p>
      <div className="ob-actions">
        <button className={`btn ${allDone ? "primary" : "ghost"} ob-next`} onClick={onFinish} key={String(allDone)} autoFocus={allDone}>
          {allDone ? "Start using susurro" : "Skip practice"}
        </button>
      </div>
    </>
  );
}
