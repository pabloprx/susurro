// Tiny shared stores (no state library, DESIGN 11) + formatting helpers used by many screens.
import { useEffect, useSyncExternalStore } from "react";
import { api, on, type DeviceKind, type KeyStatus, type MicState, type Permissions, type Settings } from "./api";

function store<T>(load: () => Promise<T>) {
  let value: T | null = null;
  let started = false;
  const subs = new Set<() => void>();
  const set = (v: T) => {
    value = v;
    subs.forEach((f) => f());
  };
  const refresh = () => load().then(set);
  return {
    get: () => value,
    set,
    refresh,
    use(): T | null {
      const v = useSyncExternalStore(
        (cb) => (subs.add(cb), () => void subs.delete(cb)),
        () => value,
      );
      useEffect(() => {
        if (!started) {
          started = true;
          refresh();
        }
      }, []);
      return v;
    },
  };
}

export const settingsStore = store<Settings>(api.getSettings);
export const micStore = store<MicState>(api.getMicState);
export const keyStore = store<KeyStatus>(api.keyStatus);
export const permStore = store<Permissions>(api.permissionsStatus);
void on("mic_state", micStore.set);

/** Optimistic save: apply locally, then take the backend's validated copy. Rejects with the backend error. */
export async function saveSettings(mutate: (s: Settings) => void): Promise<void> {
  const cur = settingsStore.get();
  if (!cur) return;
  const next = structuredClone(cur);
  mutate(next);
  settingsStore.set(next);
  try {
    settingsStore.set(await api.saveSettings(next));
  } catch (e) {
    settingsStore.set(cur);
    throw e;
  }
}

// ---------- formatting ----------

/** Timestamps: unix ms. ponytail: values below 1e12 are read as seconds, in case the backend stores seconds. */
export const toMs = (t: number) => (t < 1e12 ? t * 1000 : t);

export const clock = (t: number) =>
  new Date(toMs(t)).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });

export function dayLabel(t: number): string {
  const d = new Date(toMs(t));
  const today = new Date();
  const diff = Math.round((new Date(today.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 864e5);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
}

/** 4.8s / 1:04 */
export const secs = (ms: number) => {
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  return `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
};

export const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

export function ago(t: number): string {
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

/** "ggml-large-v3-turbo-q5_0.bin" -> "large-v3-turbo q5_0" */
export function modelName(file: string): string {
  const m = file.replace(/^ggml-/, "").replace(/\.bin$/, "");
  const q = m.match(/-(q\d_\d)$/);
  return q ? `${m.slice(0, -q[0].length)} ${q[1]}` : m;
}

export const KIND_LABEL: Record<DeviceKind, string> = {
  built_in: "built-in",
  usb: "USB",
  bluetooth: "Bluetooth",
  virtual: "virtual",
  other: "external",
};

export const PROVIDER_LABEL: Record<string, string> = {
  whisper_local: "Whisper",
  openai_compat: "Local server",
  elevenlabs: "ElevenLabs",
  openrouter: "OpenRouter",
};

const GLYPH: Record<string, string> = {
  Fn: "fn",
  Ctrl: "⌃",
  Alt: "⌥",
  Cmd: "⌘",
  Shift: "⇧",
  Escape: "esc",
  Space: "Space",
  Enter: "↩",
  Backspace: "⌫",
  Tab: "⇥",
  Up: "↑",
  Down: "↓",
  Left: "←",
  Right: "→",
};
/** "Ctrl+Cmd+V" -> ["⌃", "⌘", "V"] */
export const keysOf = (binding: string) => binding.split("+").map((k) => GLYPH[k] ?? k);

export const words = (s: string) => (s.trim() ? s.trim().split(/\s+/).length : 0);

/** Theme lives in localStorage, not Settings (PLAN 9). */
export type Theme = "system" | "dark" | "light";
export const getTheme = () => (localStorage.getItem("susurro.theme") as Theme) || "system";
export function applyTheme(t: Theme = getTheme()) {
  localStorage.setItem("susurro.theme", t);
  const light = t === "light" || (t === "system" && matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
}
