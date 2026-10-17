//! WP1: Recorder (cpal, segments, level meter, downmix + resample), resolve(), device monitor, mic test (PLAN 3).
//!
//! Measured 2026-10-17 on a Mac Studio M4 Max (macOS 26, one input: DJI Wireless Mic Rx over USB), `examples/mic.rs`, debug build:
//! - Device poll (`list_devices`: cpal enumeration + CoreAudio transport query): first call 66 ms (CoreAudio init), then 1.2 ms avg over 20.
//!   At one tick per 1.5 s that is under 0.1% of one core, so polling stays.
//! - Press-to-first-sample (`Recorder::open` -> first callback): 192 ms on the first run (cold CoreAudio), 93-101 ms on 4 later runs (each a fresh process).
//!   Steady state is under the 150 ms bar of PLAN 3.5, so there is no "keep mic warm" setting. ponytail: re-measure with a Bluetooth mic (HFP switch).

use crate::AppState;
use crate::contract::{AudioLevel, DeviceInfo, DeviceKind, KnownDevice, MicSelection, MicSettings, MicState, ToastKind};
use crate::session::toast;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

pub const RATE: u32 = 16_000;
/// Silence gate (PLAN 3.5). ponytail: -55 dBFS is a guess; needs to be measured on real mics.
pub const SILENCE_DB: f32 = -55.0;
/// Clips shorter than this are an accidental tap: discard silently.
pub const MIN_CLIP_MS: u32 = 250;
/// ponytail: polling every 1.5 s (1.2 ms per poll, measured above). Switch to AudioObjectAddPropertyListenerBlock if that grows.
const POLL: Duration = Duration::from_millis(1500);
/// Level events: at most one per 33 ms (30 Hz).
const LEVEL_EVERY: Duration = Duration::from_millis(33);
const VIRTUAL_NAMES: [&str; 6] = ["blackhole", "krisp", "loopback", "teams", "zoom", "aggregate"];

/// 16 kHz mono f32.
#[derive(Debug, Clone, Default)]
pub struct Clip {
    pub samples: Vec<f32>,
    pub duration_ms: u32,
    pub peak_db: f32,
    pub device_names: Vec<String>,
}

impl Clip {
    /// Shorter than MIN_CLIP_MS: drop without a toast.
    pub fn too_short(&self) -> bool {
        self.duration_ms < MIN_CLIP_MS
    }
    /// Peak below SILENCE_DB: toast "No audio from <name>", no history row, no provider call.
    pub fn silent(&self) -> bool {
        self.peak_db < SILENCE_DB
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    Selected,
    Fallback { wanted: String },
    Priority,
    SystemDefault,
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub info: DeviceInfo,
    pub reason: Reason,
}

/// Last device list + active id + fallback flag (AppState.mic).
#[derive(Debug, Clone, Default)]
pub struct MicRuntime {
    pub devices: Vec<DeviceInfo>,
    /// The device a dictation would use now (resolved).
    pub active: Option<String>,
    pub fallback_from: Option<String>,
    /// The device a session Recorder is capturing from right now (the monitor sends DeviceLost if it vanishes).
    pub recording: Option<String>,
}

// ---------- pure logic (unit tested) ----------

pub fn db(x: f32) -> f32 {
    // Floor at -100: JSON cannot carry -inf.
    if x <= 1e-5 { -100.0 } else { (20.0 * x.log10()).max(-100.0) }
}

fn kind_rank(k: &DeviceKind) -> u8 {
    match k {
        DeviceKind::Usb => 0,
        DeviceKind::BuiltIn => 1,
        DeviceKind::Bluetooth => 2,
        DeviceKind::Other => 3,
        DeviceKind::Virtual => 4,
    }
}

/// Name rule wins over the transport type: Krisp/Teams/Zoom report as normal devices.
pub fn classify(name: &str, transport: DeviceKind) -> DeviceKind {
    let n = name.to_lowercase();
    if VIRTUAL_NAMES.iter().any(|v| n.contains(v)) { DeviceKind::Virtual } else { transport }
}

/// Virtual devices are hidden unless `show_virtual` (the active one always stays visible).
pub fn visible(devices: &[DeviceInfo], show_virtual: bool, active: Option<&str>) -> Vec<DeviceInfo> {
    devices.iter().filter(|d| show_virtual || d.kind != DeviceKind::Virtual || Some(d.id.as_str()) == active).cloned().collect()
}

/// By id, else by name (the id can change after an OS update, PLAN 3.1).
/// By id, else by name (an OS update changed the id). A same-name device that is another known entry
/// is that entry, not this one (two "Shure MV7" mics).
fn find<'a>(priority: &[KnownDevice], connected: &'a [DeviceInfo], id: &str, name: Option<&str>) -> Option<&'a DeviceInfo> {
    connected.iter().find(|d| d.id == id).or_else(|| {
        let n = name?;
        connected.iter().find(|d| d.name == n && !priority.iter().any(|k| k.id == d.id))
    })
}

fn auto(priority: &[KnownDevice], connected: &[DeviceInfo], system_default: Option<&str>) -> Option<Resolved> {
    let r = |d: &DeviceInfo, reason| Some(Resolved { info: d.clone(), reason });
    if let Some(d) = priority.iter().find_map(|k| find(priority, connected, &k.id, Some(&k.name))) {
        return r(d, Reason::Priority);
    }
    if let Some(d) = system_default.and_then(|id| connected.iter().find(|d| d.id == id && d.kind != DeviceKind::Virtual)) {
        return r(d, Reason::SystemDefault);
    }
    let d = connected.iter().find(|d| d.kind == DeviceKind::BuiltIn).or_else(|| connected.iter().find(|d| d.kind != DeviceKind::Virtual))?;
    r(d, Reason::Any)
}

/// PLAN 3.2. Pure: runs on every dictation start and every device-list change.
pub fn resolve(sel: &MicSelection, priority: &[KnownDevice], connected: &[DeviceInfo], system_default: Option<&str>) -> Option<Resolved> {
    let MicSelection::Device { id } = sel else { return auto(priority, connected, system_default) };
    let name = priority.iter().find(|k| &k.id == id).map(|k| k.name.as_str());
    if let Some(d) = find(priority, connected, id, name) {
        return Some(Resolved { info: d.clone(), reason: Reason::Selected });
    }
    let wanted = name.unwrap_or(id).to_string();
    auto(priority, connected, system_default).map(|r| Resolved { reason: Reason::Fallback { wanted }, ..r })
}

/// Rewrite stored ids that no longer resolve but whose name matches exactly one connected device. Returns true if changed.
pub fn heal_ids(mic: &mut MicSettings, connected: &[DeviceInfo]) -> bool {
    let mut changed = false;
    for i in 0..mic.priority.len() {
        let k = &mic.priority[i];
        if connected.iter().any(|d| d.id == k.id) {
            continue;
        }
        let same: Vec<_> = connected.iter().filter(|d| d.name == k.name).collect();
        if same.len() != 1 || mic.priority.iter().any(|o| o.id == same[0].id) {
            continue;
        }
        let (old, new) = (k.id.clone(), same[0].id.clone());
        log::info!("mic: id of {:?} changed, {old} -> {new}", k.name);
        if mic.selection == (MicSelection::Device { id: old }) {
            mic.selection = MicSelection::Device { id: new.clone() };
        }
        mic.priority[i].id = new;
        changed = true;
    }
    changed
}

fn known(d: &DeviceInfo) -> KnownDevice {
    KnownDevice { id: d.id.clone(), name: d.name.clone(), kind: d.kind.clone(), last_seen: crate::history::now_ms() }
}

/// First run: seed the priority list from what is connected, USB > BuiltIn > Bluetooth > Other (PLAN 3.2). No virtual devices.
pub fn seed_priority(connected: &[DeviceInfo]) -> Vec<KnownDevice> {
    let mut v: Vec<_> = connected.iter().filter(|d| d.kind != DeviceKind::Virtual).collect();
    v.sort_by_key(|d| kind_rank(&d.kind));
    v.into_iter().map(known).collect()
}

/// Move (or insert) `id` to the top of the priority list.
fn promote(mic: &mut MicSettings, id: &str, connected: &[DeviceInfo]) {
    let k = match mic.priority.iter().position(|k| k.id == id) {
        Some(i) => mic.priority.remove(i),
        None => match connected.iter().find(|d| d.id == id) {
            Some(d) => known(d),
            None => return,
        },
    };
    mic.priority.insert(0, k);
}

/// New order = `ids` (known or connected), then any known device the list left out (never forget silently).
pub fn reorder(priority: &[KnownDevice], ids: &[String], connected: &[DeviceInfo]) -> Vec<KnownDevice> {
    let mut out: Vec<KnownDevice> = ids
        .iter()
        .filter_map(|id| priority.iter().find(|k| &k.id == id).cloned().or_else(|| connected.iter().find(|d| &d.id == id).map(known)))
        .collect();
    out.dedup_by(|a, b| a.id == b.id);
    for k in priority {
        if !out.iter().any(|o| o.id == k.id) {
            out.push(k.clone());
        }
    }
    out
}

/// Resample a mono buffer to 16 kHz (rubato poly cubic, `process_all`).
pub fn resample(samples: &[f32], rate: u32) -> Vec<f32> {
    use audioadapter_buffers::direct::InterleavedSlice;
    use rubato::{Async, FixedAsync, PolynomialDegree, Resampler};
    if rate == RATE || samples.is_empty() {
        return samples.to_vec();
    }
    let run = || -> anyhow::Result<Vec<f32>> {
        let mut r = Async::<f32>::new_poly(RATE as f64 / rate as f64, 1.0, PolynomialDegree::Cubic, 1024, 1, FixedAsync::Input)?;
        let input = InterleavedSlice::new(samples, 1, samples.len())?;
        Ok(r.process_all(&input, samples.len(), None)?.take_data())
    };
    run().unwrap_or_else(|e| {
        log::error!("resample {rate} -> {RATE} failed: {e}");
        vec![]
    })
}

/// 16 kHz mono s16 WAV file (history audio, mic example).
pub fn write_wav(path: &std::path::Path, samples: &[f32]) -> anyhow::Result<()> {
    Ok(std::fs::write(path, crate::stt::wav_bytes(samples)?)?)
}

// ---------- CoreAudio transport type (cpal 0.18 only reports "Aggregate") ----------

#[cfg(target_os = "macos")]
mod ca {
    use crate::contract::DeviceKind;
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use std::collections::HashMap;
    use std::ffi::c_void;

    #[repr(C)]
    struct Addr {
        sel: u32,
        scope: u32,
        elem: u32,
    }
    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(id: u32, a: *const Addr, qs: u32, q: *const c_void, size: *mut u32) -> i32;
        fn AudioObjectGetPropertyData(id: u32, a: *const Addr, qs: u32, q: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
    }
    const fn cc(b: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*b)
    }
    fn addr(sel: &[u8; 4]) -> Addr {
        Addr { sel: cc(sel), scope: cc(b"glob"), elem: 0 }
    }
    fn get_u32(id: u32, sel: &[u8; 4]) -> Option<u32> {
        let (mut v, mut sz) = (0u32, 4u32);
        // SAFETY: the property is a UInt32; size matches.
        let st = unsafe { AudioObjectGetPropertyData(id, &addr(sel), 0, std::ptr::null(), &mut sz, &mut v as *mut u32 as *mut c_void) };
        (st == 0).then_some(v)
    }
    fn get_str(id: u32, sel: &[u8; 4]) -> Option<String> {
        let mut r: CFStringRef = std::ptr::null();
        let mut sz = size_of::<CFStringRef>() as u32;
        // SAFETY: the property is a CFStringRef returned under the create rule.
        let st = unsafe { AudioObjectGetPropertyData(id, &addr(sel), 0, std::ptr::null(), &mut sz, &mut r as *mut CFStringRef as *mut c_void) };
        (st == 0 && !r.is_null()).then(|| unsafe { CFString::wrap_under_create_rule(r) }.to_string())
    }

    /// CoreAudio UID -> (kind from the transport type, manufacturer).
    pub fn devices() -> HashMap<String, (DeviceKind, Option<String>)> {
        let a = addr(b"dev#");
        let mut sz = 0u32;
        // SAFETY: kAudioObjectSystemObject (1) + kAudioHardwarePropertyDevices; buffer sized from the size query.
        if unsafe { AudioObjectGetPropertyDataSize(1, &a, 0, std::ptr::null(), &mut sz) } != 0 {
            return HashMap::new();
        }
        let mut ids = vec![0u32; sz as usize / 4];
        if unsafe { AudioObjectGetPropertyData(1, &a, 0, std::ptr::null(), &mut sz, ids.as_mut_ptr() as *mut c_void) } != 0 {
            return HashMap::new();
        }
        ids.truncate(sz as usize / 4);
        ids.into_iter()
            .filter_map(|id| {
                let kind = match get_u32(id, b"tran").map(|t| t.to_be_bytes()) {
                    Some(t) if &t == b"bltn" => DeviceKind::BuiltIn,
                    Some(t) if &t == b"usb " => DeviceKind::Usb,
                    Some(t) if &t == b"blue" || &t == b"blea" => DeviceKind::Bluetooth,
                    Some(t) if &t == b"virt" || &t == b"grup" || &t == b"fgrp" => DeviceKind::Virtual,
                    _ => DeviceKind::Other,
                };
                Some((get_str(id, b"uid ")?, (kind, get_str(id, b"lmak"))))
            })
            .collect()
    }
}

// ---------- hardware ----------

fn transport_info() -> std::collections::HashMap<String, (DeviceKind, Option<String>)> {
    #[cfg(target_os = "macos")]
    return ca::devices();
    #[cfg(not(target_os = "macos"))]
    return Default::default();
}

pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let info = transport_info();
    let Ok(devs) = host.input_devices() else { return vec![] };
    devs.filter_map(|d| {
        let id = d.id().ok()?.to_string();
        let desc = d.description().ok()?;
        let name = desc.name().to_string();
        // cpal id = "coreaudio:<UID>"
        let (transport, manufacturer) = id.split_once(':').and_then(|(_, uid)| info.get(uid).cloned()).unwrap_or((DeviceKind::Other, None));
        let transport = if desc.interface_type() == cpal::InterfaceType::Aggregate { DeviceKind::Virtual } else { transport };
        Some(DeviceInfo {
            kind: classify(&name, transport),
            is_default: default.as_deref() == Some(id.as_str()),
            manufacturer: manufacturer.or_else(|| desc.manufacturer().map(String::from)),
            id,
            name,
        })
    })
    .collect()
}

fn system_default(devs: &[DeviceInfo]) -> Option<&str> {
    devs.iter().find(|d| d.is_default).map(|d| d.id.as_str())
}

fn open_device(host: &cpal::Host, dev: &DeviceInfo) -> anyhow::Result<cpal::Device> {
    if let Some(d) = dev.id.parse().ok().and_then(|id| host.device_by_id(&id)) {
        return Ok(d);
    }
    host.input_devices()?
        .find(|d| d.description().map(|x| x.name() == dev.name).unwrap_or(false))
        .ok_or_else(|| anyhow::Error::new(cpal::Error::from(cpal::ErrorKind::DeviceNotAvailable)).context(format!("{} is not connected", dev.name)))
}

/// F32 at the default rate if the device offers it, else the default config (converted in the callback).
fn pick_config(d: &cpal::Device) -> anyhow::Result<cpal::SupportedStreamConfig> {
    let def = d.default_input_config()?;
    if def.sample_format() == cpal::SampleFormat::F32 {
        return Ok(def);
    }
    let f32_cfg = d.supported_input_configs()?.find(|c| c.sample_format() == cpal::SampleFormat::F32 && c.channels() == def.channels()).and_then(|c| c.try_with_sample_rate(def.sample_rate()));
    Ok(f32_cfg.unwrap_or(def))
}

type LevelFn = Arc<dyn Fn(AudioLevel) + Send + Sync>;
type LostFn = Arc<dyn Fn() + Send + Sync>;

/// Callback-side meter: RMS + peak since the last emit, at most every 33 ms.
struct Meter {
    sumsq: f64,
    n: usize,
    peak: f32,
    last: Instant,
    emit: LevelFn,
}

impl Meter {
    fn push(&mut self, mono: &[f32]) {
        for &s in mono {
            self.sumsq += (s * s) as f64;
            self.peak = self.peak.max(s.abs());
        }
        self.n += mono.len();
        if self.last.elapsed() >= LEVEL_EVERY && self.n > 0 {
            // ponytail: emits from the CoreAudio thread (serialize + post, no blocking). Move to a ticker thread if xruns show up.
            (self.emit)(AudioLevel { db: db((self.sumsq / self.n as f64).sqrt() as f32), peak_db: db(self.peak) });
            (self.sumsq, self.n, self.peak, self.last) = (0.0, 0, 0.0, Instant::now());
        }
    }
}

pub struct Recorder {
    stream: Option<cpal::Stream>,
    /// Mono samples of the current segment, at `rate`.
    cur: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    /// Finished segments (rate, mono samples). Each is resampled on its own at stop.
    segments: Vec<(u32, Vec<f32>)>,
    device: DeviceInfo,
    device_names: Vec<String>,
    lost: Arc<AtomicBool>,
    opened: Instant,
    first: Arc<OnceLock<Instant>>,
    on_level: LevelFn,
    on_lost: LostFn,
    /// Set for session recorders: keeps MicRuntime.recording in sync.
    app: Option<AppHandle>,
}

impl Recorder {
    /// Session recorder: levels go to `level_window`, device loss sends SessionCmd::DeviceLost.
    pub fn start(app: &AppHandle, dev: &Resolved, level_window: &'static str) -> anyhow::Result<Self> {
        let a = app.clone();
        let on_level: LevelFn = Arc::new(move |l| {
            let _ = a.emit_to(level_window, "audio_level", l);
        });
        let a = app.clone();
        let on_lost: LostFn = Arc::new(move || {
            log::warn!("mic: stream reports the device is gone");
            let _ = a.state::<AppState>().session_tx.send(crate::session::SessionCmd::DeviceLost);
        });
        let mut r = Self::open(&dev.info, on_level, on_lost)?;
        r.app = Some(app.clone());
        r.mark_recording();
        Ok(r)
    }

    /// Recorder without Tauri (mic test, examples).
    pub fn open(dev: &DeviceInfo, on_level: LevelFn, on_lost: LostFn) -> anyhow::Result<Self> {
        let mut r = Recorder {
            stream: None,
            cur: Default::default(),
            rate: RATE,
            segments: vec![],
            device: dev.clone(),
            device_names: vec![],
            lost: Default::default(),
            opened: Instant::now(),
            first: Default::default(),
            on_level,
            on_lost,
            app: None,
        };
        r.open_stream(dev)?;
        Ok(r)
    }

    fn open_stream(&mut self, dev: &DeviceInfo) -> anyhow::Result<()> {
        let device = open_device(&cpal::default_host(), dev)?;
        let cfg = pick_config(&device)?;
        let (rate, ch, fmt) = (cfg.sample_rate(), cfg.channels() as usize, cfg.sample_format());
        let cur = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
        let lost = Arc::new(AtomicBool::new(false));
        let ctx = Cb {
            ch,
            cur: cur.clone(),
            first: self.first.clone(),
            meter: Meter { sumsq: 0.0, n: 0, peak: 0.0, last: Instant::now(), emit: self.on_level.clone() },
        };
        let (l, on_lost, name) = (lost.clone(), self.on_lost.clone(), dev.name.clone());
        let err = move |e: cpal::Error| match e.kind() {
            cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
                if !l.swap(true, Ordering::SeqCst) {
                    log::warn!("mic {name}: {e}");
                    on_lost();
                }
            }
            _ => log::warn!("mic {name}: {e}"),
        };
        let c = cfg.config();
        use cpal::SampleFormat as F;
        let stream = match fmt {
            F::F32 => build::<f32>(&device, c, ctx, err),
            F::I16 => build::<i16>(&device, c, ctx, err),
            F::I24 => build::<cpal::I24>(&device, c, ctx, err),
            F::I32 => build::<i32>(&device, c, ctx, err),
            F::I8 => build::<i8>(&device, c, ctx, err),
            F::U8 => build::<u8>(&device, c, ctx, err),
            F::U16 => build::<u16>(&device, c, ctx, err),
            F::F64 => build::<f64>(&device, c, ctx, err),
            f => anyhow::bail!("{}: unsupported sample format {f}", dev.name),
        }?;
        stream.play()?;
        log::info!("mic: recording from {} ({rate} Hz, {ch} ch, {fmt})", dev.name);
        (self.stream, self.cur, self.rate, self.lost, self.device) = (Some(stream), cur, rate, lost, dev.clone());
        self.device_names.push(dev.name.clone());
        Ok(())
    }

    /// New segment on `dev`; samples so far are kept. On error the recorder has no stream but `stop` still returns the audio.
    pub fn switch_device(&mut self, dev: &Resolved) -> anyhow::Result<()> {
        self.stream = None; // stops the callbacks before the buffer moves
        let done = std::mem::take(&mut *self.cur.lock().unwrap());
        self.segments.push((self.rate, done));
        let r = self.open_stream(&dev.info);
        self.mark_recording();
        r
    }

    /// The device this recorder captures from.
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    /// The stream reported DeviceNotAvailable or StreamInvalidated.
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// Time from open to the first audio callback (PLAN 3.5 measurement).
    pub fn first_sample_latency(&self) -> Option<Duration> {
        self.first.get().map(|t| t.duration_since(self.opened))
    }

    /// Device loss (PLAN 3.4): re-resolve against a fresh device list and switch.
    /// Ok(None) = nothing to do (duplicate signal, device still fine). Err = no device left or the switch failed.
    pub fn recover(&mut self, sel: &MicSelection, priority: &[KnownDevice]) -> anyhow::Result<Option<Resolved>> {
        let devs = list_devices();
        let present = devs.iter().any(|d| d.id == self.device.id);
        if present && !self.lost() && self.stream.is_some() {
            return Ok(None);
        }
        // A vanished device is not in `devs`; one that only invalidated its stream (rate change) is reopened.
        let r = resolve(sel, priority, &devs, system_default(&devs)).ok_or_else(|| anyhow::anyhow!("No microphone"))?;
        let r = match self.switch_device(&r) {
            Ok(()) => r,
            // CoreAudio can still list the device that just died: try the next one before giving up.
            Err(e) if r.info.id == self.device.id => {
                let devs: Vec<DeviceInfo> = devs.into_iter().filter(|d| d.id != r.info.id).collect();
                let r2 = resolve(sel, priority, &devs, system_default(&devs)).ok_or(e)?;
                self.switch_device(&r2)?;
                r2
            }
            Err(e) => return Err(e),
        };
        log::info!("mic: switched to {}", r.info.name);
        Ok(Some(r))
    }

    pub fn stop(mut self) -> Clip {
        self.stream = None;
        let last = std::mem::take(&mut *self.cur.lock().unwrap());
        self.segments.push((self.rate, last));
        let samples: Vec<f32> = self.segments.iter().flat_map(|(rate, s)| resample(s, *rate)).collect();
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        Clip {
            duration_ms: (samples.len() as u64 * 1000 / RATE as u64) as u32,
            peak_db: db(peak),
            samples,
            device_names: std::mem::take(&mut self.device_names),
        }
    }

    fn mark_recording(&self) {
        if let Some(app) = &self.app {
            app.state::<AppState>().mic.lock().unwrap().recording = self.stream.as_ref().map(|_| self.device.id.clone());
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(app) = &self.app {
            app.state::<AppState>().mic.lock().unwrap().recording = None;
        }
    }
}

struct Cb {
    ch: usize,
    cur: Arc<Mutex<Vec<f32>>>,
    first: Arc<OnceLock<Instant>>,
    meter: Meter,
}

fn build<T>(device: &cpal::Device, c: cpal::StreamConfig, mut cb: Cb, err: impl FnMut(cpal::Error) + Send + 'static) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut mono = Vec::with_capacity(4096);
    let stream = device.build_input_stream(
        c,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let _ = cb.first.set(Instant::now());
            mono.clear();
            // Downmix: average the channels.
            mono.extend(data.chunks(cb.ch).map(|f| f.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / cb.ch as f32));
            cb.meter.push(&mono);
            cb.cur.lock().unwrap().extend_from_slice(&mono);
        },
        err,
        None,
    )?;
    Ok(stream)
}

// ---------- app glue: runtime state, monitor, toasts ----------

/// Build MicState from settings + runtime. Virtual devices are hidden unless `show_virtual`.
pub fn mic_state(state: &AppState) -> MicState {
    let s = state.settings.read().unwrap();
    let m = state.mic.lock().unwrap();
    MicState {
        devices: visible(&m.devices, s.mic.show_virtual, m.active.as_deref()),
        selection: s.mic.selection.clone(),
        priority: s.mic.priority.clone(),
        active: m.active.clone(),
        fallback_from: m.fallback_from.clone(),
    }
}

/// The device a dictation should use now. `fresh` = enumerate now; else the monitor's list (up to 1.5 s old),
/// or a fresh list if the monitor has not run yet.
pub fn resolve_current(app: &AppHandle, fresh: bool) -> Option<Resolved> {
    let state = app.state::<AppState>();
    let cached = if fresh { vec![] } else { state.mic.lock().unwrap().devices.clone() };
    let devs = if cached.is_empty() { list_devices() } else { cached };
    let s = state.settings.read().unwrap();
    resolve(&s.mic.selection, &s.mic.priority, &devs, system_default(&devs))
}

/// Session helper for SessionCmd::DeviceLost: switch and toast "Mic lost - continuing on <name>".
/// Returns false when no device is left: the session stops and transcribes what it has.
pub fn recover_session(app: &AppHandle, rec: &mut Recorder) -> bool {
    let (sel, prio) = {
        let st = app.state::<AppState>();
        let s = st.settings.read().unwrap();
        (s.mic.selection.clone(), s.mic.priority.clone())
    };
    let (old_id, old) = (rec.device().id.clone(), rec.device().name.clone());
    match rec.recover(&sel, &prio) {
        Ok(None) => true,
        Ok(Some(r)) => {
            // Same device reopened (sample-rate change): no toast.
            if r.info.id != old_id {
                toast(app, None, "mic-lost", ToastKind::Warn, format!("Mic lost - continuing on {}", r.info.name), &[], 4000);
            }
            true
        }
        Err(e) => {
            log::warn!("mic: recover failed: {e}");
            toast(app, None, "mic-lost", ToastKind::Error, format!("{old} disconnected - no microphone left"), &[], 4000);
            false
        }
    }
}

/// Resolve against `devices`, store the runtime, toast fallback transitions, emit `mic_state`.
fn apply(app: &AppHandle, devices: Vec<DeviceInfo>, toasts: bool) -> MicState {
    let state = app.state::<AppState>();
    let (sel, prio) = {
        let s = state.settings.read().unwrap();
        (s.mic.selection.clone(), s.mic.priority.clone())
    };
    let r = resolve(&sel, &prio, &devices, system_default(&devices));
    let fallback = match (&sel, &r) {
        (_, Some(Resolved { reason: Reason::Fallback { wanted }, .. })) => Some(wanted.clone()),
        (MicSelection::Device { id }, None) => Some(prio.iter().find(|k| &k.id == id).map_or(id.clone(), |k| k.name.clone())),
        _ => None,
    };
    let prev = {
        let mut m = state.mic.lock().unwrap();
        m.devices = devices;
        m.active = r.as_ref().map(|r| r.info.id.clone());
        std::mem::replace(&mut m.fallback_from, fallback.clone())
    };
    if toasts {
        let using = r.as_ref().map_or("no microphone".to_string(), |r| r.info.name.clone());
        match (prev, &fallback) {
            (None, Some(w)) => toast(app, None, "mic-fallback", ToastKind::Warn, format!("{w} disconnected - using {using}"), &[], 4000),
            (Some(w), None) if matches!(r, Some(Resolved { reason: Reason::Selected, .. })) => {
                toast(app, None, "mic-fallback", ToastKind::Info, format!("Using {w} again"), &[], 4000)
            }
            _ => {}
        }
    }
    let ms = mic_state(&state);
    let _ = app.emit("mic_state", &ms);
    ms
}

/// Holds the write lock while writing, like save_settings: the file always gets the latest in-memory copy.
fn save(state: &AppState) {
    let s = state.settings.write().unwrap();
    if let Err(e) = crate::settings::save(&state.settings_path(), &s) {
        log::error!("mic: save settings: {e}");
    }
}

/// Device list changed: heal ids, seed or prompt for new devices, apply, and flag a lost session device.
fn on_change(app: &AppHandle, devices: Vec<DeviceInfo>, added: &[DeviceInfo]) {
    let state = app.state::<AppState>();
    let new: Vec<DeviceInfo> = {
        let mut s = state.settings.write().unwrap();
        heal_ids(&mut s.mic, &devices);
        let now = crate::history::now_ms();
        for k in s.mic.priority.iter_mut().filter(|k| devices.iter().any(|d| d.id == k.id)) {
            k.last_seen = now;
        }
        let new = if s.mic.priority.is_empty() {
            s.mic.priority = seed_priority(&devices);
            vec![]
        } else {
            let show_virtual = s.mic.show_virtual;
            added.iter().filter(|d| !s.mic.priority.iter().any(|k| k.id == d.id) && (show_virtual || d.kind != DeviceKind::Virtual)).cloned().collect()
        };
        new
    };
    save(&state);
    for d in &new {
        let bt = if d.kind == DeviceKind::Bluetooth { " (Bluetooth: using its mic lowers the headset's audio quality)" } else { "" };
        toast(app, None, &format!("mic-new:{}", d.id), ToastKind::Info, format!("New mic: {}{bt}", d.name), &[("switch", "Switch"), ("keep", "Keep current")], 10_000);
    }
    let recording = state.mic.lock().unwrap().recording.clone();
    if let Some(id) = recording.as_ref().filter(|id| !devices.iter().any(|d| &d.id == *id)) {
        log::warn!("mic: session device {id} vanished from the device list");
        let _ = state.session_tx.send(crate::session::SessionCmd::DeviceLost);
    }
    // During a session the "Mic lost - continuing on" toast (recover_session) covers it.
    apply(app, devices, recording.is_none());
}

/// PLAN 3.3: poll the input list every 1.5 s and diff by id (+ the system default).
pub fn spawn_monitor(app: AppHandle) {
    std::thread::Builder::new()
        .name("mic-monitor".into())
        .spawn(move || {
            let mut last: Option<Vec<(String, bool)>> = None;
            loop {
                let devices = list_devices();
                let key: Vec<(String, bool)> = devices.iter().map(|d| (d.id.clone(), d.is_default)).collect();
                if last.as_ref() != Some(&key) {
                    let added: Vec<DeviceInfo> = match &last {
                        Some(old) => devices.iter().filter(|d| !old.iter().any(|(id, _)| id == &d.id)).cloned().collect(),
                        None => devices.clone(), // first scan: devices plugged while the app was off
                    };
                    on_change(&app, devices, &added);
                    last = Some(key);
                }
                std::thread::sleep(POLL);
            }
        })
        .expect("spawn mic monitor");
}

/// Toast actions for "mic-new:<id>" (Switch / Keep current). Both remember the device. Returns false if not a mic toast.
/// WP5: `session` routes SessionCmd::ToastAction here first.
pub fn handle_toast_action(app: &AppHandle, toast_id: &str, action_id: &str) -> bool {
    let Some(id) = toast_id.strip_prefix("mic-new:") else { return false };
    let state = app.state::<AppState>();
    match action_id {
        "switch" => {
            let _ = select(app, MicSelection::Device { id: id.into() });
        }
        _ => {
            let devices = state.mic.lock().unwrap().devices.clone();
            {
                let mut s = state.settings.write().unwrap();
                if let Some(d) = devices.iter().find(|d| d.id == id).filter(|_| !s.mic.priority.iter().any(|k| k.id == id)) {
                    s.mic.priority.push(known(d));
                }
            }
            save(&state);
            apply(app, devices, false);
        }
    }
    true
}

fn select(app: &AppHandle, selection: MicSelection) -> MicState {
    let state = app.state::<AppState>();
    let devices = state.mic.lock().unwrap().devices.clone();
    {
        let mut s = state.settings.write().unwrap();
        if let MicSelection::Device { id } = &selection {
            promote(&mut s.mic, id, &devices);
        }
        s.mic.selection = selection;
    }
    save(&state);
    apply(app, devices, false)
}

#[tauri::command]
pub fn get_mic_state(state: State<'_, AppState>) -> MicState {
    mic_state(&state)
}

/// A Device selection also moves that device to the top of the priority list.
#[tauri::command]
pub fn set_mic_selection(app: AppHandle, selection: MicSelection) -> Result<MicState, String> {
    Ok(select(&app, selection))
}

#[tauri::command]
pub fn set_mic_priority(app: AppHandle, state: State<'_, AppState>, ids: Vec<String>) -> Result<MicState, String> {
    let devices = state.mic.lock().unwrap().devices.clone();
    {
        let mut s = state.settings.write().unwrap();
        s.mic.priority = reorder(&s.mic.priority, &ids, &devices);
    }
    save(&state);
    Ok(apply(&app, devices, false))
}

/// Removes the device from the priority list; a forgotten selected device sends the selection back to Auto.
#[tauri::command]
pub fn forget_device(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<MicState, String> {
    let devices = state.mic.lock().unwrap().devices.clone();
    {
        let mut s = state.settings.write().unwrap();
        s.mic.priority.retain(|k| k.id != id);
        if s.mic.selection == (MicSelection::Device { id }) {
            s.mic.selection = MicSelection::Auto;
        }
    }
    save(&state);
    Ok(apply(&app, devices, false))
}

// ---------- mic test (PLAN 3.6) ----------

const TEST_MAX: Duration = Duration::from_secs(30);
const TEST_QUIET_DB: f32 = -70.0;
const TEST_QUIET_FOR: Duration = Duration::from_secs(3);

static TEST: Mutex<Option<(u64, Recorder)>> = Mutex::new(None);
static TEST_GEN: AtomicU64 = AtomicU64::new(0);

/// Errors start with a code the UI can switch on: "permission_denied: ..." or "unavailable: ...".
fn open_error(dev: &DeviceInfo, e: &anyhow::Error) -> String {
    match e.chain().find_map(|c| c.downcast_ref::<cpal::Error>()).map(|c| c.kind()) {
        Some(cpal::ErrorKind::PermissionDenied) => "permission_denied: Allow microphone access in System Settings > Privacy & Security > Microphone".into(),
        Some(cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::DeviceBusy) => format!("unavailable: {} is in use by another app or unavailable", dev.name),
        _ => format!("unavailable: {}: {e}", dev.name),
    }
}

/// Opens a short-lived recorder; levels go to `main`. Stops on mic_test_stop, main window blur, device loss, or after 30 s.
/// The level staying below -70 dBFS for 3 s emits a Warn toast "We hear nothing - is it muted?".
/// Note: a denied mic permission on macOS gives silent buffers, not an error; the quiet toast covers it.
#[tauri::command]
pub async fn mic_test_start(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    let dev = match device_id {
        Some(id) => list_devices().into_iter().find(|d| d.id == id).ok_or_else(|| "unavailable: that microphone is not connected".to_string())?,
        None => resolve_current(&app, false).ok_or_else(|| "unavailable: No microphone".to_string())?.info,
    };
    mic_test_stop()?;
    // ~100 ms of CoreAudio setup: async command, so it runs off the main thread.
    let level = Arc::new(AtomicU32::new((-100f32).to_bits()));
    let (a, lv) = (app.clone(), level.clone());
    let on_level: LevelFn = Arc::new(move |l: AudioLevel| {
        lv.store(l.peak_db.to_bits(), Ordering::Relaxed);
        let _ = a.emit_to("main", "audio_level", l);
    });
    let rec = Recorder::open(&dev, on_level, Arc::new(|| {})).map_err(|e| open_error(&dev, &e))?;
    let generation = TEST_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    *TEST.lock().unwrap() = Some((generation, rec));
    std::thread::spawn(move || {
        let (t0, mut loud_at, mut warned) = (Instant::now(), Instant::now(), false);
        let main = app.get_webview_window("main");
        let mut was_focused = main.as_ref().and_then(|w| w.is_focused().ok()).unwrap_or(false);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let lost = match &*TEST.lock().unwrap() {
                Some((g, r)) if *g == generation => r.lost(),
                _ => return, // stopped or replaced
            };
            if f32::from_bits(level.load(Ordering::Relaxed)) > TEST_QUIET_DB {
                loud_at = Instant::now();
            }
            if !warned && loud_at.elapsed() >= TEST_QUIET_FOR {
                warned = true;
                toast(&app, Some("main"), "mic-test", ToastKind::Warn, "We hear nothing - is it muted?", &[], 4000);
            }
            let focused = main.as_ref().and_then(|w| w.is_focused().ok()).unwrap_or(false);
            let blurred = was_focused && !focused;
            was_focused = focused;
            if lost || blurred || t0.elapsed() >= TEST_MAX {
                let mut t = TEST.lock().unwrap();
                if matches!(&*t, Some((g, _)) if *g == generation) {
                    *t = None;
                }
                if lost {
                    toast(&app, Some("main"), "mic-test", ToastKind::Warn, format!("{} disconnected - mic test stopped", dev.name), &[], 4000);
                }
                return;
            }
        }
    });
    Ok(())
}


#[tauri::command]
pub fn mic_test_stop() -> Result<(), String> {
    TEST.lock().unwrap().take();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, kind: DeviceKind, is_default: bool) -> DeviceInfo {
        DeviceInfo { id: id.into(), name: name.into(), manufacturer: None, kind, is_default }
    }
    fn k(id: &str, name: &str) -> KnownDevice {
        KnownDevice { id: id.into(), name: name.into(), ..Default::default() }
    }
    fn sel(id: &str) -> MicSelection {
        MicSelection::Device { id: id.into() }
    }
    fn usb() -> DeviceInfo {
        dev("ca:usb", "Shure MV7", DeviceKind::Usb, false)
    }
    fn builtin() -> DeviceInfo {
        dev("ca:bi", "MacBook Pro Microphone", DeviceKind::BuiltIn, false)
    }
    fn bt() -> DeviceInfo {
        dev("ca:bt", "AirPods Pro", DeviceKind::Bluetooth, false)
    }
    fn vir() -> DeviceInfo {
        dev("ca:bh", "BlackHole 2ch", DeviceKind::Virtual, true)
    }
    fn got(r: Option<Resolved>) -> Option<(String, Reason)> {
        r.map(|r| (r.info.id, r.reason))
    }

    #[test]
    fn selected_connected() {
        let c = [builtin(), usb()];
        assert_eq!(got(resolve(&sel("ca:usb"), &[], &c, Some("ca:bi"))), Some(("ca:usb".into(), Reason::Selected)));
    }

    #[test]
    fn selected_missing_falls_back_with_wanted_name() {
        let c = [builtin(), bt()];
        let p = [k("ca:usb", "Shure MV7"), k("ca:bt", "AirPods Pro")];
        assert_eq!(got(resolve(&sel("ca:usb"), &p, &c, None)), Some(("ca:bt".into(), Reason::Fallback { wanted: "Shure MV7".into() })));
        // Unknown name: the id is the label.
        assert_eq!(got(resolve(&sel("ca:x"), &[], &c, None)), Some(("ca:bi".into(), Reason::Fallback { wanted: "ca:x".into() })));
    }

    #[test]
    fn auto_priority_first_connected() {
        let c = [builtin(), bt()];
        let p = [k("ca:usb", "Shure MV7"), k("ca:bt", "AirPods Pro"), k("ca:bi", "MacBook Pro Microphone")];
        assert_eq!(got(resolve(&MicSelection::Auto, &p, &c, Some("ca:bi"))), Some(("ca:bt".into(), Reason::Priority)));
    }

    #[test]
    fn auto_system_default_unless_virtual() {
        let mut b = bt();
        b.is_default = true;
        assert_eq!(got(resolve(&MicSelection::Auto, &[], &[builtin(), b], Some("ca:bt"))), Some(("ca:bt".into(), Reason::SystemDefault)));
        // Virtual default is skipped: first BuiltIn.
        assert_eq!(got(resolve(&MicSelection::Auto, &[], &[usb(), vir(), builtin()], Some("ca:bh"))), Some(("ca:bi".into(), Reason::Any)));
    }

    #[test]
    fn auto_builtin_then_any_then_none() {
        assert_eq!(got(resolve(&MicSelection::Auto, &[], &[usb(), builtin()], None)), Some(("ca:bi".into(), Reason::Any)));
        assert_eq!(got(resolve(&MicSelection::Auto, &[], &[vir(), usb()], None)), Some(("ca:usb".into(), Reason::Any)));
        assert_eq!(resolve(&MicSelection::Auto, &[], &[vir()], None), None);
        assert_eq!(resolve(&MicSelection::Auto, &[], &[], None), None);
        assert_eq!(resolve(&sel("ca:usb"), &[k("ca:usb", "Shure MV7")], &[], None), None);
    }

    #[test]
    fn name_fallback_match_and_heal() {
        // OS update changed the UID; the name still matches.
        let moved = dev("ca:usb-new", "Shure MV7", DeviceKind::Usb, false);
        let c = [builtin(), moved.clone()];
        let p = [k("ca:usb", "Shure MV7")];
        assert_eq!(got(resolve(&sel("ca:usb"), &p, &c, None)), Some(("ca:usb-new".into(), Reason::Selected)));
        assert_eq!(got(resolve(&MicSelection::Auto, &p, &c, None)), Some(("ca:usb-new".into(), Reason::Priority)));

        let mut mic = MicSettings { selection: sel("ca:usb"), priority: p.to_vec(), show_virtual: false };
        assert!(heal_ids(&mut mic, &c));
        assert_eq!(mic.priority[0].id, "ca:usb-new");
        assert_eq!(mic.selection, sel("ca:usb-new"));
        assert!(!heal_ids(&mut mic, &c));
        // Two devices with the same name: ambiguous, no heal.
        let mut mic = MicSettings { selection: MicSelection::Auto, priority: p.to_vec(), show_virtual: false };
        assert!(!heal_ids(&mut mic, &[moved.clone(), dev("ca:usb-2", "Shure MV7", DeviceKind::Usb, false)]));
    }

    #[test]
    fn virtual_filter() {
        assert_eq!(classify("BlackHole 2ch", DeviceKind::Other), DeviceKind::Virtual);
        assert_eq!(classify("Krisp Microphone", DeviceKind::BuiltIn), DeviceKind::Virtual);
        assert_eq!(classify("Microsoft Teams Audio", DeviceKind::Other), DeviceKind::Virtual);
        assert_eq!(classify("ZoomAudioDevice", DeviceKind::Other), DeviceKind::Virtual);
        assert_eq!(classify("Shure MV7", DeviceKind::Usb), DeviceKind::Usb);
        let all = [builtin(), vir()];
        assert_eq!(visible(&all, false, None).len(), 1);
        assert_eq!(visible(&all, true, None).len(), 2);
        assert_eq!(visible(&all, false, Some("ca:bh")).len(), 2, "the active device stays visible");
        assert!(seed_priority(&all).iter().all(|k| k.id != "ca:bh"));
    }

    #[test]
    fn seed_order_usb_builtin_bt_other() {
        let other = dev("ca:o", "iPhone Microphone", DeviceKind::Other, false);
        let ids: Vec<_> = seed_priority(&[other, bt(), builtin(), usb()]).into_iter().map(|k| k.id).collect();
        assert_eq!(ids, ["ca:usb", "ca:bi", "ca:bt", "ca:o"]);
    }

    #[test]
    fn promote_and_reorder() {
        let mut mic = MicSettings { priority: vec![k("ca:bi", "b"), k("ca:usb", "u")], ..Default::default() };
        promote(&mut mic, "ca:usb", &[]);
        assert_eq!(mic.priority[0].id, "ca:usb");
        promote(&mut mic, "ca:bt", &[bt()]);
        assert_eq!(mic.priority.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["ca:bt", "ca:usb", "ca:bi"]);
        let r = reorder(&mic.priority, &["ca:bi".into(), "ca:gone".into(), "ca:bi".into()], &[]);
        assert_eq!(r.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["ca:bi", "ca:bt", "ca:usb"]);
    }

    #[test]
    fn resample_length() {
        let s: Vec<f32> = (0..48_000).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5).collect();
        let out = resample(&s, 48_000);
        assert!((out.len() as i64 - 16_000).abs() <= 1, "got {}", out.len());
        let peak = out.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.02, "amplitude kept, got {peak}");
        assert!(resample(&s[..44_100], 44_100).len().abs_diff(16_000) <= 1);
        assert_eq!(resample(&s[..16_000], 16_000).len(), 16_000);
    }

    #[test]
    fn levels_and_gate() {
        assert_eq!(db(0.0), -100.0);
        assert!((db(1.0)).abs() < 1e-6);
        assert!((db(0.5) + 6.02).abs() < 0.01);
        let quiet = Clip { samples: vec![0.0; 8000], duration_ms: 500, peak_db: db(0.001), device_names: vec![] };
        assert!(quiet.silent() && !quiet.too_short());
        let tap = Clip { duration_ms: 200, peak_db: -10.0, ..Default::default() };
        assert!(tap.too_short() && !tap.silent());
    }

    // ---------- adversarial (2026-10-17 review) ----------

    #[test]
    fn same_name_known_device_is_not_the_selected_one() {
        // Two identical mics, both known. The selected one is unplugged: the other is a fallback, not "Selected".
        let (a, b) = (dev("ca:a", "Shure MV7", DeviceKind::Usb, false), dev("ca:b", "Shure MV7", DeviceKind::Usb, false));
        let p = [k("ca:a", "Shure MV7"), k("ca:b", "Shure MV7"), k("ca:bi", "MacBook Pro Microphone")];
        let r = got(resolve(&sel("ca:a"), &p, &[builtin(), b.clone()], None));
        assert_eq!(r, Some(("ca:b".into(), Reason::Fallback { wanted: "Shure MV7".into() })));
        // Both connected: each id gets its own device.
        assert_eq!(got(resolve(&sel("ca:b"), &p, &[a, b, builtin()], None)), Some(("ca:b".into(), Reason::Selected)));
    }

    #[test]
    fn priority_skips_missing_and_virtual_default() {
        let p = [k("ca:gone1", "Old USB"), k("ca:gone2", "Old BT")];
        // nothing in priority is connected, the default is virtual: first BuiltIn
        assert_eq!(got(resolve(&MicSelection::Auto, &p, &[vir(), bt(), builtin()], Some("ca:bh"))), Some(("ca:bi".into(), Reason::Any)));
        // selected device unknown to priority and missing: fallback labels it by id
        assert_eq!(got(resolve(&sel("ca:zzz"), &p, &[bt()], None)), Some(("ca:bt".into(), Reason::Fallback { wanted: "ca:zzz".into() })));
    }

    #[test]
    fn heal_does_not_steal_another_known_id() {
        // ca:a is gone; ca:b (same name) is already known: no heal, both entries stay.
        let b = dev("ca:b", "Shure MV7", DeviceKind::Usb, false);
        let mut mic = MicSettings { selection: sel("ca:a"), priority: vec![k("ca:a", "Shure MV7"), k("ca:b", "Shure MV7")], show_virtual: false };
        assert!(!heal_ids(&mut mic, &[b]));
        assert_eq!(mic.selection, sel("ca:a"));
        assert_eq!(mic.priority.len(), 2);
    }
}
