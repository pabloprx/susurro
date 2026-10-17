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
// Mic commands, the tray and the device monitor write settings.mic in Rust: keep the cached copy in step.
void on("mic_state", (m) => {
  micStore.set(m);
  if (settingsStore.get()) void settingsStore.refresh();
});

/** Optimistic save: apply locally, then apply the same change to the backend's latest copy (other paths
 *  write Settings too, e.g. the mic selection) and take the validated result. Rejects with the backend error. */
export async function saveSettings(mutate: (s: Settings) => void): Promise<void> {
  const cur = settingsStore.get();
  if (!cur) return;
  const next = structuredClone(cur);
  mutate(next);
  settingsStore.set(next);
  try {
    const fresh = await api.getSettings();
    mutate(fresh);
    settingsStore.set(await api.saveSettings(fresh));
  } catch (e) {
    settingsStore.set(cur);
    throw e;
  }
}

export * from "./format";

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

export const words = (s: string) => (s.trim() ? s.trim().split(/\s+/).length : 0);

/** Theme lives in localStorage, not Settings (PLAN 9). */
export type Theme = "system" | "dark" | "light";
export const getTheme = () => (localStorage.getItem("susurro.theme") as Theme) || "system";
export function applyTheme(t: Theme = getTheme()) {
  localStorage.setItem("susurro.theme", t);
  const light = t === "light" || (t === "system" && matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
}
