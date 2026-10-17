// DESIGN 6.9 / PLAN 8: seven resumable steps, full window, no sidebar.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Globe, Hand, Repeat, RotateCw } from "lucide-react";
import { api, on, type HistoryItem, type ModelInfo, type ModelProgress } from "../lib/api";
import { hotkeyStore, isModOnly, keyLabel, keysOf, modelName, permStore, saveSettings, secs, settingsStore } from "../lib/store";
import { Kbd, Meter, radioKeys, Serif, useLevel } from "../ui";
import { LANGUAGES } from "./Providers";
import { useRecorder } from "./Shortcuts";
import type { Nav } from "../App";

const STEPS = 7;
/** The step to reopen on after relaunch_app (App reads it on boot). */
export const RESUME_KEY = "susurro.obStep";
const recheck = () => api.permissionsStatus().then(permStore.set);

export default function Onboarding({ start, onDone, nav }: { start: number; onDone: () => void; nav: Nav }) {
  const [step, setStep] = useState(start);
  const [dir, setDir] = useState<"fwd" | "back">("fwd");
  // reopened by a restart: a grant that shows up now still gets its check mark and advances by itself
  const [resumed, setResumed] = useState(() => localStorage.getItem(RESUME_KEY) !== null);
  useEffect(() => localStorage.removeItem(RESUME_KEY), []);
  const perms = permStore.use();
  const s = settingsStore.use();

  // macOS has no grant callback: poll every 1 s while a permission step is open (PLAN 8), and re-check the moment
  // the window comes back (the user returns from System Settings)
  useEffect(() => {
    if (step < 1 || step > 4) return;
    const t = setInterval(recheck, 1000);
    const back = () => document.visibilityState !== "hidden" && void recheck();
    addEventListener("focus", back);
    document.addEventListener("visibilitychange", back);
    return () => {
      clearInterval(t);
      removeEventListener("focus", back);
      document.removeEventListener("visibilitychange", back);
    };
  }, [step]);

  const go = (n: number) => {
    setDir(n < step ? "back" : "fwd");
    setResumed(false);
    setStep(Math.max(0, Math.min(STEPS - 1, n)));
  };
  const restart = () => {
    localStorage.setItem(RESUME_KEY, String(step));
    void api.relaunchApp();
  };
  const next = () => go(step + 1);
  const finish = async () => {
    sessionStorage.setItem("susurro.skipOnboarding", "1");
    await saveSettings((x) => void (x.onboarded = true));
    onDone();
  };

  if (!s || !perms) return <div className="onboarding" />;
  const ptt = s.hotkeys.ptt;
  const label = keyLabel(ptt);

  const steps: ReactNode[] = [
    <Welcome key="w" onNext={next} />,
    <MicStep key="m" granted={perms.microphone === "granted"} denied={perms.microphone === "denied"} onNext={next} />,
    <PermStep
      key="a"
      eyebrow="Accessibility"
      title={["Paste where you ", "type."]}
      why="Accessibility lets susurro press ⌘V in the app you are using."
      not="susurro does not read your screen."
      granted={perms.accessibility}
      onGrant={() => api.requestPermission("accessibility")}
      onNext={next}
      onRestart={restart}
      resumed={resumed}
      pane="Accessibility"
    />,
    <PermStep
      key="i"
      eyebrow="Input Monitoring"
      title={["Hold ", label, " to talk."]}
      why={`Input Monitoring lets susurro see ${label} from any app.`}
      not="It does not record what you type."
      granted={perms.input_monitoring && !perms.needs_relaunch && (ptt !== "Fn" || perms.fn_key_free !== false)}
      needsRelaunch={perms.needs_relaunch}
      onGrant={perms.input_monitoring ? null : () => api.requestPermission("input_monitoring")}
      onNext={next}
      onRestart={restart}
      resumed={resumed}
      pane="Input Monitoring"
      blocked={
        perms.input_monitoring &&
        !perms.needs_relaunch &&
        ptt === "Fn" &&
        perms.fn_key_free === false && (
          <div className="ob-panel">
            <p className="note warn-text" style={{ justifyContent: "center" }}>
              <AlertTriangle size={14} strokeWidth={1.5} /> Set &lsquo;Press fn key to&rsquo; = Do Nothing, or fn also opens the emoji picker.
            </p>
            <button className="btn primary" style={{ marginTop: 12 }} onClick={() => api.requestPermission("fn_key")}>
              Open Keyboard settings
            </button>
          </div>
        )
      }
      extra={
        <button
          className="link small"
          onClick={async () => {
            await saveSettings((x) => void (x.hotkeys.ptt = "Alt+Space"));
            next();
          }}
        >
          Use ⌥ Space instead (no permission)
        </button>
      }
    />,
    <TalkKeyStep key="k" onNext={next} onRestart={restart} onGrant={() => go(3)} />,
    <ShortcutStep key="s" onNext={next} />,
    <TryStep key="t" ptt={ptt} onFinish={finish} onCloud={async () => (await finish(), nav.go("providers"))} />,
  ];

  return (
    <div className="onboarding">
      <div className="ob-top" data-tauri-drag-region>
        <div className="ticks" aria-label={`Step ${step + 1} of ${STEPS}`}>
          {Array.from({ length: STEPS }, (_, i) => (
            <i key={i} className={i < step ? "done" : i === step ? "cur" : ""} />
          ))}
          <span className="count">
            {step + 1}/{STEPS}
          </span>
        </div>
        <span className="inline">
          {step > 0 && (
            <button className="btn ghost sm" onClick={() => go(step - 1)}>
              Back
            </button>
          )}
          <button className="btn ghost sm" onClick={finish}>
            Skip for now
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

const Head = ({ eyebrow, title }: { eyebrow: string; title: string[] }) => (
  <>
    <div className="eyebrow">{eyebrow}</div>
    <Serif as="h1" parts={title} />
  </>
);

function Welcome({ onNext }: { onNext: () => void }) {
  const [run, setRun] = useState(0);
  // the messy line: fillers strike and float FILLER, "friday no actually" collapses as BACKTRACK
  const W: [string, number?, string?][] = [["um", 200, "FILLER"], ["so", 450, "FILLER"], ["I"], ["think"], ["we"], ["should,"], ["uh,", 700, "FILLER"], ["ship"], ["it"], ["friday", 1000, "BACKTRACK"], ["no", 1000], ["actually", 1000], ["thursday"]];
  return (
    <>
      <div className="eyebrow">susurro</div>
      <h1>
        Talk. It <em>types.</em>
      </h1>
      <div className="demo" key={run} aria-label="um so I think we should, uh, ship it friday no actually thursday, becomes: I think we should ship it Thursday.">
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
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onNext} autoFocus>
          Get started
        </button>
        <button className="link small" onClick={() => setRun(run + 1)}>
          Replay
        </button>
      </div>
    </>
  );
}

function MicStep({ granted, denied, onNext }: { granted: boolean; denied: boolean; onNext: () => void }) {
  const [heard, setHeard] = useState(false);
  const lvl = useLevel(granted);
  useEffect(() => {
    if (!granted) return;
    api.micTestStart(null).catch(() => {});
    return () => void api.micTestStop();
  }, [granted]);
  useEffect(() => {
    if (lvl.db > -40) setHeard(true);
  }, [lvl.db]);
  return (
    <>
      <Head eyebrow="Microphone" title={["Let it ", "listen."]} />
      <p className="why">susurro opens the mic only while you hold the key. The orange dot in the menu bar tells you.</p>
      <p className="not">Audio stays on this Mac with Local Whisper.</p>
      {granted ? (
        <div className="ob-panel">
          <Meter db={lvl.db} peak={lvl.peak} segments={32} />
          <p className={`note ${heard ? "ok-text" : ""}`} style={{ justifyContent: "center" }}>
            {heard ? (
              <>
                <Check size={14} strokeWidth={2} /> Heard you.
              </>
            ) : (
              "Say something."
            )}
          </p>
        </div>
      ) : null}
      <div className="ob-actions">
        {!granted && (
          <button className="btn primary ob-primary" onClick={() => api.requestPermission("microphone")}>
            {denied ? "Open System Settings" : "Allow microphone"}
          </button>
        )}
        <button className={`btn ${granted ? "primary" : "ghost"} ob-next`} onClick={onNext}>
          {granted ? "Continue" : "Not now"}
        </button>
      </div>
    </>
  );
}

type PermProps = {
  eyebrow: string;
  title: string[];
  why: string;
  not: string;
  granted: boolean;
  /** Rust sees the grant but this process cannot use it until it restarts. */
  needsRelaunch?: boolean;
  onGrant: (() => void) | null;
  onNext: () => void;
  onRestart: () => void;
  resumed: boolean;
  pane: string;
  extra?: ReactNode;
  blocked?: ReactNode;
};
function PermStep(p: PermProps) {
  const was = useRef(p.granted && !p.resumed);
  const [opened, setOpened] = useState(false);
  const [stuck, setStuck] = useState(false);
  // back from System Settings and the grant still does not show after 3 s: macOS wants a new process
  useEffect(() => {
    if (!opened || p.granted) return;
    let t = 0;
    const back = () => {
      if (document.visibilityState === "hidden") return;
      clearTimeout(t);
      t = window.setTimeout(() => setStuck(true), 3000);
    };
    addEventListener("focus", back);
    document.addEventListener("visibilitychange", back);
    return () => {
      clearTimeout(t);
      removeEventListener("focus", back);
      document.removeEventListener("visibilitychange", back);
    };
  }, [opened, p.granted]);
  const restart = !p.granted && (p.needsRelaunch || stuck);
  // advance by itself once the grant shows up
  useEffect(() => {
    if (p.granted && !was.current) {
      const t = setTimeout(p.onNext, 900);
      return () => clearTimeout(t);
    }
    was.current = p.granted;
  }, [p.granted]);
  return (
    <>
      <Head eyebrow={p.eyebrow} title={p.title} />
      <p className="why">{p.why}</p>
      <p className="not">{p.not}</p>
      {/* blocked: the grant is done but a follow-up check fails; only its fix shows, not the grant block */}
      {p.blocked || (
        <>
          <div className="diagram" aria-label={`System Settings, Privacy and Security, ${p.pane}: turn susurro on`}>
            <div className="bar">
              <i />
              <i />
              <i />
              <span>Privacy &amp; Security › {p.pane}</span>
            </div>
            <div className="item">
              <span className="app-ico">T</span> Terminal
              <span className="toggle" aria-hidden>
                <span />
              </span>
            </div>
            <div className="item me">
              <span className="app-ico">s</span> susurro
              <span className="toggle on" aria-hidden>
                <span />
              </span>
            </div>
          </div>
          {p.granted ? (
            <p className="ob-granted">
              <Check size={16} strokeWidth={2} /> Granted
            </p>
          ) : restart ? (
            <p className="ob-restart" role="status">
              <RotateCw size={14} strokeWidth={1.5} /> macOS needs a restart to apply {p.pane}. susurro reopens right here.
            </p>
          ) : null}
          <div className="ob-actions">
            {restart ? (
              <button className="btn primary ob-primary" onClick={p.onRestart}>
                Restart susurro
              </button>
            ) : (
              !p.granted &&
              p.onGrant && (
                <button className="btn primary ob-primary" onClick={() => (setOpened(true), p.onGrant!())}>
                  Open System Settings
                </button>
              )
            )}
            <button className={`btn ${p.granted ? "primary" : "ghost"} ob-next`} onClick={p.onNext}>
              {p.granted ? "Continue" : "Not now"}
            </button>
          </div>
        </>
      )}
      <div className="ob-links">
        {!p.granted && !restart && !p.blocked && (
          <button className="link small" onClick={p.onRestart}>
            I already enabled it
          </button>
        )}
        {restart && p.onGrant && (
          <button className="link small" onClick={p.onGrant}>
            Open System Settings again
          </button>
        )}
        {p.extra}
      </div>
    </>
  );
}

const KEY_PICKS: [string, string][] = [
  ["RightOption", "Recommended"],
  ["RightCommand", "Beside space"],
  ["Fn", "Apple keyboards"],
];

function TalkKeyStep({ onNext, onRestart, onGrant }: { onNext: () => void; onRestart: () => void; onGrant: () => void }) {
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
  const needsIm = isModOnly(b) && !perms.input_monitoring;
  return (
    <>
      <Head eyebrow="Your talk key" title={["Pick your ", "talk", " key."]} />
      <p className="why">Hold it to talk, double-tap it for hands-free.</p>
      <p className="not">Shortcuts with it still type, like ⌥ 2 for @.</p>
      <button className={`keycap${rec ? " recording" : ""}`} onClick={() => setRec(!rec)} aria-label={rec ? "Recording a talk key. Press Escape to cancel." : `Talk key: ${label}. Press to record your own.`}>
        {rec ? (
          r.held.length ? (
            r.held.map((k, i) => <kbd key={i}>{k}</kbd>)
          ) : (
            <span className="rec-hint">Press any key or combo</span>
          )
        ) : (
          keysOf(b).map((k, i) => <kbd key={i}>{k}</kbd>)
        )}
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
            <button key={k} role="radio" aria-checked={b === k} tabIndex={b === k || (custom && k === "RightOption") ? 0 : -1} onClick={() => pick(k)}>
              <span className="cap">
                {k === "Fn" ? (
                  <>
                    fn <Globe size={12} strokeWidth={1.5} aria-hidden />
                  </>
                ) : (
                  keysOf(k)[0]
                )}
              </span>
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
            <button className="link" onClick={onGrant}>
              Grant
            </button>
          </p>
        ) : status && !status.active ? (
          <p className="note warn-text">
            <AlertTriangle size={14} strokeWidth={1.5} /> {label} is not working: {status.error ?? "unknown reason"}.
            {perms.needs_relaunch && (
              <button className="link" onClick={onRestart}>
                Restart susurro
              </button>
            )}
          </p>
        ) : heard ? (
          <p className="note ok-text">
            <Check size={14} strokeWidth={2} /> {label} works.
          </p>
        ) : (
          <p className="note">
            <span className="dot idle" aria-hidden /> {status?.active ? `${label} is ready. ` : ""}Hold it and say something.
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
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onNext}>
          Continue
        </button>
      </div>
    </>
  );
}

function ShortcutStep({ onNext }: { onNext: () => void }) {
  const s = settingsStore.use()!;
  const keys = keysOf(s.hotkeys.ptt);
  const set = (double_tap: boolean) => saveSettings((x) => void (x.hotkeys.double_tap = double_tap));
  return (
    <>
      <Head eyebrow="Your shortcut" title={["One key. ", "Your", " rhythm."]} />
      <div className="choice" role="radiogroup" aria-label="How you dictate" onKeyDown={radioKeys}>
        <button role="radio" aria-checked={!s.hotkeys.double_tap} tabIndex={s.hotkeys.double_tap ? -1 : 0} onClick={() => set(false)}>
          <span className="t">
            <Hand size={16} strokeWidth={1.5} /> Hold to talk
          </span>
          <span className="d">
            Hold <Kbd keys={keys} />, talk, release. Text lands where you type.
          </span>
        </button>
        <button role="radio" aria-checked={s.hotkeys.double_tap} tabIndex={s.hotkeys.double_tap ? 0 : -1} onClick={() => set(true)}>
          <span className="t">
            <Repeat size={16} strokeWidth={1.5} /> Tap to toggle
          </span>
          <span className="d">
            Hold works too. Double-tap <Kbd keys={keys} /> to talk hands-free, tap again to stop.
          </span>
        </button>
      </div>
      <div className="lang-row" style={{ justifyContent: "center" }}>
        <span className="label">Language</span>
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
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onNext}>
          Continue
        </button>
      </div>
    </>
  );
}

function TryStep({ ptt, onFinish, onCloud }: { ptt: string; onFinish: () => void; onCloud: () => void }) {
  const s = settingsStore.use()!;
  const [result, setResult] = useState<HistoryItem | null>(null);
  const [models, setModels] = useState<ModelInfo[] | null>(null);
  const [prog, setProg] = useState<ModelProgress | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const needModel = s.stt.provider === "whisper_local" && models !== null && !models.some((m) => m.downloaded);

  useEffect(() => {
    const offs = [on("history_added", setResult), on("model_progress", setProg)];
    api.whisperModels().then(setModels);
    return () => offs.forEach((p) => p.then((f) => f()));
  }, []);

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
      <Head eyebrow="Try it" title={["Say it ", "once."]} />
      <p className="why">
        Click the box, hold <Kbd keys={keysOf(ptt)} /> and say: <em>this is my first dictation</em>.
      </p>
      {needModel && !err && (
        <div className="ob-panel">
          <div className="progress">
            <div className="track">
              <div className="fill" style={{ width: `${prog ? (prog.done / prog.total) * 100 : 0}%` }} />
            </div>
            <span className="mono">{prog ? `${Math.floor((prog.done / prog.total) * 100)}%` : "starting"}</span>
          </div>
          <p className="small" style={{ marginTop: 8 }}>
            Downloading {modelName(s.language === "en" ? "ggml-base.en-q5_1.bin" : "ggml-base-q5_1.bin")}, the fast model, first.{" "}
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
      <textarea className="input practice" placeholder="Your words land here." defaultValue={result?.final_text} key={result?.id} aria-label="Practice field" />
      {result && (
        <p className="result-line">
          <span className="ok-text">✓ {result.words} words</span>
          <span>{secs(result.duration_ms)} spoken</span>
          <span>{result.latency_ms} ms to text</span>
        </p>
      )}
      <div className="ob-actions">
        <button className="btn primary ob-next" onClick={onFinish}>
          Finish
        </button>
      </div>
    </>
  );
}
