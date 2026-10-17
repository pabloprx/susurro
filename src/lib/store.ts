// Tiny shared stores (no state library, DESIGN 11) + formatting helpers used by many screens.
import { useEffect, useSyncExternalStore } from "react";
import { api, on, type DeviceKind, type HotkeyStatus, type KeyStatus, type MicState, type Permissions, type Settings } from "./api";

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
/** Last hotkey_status from Rust (null = none seen yet: there is no command to query it). */
export const hotkeyStore = store<HotkeyStatus | null>(async (): Promise<HotkeyStatus | null> => hotkeyStore.get());
void on("hotkey_status", hotkeyStore.set);
// Saves run one at a time: two quick changes (Use a model, then pick a language) must not race, or the second
// save reads the backend before the first one lands and writes the old value back.
let chain: Promise<unknown> = Promise.resolve();
let pending = 0;
const serial = <T,>(f: () => Promise<T>) => {
  const r = chain.then(f);
  chain = r.catch(() => {});
  return r;
};

// Mic commands, the tray and the device monitor write settings.mic in Rust: keep the cached copy in step.
// A queued save reads the backend anyway, so skip the refresh then (it would show the old values for a moment).
void on("mic_state", (m) => {
  micStore.set(m);
  if (settingsStore.get()) void serial(async () => void (pending || (await settingsStore.refresh())));
});

/** Optimistic save: apply locally, then apply the same change to the backend's latest copy (other paths
 *  write Settings too, e.g. the mic selection) and take the validated result. Rejects with the backend error. */
export function saveSettings(mutate: (s: Settings) => void): Promise<void> {
  const cur = settingsStore.get();
  if (!cur) return Promise.resolve();
  const next = structuredClone(cur);
  mutate(next);
  settingsStore.set(next);
  pending++;
  return serial(async () => {
    try {
      const fresh = await api.getSettings();
      mutate(fresh);
      const saved = await api.saveSettings(fresh);
      if (pending === 1) settingsStore.set(saved); // a later queued save sets the final copy
    } catch (e) {
      if (pending === 1) await settingsStore.refresh(); // back to what the backend has
      throw e;
    } finally {
      pending--;
    }
  });
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
