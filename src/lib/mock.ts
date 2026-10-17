// Browser-only fake backend (no Tauri). Covers every screen and state with realistic data and simulated events:
// audio_level streams, device hot-plug, pipeline phases, model download progress, permission grants.
// URL flags: ?onboarding (fresh install, onboarding flow), ?empty (fresh install, empty states), ?needscreen (the last
// voice command needed Screen Recording), /marks?state=marks|aim|pick|confirm (voice-control marks window),
// /overlay?state=cmd-listening|cmd-thinking|cmd-acting|cmd-pick|cmd-confirm|cmd-done|cmd-stopped|cmd-unsure|cmd-blocked|cmd-setup,
// /overlay?state=idle|hover|listening|handsfree|warning|processing|done|copied|fixax|nofield|error|notice (freeze one pill state).
// Console: window.mock.plug() / unplug() / dictate() / error() / toast(text).
// Hotkeys: holding the push-to-talk key (default F1) dictates; a double tap starts hands-free, the next tap stops it.
// Modifier-only keys need Input Monitoring, like the CGEventTap. window.mock.stale = true: an Input Monitoring grant
// needs relaunch_app (needs_relaunch), and an Accessibility grant never shows (a stale switch from an older build).
// mock.perms(patch) edits permissions.
import type * as C from "./api";

const q = new URLSearchParams(location.search);
const onboarding = q.has("onboarding");
const fresh = onboarding || q.has("empty");
const T0 = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

// ---------------- settings ----------------
let settings: C.Settings = {
  version: 1,
  onboarded: !onboarding,
  language: "auto",
  hotkeys: { ptt: "F1", hands_free: null, double_tap: true, cancel: "Escape", paste_last: "Ctrl+Cmd+V", copy_last: "Ctrl+Cmd+C", command: "F2" },
  mic: { selection: "auto", priority: [], show_virtual: false },
  stt: {
    provider: "whisper_local",
    whisper_model: "ggml-large-v3-turbo-q5_0.bin",
    openai_compat: { base_url: "http://127.0.0.1:8080/v1", model: "whisper-1", send_hotwords: false },
    elevenlabs: { model_id: "scribe_v2", send_keyterms: false, max_keyterms: 100 },
    openrouter_model: "",
    local_fallback: true,
  },
  cleanup: {
    level: fresh ? "none" : "light",
    provider: "openrouter",
    openrouter_model: "google/gemini-2.5-flash",
    local_base_url: "http://127.0.0.1:1234/v1",
    local_model: "",
    timeout_ms: 8000,
  },
  styles: { style: "casual", raw_apps: fresh ? [] : ["com.todesktop.230313mzl4w4u92", "com.apple.Terminal"] },
  dictionary: { fuzzy: true },
  spoken_punctuation: true,
  paste: { restore_clipboard: true, restore_delay_ms: 350, press_enter_command: false },
  recording: { max_seconds: 600, sounds: true },
  overlay: { show_idle: true },
  privacy: { store_history: true, store_audio: true, audio_retention_days: 14 },
  autostart: false,
  command: { enabled: false, confirm_risky: true, model: "typesafe/jev-1.14" },
};

let pillRect: C.PtRect | null = null;
let keys: C.KeyStatus = { elevenlabs: false, openrouter: !fresh, local_stt: false, local_llm: false };

// relaunch_app reloads the page; permissions survive it like real grants do
const relaunched = sessionStorage.getItem("mock.perms");
let perms: C.Permissions = relaunched
  ? JSON.parse(relaunched)
  : onboarding
    ? { microphone: "undetermined", accessibility: false, input_monitoring: false, fn_key_free: null, needs_relaunch: false, screen_recording: false }
    : { microphone: "granted", accessibility: true, input_monitoring: true, fn_key_free: true, needs_relaunch: false, screen_recording: false };
sessionStorage.removeItem("mock.perms");
/** Grants made in "System Settings" while stale: applied only by a relaunch. */
let pending: Partial<C.Permissions> = {};
let stale = false;
let failSave: string | null = null; // window.mock.failSave = "reason": every save_settings rejects
let capturing = false;

// ---------------- devices ----------------
const dev = (id: string, name: string, kind: C.DeviceKind, manufacturer: string | null, is_default = false): C.DeviceInfo => ({
  id,
  name,
  manufacturer,
  kind,
  is_default,
});
const known = (d: C.DeviceInfo, ago = 0): C.KnownDevice => ({ id: d.id, name: d.name, kind: d.kind, last_seen: T0 - ago });
const builtin = dev("BuiltInMicrophoneDevice", "MacBook Pro Microphone", "built_in", "Apple Inc.", true);
const airpods = dev("AirPods-7C:2A:31", "AirPods Pro", "bluetooth", "Apple Inc.");
const shure = dev("AppleUSBAudioEngine:Shure:MV7:3", "Shure MV7", "usb", "Shure Inc.");
const blackhole = dev("BlackHole2ch_UID", "BlackHole 2ch", "virtual", "Existential Audio");
const krisp = dev("KrispMicrophone", "Krisp Microphone", "virtual", "Krisp");

let devices: C.DeviceInfo[] = [builtin, blackhole, krisp];
settings.mic.priority = fresh ? [known(builtin)] : [known(builtin), known(airpods, 3 * DAY)];

function resolveActive(): { active: string | null; fallback_from: string | null } {
  const connected = (id: string) => devices.some((d) => d.id === id);
  const sel = settings.mic.selection;
  if (sel !== "auto") {
    if (connected(sel.device.id)) return { active: sel.device.id, fallback_from: null };
  }
  const first = settings.mic.priority.find((k) => connected(k.id) && k.kind !== "virtual");
  const active = first?.id ?? devices.find((d) => d.is_default)?.id ?? devices[0]?.id ?? null;
  const fallback_from = sel !== "auto" ? (settings.mic.priority.find((k) => k.id === sel.device.id)?.name ?? null) : null;
  return { active, fallback_from };
}
const micState = (): C.MicState => ({
  devices,
  selection: settings.mic.selection,
  priority: settings.mic.priority,
  ...resolveActive(),
});
const pushMic = () => mockEmit("mic_state", micState());

// ---------------- history ----------------
type Seed = [number, string, string, string, string, C.HistoryStatus?];
const seeds: Seed[] = [
  [14 * MIN, "com.tinyspeck.slackmacgap", "Slack", "um can you push the release after lunch thanks", "Can you push the release after lunch? Thanks."],
  [29 * MIN, "com.todesktop.230313mzl4w4u92", "Cursor", "rename the handler to on paste failed and add a test for the copied only path", "rename the handler to onPasteFailed and add a test for the copied only path"],
  [46 * MIN, "com.apple.mail", "Mail", "hi nadia attached is the march report uh let me know if the numbers look right best pablo", "Hi Nadia, attached is the March report. Let me know if the numbers look right.\n\nBest,\nPablo"],
  [2 * HOUR, "com.linear", "Linear", "the overlay steals focus when you click it on a second monitor we should set focusable false", "The overlay steals focus when you click it on a second monitor. We should set focusable to false."],
  [3 * HOUR, "com.apple.Notes", "Notes", "ideas for the weekend one finish the karpathy video two call mom three fix the bike", "Ideas for the weekend:\n1. Finish the Karpathy video\n2. Call mom\n3. Fix the bike"],
  [3 * HOUR + 20 * MIN, "com.tinyspeck.slackmacgap", "Slack", "contra flow is down again or is it just me", "Contaflow is down again, or is it just me?", "copied"],
  [5 * HOUR, "com.google.Chrome", "Chrome", "best mechanical keyboard for mac with low profile switches", "best mechanical keyboard for mac with low profile switches", "failed"],
  [DAY + 2 * HOUR, "com.tinyspeck.slackmacgap", "Slack", "so i think we should uh ship it friday no actually thursday", "I think we should ship it Thursday."],
  [DAY + 3 * HOUR, "net.whatsapp.WhatsApp", "WhatsApp", "hey are you free tonight i was thinking we could grab dinner", "Hey, are you free tonight? I was thinking we could grab dinner."],
  [DAY + 5 * HOUR, "com.todesktop.230313mzl4w4u92", "Cursor", "write a test for the tauri command that lists the history with a query", "write a test for the Tauri command that lists the history with a query"],
  [DAY + 6 * HOUR, "com.apple.mail", "Mail", "hi francisca thanks for the quick review i fixed the two comments and pushed", "Hi Francisca,\n\nThanks for the quick review. I fixed the two comments and pushed.\n\nBest,\nPablo"],
  [2 * DAY + HOUR, "com.linear", "Linear", "when the mic disconnects mid session we keep recording on the built in mic and show a notice in the pill", "When the mic disconnects mid-session, we keep recording on the built-in mic and show a notice in the pill."],
  [2 * DAY + 4 * HOUR, "com.tinyspeck.slackmacgap", "Slack", "sounds good see you at three", "Sounds good, see you at three."],
  [3 * DAY + 2 * HOUR, "com.apple.Notes", "Notes", "remember to measure the clipboard restore delay in electron apps", "Remember to measure the clipboard restore delay in Electron apps."],
  [3 * DAY + 5 * HOUR, "com.tinyspeck.slackmacgap", "Slack", "the deploy went fine the only thing left is the sea lion docs", "The deploy went fine. The only thing left is the CLI docs."],
  [4 * DAY + 3 * HOUR, "net.whatsapp.WhatsApp", "WhatsApp", "on my way be there in ten", "On my way, be there in ten."],
  [5 * DAY + HOUR, "com.apple.mail", "Mail", "hi team the q3 numbers are in the shared folder best pablo", "Hi team,\n\nThe Q3 numbers are in the shared folder.\n\nBest,\nPablo"],
  [6 * DAY + 2 * HOUR, "com.linear", "Linear", "add a retry button to the error pill when the provider times out", "Add a retry button to the error pill when the provider times out."],
];
const wc = (s: string) => s.trim().split(/\s+/).length;
let nextId = 1000;
let history: C.HistoryItem[] = fresh
  ? []
  : seeds.map(([ago, bundle, name, raw, fin, status = "pasted"], i) => {
      const w = wc(fin);
      return {
        id: 900 - i,
        created_at: T0 - ago,
        kind: "dictation" as const,
        app_bundle: bundle,
        app_name: name,
        raw,
        final_text: fin,
        provider: i % 5 === 3 ? "openrouter:openai/gpt-4o-mini-transcribe" : "whisper_local:large-v3-turbo q5_0",
        cleanup: bundle.startsWith("com.todesktop") ? null : "light",
        duration_ms: Math.round((w / (138 + ((i * 17) % 30))) * 60_000),
        words: w,
        latency_ms: 380 + ((i * 97) % 600),
        status,
        has_audio: ago < 14 * DAY && i !== 4,
        error: status === "failed" ? "Paste failed: Chrome did not accept the paste. Copied to clipboard." : null,
      };
    });

// voice commands live in History too (kind "command"): raw = instruction, final_text = what was done
const cmdRow = (id: number, ago: number, app: string, said: string, did: string, status: C.HistoryStatus, error: string | null = null): C.HistoryItem => ({
  id, created_at: T0 - ago, kind: "command", app_bundle: null, app_name: app, raw: said, final_text: did, provider: "typesafe/jev-1.14",
  cleanup: null, duration_ms: 1400, words: 0, latency_ms: 1650, status, has_audio: false, error,
});
if (!fresh) {
  history.push(
    cmdRow(950, 20 * MIN, "Slack", "open my DM with Ana", "Clicked “Ana Silva” (link, sidebar)", "done"),
    cmdRow(951, 3 * HOUR + 40 * MIN, "Mail", "delete this email", "", "cancelled", "Stopped before “Delete”: you pressed esc."),
  );
  history.sort((a, b) => b.created_at - a.created_at);
}

function stats(): C.Stats {
  const week = history.filter((h) => h.kind === "dictation" && T0 - h.created_at < 7 * DAY && h.status !== "cancelled");
  const words_7d = week.reduce((a, h) => a + h.words, 0);
  const speakMin = week.reduce((a, h) => a + h.duration_ms, 0) / 60_000;
  const wpm = speakMin ? words_7d / speakMin : 0;
  const daily: C.DayStat[] = [];
  for (let d = 6; d >= 0; d--) {
    const day = new Date(T0 - d * DAY);
    const key = day.toISOString().slice(0, 10);
    // pad the seed so the sparkline reads like a real week (seed holds only a few items per day)
    const words = week.filter((h) => new Date(h.created_at).toDateString() === day.toDateString()).reduce((a, h) => a + h.words, 0);
    daily.push({ day: key, words: fresh ? 0 : words * 9 + [210, 0, 480, 390, 620, 540, 300][6 - d] });
  }
  const w7 = daily.reduce((a, d) => a + d.words, 0);
  return {
    words_total: fresh ? 0 : w7 * 6 + 1204,
    words_7d: w7,
    sessions_7d: fresh ? 0 : week.length * 9,
    wpm: Math.round(wpm * 10) / 10,
    minutes_saved: wpm ? w7 / 45 - w7 / wpm : 0,
    daily,
  };
}

// ---------------- dictionary ----------------
let dictId = 100;
const word = (text: string, aliases: string[], uses: number, ago: number, starred = false): C.DictEntry => ({
  id: dictId++,
  kind: "word",
  text,
  aliases,
  expansion: null,
  exact_only: false,
  starred,
  uses,
  created_at: T0 - ago,
});
const snip = (text: string, expansion: string, uses: number, ago: number): C.DictEntry => ({
  ...word(text, [], uses, ago),
  kind: "snippet",
  expansion,
});
let dict: C.DictEntry[] = fresh
  ? []
  : [
      word("Contaflow", ["contra flow", "conta flow"], 31, 20 * DAY, true),
      word("Tauri", ["tory"], 12, 9 * DAY),
      word("Francisca Ugarte", [], 4, 6 * DAY),
      word("CLI", ["sea lion"], 9, 5 * DAY),
      word("kubectl", ["cube control", "kube cuddle"], 7, 12 * DAY),
      word("Wispr Flow", ["whisper flow"], 3, 2 * DAY),
      word("Nadia", [], 6, 15 * DAY),
      word("onPasteFailed", ["on paste failed"], 2, DAY),
      word("TypeScript", [], 18, 30 * DAY),
      word("Supabase", ["super base"], 5, 8 * DAY),
      word("Karpathy", ["car pathy"], 2, 4 * DAY),
      word("Reveniu", ["revenue"], 1, 3 * DAY),
      word("whisper.cpp", ["whisper c p p"], 3, 7 * DAY),
      word("Pablo Pérez", [], 11, 25 * DAY, true),
      snip("my calendly", "https://calendly.com/pablo/30min", 14, 18 * DAY),
      snip("sign off", "Best,\nPablo - {date}", 22, 21 * DAY),
      snip("my address", "Av. Providencia 1208, of. 402, Santiago", 3, 10 * DAY),
      snip("standup template", "Yesterday:\n- \nToday:\n- \nBlockers:\n- none", 6, 6 * DAY),
    ];

const norm = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();

function validate(e: C.DictEntryInput): C.DictEntryInput {
  const clean = (s: string) => s.trim().replace(/\s+/g, " ");
  const text = clean(e.text);
  const aliases = [...new Set(e.aliases.map(clean).filter(Boolean))];
  for (const t of [text, ...aliases]) {
    if (t.length < 1 || t.length > 60) throw `"${t.slice(0, 20)}" must be 1-60 characters`;
    if (!/[\p{L}\p{N}]/u.test(t)) throw `"${t}" needs at least one letter or number`;
  }
  if (aliases.some((a) => norm(a) === norm(text))) throw `An alias cannot equal "${text}"`;
  if ((e.expansion ?? "").length > 4000) throw "Expansion is over 4000 characters";
  for (const other of dict) {
    if (other.id === e.id) continue;
    const taken = [other.text, ...other.aliases].map(norm);
    for (const t of [text, ...aliases]) {
      if (taken.includes(norm(t))) throw `"${t}" already exists in ${other.kind === "snippet" ? "Snippets" : `"${other.text}"`}`;
    }
  }
  return { ...e, text, aliases };
}

function preview(text: string): C.Applied {
  const keys: [string, C.DictEntry, C.MatchHow][] = [];
  for (const d of dict) {
    keys.push([d.text, d, d.kind === "snippet" ? "snippet" : "squashed"]);
    for (const a of d.aliases) keys.push([a, d, "exact"]);
  }
  keys.sort((a, b) => b[0].length - a[0].length);
  if (!keys.length) return { text, matches: [] };
  const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`(?<![\\p{L}\\p{N}])(${keys.map((k) => esc(k[0])).join("|")})(?![\\p{L}\\p{N}])`, "giu");
  let out = "";
  let last = 0;
  const matches: C.Match[] = [];
  for (const m of text.matchAll(re)) {
    const hit = keys.find((k) => k[0].toLowerCase() === m[0].toLowerCase())!;
    const rep = hit[1].kind === "snippet" ? hit[1].expansion! : hit[1].text;
    out += text.slice(last, m.index);
    const start = [...out].length;
    out += rep;
    if (rep !== m[0]) matches.push({ start, end: start + [...rep].length, entry_id: hit[1].id, how: hit[2] });
    last = m.index! + m[0].length;
  }
  out += text.slice(last);
  return { text: out, matches };
}

// ---------------- models ----------------
type Model = C.ModelInfo & { fail?: boolean };
let models: Model[] = [
  { file: "ggml-base.en-q5_1.bin", size_mb: 57, downloaded: !fresh, active: false, note: "fast · English only" },
  { file: "ggml-base-q5_1.bin", size_mb: 57, downloaded: false, active: false, note: "fast · multilingual" },
  { file: "ggml-small-q5_1.bin", size_mb: 181, downloaded: false, active: false, note: "balanced · multilingual" },
  { file: "ggml-large-v3-turbo-q5_0.bin", size_mb: 547, downloaded: !fresh, active: false, note: "best · default" },
  { file: "ggml-large-v3-turbo-q8_0.bin", size_mb: 834, downloaded: false, active: false, note: "best · larger", fail: true },
];
const modelList = () => models.map(({ fail: _f, ...m }) => ({ ...m, active: m.downloaded && m.file === settings.stt.whisper_model }));

async function download(file: string) {
  const m = models.find((x) => x.file === file);
  if (!m) throw `Unknown model ${file}`;
  const total = m.size_mb * 1_048_576;
  let done = 0;
  const rate = (m.size_mb < 100 ? 9 : 26) * 1_048_576; // bytes/s, mock
  while (done < total) {
    await wait(200);
    done = Math.min(total, done + rate * (0.15 + Math.random() * 0.1));
    mockEmit("model_progress", { file, done, total } satisfies C.ModelProgress);
    if (m.fail && done / total > 0.37) {
      m.fail = false; // the retry works
      throw "Download failed: disk full. 1.1 GB needed.";
    }
  }
  m.downloaded = true;
}

// ---------------- audio levels ----------------
/** Speech-like dBFS curve: syllables at ~4 Hz inside phrases, a noise floor near -58 dB. */
function speechDb(t: number): C.AudioLevel {
  const phrase = Math.sin(t * 1.3) + Math.sin(t * 0.47) > -0.6 ? 1 : 0;
  const syll = Math.max(0, Math.sin(t * Math.PI * 2 * 3.7) * 0.7 + Math.sin(t * 11.3) * 0.3);
  const db = -58 + Math.random() * 4 + phrase * syll * (40 + Math.sin(t * 0.8) * 6);
  return { db: Math.min(-1, db), peak_db: Math.min(0, db + 3 + Math.random() * 5) };
}
let levelTimer: number | undefined;
function levels(on: boolean) {
  clearInterval(levelTimer);
  if (on) levelTimer = window.setInterval(() => mockEmit("audio_level", speechDb(performance.now() / 1000)), 33);
}

// ---------------- session ----------------
let session: C.SessionState = { phase: "idle", started_at: null, device: null, app: null, message: null, command: false };
function phase(p: C.Phase, extra: Partial<C.SessionState> = {}) {
  session = { ...session, phase: p, ...extra };
  if (p === "idle") session = { phase: "idle", started_at: null, device: null, app: null, message: null, command: false };
  levels(p === "recording" || p === "locked");
  mockEmit("session_state", session);
}
function start(p: "recording" | "locked") {
  const a = micState().active;
  phase(p, { started_at: Date.now(), device: devices.find((d) => d.id === a)?.name ?? null, app: "Slack" });
}
async function finish() {
  if (session.phase !== "recording" && session.phase !== "locked") return;
  const dur = Date.now() - (session.started_at ?? Date.now());
  phase("processing");
  await wait(900);
  const raw = "um so the new build is up uh can you test the hot plug flow";
  const fin = "The new build is up. Can you test the hot-plug flow?";
  const item: C.HistoryItem = {
    id: nextId++,
    created_at: Date.now(),
    kind: "dictation",
    app_bundle: "com.tinyspeck.slackmacgap",
    app_name: "Slack",
    raw,
    final_text: fin,
    provider: "whisper_local:large-v3-turbo q5_0",
    cleanup: settings.cleanup.level === "none" ? null : settings.cleanup.level,
    duration_ms: Math.max(1200, dur),
    words: wc(fin),
    latency_ms: 642,
    status: "pasted",
    has_audio: true,
    error: null,
  };
  history.unshift(item);
  mockEmit("history_added", item);
  phase("done", { message: "pasted" });
  await wait(700);
  phase("idle");
}

function toast(text: string, kind: C.ToastKind = "info", actions: C.ToastActionDef[] = [], ttl_ms = 5000, id = `t${Math.random()}`) {
  mockEmit("toast", { id, kind, text, actions, ttl_ms } satisfies C.Toast);
}

function plug() {
  if (devices.some((d) => d.id === shure.id)) return;
  devices = [shure, ...devices];
  pushMic();
  if (!settings.mic.priority.some((k) => k.id === shure.id))
    toast("New mic: Shure MV7. Switch to it for dictation?", "info", [
      { id: "keep", label: "Keep current" },
      { id: "switch", label: "Switch" },
    ], 10_000, `mic-new:${shure.id}`); // same id and ttl as audio.rs
}
function unplug() {
  devices = devices.filter((d) => d.id !== shure.id);
  pushMic();
  const fb = devices.find((d) => d.id === micState().active)?.name ?? "no microphone";
  if (session.phase === "recording" || session.phase === "locked") toast(`Mic lost - continuing on ${fb}`, "warn", [], 2500);
  else toast(`Shure MV7 disconnected - using ${fb}`, "warn");
}

// ---------------- commands ----------------
const handlers: Record<string, (a: Record<string, any>) => unknown> = {
  get_settings: () => settings,
  save_settings: (a) => {
    if (failSave) throw failSave;
    const s = a.settings as C.Settings;
    const all = [s.hotkeys.ptt, s.hotkeys.hands_free, s.hotkeys.cancel, s.hotkeys.paste_last, s.hotkeys.copy_last, s.command.enabled ? s.hotkeys.command : null].filter(Boolean); // off = not registered (settings.rs active_hotkeys)
    if (new Set(all).size !== all.length) throw "Two actions share one shortcut";
    const moved = s.hotkeys.ptt !== settings.hotkeys.ptt;
    settings = s;
    if (moved) hotkeyStatus();
    return settings;
  },
  key_status: () => keys,
  set_api_key: (a) => {
    keys = { ...keys, [a.name]: a.value != null && a.value !== "" };
    return keys;
  },
  get_mic_state: micState,
  set_mic_selection: (a) => {
    const sel = a.selection as C.MicSelection;
    settings.mic.selection = sel;
    if (sel !== "auto") {
      // Like audio.rs promote(): a device that is gone keeps its place (the selection still names it).
      const d = devices.find((x) => x.id === sel.device.id);
      if (d) settings.mic.priority = [known(d), ...settings.mic.priority.filter((k) => k.id !== d.id)];
    }
    pushMic();
    return micState();
  },
  set_mic_priority: (a) => {
    const ids = a.ids as string[];
    const all = [...settings.mic.priority, ...devices.map((d) => known(d))];
    settings.mic.priority = ids.map((id) => all.find((k) => k.id === id)!).filter(Boolean);
    pushMic();
    return micState();
  },
  forget_device: (a) => {
    settings.mic.priority = settings.mic.priority.filter((k) => k.id !== a.id);
    pushMic();
    return micState();
  },
  mic_test_start: () => {
    if (!devices.length) throw "No microphone found";
    levels(true);
  },
  mic_test_stop: () => levels(false),
  dictation_toggle: () => {
    if (session.phase === "idle") start("locked");
    else void finish();
  },
  dictation_stop: () => {
    if (session.phase === "recording" || session.phase === "locked") void finish();
  },
  dictation_cancel: () => {
    if (session.phase === "recording" || session.phase === "locked") {
      phase("idle");
      toast("Cancelled", "info", [{ id: "undo", label: "Undo" }], 5000, "cancel");
    }
  },
  paste_last: () => undefined,
  get_session_state: () => session,
  toast_action: (a) => {
    const id = (a.toastId as string).startsWith("mic-new:") ? (a.toastId as string).slice(8) : null;
    if (id) {
      if (a.actionId === "switch") handlers.set_mic_selection({ selection: { device: { id } } });
      else handlers.set_mic_priority({ ids: [...settings.mic.priority.map((k) => k.id), id] });
    }
  },
  dict_list: () => dict,
  dict_upsert: (a) => {
    const e = validate(a.entry as C.DictEntryInput);
    const old = dict.find((d) => d.id === e.id);
    const saved: C.DictEntry = { ...e, id: old?.id ?? dictId++, uses: old?.uses ?? 0, created_at: old?.created_at ?? Date.now() };
    dict = old ? dict.map((d) => (d.id === saved.id ? saved : d)) : [saved, ...dict];
    return saved;
  },
  dict_delete: (a) => void (dict = dict.filter((d) => !(a.ids as number[]).includes(d.id))),
  dict_preview: (a) => preview(a.text as string),
  history_list: (a) => {
    const qq = ((a.query as string) ?? "").toLowerCase();
    return history
      .filter((h) => !qq || h.raw.toLowerCase().includes(qq) || h.final_text.toLowerCase().includes(qq))
      .filter((h) => a.beforeId == null || h.id < a.beforeId)
      .slice(0, a.limit as number);
  },
  history_delete: (a) => void (history = history.filter((h) => !(a.ids as number[]).includes(h.id))),
  history_clear: () => void (history = []),
  history_retry: async (a) => {
    await wait(900);
    const h = history.find((x) => x.id === a.id)!;
    Object.assign(h, { status: "pasted", error: null, latency_ms: 512 });
    return h;
  },
  history_audio: (a) => wav(Math.min(6000, history.find((x) => x.id === a.id)?.duration_ms ?? 2000)),
  history_paste: () => undefined,
  get_stats: stats,
  whisper_models: modelList,
  whisper_download: async (a) => {
    await download(a.file as string);
  },
  whisper_delete: (a) => {
    const m = models.find((x) => x.file === a.file);
    if (m) m.downloaded = false;
  },
  test_provider: async (a) => {
    await wait(350 + Math.random() * 500);
    const fail = (error: string): C.ProviderTest => ({ ok: false, latency_ms: 0, text: "", error });
    const ok = (ms: number, text: string): C.ProviderTest => ({ ok: true, latency_ms: ms, text, error: null });
    const jfk = "And so my fellow Americans, ask not what your country can do for you, ask what you can do for your country.";
    if (a.which === "cleanup") {
      if (settings.cleanup.level === "none") return fail("Cleanup is off");
      if (settings.cleanup.provider === "openrouter") return keys.openrouter ? ok(612, "OK") : fail("401 unauthorized");
      return fail("Not reachable: connection refused");
    }
    switch (settings.stt.provider) {
      case "whisper_local":
        return modelList().some((m) => m.active) ? ok(487, jfk) : fail("No model downloaded");
      case "openai_compat":
        return fail("Not reachable: connection refused at 127.0.0.1:8080");
      case "elevenlabs":
        return keys.elevenlabs ? ok(938, jfk) : fail("401 unauthorized");
      case "openrouter":
        return keys.openrouter ? ok(1204, jfk) : fail("401 unauthorized");
    }
  },
  openrouter_models: async (a) => {
    await wait(300);
    if (!keys.openrouter) throw "OpenRouter needs a key";
    return a.kind === "stt"
      ? STT_MODELS

      : [
          ["anthropic/claude-haiku-4.5", "Anthropic: Claude Haiku 4.5"],
          ["google/gemini-2.5-flash", "Google: Gemini 2.5 Flash"],
          ["google/gemini-2.5-flash-lite", "Google: Gemini 2.5 Flash Lite"],
          ["meta-llama/llama-3.3-70b-instruct", "Meta: Llama 3.3 70B"],
          ["openai/gpt-4.1-mini", "OpenAI: GPT-4.1 mini"],
        ].map(([id, name]): C.OrModel => ({ id, name, rank: null, price_hour: null, keywords: false, recommended: id === "google/gemini-2.5-flash-lite" }));
  },
  permissions_status: () => perms,
  request_permission: async (a) => {
    await wait(1400);
    const k = a.kind as C.PermKind;
    if (stale && k === "accessibility") return; // the old switch stays on in System Settings but does not count
    if (stale && k === "input_monitoring") return void ((perms = { ...perms, input_monitoring: true, needs_relaunch: true }), (pending = { fn_key_free: true }));
    if (k === "microphone") perms = { ...perms, microphone: "granted" };
    if (k === "accessibility") perms = { ...perms, accessibility: true };
    if (k === "input_monitoring") perms = { ...perms, input_monitoring: true, fn_key_free: false };
    if (k === "fn_key") perms = { ...perms, fn_key_free: true };
    if (k === "screen_recording") perms = { ...perms, screen_recording: true, needs_relaunch: true }; // macOS applies it after a restart
    if (k === "input_monitoring") hotkeyStatus();
  },
  relaunch_app: () => {
    sessionStorage.setItem("mock.perms", JSON.stringify({ ...perms, ...pending, needs_relaunch: false }));
    location.reload();
  },
  hotkey_capture: (a) => void (capturing = a.on as boolean),
  set_pill_rect: (a) => void (pillRect = a.rect as C.PtRect),
  command_log: () => cmdLog,
};

const MOD_CODE: Record<string, string> = {
  RightOption: "AltRight",
  LeftOption: "AltLeft",
  RightCommand: "MetaRight",
  LeftCommand: "MetaLeft",
  RightControl: "ControlRight",
  LeftControl: "ControlLeft",
  RightShift: "ShiftRight",
  LeftShift: "ShiftLeft",
};
/** KeyboardEvent.code of the talk key: a held modifier (CGEventTap) or an F-key (RegisterEventHotKey). */
const pttCode = (b: string) => MOD_CODE[b] ?? (/^F\d+$/.test(b) || b === "Delete" ? b : null);
const needsTap = (b: string) => b === "Fn" || b in MOD_CODE;
function hotkeyStatus() {
  const b = settings.hotkeys.ptt;
  const st: C.HotkeyStatus = needsTap(b) && !perms.input_monitoring ? { binding: b, active: false, error: "Input Monitoring is off" } : { binding: b, active: true };
  setTimeout(() => mockEmit("hotkey_status", st), 0);
}

export async function mockInvoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  const h = handlers[cmd];
  if (!h) return undefined;
  try {
    const out = await h(args);
    return out instanceof ArrayBuffer ? out : structuredClone(out);
  } catch (e) {
    throw String(e); // Tauri errors cross IPC as strings
  }
}

const listeners = new Map<string, Set<(p: unknown) => void>>();

export function mockListen(event: string, cb: (p: unknown) => void): () => void {
  if (!listeners.has(event)) listeners.set(event, new Set());
  const own = (p: unknown) => cb(p); // one entry per call, like Tauri's listen (a shared cb must not unsubscribe twice)
  listeners.get(event)!.add(own);
  return () => listeners.get(event)?.delete(own);
}

/** Fire a fake Rust event (for mock scenarios). */
export function mockEmit(event: string, payload: unknown) {
  listeners.get(event)?.forEach((cb) => cb(structuredClone(payload)));
}

/** A short 16 kHz mono WAV (soft voiced buzz) so the History player has something to play. */
function wav(ms: number): ArrayBuffer {
  const rate = 16000;
  const n = Math.round((rate * ms) / 1000);
  const buf = new ArrayBuffer(44 + n * 2);
  const v = new DataView(buf);
  const str = (o: number, s: string) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF");
  v.setUint32(4, 36 + n * 2, true);
  str(8, "WAVEfmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i++) {
    const t = i / rate;
    const env = Math.max(0, Math.sin(t * Math.PI * 2 * 3.7)) * 0.25;
    v.setInt16(44 + i * 2, Math.sin(t * Math.PI * 2 * (140 + 20 * Math.sin(t * 2))) * env * 32767, true);
  }
  return buf;
}

// ---------------- scenarios ----------------

/** Overlay page in a browser: freeze one pill state (?state=) or loop through all of them. */
export function overlayDemo(state: string | null) {
  const now = () => Date.now();
  const freeze: Record<string, () => void> = {
    idle: () => phase("idle"),
    hover: () => phase("idle"),
    listening: () => start("recording"),
    handsfree: () => phase("locked", { started_at: now() - 42_000, device: builtin.name, app: "Slack" }),
    warning: () => phase("locked", { started_at: now() - 551_000, device: builtin.name, app: "Slack" }),
    processing: () => phase("processing"),
    done: () => phase("done", { message: "pasted" }),
    copied: () => phase("done", { message: "copied" }),
    error: () => phase("error", { message: "Mic disconnected" }),
    // paste failed: the text is on the clipboard; the notice names the fix (session.rs deliver)
    fixax: () => {
      phase("done", { message: "copied" });
      toast("Copied. Turn on Accessibility to paste", "warn", [{ id: "fix-ax", label: "Turn on" }], 600_000, "copied");
    },
    nofield: () => {
      phase("done", { message: "copied" });
      toast("No text field. Copied: press ⌘V to paste", "info", [], 600_000, "no-field");
    },
    notice: () => {
      start("recording");
      setTimeout(() => toast("Switched to MacBook Pro Microphone", "warn", [], 600_000), 400);
    },
    "cmd-listening": () => phase("recording", { started_at: now(), command: true }),
    "cmd-thinking": () => (phase("acting", { command: true }), mockEmit("command_step", CMD_STATES.thinking)),
    "cmd-acting": () => (phase("acting", { command: true }), mockEmit("command_step", CMD_STATES.aim)),
    "cmd-pick": () => (phase("acting", { command: true }), mockEmit("command_step", CMD_STATES.pick)),
    "cmd-confirm": () => (phase("acting", { command: true }), mockEmit("command_step", CMD_STATES.confirm)),
  };
  // results: Rust sends the step, then the phase goes idle; the pill holds them (frozen here: the run never ends)
  for (const k of ["done", "stopped", "unsure", "blocked", "setup"])
    freeze[`cmd-${k}`] = () => (phase("acting", { command: true }), mockEmit("command_step", CMD_STATES[k]));
  if (state && freeze[state]) return setTimeout(freeze[state], 50);
  const script: [number, () => void][] = [
    [1600, () => start("recording")],
    [3200, () => void finish()],
    [2400, () => start("locked")],
    [1800, () => toast("Switched to MacBook Pro Microphone", "warn", [], 2500)],
    [3400, () => void finish()],
    [2200, () => phase("error", { message: "OpenRouter rejected the key (401)" })],
    [4400, () => phase("idle")],
  ];
  let i = 0;
  const step = () => {
    const [ms, fn] = script[i++ % script.length];
    setTimeout(() => (fn(), step()), ms);
  };
  step();
}


// ---------------- voice control ----------------
// Ranked like stt/openrouter.rs RANKED (OpenRouter transcription, requests/week), then the rest by name.
// Prices are layout data for the mock, not a source.
const or = (id: string, name: string, rank: number | null, price_hour: number | null, keywords = false): C.OrModel => ({ id, name, rank, price_hour, keywords, recommended: id === "microsoft/mai-transcribe-2" });
const STT_MODELS: C.OrModel[] = [
  or("microsoft/mai-transcribe-2", "Microsoft: MAI-Transcribe 2", 1, 0.1, true),
  or("openai/whisper-large-v3", "OpenAI: Whisper Large V3", 2, 0.027),
  or("openai/whisper-large-v3-turbo", "OpenAI: Whisper Large V3 Turbo", 3, 0.012),
  or("elevenlabs/scribe-v2", "ElevenLabs: Scribe v2", 4, 0.11),
  or("openai/gpt-4o-mini-transcribe", "OpenAI: GPT-4o mini Transcribe", 5, null),
  or("openai/whisper-1", "OpenAI: Whisper 1", 6, 0.36),
  or("openai/gpt-transcribe", "OpenAI: GPT Transcribe", 7, null),
  or("x-ai/grok-stt-1.0", "xAI: Grok STT 1.0", 8, 0.06),
  or("qwen/qwen3-asr-1.7b", "Qwen: Qwen3 ASR 1.7B", 9, 0.009),
  or("mistralai/voxtral-mini-transcribe", "Mistral: Voxtral Mini Transcribe", 10, 0.06),
  or("assemblyai/universal-3.6-pro", "AssemblyAI: Universal-3.6 Pro", null, 0.3),
  or("deepgram/nova-3", "Deepgram: Nova 3", null, 0.26),
  or("google/chirp-3", "Google: Chirp 3", null, 0.96),
  or("openai/gpt-4o-transcribe", "OpenAI: GPT-4o Transcribe", null, null),
];

/** A Slack-like window read as an AX tree (.lo/som-experiment slack page): id, name, role, region, x, y, w, h. */
const AX: [number, string, string, string, number, number, number, number][] = [
  [1, "Huddle", "button", "channel header", 1252, 10.5, 28, 28],
  [2, "Members", "button", "channel header", 1288, 10.5, 28, 28],
  [3, "Pinned", "button", "channel header", 1324, 10.5, 28, 28],
  [4, "Search in channel", "button", "channel header", 1360, 10.5, 28, 28],
  [5, "Channel details", "button", "channel header", 1396, 10.5, 28, 28],
  [6, "Acme workspace", "button", "workspace bar", 14, 12, 36, 36],
  [7, "New message", "button", "sidebar", 280, 16, 28, 28],
  [8, "Home", "button", "workspace bar", 18, 58, 28, 28],
  [9, "Threads", "link", "sidebar", 64, 58, 260, 33],
  [10, "Mentions", "link", "sidebar", 64, 91, 260, 25],
  [11, "DMs", "button", "workspace bar", 18, 96, 28, 28],
  [12, "Saved items", "link", "sidebar", 64, 116, 260, 33],
  [13, "Activity", "button", "workspace bar", 18, 134, 28, 28],
  [14, "Add reaction", "button", "messages", 1307, 141, 28, 28],
  [15, "Reply in thread", "button", "messages", 1335, 141, 28, 28],
  [16, "Forward message", "button", "messages", 1363, 141, 28, 28],
  [17, "More actions", "button", "messages", 1391, 141, 28, 28],
  [18, "# general", "link", "sidebar", 64, 181, 260, 25],
  [19, "# engineering", "link", "sidebar", 64, 206, 260, 25],
  [20, "# design", "link", "sidebar", 64, 231, 260, 25],
  [21, "2 replies", "link", "messages", 390, 230, 45, 14],
  [22, "# random", "link", "sidebar", 64, 256, 260, 25],
  [23, "# releases", "link", "sidebar", 64, 281, 260, 25],
  [24, "+ Add channels", "link", "sidebar", 64, 306, 260, 25],
  [25, "Maria Lopez", "link", "sidebar", 64, 363, 260, 25],
  [26, "Tom Becker", "link", "sidebar", 64, 388, 260, 25],
  [27, "Ana Silva", "link", "sidebar", 64, 413, 260, 25],
  [28, "Tom Hanks (bot)", "link", "sidebar", 64, 438, 260, 25],
  [29, "Bold", "button", "composer", 347, 793, 28, 28],
  [30, "Italic", "button", "composer", 377, 793, 28, 28],
  [31, "Strikethrough", "button", "composer", 407, 793, 28, 28],
  [32, "Link", "button", "composer", 437, 793, 28, 28],
  [33, "Bulleted list", "button", "composer", 467, 793, 28, 28],
  [34, "Code block", "button", "composer", 497, 793, 28, 28],
  [35, "Add workspace", "button", "workspace bar", 18, 820, 28, 28],
  [36, "Message #engineering", "text field", "composer", 341, 825, 1082, 31],
  [37, "Attach file", "button", "composer", 347, 851, 28, 28],
  [38, "Emoji", "button", "composer", 377, 851, 28, 28],
  [39, "Mention someone", "button", "composer", 407, 851, 28, 28],
  [40, "Record audio clip", "button", "composer", 437, 851, 28, 28],
  [41, "Send now", "button", "composer", 1353, 851, 34, 28],
  [42, "Schedule message", "button", "composer", 1389, 851, 28, 28],
  [43, "Preferences", "button", "workspace bar", 18, 858, 28, 28],
];
const DISPLAY: C.PtRect = { x: 0, y: 0, w: 1440, h: 900 };
// the window sits under the 24 pt menu bar
const MARKS: C.Mark[] = AX.map(([id, name, role, region, x, y, w, h]) => ({ id, name, role, region, rect: { x, y: 24 + y * (876 / 900), w, h: h * (876 / 900) } }));

const cmd = (status: C.CmdStatus, extra: Partial<C.CommandStep> = {}): C.CommandStep => ({
  instruction: "send the message",
  step: 1,
  max: 6,
  status,
  action: null,
  detail: null,
  display: DISPLAY,
  marks: [],
  target: null,
  choices: [],
  latency_ms: 0,
  cost_usd: 0,
  message: null,
  ...extra,
});
/** One frozen state per name, for the overlay and marks pages (?state=) and the screenshots. */
export const CMD_STATES: Record<string, C.CommandStep> = {
  thinking: cmd("thinking", { instruction: "open my DM with Ana" }),
  marks: cmd("marks", { instruction: "open my DM with Ana", marks: MARKS }),
  aim: cmd("aim", { instruction: "open my DM with Ana", marks: MARKS, action: "click", target: 27, latency_ms: 702, cost_usd: 0.000046 }),
  pick: cmd("pick", { instruction: "open the second channel", marks: MARKS, action: "click", target: 20, choices: [[20, 0.56], [19, 0.31], [18, 0.08]] }),
  confirm: cmd("confirm", { instruction: "send the message", step: 2, marks: MARKS, action: "click", target: 41 }),
  done: cmd("done", { instruction: "open my DM with Ana", step: 2, action: "done" }),
  stopped: cmd("stopped", { step: 2 }),
  unsure: cmd("unsure", { instruction: "click the blue thing", message: "Not sure where. Say it again." }),
  blocked: cmd("blocked", { instruction: "type my password", message: "Password field. susurro won't touch it." }),
  setup: cmd("setup", { instruction: "click the search field", message: "screen_recording" }),
};

// ?needscreen: the last run stopped because a step needed Screen Recording (Voice control shows the permission card)
let cmdLog: C.CommandStep[] = q.has("needscreen")
  ? [cmd("setup", { instruction: "click the red button", message: "screen_recording" })]
  : fresh
  ? []
  : [
      cmd("thinking", { instruction: "open notes and click the search field", action: "open_app", detail: "Notes", step: 0, latency_ms: 190, cost_usd: 0.000009 }),
      cmd("aim", { instruction: "open notes and click the search field", step: 2, action: "click", target: 4, marks: MARKS, latency_ms: 702, cost_usd: 0.000046 }),
      cmd("done", { instruction: "open notes and click the search field", step: 3, action: "done", latency_ms: 655, cost_usd: 0.000046 }),
    ];

/** A scripted command run, as Rust would emit it: listen, STT, AX read + marks, aim, act, done. */
async function command(instruction = "open my DM with Ana", risky = false) {
  phase("recording", { started_at: Date.now(), command: true });
  await wait(1400);
  phase("processing", { command: true });
  await wait(600);
  const run: C.CommandStep[] = [];
  const emit = (c: C.CommandStep) => (run.push(c), mockEmit("command_step", c));
  phase("acting", { command: true });
  emit(cmd("thinking", { instruction }));
  await wait(300);
  emit(cmd("marks", { instruction, marks: MARKS }));
  await wait(700);
  const target = risky ? 41 : 27;
  emit(cmd(risky ? "confirm" : "aim", { instruction, marks: MARKS, action: "click", target, latency_ms: 702, cost_usd: 0.000046 }));
  await wait(risky ? 2500 : 300);
  emit(cmd("thinking", { instruction, step: 2 }));
  await wait(650);
  emit(cmd("done", { instruction, step: 2, action: "done", latency_ms: 655, cost_usd: 0.000046 }));
  cmdLog = run;
  phase("idle");
}

/** Marks page in a browser: a wireframe of the app under the marks, then one frozen state or the loop. */
export function marksDemo(state: string | null) {
  const app = document.createElement("div");
  app.className = "mock-app";
  const el = (cls: string, r: C.PtRect, text = "") => {
    const d = document.createElement("div");
    d.className = cls;
    Object.assign(d.style, { left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
    d.textContent = text;
    return app.appendChild(d);
  };
  el("pane", { x: 0, y: 24, w: 64, h: 876 }).style.background = "#3f0e40";
  el("pane", { x: 64, y: 24, w: 260, h: 876 }).style.background = "#4a154b";
  const mb = document.createElement("div");
  mb.className = "mb";
  mb.textContent = "  Slack     File   Edit   View   Go   Window   Help";
  app.appendChild(mb);
  for (const m of MARKS) {
    const e = el(`el ${m.role.replace(" ", "")}`, m.rect, m.role === "link" || m.role === "text field" ? m.name : "");
    if (m.region === "sidebar" || m.region === "workspace bar") e.style.color = "#e9dbe9";
  }
  const msgs: [number, string, string][] = [
    [96, "Maria Lopez  10:02", "Deploy to staging is done, please test the login flow."],
    [168, "Tom Becker  10:05", "Login works. The settings page is still slow though."],
    [218, "Ana Silva  10:11", "I will profile it after lunch. Can someone share the trace?"],
    [284, "Maria Lopez  10:15", "Trace attached: settings-trace.json"],
  ];
  for (const [y, who, text] of msgs) {
    const m = el("msg", { x: 390, y, w: 800, h: 40 });
    m.innerHTML = `<b></b>`;
    m.firstChild!.textContent = who;
    m.append(text);
  }
  el("el", { x: 340, y: 30, w: 300, h: 30 }, "# engineering").style.fontWeight = "700";
  document.body.prepend(app);
  if (state && CMD_STATES[state]) {
    phase("acting", { command: true });
    return setTimeout(() => mockEmit("command_step", CMD_STATES[state]), 50);
  }
  const loop = async () => {
    await command("open my DM with Ana");
    await wait(900);
    await command("send the message", true);
    await wait(900);
    void loop();
  };
  void loop();
}

/** Main window in a browser: a mic gets plugged in a few seconds after load. */
const inBrowser = !("__TAURI_INTERNALS__" in window); // api.ts loads this file only outside Tauri; keep the guard anyway
if (inBrowser && !location.pathname.startsWith("/overlay") && !fresh) setTimeout(plug, 7000);

// The CGEventTap, in the browser: hold the modifier-only ptt key to dictate, any other key while held cancels.
if (inBrowser && !location.pathname.startsWith("/overlay")) {
  hotkeyStatus();
  let held = 0; // keydown time of the talk key while it is down
  let lastTap = 0;
  const isPtt = (e: KeyboardEvent) => e.code === pttCode(settings.hotkeys.ptt);
  addEventListener("keydown", (e) => {
    if (capturing || (needsTap(settings.hotkeys.ptt) && !perms.input_monitoring)) return;
    if (isPtt(e) && !e.repeat) {
      if (session.phase === "locked") return void finish(); // a tap stops hands-free
      if (session.phase === "idle") return void ((held = Date.now()), start("recording"));
    }
    if (held && !isPtt(e)) (held = 0), phase("idle"); // chord (Option+2 = @): cancel silently
  });
  addEventListener("keyup", (e) => {
    if (!held || !isPtt(e)) return;
    const tap = Date.now() - held < 250;
    held = 0;
    if (!tap) return void finish();
    // a tap is not a dictation; two taps in 400 ms start hands-free (session.rs double tap)
    if (settings.hotkeys.double_tap && Date.now() - lastTap < 400) return void ((lastTap = 0), start("locked"));
    lastTap = Date.now();
    phase("idle");
  });
}

if (inBrowser) Object.assign(window, {
  mock: {
    plug,
    unplug,
    dictate: () => start("recording"),
    handsfree: () => start("locked"),
    stop: () => void finish(),
    error: (m = "Mic disconnected") => phase("error", { message: m }),
    toast,
    phase,
    set stale(v: boolean) {
      stale = v;
    },
    set failSave(v: string | null) {
      failSave = v;
    },
    perms: (p: Partial<C.Permissions>) => void ((perms = { ...perms, ...p }), hotkeyStatus()),
    command,
    step: (k: string) => mockEmit("command_step", CMD_STATES[k]),
    get pillRect() {
      return pillRect;
    },
  },
});
