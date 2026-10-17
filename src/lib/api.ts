// Typed IPC for every command and event in PLAN 1.5. Without Tauri (plain browser) it routes to mock.ts.
import type { CommandStep, OrModel, Permissions, PermKind, PtRect, SessionState, Settings, Applied, AudioLevel, DictEntry, DictEntryInput, HistoryItem, HotkeyStatus, KeyName, KeyStatus, MicSelection, MicState, ModelInfo, ModelProgress, Navigate, ProviderTest, Stats, Toast } from "./contract.gen";

export type * from "./contract.gen";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) return (await import("./mock")).mockInvoke(cmd, args) as Promise<T>;
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export const api = {
  getSettings: () => call<Settings>("get_settings"),
  saveSettings: (settings: Settings) => call<Settings>("save_settings", { settings }),
  keyStatus: () => call<KeyStatus>("key_status"),
  setApiKey: (name: KeyName, value: string | null) => call<KeyStatus>("set_api_key", { name, value }),
  getMicState: () => call<MicState>("get_mic_state"),
  setMicSelection: (selection: MicSelection) => call<MicState>("set_mic_selection", { selection }),
  setMicPriority: (ids: string[]) => call<MicState>("set_mic_priority", { ids }),
  forgetDevice: (id: string) => call<MicState>("forget_device", { id }),
  micTestStart: (deviceId: string | null) => call<void>("mic_test_start", { deviceId }),
  micTestStop: () => call<void>("mic_test_stop"),
  dictationToggle: () => call<void>("dictation_toggle"),
  dictationStop: () => call<void>("dictation_stop"),
  dictationCancel: () => call<void>("dictation_cancel"),
  pasteLast: () => call<void>("paste_last"),
  getSessionState: () => call<SessionState>("get_session_state"),
  toastAction: (toastId: string, actionId: string) => call<void>("toast_action", { toastId, actionId }),
  dictList: () => call<DictEntry[]>("dict_list"),
  dictUpsert: (entry: DictEntryInput) => call<DictEntry>("dict_upsert", { entry }),
  dictDelete: (ids: number[]) => call<void>("dict_delete", { ids }),
  dictPreview: (text: string) => call<Applied>("dict_preview", { text }),
  historyList: (query: string | null, beforeId: number | null, limit: number) =>
    call<HistoryItem[]>("history_list", { query, beforeId, limit }),
  historyDelete: (ids: number[]) => call<void>("history_delete", { ids }),
  historyClear: () => call<void>("history_clear"),
  historyRetry: (id: number) => call<HistoryItem>("history_retry", { id }),
  /** Raw WAV bytes. Build a Blob URL: URL.createObjectURL(new Blob([buf], { type: "audio/wav" })). */
  historyAudio: (id: number) => call<ArrayBuffer>("history_audio", { id }),
  historyPaste: (id: number, which: "raw" | "final") => call<void>("history_paste", { id, which }),
  getStats: () => call<Stats>("get_stats"),
  whisperModels: () => call<ModelInfo[]>("whisper_models"),
  whisperDownload: (file: string) => call<void>("whisper_download", { file }),
  whisperDelete: (file: string) => call<void>("whisper_delete", { file }),
  testProvider: (which: "stt" | "cleanup") => call<ProviderTest>("test_provider", { which }),
  openrouterModels: (kind: "stt" | "chat") => call<OrModel[]>("openrouter_models", { kind }),
  permissionsStatus: () => call<Permissions>("permissions_status"),
  requestPermission: (kind: PermKind) => call<void>("request_permission", { kind }),
  /** Restarts the app (macOS applies some grants only to a new process). */
  relaunchApp: () => call<void>("relaunch_app"),
  /** Pause every hotkey while the UI records a new binding. Rust resumes by itself after 30 s. */
  hotkeyCapture: (on: boolean) => call<void>("hotkey_capture", { on }),
  /** The pill's rect inside the overlay window (logical px): the window takes clicks only there. */
  setPillRect: (rect: PtRect) => call<void>("set_pill_rect", { rect }),
  /** The last voice-control run (in memory, newest step last). */
  commandLog: () => call<CommandStep[]>("command_log"),
};

export type Events = {
  session_state: SessionState;
  audio_level: AudioLevel;
  mic_state: MicState;
  toast: Toast;
  history_added: HistoryItem;
  model_progress: ModelProgress;
  navigate: Navigate;
  hotkey_status: HotkeyStatus;
  /** Pushed by Rust when a grant changes (optional: the UI also polls and re-checks on focus). */
  permissions_changed: Permissions;
  /** Voice control: one per state change of a command run (to the overlay, marks and main windows). */
  command_step: CommandStep;
};

/** Subscribe to a Rust event. Returns the unsubscribe function. */
export async function on<E extends keyof Events>(event: E, cb: (payload: Events[E]) => void): Promise<() => void> {
  if (!isTauri) return (await import("./mock")).mockListen(event, cb as (p: unknown) => void);
  const { listen } = await import("@tauri-apps/api/event");
  return listen<Events[E]>(event, (e) => cb(e.payload));
}
