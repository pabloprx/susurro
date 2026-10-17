//! Every type that crosses IPC. `cargo test` exports them all to `src/lib/contract.gen.ts` (ts-rs).
//! Rules (PLAN 1.5): snake_case fields on both sides, `#[serde(default)]` on every struct, errors cross as String.
//! Styles are cut to one global style + per-app Raw list (orchestrator decision; categories are v1.1).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! ipc {
    ($($item:item)*) => {
        $(
            #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
            #[ts(export, export_to = "contract.gen.ts")]
            $item
        )*
    };
}

// ---------- settings ----------

ipc! {
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub onboarded: bool,
    /// "auto" | ISO-639-1
    pub language: String,
    pub hotkeys: Hotkeys,
    pub mic: MicSettings,
    pub stt: SttSettings,
    pub cleanup: CleanupSettings,
    pub styles: Styles,
    pub dictionary: DictOpts,
    pub spoken_punctuation: bool,
    pub paste: PasteOpts,
    pub recording: RecordingOpts,
    pub overlay: OverlayOpts,
    pub privacy: PrivacyOpts,
    pub autostart: bool,
    /// Voice control (hold `hotkeys.command`).
    pub command: CommandSettings,
}

/// Voice control: hold or double-tap the command key and talk; the planner LLM writes the actions while you speak,
/// Jev grounds each click target on the screen (VOICE_CONTROL_V2).
#[serde(default)]
pub struct CommandSettings {
    /// Off = the command key is not registered.
    pub enabled: bool,
    /// Ask for a yes (voice, or the command key) before a step that sends, deletes or buys. Return always asks.
    pub confirm_risky: bool,
    /// Pinned: Jev answers can shift between releases. "typesafe/jev-1.14". Jev only grounds click targets now.
    pub model: String,
    /// The planner LLM on OpenRouter (any chat model id; `list_models("chat")` lists them). It turns the
    /// spoken order into actions. Default DEFAULT_PLANNER (`.lo/planner-bench.md`).
    pub planner_model: String,
}

/// One binding per action. Modifier-only bindings ("Fn", "RightOption", "LeftOption", "RightCommand", "LeftCommand",
/// "RightControl", "LeftControl", "RightShift", "LeftShift") use the CGEventTap and work only for ptt;
/// anything else is an accelerator combo for global-shortcut.
#[serde(default)]
pub struct Hotkeys {
    pub ptt: String,
    pub hands_free: Option<String>,
    pub double_tap: bool,
    /// After a hold is released, a press within this window continues hands-free (0-1500, 0 = off).
    /// The release itself never waits: the text pastes at once. Also the double-tap window.
    pub hands_free_grace_ms: u32,
    pub cancel: String,
    pub paste_last: String,
    /// Copies the last transcript to the clipboard (Wispr: Ctrl+Cmd+C).
    pub copy_last: String,
    /// Hold for voice control (default "F2"). A global-shortcut combo, never modifier-only. "" = none.
    /// Double-tap (or hold, release, press again in the grace) = a hands-free live command session.
    pub command: String,
    /// Opens the recents list (the last 9 dictations; 1-9 pastes, Cmd+1-9 copies). Default "Ctrl+" + the ptt F-key.
    pub recents: String,
}

#[serde(default)]
pub struct MicSettings {
    pub selection: MicSelection,
    pub priority: Vec<KnownDevice>,
    pub show_virtual: bool,
}

#[serde(rename_all = "snake_case")]
pub enum MicSelection {
    Auto,
    Device { id: String },
}

#[serde(default)]
pub struct KnownDevice {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub last_seen: i64,
}

#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    BuiltIn,
    Usb,
    Bluetooth,
    Virtual,
    Other,
}

pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub kind: DeviceKind,
    pub is_default: bool,
}

pub struct MicState {
    pub devices: Vec<DeviceInfo>,
    pub selection: MicSelection,
    pub priority: Vec<KnownDevice>,
    pub active: Option<String>,
    /// Name of the wanted device that is now missing.
    pub fallback_from: Option<String>,
}

#[serde(default)]
pub struct SttSettings {
    pub provider: SttProvider,
    pub whisper_model: String,
    pub openai_compat: OpenAiCompat,
    pub elevenlabs: ElevenOpts,
    pub openrouter_model: String,
    /// Cloud STT fails -> whisper local, if a model is downloaded.
    pub local_fallback: bool,
    /// MAI-Transcribe's own "clean" style (drops fillers). Independent of the LLM cleanup.
    pub mai_clean: bool,
}

#[serde(default)]
pub struct OpenAiCompat {
    pub base_url: String,
    pub model: String,
    pub send_hotwords: bool,
}

#[serde(default)]
pub struct ElevenOpts {
    pub model_id: String,
    pub send_keyterms: bool,
    pub max_keyterms: u32,
}

pub enum SttProvider {
    #[serde(rename = "whisper_local")]
    WhisperLocal,
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
    #[serde(rename = "elevenlabs")]
    ElevenLabs,
    /// OpenRouter's JSON transcription API (stt/openrouter.rs): dictionary words go as provider options.
    #[serde(rename = "openrouter")]
    OpenRouter,
}

#[serde(default)]
pub struct CleanupSettings {
    pub level: CleanupLevel,
    pub provider: LlmProvider,
    pub openrouter_model: String,
    pub local_base_url: String,
    pub local_model: String,
    pub timeout_ms: u32,
}

#[serde(rename_all = "snake_case")]
pub enum CleanupLevel {
    None,
    Light,
    Medium,
}

pub enum LlmProvider {
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "local")]
    Local,
}

/// One global style + bundle ids that get Raw (no LLM, no style rules, corrections kept).
#[serde(default)]
pub struct Styles {
    pub style: Style,
    pub raw_apps: Vec<String>,
}

#[serde(rename_all = "snake_case")]
pub enum Style {
    Formal,
    Casual,
    VeryCasual,
    Excited,
}

#[serde(default)]
pub struct DictOpts {
    pub fuzzy: bool,
}

#[serde(default)]
pub struct PasteOpts {
    pub restore_clipboard: bool,
    /// Wait before the old clipboard goes back. Slow (Electron) apps get at least 1500 ms (paste.rs).
    /// 500 ms follows Wispr; the real per-app values need to be measured (PLAN 12).
    pub restore_delay_ms: u32,
    pub press_enter_command: bool,
}

#[serde(default)]
pub struct RecordingOpts {
    /// 600 s = 19.2 MB as s16 WAV, under the 25 MB multipart cap.
    pub max_seconds: u32,
    pub sounds: bool,
}

#[serde(default)]
pub struct OverlayOpts {
    pub show_idle: bool,
}

#[serde(default)]
pub struct PrivacyOpts {
    pub store_history: bool,
    pub store_audio: bool,
    pub audio_retention_days: u32,
}

// ---------- dictionary ----------

pub struct DictEntry {
    pub id: i64,
    pub kind: DictKind,
    pub text: String,
    pub aliases: Vec<String>,
    pub expansion: Option<String>,
    pub exact_only: bool,
    pub starred: bool,
    pub uses: u32,
    pub created_at: i64,
}

/// Word: term + optional misheard aliases. Snippet: trigger (text) -> expansion.
#[serde(rename_all = "snake_case")]
pub enum DictKind {
    Word,
    Snippet,
}

pub struct DictEntryInput {
    pub id: Option<i64>,
    pub kind: DictKind,
    pub text: String,
    pub aliases: Vec<String>,
    pub expansion: Option<String>,
    pub exact_only: bool,
    pub starred: bool,
}

pub struct Applied {
    pub text: String,
    pub matches: Vec<Match>,
}

/// start/end are char offsets in the output text.
pub struct Match {
    pub start: u32,
    pub end: u32,
    pub entry_id: i64,
    pub how: MatchHow,
}

#[serde(rename_all = "snake_case")]
pub enum MatchHow {
    Exact,
    Squashed,
    Fuzzy,
    Snippet,
}

// ---------- session, history, stats ----------

/// Pill mapping (DESIGN 5): Recording = listening, Locked = handsfree, Done + "pasted" = done, Done + "copied" = copied.
pub struct SessionState {
    pub phase: Phase,
    pub started_at: Option<i64>,
    pub device: Option<String>,
    pub app: Option<String>,
    /// Done: the HistoryStatus ("pasted" | "copied" | "done"). Error: the error text.
    pub message: Option<String>,
    /// Recording / Processing / Acting belong to a voice command (the command key), not a dictation.
    pub command: bool,
    /// Recording, just after a hold was released: the overlay counts this down ("tap again for hands-free").
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_ms: Option<u32>,
}

#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Recording,
    Locked,
    Processing,
    /// A voice command runs (command_step events tell the step).
    Acting,
    Done,
    Error,
}

pub struct HistoryItem {
    pub id: i64,
    pub created_at: i64,
    /// Command rows: raw = the spoken instruction, final_text = what susurro did, error = why it stopped.
    pub kind: HistoryKind,
    pub app_bundle: Option<String>,
    pub app_name: Option<String>,
    pub raw: String,
    pub final_text: String,
    pub provider: String,
    pub cleanup: Option<String>,
    pub duration_ms: u32,
    pub words: u32,
    pub latency_ms: u32,
    pub status: HistoryStatus,
    pub has_audio: bool,
    pub error: Option<String>,
}

#[serde(rename_all = "snake_case")]
pub enum HistoryKind {
    Dictation,
    Command,
}

/// Done: a voice command finished. A command that stopped, was unsure or blocked is Cancelled; Setup/Error is Failed.
#[serde(rename_all = "snake_case")]
pub enum HistoryStatus {
    Pasted,
    Copied,
    Cancelled,
    Failed,
    Done,
}

pub struct Stats {
    pub words_total: u32,
    pub words_7d: u32,
    pub sessions_7d: u32,
    pub wpm: f32,
    pub minutes_saved: f32,
    pub daily: Vec<DayStat>,
}

pub struct DayStat {
    /// YYYY-MM-DD
    pub day: String,
    pub words: u32,
}

pub struct Toast {
    pub id: String,
    pub kind: ToastKind,
    pub text: String,
    pub actions: Vec<ToastActionDef>,
    pub ttl_ms: u32,
}

#[serde(rename_all = "snake_case")]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

pub struct ToastActionDef {
    pub id: String,
    pub label: String,
}

// ---------- models, providers, permissions ----------

pub struct ModelInfo {
    pub file: String,
    pub size_mb: u32,
    pub downloaded: bool,
    pub active: bool,
    pub note: String,
}

pub struct ModelProgress {
    pub file: String,
    pub done: u64,
    pub total: u64,
}

pub struct KeyStatus {
    pub elevenlabs: bool,
    pub openrouter: bool,
    pub local_stt: bool,
    pub local_llm: bool,
}

#[derive(Copy)]
#[serde(rename_all = "snake_case")]
pub enum KeyName {
    Elevenlabs,
    Openrouter,
    LocalStt,
    LocalLlm,
}

pub struct Permissions {
    pub microphone: PermState,
    pub accessibility: bool,
    pub input_monitoring: bool,
    /// None = unknown.
    pub fn_key_free: Option<bool>,
    /// Input Monitoring is granted, but this process cannot use it until it restarts (relaunch_app).
    pub needs_relaunch: bool,
    /// Voice control asks for it only when a step needs a screenshot.
    pub screen_recording: bool,
}

/// Payload of the `hotkey_status` event: the push-to-talk binding is live, or why not.
pub struct HotkeyStatus {
    pub binding: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

#[serde(rename_all = "snake_case")]
pub enum PermState {
    Granted,
    Denied,
    Undetermined,
}

#[serde(rename_all = "snake_case")]
pub enum PermKind {
    Microphone,
    Accessibility,
    InputMonitoring,
    FnKey,
    ScreenRecording,
}

pub struct ProviderTest {
    pub ok: bool,
    pub latency_ms: u32,
    pub text: String,
    pub error: Option<String>,
}

pub struct OrModel {
    pub id: String,
    pub name: String,
    /// 1-based place in the OpenRouter transcription ranking (hardcoded top 10, stt kind only).
    pub rank: Option<u32>,
    /// USD per hour of audio. None = priced per token (or a chat model).
    pub price_hour: Option<f64>,
    /// The model gets the dictionary words as provider options (MAI phrase list, ElevenLabs keyterms).
    pub keywords: bool,
    /// The default pick (DEFAULT_OPENROUTER_STT for STT, DEFAULT_OPENROUTER_CLEANUP for chat).
    pub recommended: bool,
}

// ---------- voice control ----------

/// Global points: the CoreGraphics space, origin top-left of the main display, y down.
#[derive(Copy, Default)]
pub struct PtRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One actionable Accessibility element, numbered for the marks window and the Jev choice.
pub struct Mark {
    pub id: u32,
    pub rect: PtRect,
    pub name: String,
    /// "button", "text field", "checkbox", ...
    pub role: String,
    /// "sidebar" | "toolbar" | "main" | "bottom" | ...
    pub region: String,
}

/// marks = numbered marks are up, the target is being grounded. pick = "Which one?" (the top 3 in `choices`).
/// confirm = a risky step or Return waits for a yes (voice, or the command key; Esc stops).
#[serde(rename_all = "snake_case")]
pub enum CmdStatus {
    /// The action waits in the live session's queue.
    Queued,
    Thinking,
    Marks,
    Aim,
    Pick,
    Confirm,
    Done,
    Stopped,
    Unsure,
    Blocked,
    Setup,
    Error,
}

/// Payload of `command_step` (all windows) and a row of `command_log`.
#[derive(Default)]
#[serde(default)]
pub struct CommandStep {
    pub instruction: String,
    /// 1-based action number in the live session.
    pub step: u32,
    /// v1 step cap; 0 in a live session.
    pub max: u32,
    pub status: CmdStatus,
    /// What the executor did: click | focus_and_type | scroll_up | scroll_down | press_key | open_app | copy | paste |
    /// select_typed | delete_typed
    pub action: Option<String>,
    /// open_app: the app; press_key: the combo; focus_and_type: the text; select/delete_typed: the typed text.
    pub detail: Option<String>,
    /// The display the marks window covers, global points.
    pub display: PtRect,
    /// Empty = no marks on screen (the marks window hides).
    pub marks: Vec<Mark>,
    /// The chosen mark (aim, confirm).
    pub target: Option<u32>,
    /// pick: the top 3 (mark id, confidence).
    pub choices: Vec<(u32, f32)>,
    pub latency_ms: u32,
    pub cost_usd: f64,
    /// unsure / blocked / stopped / error text. setup: "screen_recording" | "key" | "accessibility".
    pub message: Option<String>,
    /// Live session: the planner op (key | type | click | open_app | select_typed | delete_typed | copy | paste | scroll).
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// Live session: the order (one planner reply) this action belongs to, 1-based. 0 = none.
    pub order: u32,
}

/// Payload of `command_live` (overlay, marks, main): the live command session.
#[derive(Default)]
#[serde(default)]
pub struct LiveState {
    /// A live session runs (recording or draining its queue).
    pub on: bool,
    /// Words heard but not planned yet (the order is not complete).
    pub buffer: String,
    /// Short labels of the queued actions, next first: "⌘T", "type “ls”", "⏎".
    pub queue: Vec<String>,
    /// The question that waits for a voice answer: "Press Return? Say yes · F11 · Esc", "Which one? 4, 7 or 9".
    pub pending: Option<String>,
    /// Pablo moved the mouse or typed: the marks hide until the next order.
    pub took_over: bool,
    /// The last chunk of text heard.
    pub last_said: String,
}

/// One thing susurro did (or heard) in voice control. RAM only, newest last, 50 max.
pub struct MemoryEntry {
    /// ms since the epoch.
    pub at: i64,
    pub kind: MemKind,
    pub text: String,
    pub app: String,
    /// The field name or role it went into ("" = none).
    pub field: String,
}

#[serde(rename_all = "snake_case")]
pub enum MemKind {
    Said,
    Typed,
    Deleted,
    Copied,
    Pasted,
    Clicked,
    Key,
    Opened,
    Failed,
}

/// Payload of `recents` (overlay): the recents list. Empty items = closed.
#[derive(Default)]
#[serde(default)]
pub struct Recents {
    /// Newest first, at most 9 dictation rows. Status copied / failed = it did not land in a field.
    pub items: Vec<HistoryItem>,
    /// 1-9: this row was just pasted or copied (the overlay flashes it, then the list closes).
    pub picked: Option<u32>,
}

/// dBFS, throttled to 30 Hz.
pub struct AudioLevel {
    pub db: f32,
    pub peak_db: f32,
}

/// Payload of the `navigate` event (tray menu -> main window).
pub struct Navigate {
    pub screen: String,
}
}

// ---------- defaults (PLAN 1.5) ----------

/// v2: the default ptt became F1; a stored "Fn" moves to it once, and the old 350 ms restore delay becomes 500.
/// v3: voice control (hotkeys.command = "F2"), and an empty OpenRouter STT model becomes MAI-Transcribe 2.
/// v4: an empty OpenRouter cleanup model becomes DEFAULT_OPENROUTER_CLEANUP (empty = cleanup silently off).
/// v5: hotkeys.recents ("Ctrl+" + an F-key ptt), the old 400 ms grace becomes 1000, LLM cleanup off once for MAI STT.
pub const SETTINGS_VERSION: u32 = 5;
pub const DEFAULT_OPENROUTER_STT: &str = "microsoft/mai-transcribe-2";
/// Cheap and fast, keeps the words: 0.7 s warm on a short cleanup, $0.10/$0.40 per M tokens
/// (measured 2026-10-17 vs claude-haiku-5.5, gemini-3.1-flash-lite, gpt-4.1-nano).
pub const DEFAULT_OPENROUTER_CLEANUP: &str = "google/gemini-2.5-flash-lite";
pub const JEV_MODEL: &str = "typesafe/jev-1.14";
/// `.lo/planner-bench.md` (2026-10-17): fastest of the models with every plan right (0.78 s p50, 60/60 same effect),
/// pinned to Cerebras with reasoning low (planner.rs `extra_for`).
pub const DEFAULT_PLANNER: &str = "openai/gpt-oss-120b";

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            onboarded: false,
            language: "auto".into(),
            hotkeys: Hotkeys::default(),
            mic: MicSettings::default(),
            stt: SttSettings::default(),
            cleanup: CleanupSettings::default(),
            styles: Styles::default(),
            dictionary: DictOpts::default(),
            spoken_punctuation: true,
            paste: PasteOpts::default(),
            recording: RecordingOpts::default(),
            overlay: OverlayOpts::default(),
            privacy: PrivacyOpts::default(),
            autostart: false,
            command: CommandSettings::default(),
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            ptt: "F1".into(),
            hands_free: None,
            double_tap: true,
            hands_free_grace_ms: 1000,
            cancel: "Escape".into(),
            paste_last: "Ctrl+Cmd+V".into(),
            copy_last: "Ctrl+Cmd+C".into(),
            command: "F2".into(),
            recents: "Ctrl+F1".into(),
        }
    }
}

impl Default for MicSettings {
    fn default() -> Self {
        Self { selection: MicSelection::Auto, priority: vec![], show_virtual: false }
    }
}

impl Default for KnownDevice {
    fn default() -> Self {
        Self { id: String::new(), name: String::new(), kind: DeviceKind::Other, last_seen: 0 }
    }
}

impl Default for SttSettings {
    fn default() -> Self {
        Self {
            provider: SttProvider::WhisperLocal,
            whisper_model: "ggml-large-v3-turbo-q5_0.bin".into(),
            openai_compat: OpenAiCompat::default(),
            elevenlabs: ElevenOpts::default(),
            openrouter_model: DEFAULT_OPENROUTER_STT.into(),
            local_fallback: true,
            mai_clean: true,
        }
    }
}

impl Default for OpenAiCompat {
    fn default() -> Self {
        Self { base_url: "http://127.0.0.1:8080/v1".into(), model: "whisper-1".into(), send_hotwords: false }
    }
}

impl Default for ElevenOpts {
    fn default() -> Self {
        Self { model_id: "scribe_v2".into(), send_keyterms: false, max_keyterms: 100 }
    }
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            level: CleanupLevel::Light,
            provider: LlmProvider::OpenRouter,
            openrouter_model: DEFAULT_OPENROUTER_CLEANUP.into(),
            local_base_url: "http://127.0.0.1:1234/v1".into(),
            local_model: String::new(),
            timeout_ms: 8000,
        }
    }
}

impl Default for Styles {
    fn default() -> Self {
        Self { style: Style::Formal, raw_apps: vec![] }
    }
}

impl Default for DictOpts {
    fn default() -> Self {
        Self { fuzzy: true }
    }
}

impl Default for PasteOpts {
    fn default() -> Self {
        Self { restore_clipboard: true, restore_delay_ms: 500, press_enter_command: false }
    }
}

impl Default for RecordingOpts {
    fn default() -> Self {
        Self { max_seconds: 600, sounds: true }
    }
}

impl Default for OverlayOpts {
    fn default() -> Self {
        Self { show_idle: true }
    }
}

impl Default for PrivacyOpts {
    fn default() -> Self {
        Self { store_history: true, store_audio: true, audio_retention_days: 14 }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self { phase: Phase::Idle, started_at: None, device: None, app: None, message: None, command: false, grace_ms: None }
    }
}

impl Default for CommandSettings {
    fn default() -> Self {
        Self { enabled: false, confirm_risky: true, model: JEV_MODEL.into(), planner_model: DEFAULT_PLANNER.into() }
    }
}

impl Default for CmdStatus {
    fn default() -> Self {
        Self::Thinking
    }
}
