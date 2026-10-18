//! Voice control: the frontmost app's Accessibility tree -> numbered marks (`.lo/som-experiment/RESULTS.md`).
//! The mac reader builds a plain `Node` tree (one IPC per node, caps on depth and node count); the pure `collect`
//! filters actionable roles, names them, groups them by region and numbers them in reading order.
//! Secure text fields (subrole AXSecureTextField) are skipped with their whole subtree: never read, never marked.

use crate::contract::{Mark, PtRect};

/// Most marks one screen gets (the Jev choice and the marks window stay readable).
pub const MAX_MARKS: usize = 150;
/// The mac walk stops after this many nodes or this depth (an Electron DOM can have tens of thousands).
pub const MAX_NODES: usize = 3000;
pub const MAX_DEPTH: usize = 40;
const NAME_MAX: usize = 60;

/// One AX element as read (attributes the marks need). `h` indexes the reader's element handles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Node {
    pub role: String,
    pub subrole: String,
    pub title: String,
    pub desc: String,
    /// AXValue as text (strings; numbers for checkboxes). Never read for a secure field.
    pub value: String,
    pub help: String,
    pub placeholder: String,
    pub enabled: bool,
    pub frame: Option<PtRect>,
    pub children: Vec<Node>,
    pub h: usize,
    /// AXURL of a link (copy the link without keys).
    pub url: String,
}

/// A mark plus what the Jev criteria and the actions need.
#[derive(Debug, Clone, PartialEq)]
pub struct El {
    pub mark: Mark,
    /// "on" / "off" for checkboxes, switches and radio buttons.
    pub state: Option<String>,
    /// The reader's handle index (AXPress / focus).
    pub h: usize,
    /// AXURL (links).
    pub url: Option<String>,
    /// AXValue of a text field, 200 chars max (never for secure fields: they are not marked).
    pub value: Option<String>,
}

/// The focused element, read right before an action (VOICE_CONTROL_V2 2.1). Ranges are UTF-16 units (AX).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub app: String,
    pub bundle: String,
    pub pid: i32,
    /// AX role ("AXTextField") or "" when nothing answers (Electron without a tree).
    pub role: String,
    pub name: String,
    /// None = not readable. Never read for a secure field.
    pub value: Option<String>,
    /// AXSelectedTextRange (location, length).
    pub sel: Option<(usize, usize)>,
    pub selected: Option<String>,
    /// AXURL of the focused element (a focused link).
    pub url: Option<String>,
    pub secure: bool,
}

/// Value tail sent to the planner.
pub const VALUE_TAIL: usize = 200;

impl Field {
    /// The last `VALUE_TAIL` chars of the value.
    pub fn tail(&self) -> String {
        let v = self.value.as_deref().unwrap_or("");
        let n = v.chars().count();
        v.chars().skip(n.saturating_sub(VALUE_TAIL)).collect()
    }
}

/// AX role -> (label, takes text). Anything else is not actionable.
fn role_label(role: &str, subrole: &str) -> Option<&'static str> {
    Some(match (role, subrole) {
        ("AXCheckBox", "AXSwitch") => "switch",
        ("AXRadioButton", "AXTabButton") => "tab",
        ("AXButton", _) => "button",
        ("AXCheckBox", _) => "checkbox",
        ("AXRadioButton", _) => "radio button",
        ("AXLink", _) => "link",
        ("AXTextField", _) => "text field",
        ("AXTextArea", _) => "text area",
        ("AXComboBox", _) => "combo box",
        ("AXSearchField", _) => "search field",
        ("AXPopUpButton", _) => "pop-up button",
        ("AXMenuButton", _) => "menu button",
        ("AXDisclosureTriangle", _) => "disclosure",
        ("AXSlider", _) => "slider",
        ("AXIncrementor", _) => "stepper",
        ("AXRow", _) => "row",
        ("AXTab", _) => "tab",
        ("AXMenuItem", _) => "menu item",
        _ => return None,
    })
}

pub fn takes_text(role: &str) -> bool {
    matches!(role, "text field" | "text area" | "combo box" | "search field")
}

/// Secure fields are never marked, named or typed into.
pub fn is_secure(n: &Node) -> bool {
    n.subrole == "AXSecureTextField" || n.role == "AXSecureTextField"
}

fn clip(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > NAME_MAX { format!("{}…", s.chars().take(NAME_MAX - 1).collect::<String>()) } else { s }
}

/// The first texts under a node (Electron buttons and rows keep their label in AXStaticText children).
fn child_text(n: &Node, depth: usize, out: &mut Vec<String>) {
    for c in &n.children {
        if out.len() >= 2 || is_secure(c) {
            return;
        }
        let t = match c.role.as_str() {
            "AXStaticText" if !c.value.trim().is_empty() => c.value.trim(),
            "AXImage" if c.title.trim().is_empty() => c.desc.trim(),
            _ => c.title.trim(),
        };
        if !t.is_empty() {
            out.push(t.to_string());
        }
        if depth < 3 {
            child_text(c, depth + 1, out);
        }
    }
}

/// Title, then description, placeholder, help, child texts; the value only as a last resort (it is user content).
pub fn name_of(n: &Node) -> String {
    for s in [&n.title, &n.desc, &n.placeholder, &n.help] {
        if !s.trim().is_empty() {
            return clip(s);
        }
    }
    let mut t = vec![];
    child_text(n, 0, &mut t);
    if !t.is_empty() {
        return clip(&t.join(" "));
    }
    // A checkbox value is 0/1, not a name.
    if matches!(n.role.as_str(), "AXCheckBox" | "AXRadioButton" | "AXSlider") { String::new() } else { clip(&n.value) }
}

fn state_of(n: &Node) -> Option<String> {
    matches!(n.role.as_str(), "AXCheckBox" | "AXRadioButton").then(|| if n.value.trim() == "1" { "on" } else { "off" }.to_string())
}

/// Region by geometry inside the window: toolbar (top strip), bottom strip, sidebar (narrow, left), else main.
pub fn region_of(r: PtRect, win: PtRect) -> &'static str {
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    if cy - win.y < 56.0 && r.h < 60.0 {
        "toolbar"
    } else if win.y + win.h - cy < 72.0 && r.h < 80.0 {
        "bottom"
    } else if cx - win.x < 0.25 * win.w && r.w < 0.3 * win.w {
        "sidebar"
    } else {
        "main"
    }
}

fn overlaps(a: PtRect, b: PtRect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

pub fn same_rect(a: PtRect, b: PtRect) -> bool {
    (a.x - b.x).abs() < 2.0 && (a.y - b.y).abs() < 2.0 && (a.w - b.w).abs() < 2.0 && (a.h - b.h).abs() < 2.0
}

/// Actionable, enabled, named-or-not elements visible in `win`, numbered 1.. in reading order (top to bottom,
/// then left to right). A nested element with the same rect as one already taken is a duplicate.
pub fn collect(root: &Node, win: PtRect) -> Vec<El> {
    fn walk(n: &Node, win: PtRect, depth: usize, out: &mut Vec<El>) {
        if out.len() >= MAX_MARKS || depth > MAX_DEPTH || is_secure(n) {
            return;
        }
        if let (Some(label), Some(r)) = (role_label(&n.role, &n.subrole), n.frame) {
            let visible = r.w >= 4.0 && r.h >= 4.0 && overlaps(r, win);
            if n.enabled && visible && !out.iter().any(|e| same_rect(e.mark.rect, r)) {
                let mark = Mark { id: 0, rect: r, name: name_of(n), role: label.into(), region: region_of(r, win).into() };
                let value = (takes_text(label) && !n.value.is_empty()).then(|| n.value.chars().take(VALUE_TAIL).collect());
                let url = (!n.url.is_empty()).then(|| n.url.clone());
                out.push(El { mark, state: state_of(n), h: n.h, url, value });
            }
        }
        for c in &n.children {
            walk(c, win, depth + 1, out);
        }
    }
    let mut out = vec![];
    walk(root, win, 0, &mut out);
    // Rows within 4 pt count as one line.
    out.sort_by(|a, b| {
        let (ra, rb) = (a.mark.rect, b.mark.rect);
        ((ra.y / 4.0).round(), ra.x).partial_cmp(&((rb.y / 4.0).round(), rb.x)).unwrap()
    });
    for (i, e) in out.iter_mut().enumerate() {
        e.mark.id = i as u32 + 1;
    }
    out
}

/// What Jev reads for one mark: `Send now (button, bottom)`, `Dark mode (switch, main, off)`.
pub fn describe(e: &El) -> String {
    let m = &e.mark;
    let name = if m.name.is_empty() { "(no name)" } else { m.name.as_str() };
    match &e.state {
        Some(s) => format!("{name} ({}, {}, {s})", m.role, m.region),
        None => format!("{name} ({}, {})", m.role, m.region),
    }
}

// ---------- macOS reader ----------

#[cfg(target_os = "macos")]
pub mod mac {
    use super::*;
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementCopyElementAtPosition(el: CFTypeRef, x: f32, y: f32, out: *mut CFTypeRef) -> i32;
        fn AXUIElementSetMessagingTimeout(el: CFTypeRef, secs: f32) -> i32;
        fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFStringRef, out: *mut CFTypeRef) -> i32;
        fn AXUIElementCopyMultipleAttributeValues(el: CFTypeRef, attrs: CFArrayRef, opts: u32, out: *mut CFArrayRef) -> i32;
        fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFStringRef, v: CFTypeRef) -> i32;
        fn AXUIElementPerformAction(el: CFTypeRef, action: CFStringRef) -> i32;
        fn AXValueGetType(v: CFTypeRef) -> u32;
        fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> u8;
        fn AXValueGetTypeID() -> usize;
        fn AXUIElementGetTypeID() -> usize;
    }
    const AX_CGPOINT: u32 = 1;
    const AX_CGSIZE: u32 = 2;

    /// One read of the frontmost window: the tree, the window frame, the element handles (`Node::h`).
    pub struct Tree {
        pub root: Node,
        pub window: PtRect,
        pub handles: Vec<CFType>,
    }

    fn attr(el: &CFType, name: &str) -> Option<CFType> {
        let mut out: CFTypeRef = std::ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(el.as_CFTypeRef(), CFString::new(name).as_concrete_TypeRef(), &mut out) };
        (err == 0 && !out.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(out) })
    }

    fn array(v: &CFType) -> Option<CFArray<CFType>> {
        // SAFETY: the type id says it is a CFArray; get rule = we retain our own reference.
        v.instance_of::<CFArray>().then(|| unsafe { CFArray::wrap_under_get_rule(v.as_CFTypeRef() as CFArrayRef) })
    }

    fn text(v: &CFType) -> String {
        if let Some(s) = v.downcast::<CFString>() {
            return s.to_string();
        }
        if let Some(b) = v.downcast::<CFBoolean>() {
            return if bool::from(b) { "1" } else { "0" }.into();
        }
        v.downcast::<CFNumber>().and_then(|n| n.to_i64()).map(|n| n.to_string()).unwrap_or_default()
    }

    fn url_text(v: &CFType) -> String {
        if let Some(u) = v.downcast::<core_foundation::url::CFURL>() {
            return u.get_string().to_string();
        }
        text(v)
    }

    fn ax_value<T: Default>(v: &CFType, ty: u32) -> Option<T> {
        if v.type_of() as usize != unsafe { AXValueGetTypeID() } || unsafe { AXValueGetType(v.as_CFTypeRef()) } != ty {
            return None;
        }
        let mut out = T::default();
        (unsafe { AXValueGetValue(v.as_CFTypeRef(), ty, &mut out as *mut T as *mut c_void) } != 0).then_some(out)
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct P2 {
        a: f64,
        b: f64,
    }

    const NAMES: [&str; 10] = ["AXRole", "AXSubrole", "AXTitle", "AXDescription", "AXHelp", "AXPlaceholderValue", "AXEnabled", "AXPosition", "AXSize", "AXChildren"];

    struct Walk {
        handles: Vec<CFType>,
        names: CFArray<CFString>,
    }

    impl Walk {
        fn node(&mut self, el: CFType, depth: usize) -> Node {
            let mut out: CFArrayRef = std::ptr::null();
            let err = unsafe { AXUIElementCopyMultipleAttributeValues(el.as_CFTypeRef(), self.names.as_concrete_TypeRef(), 0, &mut out) };
            let h = self.handles.len();
            let mut n = Node { h, enabled: true, ..Default::default() };
            if err != 0 || out.is_null() {
                self.handles.push(el);
                return n;
            }
            let vals: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(out) };
            let get = |i: usize| vals.get(i as isize).map(|v| v.clone());
            let s = |i: usize| get(i).map(|v| text(&v)).unwrap_or_default();
            n.role = s(0);
            n.subrole = s(1);
            if is_secure(&n) {
                // Nothing else is read from a password field, and it gets no handle use.
                self.handles.push(el);
                return n;
            }
            (n.title, n.desc, n.help, n.placeholder) = (s(2), s(3), s(4), s(5));
            n.enabled = get(6).and_then(|v| v.downcast::<CFBoolean>()).is_none_or(bool::from);
            let pos = get(7).and_then(|v| ax_value::<P2>(&v, AX_CGPOINT));
            let size = get(8).and_then(|v| ax_value::<P2>(&v, AX_CGSIZE));
            if let (Some(p), Some(z)) = (pos, size) {
                n.frame = Some(PtRect { x: p.a, y: p.b, w: z.a, h: z.b });
            }
            // The value is user content: read it only where it names or states something.
            if n.role == "AXStaticText" || role_label(&n.role, &n.subrole).is_some() {
                n.value = attr(&el, "AXValue").map(|v| text(&v)).unwrap_or_default();
            }
            if n.role == "AXLink" {
                n.url = attr(&el, "AXURL").map(|v| url_text(&v)).unwrap_or_default();
            }
            let kids = get(9).filter(|_| depth < MAX_DEPTH).and_then(|v| array(&v));
            self.handles.push(el);
            if let Some(kids) = kids {
                let ax_type = unsafe { AXUIElementGetTypeID() };
                for k in kids.iter() {
                    if self.handles.len() >= MAX_NODES {
                        break;
                    }
                    if k.type_of() as usize == ax_type {
                        let child = self.node(k.clone(), depth + 1);
                        n.children.push(child);
                    }
                }
            }
            n
        }
    }

    /// The focused window of app `pid` (else its first window). None = no AX access or no window.
    pub fn read(pid: i32) -> Option<Tree> {
        let app = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateApplication(pid)) };
        unsafe { AXUIElementSetMessagingTimeout(app.as_CFTypeRef(), 0.5) };
        // Chromium / Electron build their AX tree only when asked (Slack, VS Code, Chrome).
        let yes = CFBoolean::true_value();
        unsafe { AXUIElementSetAttributeValue(app.as_CFTypeRef(), CFString::new("AXManualAccessibility").as_concrete_TypeRef(), yes.as_CFTypeRef()) };
        let win = attr(&app, "AXFocusedWindow").or_else(|| {
            attr(&app, "AXWindows").and_then(|w| array(&w)).and_then(|a| a.get(0).map(|v| v.clone()))
        })?;
        let names = CFArray::from_CFTypes(&NAMES.map(CFString::new));
        let mut w = Walk { handles: vec![], names };
        let root = w.node(win, 0);
        let window = root.frame?;
        Some(Tree { root, window, handles: w.handles })
    }

    /// The element at a global point is a password field (VOICE_CONTROL.md: the box fallback checks before a click).
    /// No AX answer = false: the secure input check after the focus still stops a type.
    pub fn secure_at(p: (f64, f64)) -> bool {
        let sys = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let mut out: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyElementAtPosition(sys.as_CFTypeRef(), p.0 as f32, p.1 as f32, &mut out) } != 0 || out.is_null() {
            return false;
        }
        let el = unsafe { CFType::wrap_under_create_rule(out) };
        let s = |a| attr(&el, a).map(|v| text(&v)).unwrap_or_default();
        is_secure(&Node { role: s("AXRole"), subrole: s("AXSubrole"), ..Default::default() })
    }

    /// AXPress. false = the element has no press action (the caller clicks its center instead).
    pub fn press(el: &CFType) -> bool {
        unsafe { AXUIElementPerformAction(el.as_CFTypeRef(), CFString::new("AXPress").as_concrete_TypeRef()) == 0 }
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct CFRange {
        location: isize,
        length: isize,
    }
    const AX_CFRANGE: u32 = 4;

    unsafe extern "C" {
        fn AXUIElementGetPid(el: CFTypeRef, pid: *mut i32) -> i32;
        fn AXValueCreate(ty: u32, ptr: *const c_void) -> CFTypeRef;
    }

    /// The focused element (system-wide) and its handle, for set_selection. None = no AX answer.
    pub fn focused() -> Option<(Field, CFType)> {
        let sys = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        unsafe { AXUIElementSetMessagingTimeout(sys.as_CFTypeRef(), 0.25) };
        let el = attr(&sys, "AXFocusedUIElement")?;
        let s = |a| attr(&el, a).map(|v| text(&v)).unwrap_or_default();
        let mut f = Field { role: s("AXRole"), ..Default::default() };
        let subrole = s("AXSubrole");
        unsafe { AXUIElementGetPid(el.as_CFTypeRef(), &mut f.pid) };
        f.secure = is_secure(&Node { role: f.role.clone(), subrole, ..Default::default() });
        f.name = [s("AXTitle"), s("AXDescription"), s("AXPlaceholderValue")].into_iter().find(|t| !t.trim().is_empty()).unwrap_or_default();
        if f.secure {
            return Some((f, el));
        }
        f.value = attr(&el, "AXValue").and_then(|v| v.downcast::<CFString>()).map(|v| v.to_string());
        f.selected = attr(&el, "AXSelectedText").and_then(|v| v.downcast::<CFString>()).map(|v| v.to_string());
        f.sel = attr(&el, "AXSelectedTextRange").and_then(|v| ax_value::<CFRange>(&v, AX_CFRANGE)).map(|r| (r.location.max(0) as usize, r.length.max(0) as usize));
        f.url = attr(&el, "AXURL").map(|v| url_text(&v)).filter(|u| !u.is_empty());
        Some((f, el))
    }

    /// AXSelectedTextRange = (start, len), UTF-16 units. false = refused (the caller tries the next strategy).
    pub fn set_selection(el: &CFType, start: usize, len: usize) -> bool {
        let r = CFRange { location: start as isize, length: len as isize };
        let v = unsafe { AXValueCreate(AX_CFRANGE, &r as *const _ as *const c_void) };
        if v.is_null() {
            return false;
        }
        let v = unsafe { CFType::wrap_under_create_rule(v) };
        unsafe { AXUIElementSetAttributeValue(el.as_CFTypeRef(), CFString::new("AXSelectedTextRange").as_concrete_TypeRef(), v.as_CFTypeRef()) == 0 }
    }

    /// Keyboard focus on a field (AXFocused = true). false = refused.
    pub fn focus(el: &CFType) -> bool {
        let yes = CFBoolean::true_value();
        unsafe { AXUIElementSetAttributeValue(el.as_CFTypeRef(), CFString::new("AXFocused").as_concrete_TypeRef(), yes.as_CFTypeRef()) == 0 }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn n(role: &str, title: &str, r: (f64, f64, f64, f64), kids: Vec<Node>) -> Node {
        Node { role: role.into(), title: title.into(), enabled: true, frame: Some(PtRect { x: r.0, y: r.1, w: r.2, h: r.3 }), children: kids, ..Default::default() }
    }

    const WIN: PtRect = PtRect { x: 0.0, y: 0.0, w: 1440.0, h: 900.0 };

    fn slack() -> Node {
        let label = |t: &str| Node { role: "AXStaticText".into(), value: t.into(), ..Default::default() };
        let mut pw = n("AXTextField", "Password", (500.0, 300.0, 200.0, 30.0), vec![]);
        pw.subrole = "AXSecureTextField".into();
        pw.value = "hunter2".into();
        let mut dark = n("AXCheckBox", "Dark mode", (900.0, 400.0, 30.0, 18.0), vec![]);
        (dark.subrole, dark.value) = ("AXSwitch".into(), "1".into());
        let mut off = n("AXButton", "Disabled", (900.0, 450.0, 80.0, 24.0), vec![]);
        off.enabled = false;
        let mut desc_only = n("AXButton", "", (1396.0, 10.0, 28.0, 28.0), vec![]);
        desc_only.desc = "Channel details".into();
        n(
            "AXWindow",
            "Slack",
            (0.0, 0.0, 1440.0, 900.0),
            vec![
                n("AXGroup", "", (0.0, 0.0, 320.0, 900.0), vec![
                    n("AXLink", "", (64.0, 120.0, 260.0, 26.0), vec![label("# general")]),
                    n("AXLink", "", (64.0, 146.0, 260.0, 26.0), vec![label("# design")]),
                    // a link inside a row with the same rect: one mark
                    n("AXRow", "", (64.0, 172.0, 260.0, 26.0), vec![n("AXLink", "# random", (64.0, 172.0, 260.0, 26.0), vec![])]),
                ]),
                desc_only,
                pw,
                dark,
                off,
                n("AXStaticText", "Hello", (400.0, 200.0, 300.0, 20.0), vec![]),
                n("AXButton", "Off screen", (2000.0, 100.0, 40.0, 40.0), vec![]),
                n("AXButton", "Send now", (1380.0, 850.0, 32.0, 32.0), vec![]),
                n("AXTextField", "", (400.0, 840.0, 900.0, 44.0), vec![Node { role: "AXStaticText".into(), value: "Message #general".into(), ..Default::default() }]),
            ],
        )
    }

    #[test]
    fn maps_the_tree_to_marks() {
        let els = collect(&slack(), WIN);
        let got: Vec<String> = els.iter().map(|e| format!("{} {}", e.mark.id, describe(e))).collect();
        assert_eq!(
            got,
            [
                "1 Channel details (button, toolbar)",
                "2 # general (link, sidebar)",
                "3 # design (link, sidebar)",
                "4 # random (row, sidebar)",
                "5 Dark mode (switch, main, on)",
                "6 Message #general (text field, bottom)",
                "7 Send now (button, bottom)",
            ]
        );
    }

    #[test]
    fn secure_fields_are_never_marked_or_named() {
        let mut tree = slack();
        // A secure field whose child text would name its parent: still skipped, and its text never leaks.
        let mut pw = n("AXTextField", "", (10.0, 10.0, 100.0, 20.0), vec![]);
        pw.subrole = "AXSecureTextField".into();
        tree.children.push(n("AXButton", "", (10.0, 10.0, 100.0, 20.0), vec![pw]));
        let els = collect(&tree, WIN);
        assert!(els.iter().all(|e| e.mark.name != "Password" && !e.mark.name.contains("hunter2")));
        assert!(!els.iter().any(|e| e.mark.rect.y == 300.0));
    }

    #[test]
    fn caps_at_150_marks() {
        let kids = (0..400).map(|i| n("AXButton", &format!("b{i}"), (10.0 + (i % 20) as f64 * 60.0, 60.0 + (i / 20) as f64 * 30.0, 50.0, 20.0), vec![])).collect();
        let els = collect(&n("AXWindow", "", (0.0, 0.0, 1440.0, 900.0), kids), WIN);
        assert_eq!(els.len(), MAX_MARKS);
        assert_eq!(els.last().unwrap().mark.id, 150);
    }

    #[test]
    fn regions_by_geometry() {
        let r = |x, y, w, h| region_of(PtRect { x, y, w, h }, WIN);
        assert_eq!(r(1300.0, 10.0, 28.0, 28.0), "toolbar");
        assert_eq!(r(400.0, 850.0, 600.0, 40.0), "bottom");
        assert_eq!(r(64.0, 300.0, 260.0, 26.0), "sidebar");
        assert_eq!(r(500.0, 400.0, 400.0, 30.0), "main");
        // a window on a display left of the main one
        let w2 = PtRect { x: -1920.0, y: 0.0, w: 1920.0, h: 1080.0 };
        assert_eq!(region_of(PtRect { x: -1900.0, y: 400.0, w: 200.0, h: 30.0 }, w2), "sidebar");
    }

    #[test]
    fn names_fall_back_in_order() {
        let mut b = n("AXButton", "", (0.0, 0.0, 10.0, 10.0), vec![]);
        b.help = "Opens settings".into();
        assert_eq!(name_of(&b), "Opens settings");
        b.placeholder = "Search".into();
        assert_eq!(name_of(&b), "Search");
        let long = n("AXButton", &"x".repeat(100), (0.0, 0.0, 10.0, 10.0), vec![]);
        assert_eq!(name_of(&long).chars().count(), NAME_MAX);
        let mut cb = n("AXCheckBox", "", (0.0, 0.0, 10.0, 10.0), vec![]);
        cb.value = "0".into();
        assert_eq!((name_of(&cb).as_str(), state_of(&cb).as_deref()), ("", Some("off")));
    }
}
