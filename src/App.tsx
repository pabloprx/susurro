// Main window shell (DESIGN 6): sidebar + one screen, onboarding over everything when needed.
import { Component, useEffect, useState, type ComponentType, type ReactNode } from "react";
import { AudioLines, BookA, Cpu, House, Keyboard, Mic, PenLine, Settings2 } from "lucide-react";
import { on, type SessionState } from "./lib/api";
import { applyTheme, getTheme, micStore, modelName, permStore, PROVIDER_LABEL, settingsStore, keyStore } from "./lib/store";
import { S } from "./lib/strings";
import { Dot, Meter, Toasts, useLevel } from "./ui";
import Home from "./screens/Home";
import History from "./screens/History";
import Dictionary from "./screens/Dictionary";
import Styles from "./screens/Styles";
import Providers from "./screens/Providers";
import Audio from "./screens/Audio";
import Shortcuts from "./screens/Shortcuts";
import Settings from "./screens/Settings";
import Onboarding from "./screens/Onboarding";
import "./styles/tokens.css";
import "./styles/app.css";

export type Screen = keyof typeof S.nav;
export type Nav = {
  go: (s: Screen) => void;
  setTesting: (on: boolean) => void;
  openOnboarding: (step: number) => void;
};

const NAV: [Screen, ComponentType<{ size?: number; strokeWidth?: number }>][] = [
  ["home", House],
  ["history", AudioLines],
  ["dictionary", BookA],
  ["styles", PenLine],
  ["providers", Cpu],
  ["audio", Mic],
  ["shortcuts", Keyboard],
  ["settings", Settings2],
];
const SCREENS: Record<Screen, ComponentType<{ nav: Nav }>> = {
  home: Home,
  history: History,
  dictionary: Dictionary,
  styles: Styles,
  providers: Providers,
  audio: Audio,
  shortcuts: Shortcuts,
  settings: Settings,
};
const ANNOUNCE: Record<string, string> = {
  recording: "Listening",
  locked: "Listening, hands-free",
  processing: "Processing",
  error: "Error",
};

export default function App() {
  const [screen, setScreen] = useState<Screen>("home");
  const [testing, setTesting] = useState(false);
  const [onboardStep, setOnboardStep] = useState<number | null>(null);
  const [announce, setAnnounce] = useState("");
  const settings = settingsStore.use();
  const perms = permStore.use();

  // theme: localStorage, system resolved live
  useEffect(() => {
    applyTheme();
    const mq = matchMedia("(prefers-color-scheme: light)");
    const f = () => getTheme() === "system" && applyTheme("system");
    mq.addEventListener("change", f);
    return () => mq.removeEventListener("change", f);
  }, []);

  // first launch, or a required permission is missing -> onboarding, resumed at the first missing step
  useEffect(() => {
    if (!settings || !perms || onboardStep !== null || sessionStorage.getItem("susurro.skipOnboarding")) return;
    const missing = [
      perms.microphone !== "granted",
      !perms.accessibility,
      settings.hotkeys.ptt === "Fn" && (!perms.input_monitoring || perms.fn_key_free === false),
    ];
    if (!settings.onboarded) setOnboardStep(0);
    else if (missing.some(Boolean)) setOnboardStep(1 + missing.indexOf(true));
  }, [settings, perms]);

  useEffect(() => {
    const offs = [
      on("navigate", (n) => n.screen in SCREENS && setScreen(n.screen as Screen)),
      on("session_state", (s: SessionState) =>
        setAnnounce(
          s.phase === "done" ? (s.message === "copied" ? "Copied to clipboard" : "Pasted") : s.phase === "error" ? `Error: ${s.message ?? ""}` : (ANNOUNCE[s.phase] ?? ""),
        ),
      ),
    ];
    const key = (e: KeyboardEvent) => {
      if (e.metaKey && !e.shiftKey && !e.altKey && /^[1-8]$/.test(e.key)) {
        e.preventDefault();
        setScreen(NAV[+e.key - 1][0]);
      }
    };
    window.addEventListener("keydown", key);
    return () => {
      offs.forEach((p) => p.then((f) => f()));
      window.removeEventListener("keydown", key);
    };
  }, []);

  const nav: Nav = { go: setScreen, setTesting, openOnboarding: setOnboardStep };
  const Current = SCREENS[screen];

  if (onboardStep !== null)
    return (
      <>
        <Onboarding start={onboardStep} onDone={() => setOnboardStep(null)} nav={nav} />
        <Toasts />
      </>
    );

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="drag" data-tauri-drag-region />
        <div className="wordmark">susurro</div>
        <nav className="nav" aria-label="Screens">
          {NAV.map(([id, Icon], i) => (
            <div key={id}>
              {i === 4 && <div className="nav-group label">Setup</div>}
              <button className={screen === id ? "active" : ""} aria-current={screen === id ? "page" : undefined} onClick={() => setScreen(id)} title={`${S.nav[id]}  ⌘${i + 1}`}>
                <Icon size={16} strokeWidth={1.5} />
                {S.nav[id]}
              </button>
            </div>
          ))}
        </nav>
        <Instruments testing={testing} go={setScreen} />
      </aside>
      <main className="content" key={screen}>
        <div className="column">
          <Guard>
            <Current nav={nav} />
          </Guard>
        </div>
      </main>
      <div className="sr-only" aria-live="polite">
        {announce}
      </div>
      <Toasts />
    </div>
  );
}

/** Sidebar footer: the instrument panel (input device + mini meter, STT engine + status). */
function Instruments({ testing, go }: { testing: boolean; go: (s: Screen) => void }) {
  const mic = micStore.use();
  const s = settingsStore.use();
  const keys = keyStore.use();
  const lvl = useLevel(testing);
  const device = mic?.devices.find((d) => d.id === mic.active);
  const p = s?.stt.provider ?? "whisper_local";
  const needsKey = keys && ((p === "elevenlabs" && !keys.elevenlabs) || (p === "openrouter" && !keys.openrouter));
  const model = p === "whisper_local" ? modelName(s?.stt.whisper_model ?? "") : p === "openrouter" ? s?.stt.openrouter_model || "no model" : p === "elevenlabs" ? s?.stt.elevenlabs.model_id : s?.stt.openai_compat.model;
  return (
    <div className="instruments">
      <button onClick={() => go("audio")}>
        <span className="label">Input</span>
        <span className="inst-value">{device?.name ?? "No microphone"}</span>
        <Meter db={lvl.db} peak={lvl.peak} segments={10} mini label="Sidebar input level" />
      </button>
      <button onClick={() => go("providers")}>
        <span className="label">{PROVIDER_LABEL[p]}</span>
        <span className="inst-value">
          <span className="ellipsis">{needsKey ? `${model} · needs key` : model}</span>
          <Dot tone={needsKey ? "idle" : "ok"} />
        </span>
      </button>
    </div>
  );
}

/** A render error in one screen must not blank the window: show it, keep the sidebar. `main` is keyed by screen, so
 *  opening another screen resets this. */
class Guard extends Component<{ children: ReactNode }, { err: string | null }> {
  state = { err: null as string | null };
  static getDerivedStateFromError(e: unknown) {
    return { err: String(e) };
  }
  render() {
    return this.state.err ? (
      <p className="note danger-text" role="alert">
        This screen failed to draw: {this.state.err}. Open another screen, then come back.
      </p>
    ) : (
      this.props.children
    );
  }
}
