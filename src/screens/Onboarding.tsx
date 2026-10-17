// DESIGN 6.9 / PLAN 8: six resumable steps, full window, no sidebar.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Hand, Repeat } from "lucide-react";
import { api, on, type HistoryItem, type ModelInfo, type ModelProgress } from "../lib/api";
import { keysOf, modelName, permStore, saveSettings, secs, settingsStore } from "../lib/store";
import { Kbd, Meter, radioKeys, Serif, useLevel } from "../ui";
import { LANGUAGES } from "./Providers";
import type { Nav } from "../App";

const STEPS = 6;

export default function Onboarding({ start, onDone, nav }: { start: number; onDone: () => void; nav: Nav }) {
  const [step, setStep] = useState(start);
  const [dir, setDir] = useState<"fwd" | "back">("fwd");
  const perms = permStore.use();
  const s = settingsStore.use();

  // macOS has no grant callback: poll every 1 s while a permission step is open (PLAN 8)
  useEffect(() => {
    if (step < 1 || step > 3) return;
    const t = setInterval(() => api.permissionsStatus().then(permStore.set), 1000);
    return () => clearInterval(t);
  }, [step]);

  const go = (n: number) => {
    setDir(n < step ? "back" : "fwd");
    setStep(Math.max(0, Math.min(STEPS - 1, n)));
  };
  const next = () => go(step + 1);
  const finish = async () => {
    sessionStorage.setItem("susurro.skipOnboarding", "1");
    await saveSettings((x) => void (x.onboarded = true));
    onDone();
  };

  if (!s || !perms) return <div className="onboarding" />;
  const ptt = keysOf(s.hotkeys.ptt).join(" ");

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
      pane="Accessibility"
    />,
    <PermStep
      key="i"
      eyebrow="Input Monitoring"
      title={["Hold ", "fn", " to talk."]}
      why="Input Monitoring lets susurro see the fn key, from any app."
      not="It does not record what you type."
      granted={perms.input_monitoring && perms.fn_key_free !== false}
      onGrant={perms.input_monitoring ? null : () => api.requestPermission("input_monitoring")}
      onNext={next}
      pane="Input Monitoring"
      blocked={
        perms.input_monitoring &&
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

function PermStep(p: { eyebrow: string; title: string[]; why: string; not: string; granted: boolean; onGrant: (() => void) | null; onNext: () => void; pane: string; extra?: ReactNode; blocked?: ReactNode }) {
  const was = useRef(p.granted);
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
          ) : null}
          <div className="ob-actions">
            {!p.granted && p.onGrant && (
              <button className="btn primary ob-primary" onClick={p.onGrant}>
                Open System Settings
              </button>
            )}
            <button className={`btn ${p.granted ? "primary" : "ghost"} ob-next`} onClick={p.onNext}>
              {p.granted ? "Continue" : "Not now"}
            </button>
          </div>
        </>
      )}
      {p.extra}
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
        Click the box, hold <Kbd keys={ptt.split(" ")} /> and say: <em>this is my first dictation</em>.
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
