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
}

/// One binding per action. "Fn" is the only modifier-only binding (CGEventTap); others use global-shortcut.
#[serde(default)]
pub struct Hotkeys {
    pub ptt: String,
    pub hands_free: Option<String>,
    pub double_tap: bool,
    pub cancel: String,
    pub paste_last: String,
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
    /// The openai_compat client with base https://openrouter.ai/api/v1.
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
    /// 350 ms is a guess; needs to be measured per app (PLAN 12).
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
    /// Done: the HistoryStatus ("pasted" | "copied"). Error: the error text.
    pub message: Option<String>,
}

#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Recording,
    Locked,
    Processing,
    Done,
    Error,
}

pub struct HistoryItem {
    pub id: i64,
    pub created_at: i64,
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
pub enum HistoryStatus {
    Pasted,
    Copied,
    Cancelled,
    Failed,
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

pub const SETTINGS_VERSION: u32 = 1;

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
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            ptt: "Fn".into(),
            hands_free: None,
            double_tap: true,
            cancel: "Escape".into(),
            paste_last: "Ctrl+Cmd+V".into(),
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
            openrouter_model: String::new(),
            local_fallback: true,
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
            openrouter_model: String::new(),
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
        Self { restore_clipboard: true, restore_delay_ms: 350, press_enter_command: false }
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
        Self { phase: Phase::Idle, started_at: None, device: None, app: None, message: None }
    }
}
