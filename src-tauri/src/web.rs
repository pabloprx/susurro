//! Browser pages through Apple Events JavaScript (next-build-macos item 1, `.lo/macos-control-research.md` 1).
//! One `osascript -l JavaScript` per read or act runs `web.js` in the active tab: scan returns the visible
//! actionable DOM elements (they become marks), act clicks / focuses / fills one by its data-susurro-id.
//! The JS needs "Allow JavaScript from Apple Events" (one-time, Pablo turns it on). Off = AX, and a one-time guide.

use crate::ax::El;
use crate::contract::{Mark, PtRect};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Read;
use std::time::{Duration, Instant};

pub const JS: &str = include_str!("web.js");
/// Arc hangs on a loading tab until osascript's own 30 s timeout (research 1): always kill at this.
const TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Chromium,
    Safari,
}

/// Chromium browsers: bundle id -> profile folder under ~/Library/Application Support.
const CHROMIUM: [(&str, &str); 4] = [
    ("com.google.Chrome", "Google/Chrome"),
    ("com.microsoft.edgemac", "Microsoft Edge"),
    ("com.brave.Browser", "BraveSoftware/Brave-Browser"),
    ("company.thebrowser.Browser", "Arc/User Data"),
];
const SAFARI: &str = "com.apple.Safari";

pub fn kind(bundle: &str) -> Option<Kind> {
    if bundle == SAFARI {
        return Some(Kind::Safari);
    }
    CHROMIUM.iter().any(|(b, _)| *b == bundle).then_some(Kind::Chromium)
}

/// The JXA source that runs `web.js` with `cmd` in the active tab. Strings go in as JSON literals, never spliced.
pub fn script(kind: Kind, bundle: &str, cmd: &Value) -> String {
    let js = serde_json::to_string(&format!("{JS}({cmd})")).unwrap();
    let app = serde_json::to_string(bundle).unwrap();
    match kind {
        Kind::Chromium => format!("Application({app}).windows[0].activeTab.execute({{javascript: {js}}})"),
        Kind::Safari => format!("var s = Application({app}); s.doJavaScript({js}, {{in: s.windows[0].currentTab}})"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// "Allow JavaScript from Apple Events" is off (Chrome says "not running" then, research 1).
    Off,
    /// macOS Automation denied susurro (-1743): System Settings > Privacy > Automation.
    Denied,
    /// No answer in TIMEOUT (a loading tab, a navigation that started).
    Timeout,
    Other(String),
}

/// Maps a failed osascript run to its cause.
pub fn error_of(stderr: &str) -> Error {
    if stderr.contains("-1743") {
        Error::Denied
    } else {
        Error::Off
    }
}

fn osascript(src: &str) -> Result<String, Error> {
    let mut child = std::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", src])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(e.to_string()))?;
    // read on threads: a big scan fills the pipe and the child would never exit
    let pipe = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let out = pipe(Box::new(child.stdout.take().unwrap()));
    let err = pipe(Box::new(child.stderr.take().unwrap()));
    let until = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout);
            }
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if !status.success() {
        log::info!("web js: {}", err.trim());
        return Err(error_of(&err));
    }
    Ok(out)
}

/// Runs `web.js` with `cmd`. The JS answers a JSON string; `{err}` = the page refused (element gone, secure field).
pub fn call(kind: Kind, bundle: &str, cmd: &Value) -> Result<Value, Error> {
    let t = Instant::now();
    let out = osascript(&script(kind, bundle, cmd))?;
    log::info!("web {}: {} ms", cmd["op"], t.elapsed().as_millis());
    let v: Value = serde_json::from_str(out.trim()).map_err(|e| Error::Other(format!("web.js answer: {e}")))?;
    match v.get("err").and_then(Value::as_str) {
        Some(e) => Err(Error::Other(e.into())),
        None => Ok(v),
    }
}

/// The setting as the browser saved it, read without an Apple Event. Chromium: `browser.allow_javascript_apple_events`
/// in the last used profile (absent = off, research 1). None = not known (Safari, no file): just try.
pub fn js_allowed(bundle: &str) -> Option<bool> {
    let (_, dir) = CHROMIUM.iter().find(|(b, _)| *b == bundle)?;
    let base = std::path::Path::new(&std::env::var("HOME").ok()?).join("Library/Application Support").join(dir);
    let local: Value = std::fs::read_to_string(base.join("Local State")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let profile = local["profile"]["last_used"].as_str().unwrap_or("Default");
    let prefs: Value = serde_json::from_str(&std::fs::read_to_string(base.join(profile).join("Preferences")).ok()?).ok()?;
    Some(allowed_in(&prefs))
}

pub fn allowed_in(prefs: &Value) -> bool {
    prefs["browser"]["allow_javascript_apple_events"].as_bool() == Some(true)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Raw {
    pub id: u32,
    pub role: String,
    pub name: String,
    #[serde(default)]
    pub href: String,
    #[serde(default)]
    pub value: String,
    pub state: Option<String>,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One scan: the page and its elements in CSS px of the viewport.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Scan {
    pub url: String,
    pub title: String,
    pub iw: f64,
    pub ih: f64,
    pub sx: f64,
    pub sy: f64,
    pub ow: f64,
    pub oh: f64,
    pub els: Vec<Raw>,
    /// Visible iframes over a quarter of the viewport: their content is not in this DOM.
    #[serde(default)]
    pub frames: u32,
}

pub fn parse(v: Value) -> Result<Scan, String> {
    serde_json::from_value(v).map_err(|e| format!("web.js scan: {e}"))
}

/// The viewport in screen points from the window metrics (research 1: zoom 100 %, no side panel). The AX web
/// area frame is exact; this is the guess where to look for it, and the fallback.
pub fn guess_viewport(s: &Scan) -> PtRect {
    PtRect { x: s.sx + (s.ow - s.iw).max(0.0) / 2.0, y: s.sy + (s.oh - s.ih).max(0.0), w: s.iw, h: s.ih }
}

/// CSS px of the viewport -> screen points.
pub fn to_screen(r: (f64, f64, f64, f64), vp: PtRect, s: &Scan) -> PtRect {
    let z = if s.iw > 0.0 { vp.w / s.iw } else { 1.0 };
    PtRect { x: vp.x + r.0 * z, y: vp.y + r.1 * z, w: r.2 * z, h: r.3 * z }
}

/// Marks from a scan: numbered 1.. in the JS reading order; `El::h` keeps the DOM id (act finds it by that).
pub fn els(s: &Scan, vp: PtRect) -> Vec<El> {
    s.els
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let rect = to_screen((r.x, r.y, r.w, r.h), vp, s);
            // the top strip of a page is its header, not browser chrome (the case-23 prior skips the toolbar)
            let region = match crate::ax::region_of(rect, vp) { "toolbar" => "header", r => r };
            let mark = Mark { id: i as u32 + 1, rect, name: r.name.clone(), role: r.role.clone(), region: region.into() };
            let some = |t: &str| (!t.is_empty()).then(|| t.to_string());
            El { mark, state: r.state.clone(), h: r.id as usize, url: some(&r.href), value: some(&r.value) }
        })
        .collect()
}

/// The page the marks came from: what an act needs.
#[derive(Debug, Clone)]
pub struct Page {
    pub kind: Kind,
    pub bundle: String,
    pub scan: Scan,
    pub viewport: PtRect,
}

impl Page {
    /// One act on the DOM element `dom_id`, which must still carry `name` (a re-rendered page reuses nodes: a
    /// different name = `gone`, never a click on the wrong row). Ok = where the element is now (screen points, None =
    /// outside the viewport) and whether the page has the keyboard (false = the address bar has it).
    pub fn act(&self, op: &str, dom_id: usize, name: &str) -> Result<(Option<(f64, f64)>, bool), Error> {
        let v = call(self.kind, &self.bundle, &json!({ "op": op, "id": dom_id, "name": name }))?;
        let r = &v["rect"];
        let at = r["w"].as_f64().map(|w| {
            let p = to_screen((r["x"].as_f64().unwrap_or(0.0), r["y"].as_f64().unwrap_or(0.0), w, r["h"].as_f64().unwrap_or(0.0)), self.viewport, &self.scan);
            crate::command::center(p)
        });
        Ok((at.filter(|p| inside(*p, self.viewport)), v["hasFocus"].as_bool().unwrap_or(true)))
    }

    /// The page signature for the click verify (vc-feedback 21, `Desk::sig`): URL, title, DOM size, text, toggles, a
    /// navigation that started. A timeout = the page is navigating: a fresh value (changed). None = cannot tell.
    pub fn sig(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        match call(self.kind, &self.bundle, &json!({ "op": "sig" })) {
            Ok(v) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                v["sig"].as_str()?.hash(&mut h);
                Some(h.finish())
            }
            Err(Error::Timeout) => {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                Some(u64::MAX - N.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
            }
            Err(_) => None,
        }
    }
}

fn inside(p: (f64, f64), r: PtRect) -> bool {
    p.0 >= r.x && p.1 >= r.y && p.0 < r.x + r.w && p.1 < r.y + r.h
}

// ---------- the one-time guide ----------

/// Where Pablo turns the setting on (research 1).
pub fn menu_path(bundle: &str) -> &'static str {
    match bundle {
        SAFARI => "Safari > Settings > Advanced > Show features for web developers, then Developer > Allow JavaScript from Apple Events",
        "com.microsoft.edgemac" => "View (or Tools) > Developer > Allow JavaScript from Apple Events",
        _ => "View > Developer > Allow JavaScript from Apple Events",
    }
}

const GUIDE_PREFIX: &str = "web-guide:";

/// The guide shows once per browser, ever (a line per bundle id in {data_dir}/web-guide.txt).
/// Returns true when it was shown now.
pub fn guide(app: &tauri::AppHandle, bundle: &str, name: &str, why: &Error) -> bool {
    use tauri::Manager;
    let path = app.state::<crate::AppState>().data_dir.join("web-guide.txt");
    let seen = std::fs::read_to_string(&path).unwrap_or_default();
    let key = format!("{bundle}:{why:?}");
    if seen.lines().any(|l| l == key) {
        return false;
    }
    let _ = std::fs::write(&path, format!("{seen}{key}\n"));
    let (text, actions): (String, &[(&str, &str)]) = match why {
        Error::Denied => (format!("Let susurro control {name}: System Settings > Privacy & Security > Automation, turn on {name} under susurro."), &[("automation", "Open Settings"), ("later", "Not now")]),
        _ if bundle == SAFARI => (format!("Faster, exact clicks in {name}: {}.", menu_path(bundle)), &[("later", "OK")]),
        _ => (format!("Faster, exact clicks in {name}: {}. One time.", menu_path(bundle)), &[("open", "Show me"), ("later", "Not now")]),
    };
    crate::session::toast(app, None, &format!("{GUIDE_PREFIX}{bundle}"), crate::contract::ToastKind::Info, text, actions, 20_000);
    true
}

/// Toast Show me / Open Settings. true = the toast was ours.
pub fn handle_toast_action(toast_id: &str, action_id: &str) -> bool {
    let Some(bundle) = toast_id.strip_prefix(GUIDE_PREFIX) else { return false };
    match action_id {
        // off the session thread: open_menu waits up to 2 s for the browser to come to the front
        "open" => {
            let b = bundle.to_string();
            std::thread::spawn(move || open_menu(&b));
        }
        "automation" => {
            let _ = std::process::Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation").status();
        }
        _ => {}
    }
    true
}

/// Brings the browser to the front and opens the menu that holds "Developer". Pablo clicks the setting himself
/// (Chromium treats an app that flips it as an attack, research 1). Not verified on a real browser.
fn open_menu(bundle: &str) {
    let _ = std::process::Command::new("/usr/bin/open").args(["-b", bundle]).status();
    for _ in 0..20 {
        if let Some(f) = crate::paste::frontmost_app().filter(|f| f.bundle_id.as_deref() == Some(bundle)) {
            if !crate::ax::mac::open_menu_with(f.pid, "Developer") {
                log::warn!("web guide: no menu with Developer in {bundle}");
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan() -> Scan {
        parse(json!({
            "url": "https://example.com/", "title": "Example", "iw": 1200.0, "ih": 800.0, "sx": 100.0, "sy": 50.0, "ow": 1200.0, "oh": 888.0, "dpr": 2.0,
            "els": [
                {"id": 7, "role": "link", "name": "Pricing", "href": "https://example.com/pricing", "value": "", "state": null, "x": 10.0, "y": 20.0, "w": 80.0, "h": 20.0},
                {"id": 9, "role": "text field", "name": "Search", "href": "", "value": "hola", "state": null, "x": 300.0, "y": 400.0, "w": 400.0, "h": 30.0},
                {"id": 11, "role": "checkbox", "name": "Remember me", "state": "off", "x": 20.0, "y": 760.0, "w": 16.0, "h": 16.0}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn kinds() {
        assert_eq!(kind("com.microsoft.edgemac"), Some(Kind::Chromium));
        assert_eq!(kind("company.thebrowser.Browser"), Some(Kind::Chromium));
        assert_eq!(kind("com.apple.Safari"), Some(Kind::Safari));
        assert_eq!(kind("com.tinyspeck.slackmacgap"), None);
    }

    #[test]
    fn script_quotes_everything() {
        let s = script(Kind::Chromium, "com.google.Chrome", &json!({"op": "fill", "id": 3, "value": "it's \"quoted\"\n`${x}`"}));
        assert!(s.starts_with("Application(\"com.google.Chrome\").windows[0].activeTab.execute({javascript: \""));
        // the whole page script is one JSON string literal: it parses back to web.js + the call
        let lit = s.trim_start_matches("Application(\"com.google.Chrome\").windows[0].activeTab.execute({javascript: ").trim_end_matches("})");
        let js: String = serde_json::from_str(lit).unwrap();
        assert!(js.starts_with(JS));
        assert!(js.ends_with(r#"({"id":3,"op":"fill","value":"it's \"quoted\"\n`${x}`"})"#), "{}", &js[js.len() - 80..]);
        let s = script(Kind::Safari, "com.apple.Safari", &json!({"op": "scan"}));
        assert!(s.starts_with("var s = Application(\"com.apple.Safari\"); s.doJavaScript(\""));
        assert!(s.ends_with(", {in: s.windows[0].currentTab})"));
    }

    #[test]
    fn errors() {
        assert_eq!(error_of("execution error: Google Chrome is not running. (-600)"), Error::Off);
        assert_eq!(error_of("Executing JavaScript through AppleScript is turned off. (12)"), Error::Off);
        assert_eq!(error_of("Not authorized to send Apple events to Microsoft Edge. (-1743)"), Error::Denied);
    }

    #[test]
    fn prefs() {
        assert!(allowed_in(&json!({"browser": {"allow_javascript_apple_events": true}})));
        assert!(!allowed_in(&json!({"browser": {"allow_javascript_apple_events": false}})));
        assert!(!allowed_in(&json!({"browser": {}})));
    }

    #[test]
    fn marks_in_screen_points() {
        let s = scan();
        // toolbar = outer - inner height; no side frame
        let vp = guess_viewport(&s);
        assert_eq!(vp, PtRect { x: 100.0, y: 138.0, w: 1200.0, h: 800.0 });
        let e = els(&s, vp);
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].mark.id, e[0].h, e[0].mark.name.as_str(), e[0].mark.role.as_str()), (1, 7, "Pricing", "link"));
        assert_eq!(e[0].mark.rect, PtRect { x: 110.0, y: 158.0, w: 80.0, h: 20.0 });
        assert_eq!(e[0].url.as_deref(), Some("https://example.com/pricing"));
        assert_eq!(e[0].mark.region, "header");
        assert_eq!((e[1].mark.region.as_str(), e[1].value.as_deref(), e[1].url.as_deref()), ("main", Some("hola"), None));
        assert_eq!((e[2].mark.region.as_str(), e[2].state.as_deref()), ("bottom", Some("off")));
        // page zoom 150 %: the AX web area is 1200 pt wide for 800 CSS px
        let mut z = s.clone();
        z.iw = 800.0;
        let r = to_screen((10.0, 20.0, 80.0, 20.0), PtRect { x: 0.0, y: 100.0, w: 1200.0, h: 800.0 }, &z);
        assert_eq!(r, PtRect { x: 15.0, y: 130.0, w: 120.0, h: 30.0 });
    }

    #[test]
    fn menu_paths() {
        assert!(menu_path("com.google.Chrome").starts_with("View > Developer"));
        assert!(menu_path("com.apple.Safari").contains("Show features for web developers"));
    }
}
