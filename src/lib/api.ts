// Typed IPC for every command and event in PLAN 1.5. Without Tauri (plain browser) it routes to mock.ts.
import type * as C from "./contract.gen";

export type * from "./contract.gen";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) return (await import("./mock")).mockInvoke(cmd, args) as Promise<T>;
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export const api = {
  getSettings: () => call<C.Settings>("get_settings"),
  saveSettings: (settings: C.Settings) => call<C.Settings>("save_settings", { settings }),
  keyStatus: () => call<C.KeyStatus>("key_status"),
  setApiKey: (name: C.KeyName, value: string | null) => call<C.KeyStatus>("set_api_key", { name, value }),
  getMicState: () => call<C.MicState>("get_mic_state"),
  setMicSelection: (selection: C.MicSelection) => call<C.MicState>("set_mic_selection", { selection }),
  setMicPriority: (ids: string[]) => call<C.MicState>("set_mic_priority", { ids }),
  forgetDevice: (id: string) => call<C.MicState>("forget_device", { id }),
  micTestStart: (deviceId: string | null) => call<void>("mic_test_start", { deviceId }),
  micTestStop: () => call<void>("mic_test_stop"),
  dictationToggle: () => call<void>("dictation_toggle"),
  dictationStop: () => call<void>("dictation_stop"),
  dictationCancel: () => call<void>("dictation_cancel"),
  pasteLast: () => call<void>("paste_last"),
  getSessionState: () => call<C.SessionState>("get_session_state"),
  toastAction: (toastId: string, actionId: string) => call<void>("toast_action", { toastId, actionId }),
  dictList: () => call<C.DictEntry[]>("dict_list"),
  dictUpsert: (entry: C.DictEntryInput) => call<C.DictEntry>("dict_upsert", { entry }),
  dictDelete: (ids: number[]) => call<void>("dict_delete", { ids }),
  dictPreview: (text: string) => call<C.Applied>("dict_preview", { text }),
  historyList: (query: string | null, beforeId: number | null, limit: number) =>
    call<C.HistoryItem[]>("history_list", { query, beforeId, limit }),
  historyDelete: (ids: number[]) => call<void>("history_delete", { ids }),
  historyClear: () => call<void>("history_clear"),
  historyRetry: (id: number) => call<C.HistoryItem>("history_retry", { id }),
  /** Raw WAV bytes. Build a Blob URL: URL.createObjectURL(new Blob([buf], { type: "audio/wav" })). */
  historyAudio: (id: number) => call<ArrayBuffer>("history_audio", { id }),
  historyPaste: (id: number, which: "raw" | "final") => call<void>("history_paste", { id, which }),
  getStats: () => call<C.Stats>("get_stats"),
  whisperModels: () => call<C.ModelInfo[]>("whisper_models"),
  whisperDownload: (file: string) => call<void>("whisper_download", { file }),
  whisperDelete: (file: string) => call<void>("whisper_delete", { file }),
  testProvider: (which: "stt" | "cleanup") => call<C.ProviderTest>("test_provider", { which }),
  openrouterModels: (kind: "stt" | "chat") => call<C.OrModel[]>("openrouter_models", { kind }),
  permissionsStatus: () => call<C.Permissions>("permissions_status"),
  requestPermission: (kind: C.PermKind) => call<void>("request_permission", { kind }),
};

export type Events = {
  session_state: C.SessionState;
  audio_level: C.AudioLevel;
  mic_state: C.MicState;
  toast: C.Toast;
  history_added: C.HistoryItem;
  model_progress: C.ModelProgress;
  navigate: C.Navigate;
};

/** Subscribe to a Rust event. Returns the unsubscribe function. */
export async function on<E extends keyof Events>(event: E, cb: (payload: Events[E]) => void): Promise<() => void> {
  if (!isTauri) return (await import("./mock")).mockListen(event, cb as (p: unknown) => void);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<Events[E]>(event, (e) => cb(e.payload));
}
