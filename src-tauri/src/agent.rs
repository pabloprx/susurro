//! Agent mode v2 (Pablo 2026-10-18: "leave agent mode always enabled; make it more responsive; it repeats / forgets
//! what it already did; it must control my mouse"). Every voice order goes to his local Claude Code (`claude -p`, his
//! subscription), except the trivial ones (`local_intent`: no LLM at all). ONE warm `claude -p --input-format
//! stream-json` process per binary: spawned when F11 goes down (its start overlaps his speech), each order a new user
//! message in the same conversation (it remembers what it did), killed after IDLE, on Esc and at quit. The session
//! ledger (agent-memory.json: each order, how it ended and its actions) goes with an order to any conversation that
//! has not seen those orders yet: a new process after Esc, a stop or the idle kill, and the OpenRouter fallback. So
//! a "continue" after Esc does not redo what the stopped order did (Pablo 2026-10-18: "it has issues on things that
//! he already did").
//! susurro serves its own tools over MCP (HTTP on 127.0.0.1, a random token per process); every tool call runs on the
//! exec thread through `command::exec` (verify + real-click fallback, risky confirm, password fields blocked) or the
//! raw mouse acts (drag, double / right click, move). Every action answers with what changed and the new marks, so
//! the model rarely needs a screenshot. No claude binary, or claude fails before its first tool call (not logged
//! in): the same tools over OpenRouter function calling.

use crate::command::{self, Act, Ask, Cfg, Desk, End, Memory, Screen, Target, Wait};
use crate::contract::{CmdStatus, CommandSettings, CommandStep, PtRect};
use crate::planner::{Action, Plan};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Actions per order (screenshots, list_elements and wait do not count).
pub const STEP_CAP: u32 = 25;
pub const TIMEOUT: Duration = Duration::from_secs(300);
const WAIT_MAX_MS: u64 = 5000;
/// wait reads the screen this often, and returns once it is the same twice.
const WAIT_POLL_MS: u64 = 250;
/// A screen that never changed counts as settled after this.
const WAIT_MIN_MS: u64 = 750;
/// An MCP tool call waits this long for the exec thread (a risky confirm alone waits 10 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_MAX: usize = 1 << 20;
const PROTOCOL: &str = "2025-06-18";
/// The OpenRouter fallback's request timeout.
const OR_TIMEOUT: Duration = Duration::from_secs(60);
/// The agent's screenshots: the long side in pixels (Pablo: <= 1280).
pub const IMG_MAX: f64 = 1280.0;
/// Marks listed with an order or an action answer (the ones for the order: `pick`); list_elements lists them all.
/// 80 -> 40 and one line per region: the order's message 4.7k -> 2.4k chars on a 120-mark window (smoke).
pub const MARKS_MAX: usize = 40;
/// A warm claude process with no order for this long is killed.
pub const IDLE: Duration = Duration::from_secs(300);
/// Orders kept in the ledger file (agent-memory.json).
pub const MEMORY_MAX: usize = 30;
/// Orders a conversation gets from the ledger, the newest; actions per order.
const LEDGER_ORDERS: usize = 12;
const LEDGER_STEPS: usize = 8;
/// A pause longer than this between two orders starts a new session: the orders before it are not sent (an action
/// from this morning is no reason to skip one now).
pub const SESSION_GAP_MS: i64 = 30 * 60 * 1000;
/// After an action, before the screen is read again (exec's own settle already covers a click).
const AFTER_MS: u64 = 120;

pub const SYSTEM: &str = "You control the user's Mac with the susurro tools, and only with them. The user talks; each \
user message is one spoken order plus the screen as it is now: the front app, the window, the focus and the numbered \
marks (the clickable elements). \
Act at once: no plan in prose, no questions. Take a screenshot only to see pixels (a canvas, an image, colors, where \
to draw); the marks and the window title are faster, never a screenshot just to look around. \
Do every part of the order, in its order, even when the screen seems to show the result already: \"a new tab\" is \
cmd+t even when the page is open in this tab. \
If the app the order names is already the front app (the App: line), do not call open_app: act in it. \
Web orders: a site you know (YouTube, Gmail, ChatGPT, OpenRouter, GitHub...) is a URL, never a search: cmd+l (cmd+t \
first when the order says a new tab), type its address (youtube.com, chatgpt.com, openrouter.ai/keys), return. \
\"Search X on Google\" or \"busca X\": type google.com/search?q=X in the address bar; \"search X on YouTube\": \
youtube.com/results?search_query=X. Do the whole task the user means, not only the first step: \"ask ChatGPT about \
X\" or \"talk to ChatGPT\" = open chatgpt.com, start a new chat, type the question, return, wait for the answer, \
read it, and put a one-sentence summary of it in done. To read or review a page, look at it (scroll if needed) and \
say what you found in done. \
Prefer keyboard shortcuts (cmd+t new tab, cmd+l address bar, cmd+f find, cmd+w, return, tab, escape) over clicks, and \
click {mark} over x,y. Click a text field before you type into it. \
Every action answers with what changed (app, window, focus, screen changed) and the new marks: trust it, use those \
mark numbers, and never redo a step whose answer already shows it worked. \
In Finder (and in an Open or Save dialog) go to a folder with cmd+shift+g, type its path (~/name), return; never \
search the Mac with cmd+f and never guess click points. To create a folder that may exist already, first \
cmd+shift+g to its path: if it opens, it exists, so work inside it; else go to the parent and cmd+shift+n. \
An action answers with the screen after it settles: do not call wait after it unless that answer shows the page \
still loading; wait returns as soon as the screen stops changing. \
To draw a straight line in a paint app, pick its Line tool (a mark) when it has one, then drag. \
To draw, move or resize (a paint app, a slider, a file onto a folder) use drag with a path in screenshot pixels: take \
a screenshot first to see where. The mouse tools (drag, double_click, right_click, mouse_move, click with modifiers) \
act on the real mouse. \
Earlier orders in this conversation, and the ones listed under \"Done earlier in this session\", are done: never \
repeat their actions (a stopped order stopped after the actions listed), unless the user asks again; use them for \
\"it\", \"again\", \"the same\", \"continue\". \
Risky actions (send, delete, buy) ask the user first; if a tool says the user said no, stop. Never type passwords. \
When the last action of an order finishes it, give that action done: \"<one short sentence>\" (it saves a round \
trip); else, when the order is done or cannot be done, call done. The sentence is in the language of the order.";

/// The system prompt for computer use (CommandSettings::agent_vision): Claude's computer-use way, over susurro's tools.
pub const SYSTEM_VISION: &str = "You control the user's Mac with the susurro tools, and only with them, the way computer \
use works: you see the screen and point at it. The user talks; each user message is one spoken order, the front app, \
the window, the focus, and a screenshot of the whole screen. \
Act at once: no plan in prose, no questions. Every action answers with a new screenshot of the whole screen: look at \
it before the next action and never redo a step it shows worked. Coordinates are x,y in the pixels of the last \
screenshot; aim at the center of the element. \
Do every part of the order, in its order, even when the screen seems to show the result already: \"a new tab\" is \
cmd+t even when the page is open in this tab. \
If the app the order names is already the front app (the App: line), do not call open_app: act in it. \
Web orders: a site you know (YouTube, Gmail, ChatGPT, OpenRouter, GitHub...) is a URL, never a search: cmd+l (cmd+t \
first when the order says a new tab), type its address (youtube.com, chatgpt.com, openrouter.ai/keys), return. \
\"Search X on Google\" or \"busca X\": type google.com/search?q=X in the address bar; \"search X on YouTube\": \
youtube.com/results?search_query=X. Do the whole task the user means, not only the first step: \"ask ChatGPT about \
X\" or \"talk to ChatGPT\" = open chatgpt.com, start a new chat, type the question, return, wait for the answer, \
read it, and put a one-sentence summary of it in done. To read or review a page, look at it (scroll if needed) and \
say what you found in done. \
Prefer keyboard shortcuts (cmd+t new tab, cmd+l address bar, cmd+f find, cmd+w, return, tab, escape) over clicks. \
Click a text field before you type into it. \
In Finder (and in an Open or Save dialog) go to a folder with cmd+shift+g, type its path (~/name), return. \
If a page is still loading in the screenshot, call wait. \
To draw, move or resize (a paint app, a slider, a file onto a folder) use drag with a path in screenshot pixels. \
Earlier orders in this conversation are done: never repeat them; use them for \"it\", \"again\", \"the same\". \
Risky actions (send, delete, buy) ask the user first; if a tool says the user said no, stop. Never type passwords. \
When the last action of an order finishes it, give that action done: \"<one short sentence>\" (it saves a round \
trip); else, when the order is done or cannot be done, call done. The sentence is in the language of the order.";

/// The system prompt for the mode.
pub fn system_for(vision: bool) -> &'static str {
    if vision { SYSTEM_VISION } else { SYSTEM }
}

/// "modo agente: abre WhatsApp" -> Some("abre WhatsApp"); "modo agente" alone -> Some(""). None = not agent mode.
pub fn task_in(text: &str) -> Option<String> {
    let w: Vec<&str> = text.split_whitespace().collect();
    if w.len() < 2 {
        return None;
    }
    let head: String = crate::text::norm(&format!("{} {}", w[0], w[1])).chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect();
    ["modo agente", "agent mode"]
        .contains(&head.as_str())
        .then(|| w[2..].join(" ").trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace()).to_string())
}

/// The live session's plan for an agent order: one `agent` action with the task, held (rest = the whole buffer) until
/// a pause or the release, so the task can span chunks. An empty task waits for the words.
pub fn hold_plan(task: &str, text: &str, last: bool) -> Plan {
    let actions = if task.trim().is_empty() { vec![] } else { vec![Action { op: "agent".into(), text: Some(task.trim().to_string()), ..Default::default() }] };
    Plan { complete: last, rest: if last { String::new() } else { text.to_string() }, actions, ..Default::default() }
}

/// "modo agente: <task>" (Fast mode). None = not agent mode.
pub fn plan(text: &str, last: bool) -> Option<Plan> {
    Some(hold_plan(&task_in(text)?, text, last))
}

// ---------- the trivial orders: no LLM ----------

/// norm, punctuation out, "por favor" / "please" off the ends.
fn plain(text: &str) -> String {
    let n = crate::text::norm(text);
    let mut s = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    for p in ["por favor ", "please "] {
        s = s.strip_prefix(p).map(String::from).unwrap_or(s);
    }
    for p in [" por favor", " please"] {
        s = s.strip_suffix(p).map(String::from).unwrap_or(s);
    }
    s
}

/// "stop", "para", "detente" said alone: the run stops like Esc.
pub fn is_stop(text: &str) -> bool {
    ["stop", "para", "parar", "paralo", "detente", "alto", "basta", "cancela", "cancelar", "stop it", "para ya"].contains(&plain(text).as_str())
}

/// A spoken key name -> a paste::parse_combo name.
fn key_name(s: &str) -> Option<&'static str> {
    Some(match s {
        "enter" | "intro" | "return" | "retorno" | "entrar" => "return",
        "escape" | "esc" | "escapar" => "escape",
        "tab" | "tabulador" => "tab",
        "espacio" | "space" | "barra espaciadora" | "spacebar" => "space",
        "borrar" | "delete" | "backspace" | "retroceso" => "delete",
        "flecha arriba" | "arriba" | "up" | "up arrow" => "up",
        "flecha abajo" | "abajo" | "down" | "down arrow" => "down",
        "flecha izquierda" | "izquierda" | "left" | "left arrow" => "left",
        "flecha derecha" | "derecha" | "right" | "right arrow" => "right",
        _ => return None,
    })
}

/// The installed app a spoken name surely means: the same name, a whole word of it, an alias, or a typo of it. A bare
/// prefix ("chat" -> ChatGPT, "menu" -> MenuMeters) is not sure: the agent reads it.
fn sure_app(spoken: &str, apps: &[String]) -> Option<String> {
    let got = command::match_app(spoken, apps)?;
    let sq = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let want = sq(spoken);
    let alias = command::APP_ALIASES.iter().any(|(a, _)| sq(a) == want);
    // ponytail: "2 letters missing at most" is a guess ("whatsap" yes, "chat" -> ChatGPT no)
    let prefix_only = sq(&got).starts_with(&want) && sq(&got).len() > want.len() + 2 && !got.split(|c: char| !c.is_alphanumeric()).any(|w| sq(w) == want);
    (alias || !prefix_only).then_some(got)
}

/// The orders susurro runs at once without any LLM (Pablo: "trivial local intents executed instantly"): open or
/// switch to an app by name, new tab, paste / copy the last transcript, scroll up / down, press a named key. "stop" is
/// `is_stop`. None = the agent takes it.
pub fn local_intent(text: &str, apps: &[String]) -> Option<Action> {
    let s = plain(text);
    let a = |op: &str| Action { op: op.into(), ..Default::default() };
    let key = |k: &str| Action { keys: Some(k.into()), ..a("key") };
    let scroll = |d: &str| Action { dir: Some(d.into()), ..a("scroll") };
    let one_of = |l: &[&str]| l.contains(&s.as_str());
    if one_of(&["nueva pestana", "abre una nueva pestana", "abre una pestana nueva", "abre nueva pestana", "abre otra pestana", "otra pestana", "pestana nueva", "new tab", "open a new tab", "open new tab"]) {
        return Some(key("cmd+t"));
    }
    if one_of(&["baja", "bajar", "mas abajo", "baja la pagina", "scroll down", "scroll abajo", "desplazate abajo", "desplaza abajo", "haz scroll abajo", "haz scroll hacia abajo", "baja un poco"]) {
        return Some(scroll("down"));
    }
    if one_of(&["sube", "subir", "mas arriba", "sube la pagina", "scroll up", "scroll arriba", "desplazate arriba", "desplaza arriba", "haz scroll arriba", "haz scroll hacia arriba", "sube un poco"]) {
        return Some(scroll("up"));
    }
    if one_of(&["pega lo ultimo", "pegar lo ultimo", "pega el ultimo", "pega la ultima", "pega lo que dije", "paste last", "paste the last one", "paste the last"]) {
        return Some(a("paste_last"));
    }
    if one_of(&["copia lo ultimo", "copiar lo ultimo", "copia el ultimo", "copia la ultima", "copy last", "copy the last one", "copy the last"]) {
        return Some(a("copy_last"));
    }
    for verb in ["presiona la tecla", "pulsa la tecla", "aprieta la tecla", "presiona", "pulsa", "aprieta", "dale a", "press the", "press", "hit"] {
        if let Some(k) = s.strip_prefix(&format!("{verb} ")).and_then(|r| key_name(r.trim_end_matches(" key"))) {
            return Some(key(k));
        }
    }
    // a bare key name, the unambiguous ones only ("abajo" alone is a scroll)
    if one_of(&["enter", "intro", "escape", "esc", "tab", "tabulador"]) {
        return key_name(&s).map(key);
    }
    for verb in ["abre la app", "abre la aplicacion", "abre", "abrir", "open", "ve a", "vete a", "ir a", "anda a", "cambia a", "cambiate a", "switch to", "go to", "pasa a", "muestrame", "muestra", "show"] {
        let Some(rest) = s.strip_prefix(&format!("{verb} ")) else { continue };
        let rest = ["el ", "la ", "the ", "app ", "aplicacion "].iter().fold(rest, |r, p| r.strip_prefix(p).unwrap_or(r));
        if (1..=3).contains(&rest.split_whitespace().count()) && let Some(app) = sure_app(rest, apps) {
            return Some(Action { app: Some(app), ..a("open_app") });
        }
    }
    None
}

// ---------- the tools ----------

/// The tool schema: MCP `tools/list`, and the OpenRouter fallback's functions.
pub fn tools() -> Value {
    let mark = json!({ "type": "integer", "description": "A mark number from the last answer, screenshot or list_elements" });
    let xy = |what: &str| json!({ "type": "number", "description": format!("{what} in screenshot pixels") });
    let mods = json!({ "type": "string", "description": "Modifier keys held: cmd, shift, opt, ctrl, joined with + (cmd+shift)" });
    let at = json!({ "type": "object", "properties": { "mark": mark, "x": xy("x"), "y": xy("y"), "modifiers": mods } });
    let none = json!({ "type": "object", "properties": {} });
    let done = json!({ "type": "string", "description": "If this action finishes the order: the short sentence for the user (no done call needed)" });
    let mut t = json!([
        { "name": "screenshot", "description": "The front window as a JPEG (at most 1280 px) with the numbered marks drawn on it, and the marks as text. Only to see pixels: a canvas, an image, colors, where to draw.", "inputSchema": none },
        { "name": "list_elements", "description": "All the marks of the front window as text, one line per region: region: [N] name (role) · ... Faster than a screenshot.", "inputSchema": none },
        { "name": "click", "description": "Click a mark (preferred), or a point x,y in screenshot pixels. With modifiers it is a raw click (cmd+click opens in a new tab).", "inputSchema": at },
        { "name": "double_click", "description": "Double-click a mark or a point x,y (open a file, select a word).", "inputSchema": at },
        { "name": "right_click", "description": "Right-click a mark or a point x,y (the context menu).", "inputSchema": at },
        { "name": "mouse_move", "description": "Move the mouse to a mark or a point x,y (hover menus, tooltips).", "inputSchema": at },
        { "name": "drag", "description": "Press the left button at the first point, move through every point, release at the last: draw a line (2 points), a shape (a closed path), move or resize things. Points in screenshot pixels: take a screenshot first.", "inputSchema": { "type": "object", "properties": {
            "path": { "type": "array", "minItems": 2, "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 }, "description": "[[x,y], [x,y], ...]" },
            "modifiers": mods
        }, "required": ["path"] } },
        { "name": "type", "description": "Type text into the focused field. Click the field first.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } },
        { "name": "key", "description": "Press a key or a combo: return, escape, tab, down, cmd+t, cmd+shift+n. hold_ms holds it down (a game, a long press).", "inputSchema": { "type": "object", "properties": { "combo": { "type": "string" }, "hold_ms": { "type": "integer" } }, "required": ["combo"] } },
        { "name": "scroll", "description": "Scroll the front window, or at a mark, or at a point x,y.", "inputSchema": { "type": "object", "properties": { "dir": { "type": "string", "enum": ["up", "down"] }, "mark": mark, "x": xy("x"), "y": xy("y") }, "required": ["dir"] } },
        { "name": "open_app", "description": "Open or bring an app to the front by its name.", "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] } },
        { "name": "wait", "description": "Wait for a page load: returns as soon as the screen stops changing, at most ms (max 5000), with the new marks.", "inputSchema": { "type": "object", "properties": { "ms": { "type": "integer" } }, "required": ["ms"] } },
        { "name": "done", "description": "The order is finished, or cannot be done: one short sentence for the user, in the language of the order.", "inputSchema": { "type": "object", "properties": { "summary": { "type": "string" } }, "required": ["summary"] } }
    ]);
    // every action can end the order itself (one model round trip less: ~1.5 s measured per round trip on sonnet)
    for x in t.as_array_mut().unwrap() {
        if !["screenshot", "list_elements", "wait", "done"].contains(&x["name"].as_str().unwrap_or("")) {
            x["inputSchema"]["properties"]["done"] = done.clone();
        }
    }
    t
}

/// The tools for computer use: the same handlers, with no marks. Every answer carries a screenshot of the whole screen.
pub fn vision_tools() -> Value {
    let mut t = tools();
    let v = t.as_array_mut().unwrap();
    v.retain(|x| x["name"] != "list_elements");
    for x in v.iter_mut() {
        if let Some(p) = x["inputSchema"]["properties"].as_object_mut() {
            p.remove("mark");
        }
        let d = match x["name"].as_str().unwrap_or("") {
            "screenshot" => "The whole screen as a JPEG (at most 1280 px). Every other tool already answers with one: call it only when you need a fresh look without acting.",
            "click" => "Click the point x,y (screenshot pixels). With modifiers it is a raw click (cmd+click opens in a new tab).",
            "double_click" => "Double-click the point x,y (open a file, select a word).",
            "right_click" => "Right-click the point x,y (the context menu).",
            "mouse_move" => "Move the mouse to the point x,y (hover menus, tooltips).",
            "drag" => "Press the left button at the first point, move through every point, release at the last: draw a line (2 points), a shape (a closed path), move or resize things. Points in screenshot pixels.",
            "scroll" => "Scroll the front window, or at the point x,y.",
            "wait" => "Wait for a page load: returns as soon as the screen stops changing, at most ms (max 5000), with a screenshot.",
            _ => continue,
        };
        x["description"] = json!(d);
    }
    t
}

/// The tool list for the mode.
pub fn tools_for(vision: bool) -> Value {
    if vision { vision_tools() } else { tools() }
}

/// One tool result: text, an optional JPEG.
#[derive(Debug, Default, PartialEq)]
pub struct Out {
    pub text: String,
    pub image: Option<Vec<u8>>,
    pub error: bool,
}

impl Out {
    fn ok(t: impl Into<String>) -> Self {
        Out { text: t.into(), ..Default::default() }
    }
    fn err(t: impl Into<String>) -> Self {
        Out { text: t.into(), error: true, ..Default::default() }
    }
    /// The MCP `tools/call` result.
    pub fn mcp(&self) -> Value {
        use base64::Engine;
        let mut content = vec![json!({ "type": "text", "text": self.text })];
        if let Some(b) = &self.image {
            content.push(json!({ "type": "image", "mimeType": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(b) }));
        }
        json!({ "content": content, "isError": self.error })
    }
}

/// The screenshot size `snapshot(r, IMG_MAX)` returns: sips fits the longest side to IMG_MAX (it also scales up).
pub fn img_size(r: PtRect) -> (f64, f64) {
    let k = IMG_MAX / r.w.max(r.h).max(1.0);
    ((r.w * k).round(), (r.h * k).round())
}

/// Screenshot pixels -> global points.
pub fn to_points(r: PtRect, px: (f64, f64)) -> (f64, f64) {
    let k = r.w.max(r.h).max(1.0) / IMG_MAX;
    (r.x + px.0 * k, r.y + px.1 * k)
}

/// "cmd+shift" -> CGEventFlags bits.
pub fn modifiers(s: &str) -> Result<u64, String> {
    s.split('+').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).try_fold(0, |f, p| {
        Ok(f | match p.as_str() {
            "cmd" | "command" | "meta" => crate::paste::CMD,
            "shift" => crate::paste::SHIFT,
            "opt" | "option" | "alt" => crate::paste::OPT,
            "ctrl" | "control" => crate::paste::CTRL,
            _ => return Err(format!("unknown modifier {p}")),
        })
    })
}

/// One action of an order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub at: i64,
    pub app: String,
    pub did: String,
    pub ok: bool,
}

/// One order in the session ledger: the words, how it ended, its actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    /// When it started (ms): also its id.
    pub at: i64,
    pub said: String,
    /// "done", "stopped", "not finished: <why>".
    pub end: String,
    pub steps: Vec<Step>,
}

/// The ledger file. The old format (a list of actions) reads as empty.
pub fn load_memory(p: &Path) -> Vec<Order> {
    std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Appends `order`, keeps the last MEMORY_MAX. An order that did nothing is not kept: there is nothing to redo.
pub fn save_order(p: &Path, order: Order) {
    if order.steps.is_empty() {
        return;
    }
    let mut all = load_memory(p);
    all.push(order);
    all.drain(..all.len().saturating_sub(MEMORY_MAX));
    if let Err(e) = std::fs::write(p, serde_json::to_vec(&all).unwrap_or_default()) {
        log::warn!("agent: memory {}: {e}", p.display());
    }
}

/// The orders of the session going on at `now`: back from the newest until a pause over SESSION_GAP_MS.
pub fn session(all: &[Order], now: i64) -> Vec<Order> {
    let mut next = now;
    let from = all.iter().rposition(|o| {
        let gap = next - o.at;
        next = o.at;
        gap > SESSION_GAP_MS
    });
    all[from.map_or(0, |i| i + 1)..].to_vec()
}

/// How an order ended, for the ledger.
pub fn outcome(end: &End) -> String {
    match (&end.status, end.message.as_deref().filter(|m| !m.is_empty())) {
        (CmdStatus::Done, _) => "done".into(),
        (CmdStatus::Stopped, Some(m)) => format!("stopped: {m}"),
        (CmdStatus::Stopped, None) => "stopped".into(),
        (s, m) => format!("not finished: {}", m.map(String::from).unwrap_or_else(|| format!("{s:?}").to_lowercase())),
    }
}

/// The orders a conversation has not seen, as a block before its order. Empty = nothing to tell.
pub fn ledger_text(orders: &[Order], now: i64) -> String {
    let orders = &orders[orders.len().saturating_sub(LEDGER_ORDERS)..];
    if orders.is_empty() {
        return String::new();
    }
    let ago = |at: i64| match (now - at).max(0) / 1000 {
        s if s < 60 => format!("{s} s ago"),
        s => format!("{} min ago", s / 60),
    };
    let lines: Vec<String> = orders
        .iter()
        .map(|o| {
            let steps = &o.steps[o.steps.len().saturating_sub(LEDGER_STEPS)..];
            let did: Vec<String> = steps.iter().map(|s| format!("[{}] {}{}", s.app, s.did, if s.ok { "" } else { " (failed)" })).collect();
            let more = if steps.len() < o.steps.len() { format!("{} earlier actions, then ", o.steps.len() - steps.len()) } else { String::new() };
            format!("- {}: “{}” ({}): {more}{}", ago(o.at), o.said.trim(), o.end, did.join("; "))
        })
        .collect();
    format!("Done earlier in this session (oldest first; these actions happened, do not redo them):\n{}\n\n", lines.join("\n"))
}

/// GUI round 2: "abre una pestaña nueva, ve a jspaint.app" from Finder opened Safari, not Pablo's Edge.
pub fn browser_note(browser: Option<String>) -> String {
    browser.map(|b| format!("\n\nThe user's web browser is {b}: a web order that names no browser goes there.")).unwrap_or_default()
}

/// The app that opens https links, by name ("Microsoft Edge").
#[cfg(target_os = "macos")]
pub fn default_browser() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str("https://example.com"))?;
    let app = NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&url)?;
    Some(app.lastPathComponent()?.to_string().trim_end_matches(".app").to_string())
}
#[cfg(not(target_os = "macos"))]
pub fn default_browser() -> Option<String> {
    None
}

/// The front window's state in one line, for the model.
fn state_line(s: &Screen, focus: &crate::ax::Field, changed: Option<bool>, vision: bool) -> String {
    let (w, h) = img_size(frame(s, vision));
    let mut l = format!("App: {}.", s.app);
    if !s.title.trim().is_empty() {
        l += &format!(" Window: “{}”.", s.title.trim());
    }
    let role = focus.role.trim_start_matches("AX");
    if !role.is_empty() {
        l += &format!(" Focus: {role}{}.", if focus.name.is_empty() { String::new() } else { format!(" “{}”", focus.name) });
    }
    if let Some(c) = changed {
        l += if c { " Screen changed: yes." } else { " Screen changed: no (a drawing on a canvas does not show here)." };
    }
    l + &format!(" Screenshot frame: {w}x{h} px.")
}

/// The marks most useful for `task`, at most `max`, in reading order: first the ones whose name or role shares a
/// word with the order (a rare word counts more: "the" is in every mail row, "Send" in one button), then the page
/// (main region), then the rest (toolbar, sidebar, bottom).
fn pick<'e>(els: &'e [crate::ax::El], task: &str, max: usize) -> Vec<&'e crate::ax::El> {
    let t = plain(task);
    let words: Vec<&str> = t.split(' ').filter(|w| w.chars().count() >= 3 || w.chars().all(|c| c.is_ascii_digit())).collect();
    let names: Vec<String> = els.iter().map(|e| format!(" {} ", plain(&format!("{} {}", e.mark.name, e.mark.role)))).collect();
    let has = |n: &str, w: &str| n.contains(&format!(" {w} "));
    let df: Vec<usize> = words.iter().map(|w| names.iter().filter(|n| has(n, w)).count()).collect();
    let mut v: Vec<(usize, &crate::ax::El)> = els.iter().enumerate().collect();
    v.sort_by_cached_key(|(i, e)| {
        let score: f64 = words.iter().zip(&df).filter(|(w, _)| has(&names[*i], w)).map(|(_, d)| 1.0 / *d as f64).sum();
        (std::cmp::Reverse((score * 1e6) as u64), e.mark.region != "main")
    });
    let mut v: Vec<&crate::ax::El> = v.into_iter().map(|(_, e)| e).collect();
    v.truncate(max);
    v.sort_by_key(|e| e.mark.id);
    v
}

/// One line per region: "main: [1] Message (text field) · [2] Send (button)".
fn mark_lines(els: &[&crate::ax::El]) -> String {
    let mut regions: Vec<(&str, Vec<String>)> = vec![];
    for e in els {
        let m = &e.mark;
        let name = if m.name.is_empty() { "(no name)" } else { m.name.as_str() };
        let item = match &e.state {
            Some(s) => format!("[{}] {name} ({}, {s})", m.id, m.role),
            None => format!("[{}] {name} ({})", m.id, m.role),
        };
        match regions.iter_mut().find(|r| r.0 == m.region) {
            Some(r) => r.1.push(item),
            None => regions.push((&m.region, vec![item])),
        }
    }
    regions.iter().map(|(r, items)| format!("{r}: {}", items.join(" · "))).collect::<Vec<_>>().join("\n")
}

fn marks_text(total: usize, els: &[&crate::ax::El]) -> String {
    let more = if els.len() < total { format!(" ({} for this order; list_elements for all)", els.len()) } else { String::new() };
    format!("{total} marks{more}:\n{}", mark_lines(els))
}

/// The same screen for the model: app, window, the marks and where they are.
fn same(a: &Screen, b: &Screen) -> bool {
    let key = |s: &Screen| s.els.iter().map(|e| (e.mark.name.clone(), e.mark.role.clone(), e.mark.rect.x.round() as i64, e.mark.rect.y.round() as i64)).collect::<Vec<_>>();
    a.app == b.app && a.title == b.title && key(a) == key(b)
}

/// The screenshot frame: the whole display in computer use (`vision`), else the front window.
fn frame(s: &Screen, vision: bool) -> PtRect {
    if s.window.w > 0.0 && !vision { s.window } else { s.display }
}

/// One order's tool handlers over the executor.
pub struct Agent<'a, 'b> {
    cfg: Cfg,
    task: String,
    ask: &'a mut Ask<'b>,
    pub desk: &'a mut dyn Desk,
    mem: &'a mut Memory,
    pub step: u32,
    /// Tool calls of any kind (0 = the model never acted: the fallback may take the task).
    pub calls: u32,
    /// The last read: mark labels and the screenshot frame.
    shot: Option<Screen>,
    pub done: Option<String>,
    pub capped: bool,
    /// What the actions did ("clicked “Send” (button)").
    pub did: Vec<String>,
    /// The actions for the rolling memory.
    pub log: Vec<Step>,
    /// Pablo pressed a key or clicked during the order: it stops.
    pub took_over: bool,
    /// ms from the order sent to claude's first tool call (the smoke measures it).
    pub first_tool: Option<u128>,
    /// The screen whose marks the model last got (this order or the warm process's last one), and which of them.
    seen: Option<Screen>,
    shown: Vec<u32>,
    /// Computer use (CommandSettings::agent_vision): the whole screen as a screenshot after every action, x,y only,
    /// no marks for the model.
    pub vision: bool,
    /// Computer use: the screen with the order's first message.
    pub first_image: Option<Vec<u8>>,
}

impl<'a, 'b> Agent<'a, 'b> {
    pub fn new(task: &str, confirm_risky: bool, ask: &'a mut Ask<'b>, desk: &'a mut dyn Desk, mem: &'a mut Memory) -> Self {
        // the agent picks its own targets: no "Which one?" per click
        let cfg = Cfg { confirm_risky, choose: false, max: STEP_CAP };
        Agent { cfg, task: task.into(), ask, desk, mem, step: 0, calls: 0, shot: None, done: None, capped: false, did: vec![], log: vec![], took_over: false, first_tool: None, seen: None, shown: vec![], vision: false, first_image: None }
    }

    /// A pill line between actions.
    pub fn say(&mut self, text: &str) {
        let st = CommandStep { instruction: text.chars().take(120).collect(), step: self.step, max: self.cfg.max, status: CmdStatus::Thinking, ..Default::default() };
        self.desk.emit(&st);
    }

    /// The order's first user message: the words and the screen now (no round trip for a first look).
    pub fn first_message(&mut self) -> String {
        match self.desk.read() {
            Ok(scr) => {
                self.shot = Some(scr);
                if self.vision {
                    self.first_image = self.snap();
                }
                self.order_text()
            }
            Err(e) if e == "accessibility" => self.order_with("susurro has no Accessibility permission: call done and say so.".into()),
            Err(e) => self.order_with(format!("The screen cannot be read: {e}.")),
        }
    }

    /// The order and the last read screen, against what the model has seen (`seen`).
    fn order_text(&mut self) -> String {
        let Some(scr) = self.shot.clone() else { return self.order_with("The screen cannot be read.".into()) };
        let f = self.desk.focused();
        let now = format!("{}\n{}", state_line(&scr, &f, None, self.vision), self.marks(&scr));
        self.order_with(now)
    }

    fn order_with(&self, now: String) -> String {
        format!("Order: {}\n\nThe screen now:\n{now}", self.task)
    }

    /// The marks for the model: the order's MARKS_MAX (`pick`) it has not got yet for this screen. A screen it saw
    /// already costs one line (Pablo: "why is it so slow": every order and answer re-sent 80 marks).
    fn marks(&mut self, scr: &Screen) -> String {
        if self.vision {
            self.seen = Some(scr.clone());
            return if self.first_image.is_some() || self.calls > 0 { "The screenshot shows the screen now.".into() } else { "No screenshot: take one.".into() };
        }
        if !self.seen.as_ref().is_some_and(|s| same(s, scr)) {
            self.seen = Some(scr.clone());
            let els = pick(&scr.els, &self.task, MARKS_MAX);
            self.shown = els.iter().map(|e| e.mark.id).collect();
            return marks_text(scr.els.len(), &els);
        }
        let new: Vec<&crate::ax::El> = pick(&scr.els, &self.task, MARKS_MAX).into_iter().filter(|e| !self.shown.contains(&e.mark.id)).collect();
        self.shown.extend(new.iter().map(|e| e.mark.id));
        if new.is_empty() { "The marks are the ones you got last.".into() } else { format!("The marks you got last, plus:\n{}", mark_lines(&new)) }
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Out {
        self.calls += 1;
        if self.desk.aborted() {
            return Out::err("Stopped by the user.");
        }
        // Pablo took the mouse or the keyboard: never act over what he does by hand
        if self.desk.paused() {
            self.took_over = true;
            return Out::err("The user took over the mouse or keyboard. Stop now.");
        }
        let mods = match modifiers(args["modifiers"].as_str().unwrap_or("")) {
            Ok(m) => m,
            Err(e) => return Out::err(e),
        };
        let mut out = self.tool(name, args, mods);
        // computer use: every answer shows the screen after it (no extra screenshot round trip)
        if self.vision && out.image.is_none() && !["screenshot", "done"].contains(&name) && !self.desk.aborted() && !self.took_over {
            out.image = self.snap();
        }
        // an action that finishes the order ends it now (no done call, no round trip)
        if let Some(d) = args["done"].as_str().filter(|_| !out.error && name != "done") {
            self.done = Some(d.to_string());
            out.text += "\nThe order is finished. End your turn now: no more tools, no text.";
        }
        out
    }

    fn tool(&mut self, name: &str, args: &Value, mods: u64) -> Out {
        let s = |k: &str| args[k].as_str().map(String::from);
        let mark = |k: &str| args[k].as_u64().map(|m| m as u32);
        match name {
            "screenshot" => self.look(true),
            "list_elements" => self.look(false),
            "wait" => self.wait(args["ms"].as_u64().unwrap_or(500).min(WAIT_MAX_MS)),
            "done" => {
                self.done = Some(s("summary").unwrap_or_default());
                Out::ok("Finished. End your turn now: no more tools, no text.")
            }
            "click" if mods == 0 => match (mark("mark"), args["x"].as_f64(), args["y"].as_f64()) {
                (Some(m), _, _) => self.act(Action { op: "click".into(), target: Some(self.label(m)), ..Default::default() }),
                (None, Some(x), Some(y)) => self.click_point(x, y),
                _ => Out::err("click needs mark, or x and y"),
            },
            "click" | "double_click" | "right_click" => {
                let (right, count) = (name == "right_click", if name == "double_click" { 2 } else { 1 });
                let what = format!("{}{}", name.replace('_', " "), if mods == 0 { String::new() } else { format!(" with {}", args["modifiers"].as_str().unwrap_or("")) });
                match self.at(args) {
                    Ok((p, label)) => self.raw(&format!("{what} {label}"), Act::ClickAt { at: p, right, count, flags: mods }, Some(p)),
                    Err(o) => o,
                }
            }
            "mouse_move" => match self.at(args) {
                Ok((p, label)) => self.raw(&format!("moved the mouse to {label}"), Act::MouseMove(p.0, p.1), None),
                Err(o) => o,
            },
            "drag" => self.drag(&args["path"], mods),
            "type" => self.act(Action { op: "type".into(), text: s("text"), ..Default::default() }),
            "key" => match args["hold_ms"].as_u64().filter(|ms| *ms > 0) {
                Some(ms) => self.hold(&s("combo").unwrap_or_default(), ms.min(WAIT_MAX_MS)),
                None => self.act(Action { op: "key".into(), keys: s("combo"), ..Default::default() }),
            },
            "scroll" => match (mark("mark"), args["x"].as_f64(), args["y"].as_f64()) {
                (None, Some(_), Some(_)) => {
                    let down = s("dir").as_deref() != Some("up");
                    match self.at(args) {
                        Ok((p, label)) => self.raw(&format!("scrolled {} at {label}", if down { "down" } else { "up" }), Act::Scroll { down, at: Some(Target::Point(p.0, p.1)) }, None),
                        Err(o) => o,
                    }
                }
                (m, _, _) => {
                    let target = m.map(|m| self.label(m));
                    self.act(Action { op: "scroll".into(), dir: s("dir"), target, ..Default::default() })
                }
            },
            "open_app" => {
                // already in front (GUI round 1: "abre Docker" with Docker in front): no open -a, no step
                let front = self.shot.as_ref().map(|x| x.app.clone()).unwrap_or_default();
                let want = s("name").unwrap_or_default();
                let named = command::match_app(&want, &self.desk.apps()).unwrap_or(want);
                if !front.is_empty() && named.eq_ignore_ascii_case(&front) {
                    return Out::ok(format!("{front} is already the front app: act in it."));
                }
                self.act(Action { op: "open_app".into(), app: Some(named), ..Default::default() })
            }
            n => Out::err(format!("unknown tool {n}")),
        }
    }

    /// "[12] Send (button, main)": the target text the executor grounds (planner style), else the bare number.
    fn label(&self, id: u32) -> String {
        match self.el(id) {
            Some(e) => format!("[{id}] {}", crate::ax::describe(e)),
            None => id.to_string(),
        }
    }

    fn el(&self, id: u32) -> Option<&crate::ax::El> {
        self.shot.as_ref().and_then(|s| s.els.iter().find(|e| e.mark.id == id))
    }

    /// A mark's center or a screenshot point, in global points, and how to name it.
    fn at(&self, args: &Value) -> Result<((f64, f64), String), Out> {
        if let Some(id) = args["mark"].as_u64() {
            let e = self.el(id as u32).ok_or_else(|| Out::err(format!("No mark {id} now: use the marks of the last answer.")))?;
            return Ok((command::center(e.mark.rect), format!("“{}”", e.mark.name)));
        }
        let (Some(x), Some(y)) = (args["x"].as_f64(), args["y"].as_f64()) else { return Err(Out::err("Give mark, or x and y.")) };
        let scr = self.shot.as_ref().ok_or_else(|| Out::err("Take a screenshot first."))?;
        Ok((to_points(frame(scr, self.vision), (x, y)), format!("{x:.0},{y:.0}")))
    }

    fn look(&mut self, image: bool) -> Out {
        let scr = match self.desk.read() {
            Ok(s) => s,
            Err(e) if e == "accessibility" => return Out::err("susurro has no Accessibility permission. Call done."),
            Err(e) => return Out::err(e),
        };
        let win = frame(&scr, self.vision);
        let f = self.desk.focused();
        let mut head = state_line(&scr, &f, None, self.vision);
        let mut out = Out::default();
        if image && self.vision {
            match self.snap_of(win) {
                Ok(b) => {
                    head += " The image is the whole screen (x,y for the mouse tools are in its pixels).";
                    out.image = Some(b);
                }
                Err(e) => head += &format!(" No image: {e}."),
            }
            out.text = head;
            self.seen = Some(scr.clone());
            self.shot = Some(scr);
            return out;
        }
        if image {
            // The marks window draws the numbers on screen: the capture shows them.
            let st = CommandStep {
                instruction: format!("step {}: look", self.step),
                step: self.step,
                max: self.cfg.max,
                status: CmdStatus::Marks,
                display: scr.display,
                marks: scr.els.iter().map(|e| e.mark.clone()).collect(),
                ..Default::default()
            };
            self.desk.emit(&st);
            if self.desk.screen_ok() {
                self.desk.sleep(command::MARKS_DRAW_MS);
                match self.desk.snapshot(win, IMG_MAX) {
                    Ok(b) => {
                        head += " The image is that frame (x,y for the point tools are in its pixels).";
                        out.image = Some(b);
                    }
                    Err(e) => head += &format!(" No image: {e:#}."),
                }
            } else {
                head += " No image: susurro has no Screen Recording permission. Use the marks.";
            }
        }
        let all: Vec<&crate::ax::El> = scr.els.iter().collect();
        out.text = format!("{head}\n{}", marks_text(all.len(), &all));
        self.shown = scr.els.iter().map(|e| e.mark.id).collect();
        self.seen = Some(scr.clone());
        self.shot = Some(scr);
        out
    }

    /// Computer use: the last read screen's display as the model's screenshot. None = no image (no permission, no read).
    fn snap(&mut self) -> Option<Vec<u8>> {
        let r = frame(self.shot.as_ref()?, true);
        self.snap_of(r).inspect_err(|e| log::warn!("agent: screenshot: {e}")).ok()
    }

    fn snap_of(&mut self, r: PtRect) -> Result<Vec<u8>, String> {
        if !self.desk.screen_ok() {
            return Err("susurro has no Screen Recording permission".into());
        }
        self.desk.snapshot(r, IMG_MAX).map_err(|e| format!("{e:#}"))
    }

    /// Waits until the screen stops changing, at most `max` ms (GUI round 1: a 3 s wait ran in full on a loaded page).
    /// Settled = two reads WAIT_POLL_MS apart are the same, and the screen changed since the last answer or
    /// WAIT_MIN_MS went by (a navigation that has not started yet).
    fn wait(&mut self, max: u64) -> Out {
        let start = self.shot.clone();
        let mut prev = self.desk.read().ok();
        let mut slept = 0;
        while slept < max {
            let ms = WAIT_POLL_MS.min(max - slept);
            self.desk.sleep(ms);
            slept += ms;
            let cur = self.desk.read().ok();
            let stable = matches!((&prev, &cur), (Some(a), Some(b)) if same(a, b));
            let moved = matches!((&start, &cur), (Some(a), Some(b)) if !same(a, b));
            prev = cur;
            if stable && (moved || slept >= WAIT_MIN_MS) {
                break;
            }
        }
        let f = self.desk.focused();
        let Some(scr) = prev else { return Out::ok(format!("waited {slept} ms")) };
        let t = format!("waited {slept} ms\n{}\n{}", state_line(&scr, &f, Some(start.as_ref().is_none_or(|b| !same(b, &scr))), self.vision), self.marks(&scr));
        self.shot = Some(scr);
        Out::ok(t)
    }

    /// What changed after an action, and the new marks (the model rarely needs a screenshot then).
    fn now(&mut self, before: Option<&Screen>) -> String {
        self.desk.sleep(AFTER_MS);
        match self.desk.read() {
            Ok(scr) => {
                let f = self.desk.focused();
                let t = format!("{}\n{}", state_line(&scr, &f, Some(before.is_none_or(|b| !same(b, &scr))), self.vision), self.marks(&scr));
                self.shot = Some(scr);
                t
            }
            Err(e) => format!("The screen cannot be read now: {e}."),
        }
    }

    fn remember(&mut self, did: &str, ok: bool) {
        let app = self.shot.as_ref().map(|s| s.app.clone()).unwrap_or_default();
        self.log.push(Step { at: crate::history::now_ms(), app, did: did.chars().take(160).collect(), ok });
    }

    /// Counts the step. false = the cap is reached.
    fn next_step(&mut self) -> bool {
        if self.step >= self.cfg.max {
            self.capped = true;
            return false;
        }
        self.step += 1;
        true
    }

    fn act(&mut self, mut a: Action) -> Out {
        if !self.next_step() {
            return Out::err("Step cap reached. Call done.");
        }
        a.said = self.task.clone();
        let before = self.shot.clone();
        let label = crate::planner::label(&a);
        let end = command::exec(&a, self.step, 0, &self.cfg, self.ask, self.desk, self.mem);
        let ok = end.status == CmdStatus::Done;
        self.remember(if ok { &end.summary } else { &label }, ok);
        let mut out = self.report(end);
        if !self.desk.aborted() && !out.text.starts_with("The user said no") {
            out.text = format!("{}\n{}", out.text, self.now(before.as_ref()));
        }
        out
    }

    fn report(&mut self, end: End) -> Out {
        let msg = end.message.unwrap_or_default();
        match end.status {
            CmdStatus::Done => {
                self.did.push(end.summary.clone());
                Out::ok(end.summary)
            }
            CmdStatus::Stopped if msg.is_empty() => Out::err("The user said no or stopped it. Do not retry: call done."),
            CmdStatus::Blocked => Out::err(format!("Blocked: {msg}")),
            s => Out::err(format!("{}: {msg}", format!("{s:?}").to_lowercase())),
        }
    }

    /// A risky raw click (a modified / double / right click on "Delete"): the same yes as exec's.
    fn confirm(&mut self, what: &str) -> bool {
        let st = CommandStep { instruction: format!("step {}: {what}", self.step), step: self.step, max: self.cfg.max, status: CmdStatus::Confirm, message: Some("Do it? Say yes · F11 · Esc".into()), ..Default::default() };
        self.desk.emit(&st);
        self.desk.wait(Wait::Confirm, command::CONFIRM_TIMEOUT) == command::Answer::Yes
    }

    /// A raw act (the mouse tools): password check at the point, the act, what changed.
    fn raw(&mut self, what: &str, act: Act, at: Option<(f64, f64)>) -> Out {
        if !self.next_step() {
            return Out::err("Step cap reached. Call done.");
        }
        if at.is_some_and(|p| self.desk.secure_at(p)) {
            return Out::err("Blocked: a password field. susurro won't touch it.");
        }
        if self.cfg.confirm_risky && command::risky_name(what) && !self.confirm(what) {
            self.remember(what, false);
            return Out::err("The user said no or stopped it. Do not retry: call done.");
        }
        if self.desk.aborted() {
            return Out::err("Stopped by the user.");
        }
        let before = self.shot.clone();
        if let Err(e) = self.desk.act(&act) {
            self.remember(what, false);
            return Out::err(format!("error: {e:#}"));
        }
        let st = CommandStep { instruction: format!("step {}: {what}", self.step), step: self.step, max: self.cfg.max, status: CmdStatus::Done, ..Default::default() };
        self.desk.emit(&st);
        self.did.push(what.to_string());
        self.remember(what, true);
        Out::ok(format!("{what}\n{}", self.now(before.as_ref())))
    }

    /// A point click: the mark under it when there is one (its checks, its verify), else a raw click.
    fn click_point(&mut self, x: f64, y: f64) -> Out {
        let Some(scr) = self.shot.clone() else { return Out::err("Take a screenshot first.") };
        let p = to_points(frame(&scr, self.vision), (x, y));
        // computer use: a plain click where it points (the executor would draw the marks into the next screenshot);
        // the element under it names the click, so the risky check still asks before "Delete"
        if self.vision {
            let under = command::snap(p, &scr.els).map(|e| e.mark.name.clone()).filter(|n| !n.trim().is_empty());
            let what = match under {
                Some(n) => format!("clicked “{n}” at {x:.0},{y:.0}"),
                None => format!("clicked at {x:.0},{y:.0}"),
            };
            return self.raw(&what, Act::Click(Target::Point(p.0, p.1)), Some(p));
        }
        if let Some(id) = command::snap(p, &scr.els).map(|e| e.mark.id) {
            return self.act(Action { op: "click".into(), target: Some(self.label(id)), ..Default::default() });
        }
        self.raw(&format!("clicked at {x:.0},{y:.0}"), Act::Click(Target::Point(p.0, p.1)), Some(p))
    }

    fn drag(&mut self, path: &Value, mods: u64) -> Out {
        let Some(scr) = self.shot.clone() else { return Out::err("Take a screenshot first.") };
        let px: Vec<(f64, f64)> = path.as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect();
        if px.len() < 2 {
            return Out::err("drag needs a path of 2 or more [x,y] points.");
        }
        let pts: Vec<(f64, f64)> = px.iter().map(|p| to_points(frame(&scr, self.vision), *p)).collect();
        let (a, b) = (px[0], px[px.len() - 1]);
        let what = format!("dragged from {:.0},{:.0} to {:.0},{:.0} through {} points", a.0, a.1, b.0, b.1, px.len());
        self.raw(&what, Act::Drag(pts.clone(), mods), Some(pts[0]))
    }

    /// A key held down (key with hold_ms).
    fn hold(&mut self, combo: &str, ms: u64) -> Out {
        let (code, flags) = match crate::paste::parse_combo(combo) {
            Ok(k) => k,
            Err(e) => return Out::err(e),
        };
        if crate::paste::blocked_combo(code, flags) {
            return Out::err(format!("Blocked: {combo} is blocked for voice control"));
        }
        if self.desk.secure_input() {
            return Out::err("Blocked: a password field. susurro won't touch it.");
        }
        self.raw(&format!("held {combo} for {ms} ms"), Act::KeyHold(code, flags, ms), None)
    }
}

// ---------- the MCP server (streamable HTTP, JSON responses only) ----------

pub enum Msg {
    /// A tools/call: name, arguments, where the MCP result goes.
    Call(String, Value, mpsc::Sender<Value>),
    /// One stdout line of the claude process.
    Line(String),
    Eof,
}

/// The run's MCP endpoint. Dropping it stops the accept loop.
pub struct Server {
    pub url: String,
    /// tools/list answers the computer-use tools.
    pub vision: bool,
    pub token: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
}

fn token() -> String {
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(Instant::now().elapsed().as_nanos());
        b[..8].copy_from_slice(&h.finish().to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Server {
    pub fn start(tx: mpsc::Sender<Msg>, vision: bool) -> std::io::Result<Server> {
        let l = TcpListener::bind("127.0.0.1:0")?;
        let addr = l.local_addr()?;
        let (tok, stop) = (token(), Arc::new(AtomicBool::new(false)));
        let (t, s) = (tok.clone(), stop.clone());
        std::thread::Builder::new().name("agent-mcp".into()).spawn(move || {
            for c in l.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(c) = c else { continue };
                let (t, tx) = (t.clone(), tx.clone());
                std::thread::spawn(move || {
                    if let Err(e) = serve(c, &t, &tx, vision) {
                        log::debug!("agent mcp: {e}");
                    }
                });
            }
        })?;
        Ok(Server { url: format!("http://{addr}/mcp"), vision, token: tok, addr, stop })
    }

    /// The --mcp-config JSON for this server.
    pub fn config(&self) -> String {
        json!({ "mcpServers": { "susurro": { "type": "http", "url": self.url, "headers": { "Authorization": format!("Bearer {}", self.token) } } } }).to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr); // wakes the accept loop
    }
}

fn serve(c: TcpStream, token: &str, tx: &mpsc::Sender<Msg>, vision: bool) -> std::io::Result<()> {
    let mut r = BufReader::new(c.try_clone()?);
    let mut w = c;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let method = line.split_whitespace().next().unwrap_or("").to_string();
        let (mut len, mut auth) = (0usize, false);
        loop {
            let mut h = String::new();
            if r.read_line(&mut h)? == 0 {
                return Ok(());
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => len = v.trim().parse().unwrap_or(0),
                    "authorization" => auth = v.trim() == format!("Bearer {token}"),
                    _ => {}
                }
            }
        }
        if len > BODY_MAX {
            return respond(&mut w, 413, None);
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        let (status, out) = if !auth {
            (401, None)
        } else if method != "POST" {
            (405, None) // no SSE stream: every answer is a JSON response
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(req) => match rpc(&req, tx, vision) {
                    Some(v) => (200, Some(v)),
                    None => (202, None),
                },
                Err(_) => (400, None),
            }
        };
        respond(&mut w, status, out)?;
    }
}

fn respond(w: &mut TcpStream, status: u16, body: Option<Value>) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        405 => "Method Not Allowed",
        _ => "Payload Too Large",
    };
    let b = body.map(|v| v.to_string()).unwrap_or_default();
    let ct = if b.is_empty() { "" } else { "Content-Type: application/json\r\n" };
    write!(w, "HTTP/1.1 {status} {reason}\r\n{ct}Content-Length: {}\r\n\r\n{b}", b.len())?;
    w.flush()
}

/// One JSON-RPC message. None = a notification (202, no body).
pub fn rpc(req: &Value, tx: &mpsc::Sender<Msg>, vision: bool) -> Option<Value> {
    let id = req.get("id")?.clone();
    let p = &req["params"];
    let result: Result<Value, (i32, String)> = match req["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": p["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "susurro", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for(vision) })),
        "tools/call" => {
            let (rtx, rrx) = mpsc::channel();
            let name = p["name"].as_str().unwrap_or("").to_string();
            let args = if p["arguments"].is_object() { p["arguments"].clone() } else { json!({}) };
            let gone = || Out::err("The agent run ended.").mcp();
            Ok(match tx.send(Msg::Call(name, args, rtx)) {
                Ok(()) => rrx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| gone()),
                Err(_) => gone(),
            })
        }
        m => Err((-32601, format!("Method not found: {m}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

// ---------- the run ----------

#[derive(Clone)]
pub struct Opts {
    /// The claude binary. None = straight to the fallback.
    pub claude: Option<PathBuf>,
    pub model: String,
    pub fallback_model: String,
    /// The OpenRouter key (fallback). Empty = no fallback.
    pub key: String,
    pub base: String,
    pub timeout: Duration,
    /// The rolling memory file (agent-memory.json). None = no memory.
    pub memory: Option<PathBuf>,
    /// Computer use (CommandSettings::agent_vision).
    pub vision: bool,
}

impl Opts {
    pub fn from(s: &CommandSettings, key: &str, data_dir: &Path) -> Self {
        Opts {
            claude: find_claude(),
            model: s.agent_model.clone(),
            fallback_model: s.agent_fallback.clone(),
            key: key.into(),
            base: crate::stt::OPENROUTER_BASE.into(),
            timeout: TIMEOUT,
            memory: Some(data_dir.join("agent-memory.json")),
            vision: s.agent_vision,
        }
    }
}

/// The app runs without the shell PATH: the usual install places. SUSURRO_CLAUDE overrides.
pub fn find_claude() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    std::env::var("SUSURRO_CLAUDE")
        .ok()
        .into_iter()
        .chain([format!("{home}/.local/bin/claude"), "/opt/homebrew/bin/claude".into(), "/usr/local/bin/claude".into()])
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// `claude -p` with stream-json in and out (one process, many orders), locked down to susurro's tools: no built-in
/// tools (no Bash, Read, Write, Edit, WebFetch), no other MCP server, no user/project settings (hooks, permissions;
/// CLAUDE.md measured not loaded: ~2k prompt tokens), no prompts (dontAsk), nothing saved. Not --safe-mode: it drops
/// the --mcp-config server too (measured on 2.1.296), and not --bare: it never reads the subscription login.
pub fn claude_args(model: &str, system: &str, mcp_config: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--model", model, "--system-prompt", system,
        "--mcp-config", mcp_config, "--strict-mcp-config", "--tools", "", "--allowedTools", "mcp__susurro", "--permission-mode", "dontAsk",
        "--permission-prompts", "none", "--setting-sources", "", "--disable-slash-commands", "--no-session-persistence",
    ]
    .map(String::from)
    .to_vec();
    if model.trim().is_empty() {
        let i = a.iter().position(|x| x == "--model").unwrap();
        a.drain(i..i + 2);
    }
    a
}

/// Runs one order to its end. `confirm_risky`: the Voice control setting.
pub fn run(task: &str, o: &Opts, confirm_risky: bool, ask: &mut Ask, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    let t0 = Instant::now();
    let at = crate::history::now_ms();
    let past = o.memory.as_deref().map(|p| session(&load_memory(p), at)).unwrap_or_default();
    let mut ag = Agent::new(task, confirm_risky, ask, desk, mem);
    ag.vision = o.vision && o.claude.is_some();
    ag.say(&format!("agent: {task}"));
    let first = ag.first_message();
    log::info!("agent: first look {} ms", t0.elapsed().as_millis());
    let claude = o.claude.as_deref().map(|b| run_claude(b, &first, &past, at, o, &mut ag));
    let end = match claude {
        Some(Ok(end)) => end,
        other => {
            if let Some(Err(why)) = other {
                log::warn!("agent: claude failed before any tool call ({why}): the OpenRouter fallback takes it");
            }
            if o.key.is_empty() {
                End { status: CmdStatus::Setup, summary: String::new(), message: Some("key".into()), steps: 0 }
            } else {
                ag.say(&format!("agent ({}): {task}", o.fallback_model));
                // the fallback model reads the marks (it points badly at pixels): no computer use
                let first = if ag.vision {
                    (ag.vision, ag.seen, ag.first_image) = (false, None, None);
                    ag.order_text()
                } else {
                    first
                };
                run_openrouter(&format!("{}{first}", ledger_text(&past, at)), o, &mut ag)
            }
        }
    };
    if let Some(p) = &o.memory {
        save_order(p, Order { at, said: task.into(), end: outcome(&end), steps: ag.log.clone() });
    }
    log::info!("agent: {:?} in {} ms, {} steps, {} calls: {} {:?}", end.status, t0.elapsed().as_millis(), ag.step, ag.calls, end.summary, end.message);
    End { summary: if end.summary.is_empty() { end.summary } else { format!("agent: {}", end.summary) }, ..end }
}

/// How a run ended, before the End.
fn finish(ag: &Agent, why: Option<String>) -> End {
    let mk = |status, summary: String, message: Option<String>| End { status, summary, message, steps: ag.step };
    if ag.desk.aborted() {
        return mk(CmdStatus::Stopped, String::new(), None);
    }
    if ag.took_over && ag.done.is_none() {
        return mk(CmdStatus::Stopped, ag.did.join("; "), Some("Paused: you took over".into()));
    }
    if let Some(s) = &ag.done {
        let s = if s.trim().is_empty() { ag.did.join("; ") } else { s.trim().to_string() };
        return mk(CmdStatus::Done, s, None);
    }
    if ag.capped {
        return mk(CmdStatus::Unsure, ag.did.join("; "), Some(format!("Stopped at the {STEP_CAP}-step cap")));
    }
    match why {
        Some(m) => mk(CmdStatus::Error, ag.did.join("; "), Some(m)),
        None => mk(CmdStatus::Done, ag.did.join("; "), None),
    }
}

// ---------- the warm claude process ----------

/// One `claude -p --input-format stream-json` process and its MCP server.
struct Warm {
    bin: PathBuf,
    model: String,
    /// Started with the computer-use tools and prompt: a change of the setting needs a new process.
    vision: bool,
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Msg>,
    _srv: Server,
    /// User messages sent and `result` lines seen: order N ends at the N-th result.
    sent: u32,
    results: u32,
    last: Instant,
    /// The screen and marks this conversation last got (Agent::seen): the next order sends only what is new.
    seen: Option<Screen>,
    shown: Vec<u32>,
    /// The newest ledger order this conversation knows (it ran it, or got it in a message): 0 = none.
    known: i64,
}

impl Drop for Warm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_result(line: &str) -> bool {
    line.contains("\"result\"") && serde_json::from_str::<Value>(line).is_ok_and(|v| v["type"] == "result")
}

impl Warm {
    fn spawn(bin: &Path, o: &Opts) -> Result<Warm, String> {
        use std::process::{Command, Stdio};
        let (tx, rx) = mpsc::channel();
        let srv = Server::start(tx.clone(), o.vision).map_err(|e| format!("mcp server: {e}"))?;
        // An empty folder: no project CLAUDE.md, .mcp.json or settings to pick up.
        let dir = std::env::temp_dir().join("susurro-agent");
        let _ = std::fs::create_dir_all(&dir);
        let system = format!("{}{}", system_for(o.vision), browser_note(default_browser()));
        let mut child = Command::new(bin)
            .args(claude_args(&o.model, &system, &srv.config()))
            .current_dir(&dir)
            .env_remove("ANTHROPIC_API_KEY") // his subscription login, never a stray key
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(Msg::Line(l)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::Eof);
        });
        let err = child.stderr.take().unwrap();
        std::thread::spawn(move || BufReader::new(err).lines().map_while(Result::ok).for_each(|l| log::warn!("agent: claude stderr: {l}")));
        let stdin = child.stdin.take().unwrap();
        log::info!("agent: claude started (pid {}, {}{})", child.id(), o.model, if o.vision { ", computer use" } else { "" });
        reaper();
        Ok(Warm { bin: bin.into(), model: o.model.clone(), vision: o.vision, child, stdin, rx, _srv: srv, sent: 0, results: 0, last: Instant::now(), seen: None, shown: vec![], known: 0 })
    }

    /// Reads what came between two orders (a late result, a tool call after done). false = the process is gone.
    fn settle(&mut self) -> bool {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Eof => return false,
                Msg::Line(l) if is_result(&l) => self.results += 1,
                Msg::Line(_) => {}
                Msg::Call(_, _, reply) => drop(reply.send(Out::err("That order is over. End your turn.").mcp())),
            }
        }
        self.child.try_wait().is_ok_and(|s| s.is_none())
    }

    /// One order. `image`: the screen as a JPEG (computer use).
    fn send(&mut self, text: &str, image: Option<&[u8]>) -> std::io::Result<()> {
        use base64::Engine;
        let mut content = vec![json!({ "type": "text", "text": text })];
        if let Some(b) = image {
            content.push(json!({ "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(b) } }));
        }
        let m = json!({ "type": "user", "message": { "role": "user", "content": content } });
        writeln!(self.stdin, "{m}")?;
        self.stdin.flush()?;
        self.sent += 1;
        Ok(())
    }
}

/// Warm processes by binary (tests run several fakes at once); `busy` = an order holds one.
struct Pool {
    idle: Vec<Warm>,
    busy: Vec<PathBuf>,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { idle: Vec::new(), busy: Vec::new() });
static REAPER: std::sync::Once = std::sync::Once::new();

/// Kills the warm processes idle for IDLE (checked every 10 s).
fn reaper() {
    REAPER.call_once(|| {
        let _ = std::thread::Builder::new().name("agent-reaper".into()).spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(10));
                POOL.lock().unwrap().idle.retain(|w| {
                    let keep = w.last.elapsed() < IDLE;
                    if !keep {
                        log::info!("agent: claude idle for {} s, killed", IDLE.as_secs());
                    }
                    keep
                });
            }
        });
    });
}

/// F11 went down: start (or keep) the process while Pablo talks, so its start overlaps his speech.
pub fn prewarm(o: Opts) {
    let Some(bin) = o.claude.clone() else { return };
    std::thread::spawn(move || {
        let mut p = POOL.lock().unwrap();
        if p.busy.contains(&bin) {
            return;
        }
        if let Some(i) = p.idle.iter().position(|w| w.bin == bin) {
            let w = &mut p.idle[i];
            if w.model == o.model && w.vision == o.vision && w.settle() {
                w.last = Instant::now();
                return;
            }
            p.idle.remove(i); // a dead one, or another model: drop kills it
        }
        match Warm::spawn(&bin, &o) {
            Ok(w) => p.idle.push(w),
            Err(e) => log::warn!("agent: prewarm: {e}"),
        }
    });
}

/// The warm process for this order (a fresh one if it died or the model changed). The pool marks it busy.
fn take(bin: &Path, o: &Opts) -> Result<Warm, String> {
    let mut p = POOL.lock().unwrap();
    let warm = p.idle.iter().position(|w| w.bin == bin).map(|i| p.idle.remove(i));
    p.busy.push(bin.into());
    drop(p);
    if let Some(mut w) = warm
        && w.model == o.model
        && w.vision == o.vision
        && w.settle()
    {
        return Ok(w);
    }
    Warm::spawn(bin, o).inspect_err(|_| release(bin))
}

fn release(bin: &Path) {
    let mut p = POOL.lock().unwrap();
    if let Some(i) = p.busy.iter().position(|b| b == bin) {
        p.busy.remove(i);
    }
}

fn give_back(mut w: Warm) {
    w.last = Instant::now();
    let bin = w.bin.clone();
    let mut p = POOL.lock().unwrap();
    if let Some(i) = p.busy.iter().position(|b| *b == bin) {
        p.busy.remove(i);
    }
    p.idle.push(w);
}

/// Esc, "stop", the app quits: every idle process dies (a running order kills its own on the abort).
pub fn kill() {
    let idle = std::mem::take(&mut POOL.lock().unwrap().idle);
    if !idle.is_empty() {
        log::info!("agent: {} warm claude process(es) killed", idle.len());
    }
}

/// For GET /state (test hook) and the logs.
pub fn status() -> Value {
    let p = POOL.lock().unwrap();
    let warm: Vec<Value> = p.idle.iter().map(|w| json!({ "pid": w.child.id(), "model": w.model, "orders": w.sent, "idle_s": w.last.elapsed().as_secs() })).collect();
    json!({ "warm": warm, "busy": p.busy.len() })
}

/// Err = claude failed before any tool call (missing login, a bad flag): the fallback may run.
/// `past`: the session's earlier orders (the ones this conversation has not seen go before the order); `at`: this
/// order's id in the ledger.
fn run_claude(bin: &Path, first: &str, past: &[Order], at: i64, o: &Opts, ag: &mut Agent) -> Result<End, String> {
    let t0 = Instant::now();
    let mut w = take(bin, o)?;
    let order = w.sent + 1;
    // this conversation saw a screen already: the order carries only what changed. `own` = the fresh view of
    // `first`, back in place if the fallback takes the order (a new conversation).
    let own = w.seen.take().map(|s| (std::mem::replace(&mut ag.seen, Some(s)), std::mem::replace(&mut ag.shown, std::mem::take(&mut w.shown))));
    let first = if own.is_some() && ag.shot.is_some() { ag.order_text() } else { first.to_string() };
    let unseen: Vec<Order> = past.iter().filter(|p| p.at > w.known).cloned().collect();
    let first = format!("{}{first}", ledger_text(&unseen, at));
    let restore = |ag: &mut Agent, own: Option<(Option<Screen>, Vec<u32>)>| {
        if let Some((seen, shown)) = own {
            (ag.seen, ag.shown) = (seen, shown);
        }
    };
    if let Err(e) = w.send(&first, ag.first_image.as_deref()) {
        release(bin);
        restore(ag, own);
        return Err(format!("claude stdin: {e}"));
    }
    let mut result: Option<(bool, String)> = None;
    let mut first_tool: Option<u128> = None;
    let mut usage = Value::Null;
    let (mut why, mut keep) = (None, true);
    loop {
        if ag.desk.aborted() || ag.capped || ag.took_over {
            keep = false; // mid-turn: the conversation would go on without us
            break;
        }
        if ag.done.is_some() {
            break; // its result line comes later: the next order skips it (`results`)
        }
        if t0.elapsed() > o.timeout {
            (why, keep) = (Some(format!("No answer in {} s", o.timeout.as_secs())), false);
            break;
        }
        match w.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Call(name, args, reply)) => {
                log::info!("agent: {} ms {name} {args}", t0.elapsed().as_millis());
                let out = ag.call(&name, &args);
                let _ = reply.send(out.mcp());
            }
            Ok(Msg::Line(l)) => {
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                match v["type"].as_str() {
                    Some("assistant") => {
                        if usage.is_null() {
                            usage = v["message"]["usage"].clone(); // the first API call: the one before the first tool
                        }
                        for c in v["message"]["content"].as_array().into_iter().flatten() {
                            match c["type"].as_str() {
                                Some("tool_use") => {
                                    first_tool.get_or_insert(t0.elapsed().as_millis());
                                    log::info!("agent: claude asks {} {}", c["name"].as_str().unwrap_or(""), c["input"]);
                                }
                                Some("text") => ag.say(c["text"].as_str().unwrap_or("")),
                                _ => {}
                            }
                        }
                    }
                    Some("result") => {
                        w.results += 1;
                        if w.results < w.sent {
                            continue; // an earlier order's late result
                        }
                        let bad = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success");
                        result = Some((bad, v["result"].as_str().unwrap_or("").to_string()));
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Msg::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                keep = false;
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    ag.first_tool = first_tool;
    let tok = |k: &str| usage[k].as_u64().unwrap_or(0);
    let line = format!(
        "agent: order {order} of claude pid {} ran {} ms, first tool call at {first_tool:?} ms; sent {} chars; first call tokens in {} cache read {} cache write {} out {}",
        w.child.id(),
        t0.elapsed().as_millis(),
        first.len(),
        tok("input_tokens"),
        tok("cache_read_input_tokens"),
        tok("cache_creation_input_tokens"),
        tok("output_tokens")
    );
    log::info!("{line}");
    #[cfg(test)]
    println!("{line}");
    if keep && !result.as_ref().is_some_and(|r| r.0) {
        (w.seen, w.shown) = (ag.seen.clone(), ag.shown.clone());
        // no tool call: the fallback may take the order, and its actions reach this conversation with the next one
        w.known = if ag.calls > 0 { at } else { past.last().map_or(w.known, |p| p.at.max(w.known)) };
        give_back(w);
    } else {
        release(bin);
        drop(w);
    }
    // a prose answer with no tool call ("hola") is an answer, not a failure
    if ag.calls == 0 && !ag.desk.aborted() && why.is_none() && !matches!(result, Some((false, _))) {
        restore(ag, own);
        return Err(result.map(|r| r.1).filter(|t| !t.is_empty()).unwrap_or_else(|| "no tool call".into()));
    }
    let why = why.or_else(|| match &result {
        Some((true, t)) => Some(if t.is_empty() { "Claude failed".into() } else { t.clone() }),
        None if ag.done.is_none() && !ag.capped && !ag.desk.aborted() => Some("Claude stopped with no result".into()),
        _ => None,
    });
    if ag.done.is_none() && let Some((false, t)) = &result && !t.trim().is_empty() {
        ag.done = Some(t.trim().to_string()); // finished in prose, without the done tool
    }
    Ok(finish(ag, why))
}

/// The same tools over OpenRouter function calling (a vision model; screenshots go in as user images).
fn run_openrouter(first: &str, o: &Opts, ag: &mut Agent) -> End {
    use crate::stt::{http, send_json, url};
    let t0 = Instant::now();
    let fns: Vec<Value> = tools()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| json!({ "type": "function", "function": { "name": t["name"], "description": t["description"], "parameters": t["inputSchema"] } }))
        .collect();
    let system = format!("{SYSTEM}{}", browser_note(default_browser()));
    let mut msgs = vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": first })];
    // ponytail: every screenshot stays in the history; drop old ones if long runs get slow or costly.
    for _ in 0..STEP_CAP * 3 {
        if ag.desk.aborted() || ag.done.is_some() || ag.capped || ag.took_over {
            break;
        }
        if t0.elapsed() > o.timeout {
            return finish(ag, Some(format!("No answer in {} s", o.timeout.as_secs())));
        }
        let rb = http()
            .post(url(&o.base, "chat/completions"))
            .bearer_auth(&o.key)
            .header("HTTP-Referer", "https://susurro.app")
            .header("X-Title", "susurro")
            .timeout(OR_TIMEOUT)
            .json(&json!({ "model": o.fallback_model, "messages": msgs, "tools": fns }));
        let v = match send_json(rb, "Agent") {
            Ok(v) => v,
            Err(e) => return finish(ag, Some(format!("{e:#}"))),
        };
        if let Some(e) = v.get("error") {
            return finish(ag, Some(format!("Agent: {}", e["message"].as_str().unwrap_or("error"))));
        }
        let m = v["choices"][0]["message"].clone();
        let calls = m["tool_calls"].as_array().cloned().unwrap_or_default();
        msgs.push(m.clone());
        if calls.is_empty() {
            // an answer in prose: the model is done
            ag.done.get_or_insert(m["content"].as_str().unwrap_or("").to_string());
            break;
        }
        let mut images = vec![];
        for c in &calls {
            let name = c["function"]["name"].as_str().unwrap_or("");
            let args: Value = c["function"]["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(json!({}));
            log::info!("agent (fallback): {} ms {name} {args}", t0.elapsed().as_millis());
            let out = ag.call(name, &args);
            msgs.push(json!({ "role": "tool", "tool_call_id": c["id"], "content": out.text }));
            images.extend(out.image.map(|b| crate::jev::image(&b, "image/jpeg")));
        }
        if !images.is_empty() {
            let mut content = vec![json!({ "type": "text", "text": "The screenshot:" })];
            content.extend(images);
            msgs.push(json!({ "role": "user", "content": content }));
        }
    }
    finish(ag, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::{El, Field};
    use crate::command::{Answer, Wait};
    use crate::contract::Mark;
    use std::collections::VecDeque;

    /// A fake desk: one window with a message field, Send and Attach. Records every act.
    #[derive(Default)]
    pub struct Fake {
        pub app: String,
        pub els: Vec<El>,
        pub acts: Vec<Act>,
        pub typed: Vec<String>,
        pub steps: Vec<CommandStep>,
        pub waits: Vec<Wait>,
        pub board: Vec<El>,
        pub jpeg: Vec<u8>,
        /// The front window when it is not the whole display.
        pub window: Option<PtRect>,
        pub abort: bool,
        pub secure: bool,
        /// Answer No to a confirm.
        pub deny: bool,
        /// Pablo took over by hand.
        pub paused: bool,
        /// Every sleep, in ms.
        pub slept: u64,
    }

    fn el(id: u32, name: &str, role: &str) -> El {
        let r = PtRect { x: 100.0 * id as f64, y: 500.0, w: 80.0, h: 24.0 };
        El { mark: Mark { id, rect: r, name: name.into(), role: role.into(), region: "main".into() }, state: None, h: id as usize, url: None, value: None }
    }

    pub fn chat() -> Fake {
        Fake { app: "Chat".into(), els: vec![el(1, "Message", "text field"), el(2, "Send", "button"), el(3, "Attach", "button")], jpeg: b"\xff\xd8jpeg".to_vec(), ..Default::default() }
    }

    /// JS Paint in Edge: the tools and a canvas.
    fn paint() -> Fake {
        let canvas = El { mark: Mark { id: 4, rect: PtRect { x: 100.0, y: 100.0, w: 1200.0, h: 700.0 }, name: "".into(), role: "canvas".into(), region: "main".into() }, state: None, h: 4, url: None, value: None };
        Fake { app: "Microsoft Edge".into(), els: vec![el(1, "Pencil", "button"), el(2, "Line", "button"), el(3, "OK", "button"), canvas], jpeg: b"\xff\xd8jpeg".to_vec(), ..Default::default() }
    }

    impl Desk for Fake {
        fn read(&mut self) -> Result<Screen, String> {
            let display = PtRect { x: 0.0, y: 0.0, w: 1568.0, h: 980.0 };
            let window = self.window.unwrap_or(display);
            Ok(Screen { app: self.app.clone(), display, scale: 2.0, els: self.els.clone(), window, title: "untitled".into(), ..Default::default() })
        }
        fn focused(&mut self) -> Field {
            Field { app: self.app.clone(), role: "AXTextField".into(), name: "Search".into(), secure: self.secure, ..Default::default() }
        }
        fn set_selection(&mut self, _: usize, _: usize) -> bool {
            false
        }
        fn clipboard(&mut self) -> (Option<String>, isize) {
            (None, 0)
        }
        fn set_clipboard(&mut self, _: &str) {}
        fn type_text(&mut self, s: &str) -> anyhow::Result<String> {
            self.typed.push(s.into());
            Ok(s.into())
        }
        fn screen_ok(&mut self) -> bool {
            true
        }
        fn capture(&mut self, _: PtRect, _: bool) -> anyhow::Result<Vec<u8>> {
            Ok(self.jpeg.clone())
        }
        fn act(&mut self, a: &Act) -> anyhow::Result<()> {
            self.acts.push(a.clone());
            Ok(())
        }
        fn secure_input(&mut self) -> bool {
            self.secure
        }
        fn secure_at(&mut self, _: (f64, f64)) -> bool {
            self.secure
        }
        fn emit(&mut self, s: &CommandStep) {
            if !s.marks.is_empty() {
                self.board = self.els.clone();
            }
            self.steps.push(s.clone());
        }
        fn sleep(&mut self, ms: u64) {
            self.slept += ms;
        }
        fn wait(&mut self, w: Wait, _: Duration) -> Answer {
            self.waits.push(w);
            if self.deny { Answer::No } else { Answer::Yes }
        }
        fn aborted(&self) -> bool {
            self.abort
        }
        fn paused(&self) -> bool {
            self.paused
        }
        fn board(&mut self) -> Vec<El> {
            self.board.clone()
        }
    }

    fn no_jev(_: &Value, _: &Value) -> anyhow::Result<crate::jev::Reply> {
        anyhow::bail!("no Jev in agent tests")
    }

    #[test]
    fn task_words() {
        assert_eq!(task_in("Modo agente: abre WhatsApp y manda hola."), Some("abre WhatsApp y manda hola".into()));
        assert_eq!(task_in("agent mode, click the Send button"), Some("click the Send button".into()));
        assert_eq!(task_in("modo agente"), Some(String::new()));
        assert_eq!(task_in("abre el modo agente"), None);
        assert_eq!(task_in("agente"), None);
    }

    #[test]
    fn agent_order_waits_for_the_whole_task() {
        use crate::live::{Core, Heard};
        let mut core = Core::default();
        let mut plan = |t: &str, last: bool, _: Option<&str>| Ok(super::plan(t, last).unwrap_or_default());
        assert_eq!(core.hear("Modo agente:", false, None, &mut plan), Heard::Nothing);
        assert_eq!(core.hear("abre WhatsApp", false, None, &mut plan), Heard::Nothing, "held for the next words");
        assert_eq!(core.hear("y manda hola", false, None, &mut plan), Heard::Nothing);
        match core.hear("", false, None, &mut plan) {
            Heard::Order(_, a, _) => assert_eq!((a[0].op.as_str(), a[0].text.as_deref()), ("agent", Some("abre WhatsApp y manda hola"))),
            h => panic!("{h:?}"),
        }
        assert!(core.buffer.is_empty());
    }

    #[test]
    fn agent_first_holds_any_order_until_a_pause() {
        use crate::live::{Core, Heard};
        let mut core = Core::default();
        let mut plan = |t: &str, last: bool, _: Option<&str>| Ok(hold_plan(t, t, last));
        assert_eq!(core.hear("¿Puedes dibujar", false, None, &mut plan), Heard::Nothing);
        assert_eq!(core.hear("una línea?", false, None, &mut plan), Heard::Nothing);
        match core.hear("", false, None, &mut plan) {
            Heard::Order(_, a, _) => assert_eq!((a[0].op.as_str(), a[0].text.as_deref()), ("agent", Some("¿Puedes dibujar una línea?"))),
            h => panic!("{h:?}"),
        }
    }

    #[test]
    fn local_intents_table() {
        let apps: Vec<String> = ["Slack", "Microsoft Edge", "Finder", "Docker", "ChatGPT", "MenuMeters", "WhatsApp", "Visual Studio Code"].map(String::from).to_vec();
        let li = |t: &str| local_intent(t, &apps).map(|a| (a.op, a.keys.or(a.dir).or(a.app).unwrap_or_default()));
        let is = |op: &str, x: &str| Some((op.to_string(), x.to_string()));
        assert_eq!(li("Abre Slack."), is("open_app", "Slack"));
        assert_eq!(li("cambia a edge"), is("open_app", "Microsoft Edge"));
        assert_eq!(li("switch to Finder please"), is("open_app", "Finder"));
        assert_eq!(li("ve a la app Docker"), is("open_app", "Docker"));
        assert_eq!(li("abre whatsap"), is("open_app", "WhatsApp"), "a typo of a name");
        assert_eq!(li("abre code"), is("open_app", "Visual Studio Code"), "an alias");
        assert_eq!(li("Nueva pestaña"), is("key", "cmd+t"));
        assert_eq!(li("open a new tab"), is("key", "cmd+t"));
        assert_eq!(li("baja"), is("scroll", "down"));
        assert_eq!(li("Scroll up."), is("scroll", "up"));
        assert_eq!(li("presiona enter"), is("key", "return"));
        assert_eq!(li("pulsa la tecla escape"), is("key", "escape"));
        assert_eq!(li("press tab"), is("key", "tab"));
        assert_eq!(li("Enter."), is("key", "return"));
        assert_eq!(li("pega lo último"), is("paste_last", ""));
        assert_eq!(li("copy last"), is("copy_last", ""));
        // the agent's: anything more than the bare intent, and names that are not surely an app
        for t in ["¿Puedes dibujar una línea?", "abre Slack y escribe hola", "abre el chat", "abre el menú", "abre una ventana nueva de incógnito", "abre mis descargas", "presiona el botón azul", "manda hola a Nadia", "abajo del todo a la derecha hay un botón"] {
            assert_eq!(li(t), None, "{t}");
        }
        assert!(is_stop("Stop.") && is_stop("¡para!") && is_stop("detente por favor"));
        assert!(!is_stop("para el video") && !is_stop("stop the music"));
    }

    #[test]
    fn schema_names_and_required() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["screenshot", "list_elements", "click", "double_click", "right_click", "mouse_move", "drag", "type", "key", "scroll", "open_app", "wait", "done"]);
        for x in t.as_array().unwrap() {
            assert_eq!(x["inputSchema"]["type"], "object", "{}", x["name"]);
            assert!(x["description"].as_str().unwrap().len() > 10);
        }
        assert_eq!(t[7]["inputSchema"]["required"], json!(["text"]));
        assert_eq!(t[6]["inputSchema"]["required"], json!(["path"]));
    }

    #[test]
    fn pixels_to_points_and_modifiers() {
        let r = PtRect { x: 100.0, y: 50.0, w: 640.0, h: 400.0 };
        assert_eq!(img_size(r), (1280.0, 800.0), "<= 1280 px on the long side");
        assert_eq!(to_points(r, (1280.0, 800.0)), (740.0, 450.0));
        assert_eq!(to_points(r, (0.0, 0.0)), (100.0, 50.0));
        assert_eq!(modifiers("cmd+shift"), Ok(crate::paste::CMD | crate::paste::SHIFT));
        assert_eq!(modifiers(""), Ok(0));
        assert!(modifiers("hyper").is_err());
        let p = command::drag_points(&[(0.0, 0.0), (16.0, 0.0), (16.0, 4.0)]);
        assert_eq!(p, [(8.0, 0.0), (16.0, 0.0), (16.0, 4.0)], "a point every 8 pt at most, the corner kept");
    }

    #[test]
    fn handlers_go_through_the_executor() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("send hi", true, &mut ask, &mut d, &mut mem);
        let shot = ag.call("screenshot", &json!({}));
        assert!(shot.text.contains("3 marks:\nmain: [1] Message (text field) · [2] Send (button) · [3] Attach (button)"), "{}", shot.text);
        assert!(shot.text.contains("Screenshot frame: 1280x800 px"), "{}", shot.text);
        assert_eq!(shot.image.as_deref(), Some(&b"\xff\xd8jpeg"[..]));
        assert_eq!(shot.mcp()["content"][1]["mimeType"], "image/jpeg");
        assert_eq!(ag.step, 0, "a look is not a step");
        let click = ag.call("click", &json!({ "mark": 1 }));
        assert!(!click.error);
        // every action answers with the state now and the marks: no screenshot needed
        assert!(click.text.contains("App: Chat. Window: “untitled”. Focus: TextField “Search”. Screen changed: no"), "{}", click.text);
        assert!(click.text.ends_with("The marks are the ones you got last."), "the same screen: no marks again, {}", click.text);
        assert!(!ag.call("type", &json!({ "text": "hola" })).error);
        // Send is risky: the executor asks (the fake says yes) before the click
        let send = ag.call("click", &json!({ "mark": 2 }));
        assert!(!send.error, "{}", send.text);
        assert!(!ag.call("key", &json!({ "combo": "cmd+t" })).error);
        assert!(ag.call("click", &json!({})).error);
        ag.call("done", &json!({ "summary": "Sent hola" }));
        assert_eq!(ag.done.as_deref(), Some("Sent hola"));
        assert_eq!(ag.step, 4);
        assert_eq!(ag.log.len(), 4, "every action goes to the rolling memory");
        assert!(ag.log.iter().all(|s| s.app == "Chat" && s.ok));
        drop(ag);
        let steps = &d.steps;
        assert_eq!(d.waits, [Wait::Confirm], "the risky Send asked first");
        assert_eq!(d.typed, ["hola"]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(1)), Act::Click(Target::Mark(2)), Act::Combo(17, crate::paste::CMD)]);
        assert!(steps.iter().any(|s| s.instruction.starts_with("step 3: click") && s.max == STEP_CAP), "{steps:?}");
    }

    #[test]
    fn an_action_with_done_ends_the_order() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("click attach", false, &mut ask, &mut d, &mut mem);
        ag.first_message();
        assert!(ag.call("click", &json!({ "mark": 99, "done": "Listo" })).error);
        assert_eq!(ag.done, None, "a failed action does not end it");
        assert!(!ag.call("click", &json!({ "mark": 3, "done": "Abrí adjuntar" })).error);
        assert_eq!(ag.done.as_deref(), Some("Abrí adjuntar"));
        assert!(tools()[2]["inputSchema"]["properties"]["done"].is_object() && tools()[0]["inputSchema"]["properties"]["done"].is_null());
    }

    #[test]
    fn wait_returns_once_the_screen_is_still() {
        // GUI round 1: wait {"ms":3000} on a loaded page ran the full 3 s
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("load", false, &mut ask, &mut d, &mut mem);
        ag.first_message();
        let w = ag.call("wait", &json!({ "ms": 3000 }));
        assert!(w.text.starts_with(&format!("waited {WAIT_MIN_MS} ms")) && w.text.ends_with("The marks are the ones you got last."), "{}", w.text);
        assert_eq!(ag.step, 0, "a wait is not a step");
        drop(ag);
        assert_eq!(d.slept, WAIT_MIN_MS);
    }

    #[test]
    fn open_app_of_the_front_app_is_a_no_op() {
        // GUI round 1: "abre Docker Desktop" with Docker in front cost an open -a
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("abre Chat", false, &mut ask, &mut d, &mut mem);
        ag.first_message();
        let o = ag.call("open_app", &json!({ "name": "chat" }));
        assert!(!o.error && o.text.contains("already the front app"), "{}", o.text);
        assert_eq!(ag.step, 0);
        assert!(!ag.call("open_app", &json!({ "name": "Slack" })).error);
        drop(ag);
        assert_eq!(d.acts, [Act::OpenApp("Slack".into())], "another app still opens");
    }

    #[test]
    fn the_browser_note_names_the_default_browser() {
        assert_eq!(browser_note(None), "");
        assert!(browser_note(Some("Microsoft Edge".into())).contains("browser is Microsoft Edge: a web order that names no browser"));
        #[cfg(target_os = "macos")]
        assert!(default_browser().is_some_and(|b| !b.is_empty() && !b.ends_with(".app")), "{:?}", default_browser());
    }

    #[test]
    fn the_prompt_carries_the_round_1_rules() {
        // haiku skipped the new tab (T4) and searched the Mac for a folder (T3); both models re-opened the front app
        for rule in ["\"a new tab\" is cmd+t", "cmd+shift+g", "do not call open_app", "do not call wait", "Line tool", "if it opens, it exists"] {
            assert!(SYSTEM.contains(rule), "{rule}");
        }
    }

    #[test]
    fn the_first_message_carries_the_screen() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("send hi", true, &mut ask, &mut d, &mut mem);
        let m = ag.first_message();
        assert!(m.starts_with("Order: send hi\n\nThe screen now:\nApp: Chat."), "{m}");
        assert!(m.ends_with("3 marks:\nmain: [1] Message (text field) · [2] Send (button) · [3] Attach (button)"), "{m}");
        // the marks of the first message are valid for a click at once
        assert!(!ag.call("click", &json!({ "mark": 2 })).error);
    }

    #[test]
    fn an_order_gets_the_marks_for_it_and_only_new_ones_on_a_seen_screen() {
        let (mut d, mut mem, mut ask) = (gmail(), Memory::new(), no_jev);
        let mut ag = Agent::new("open Mail 77", false, &mut ask, &mut d, &mut mem);
        let m = ag.first_message();
        assert!(m.contains("120 marks (40 for this order; list_elements for all):\nmain: [31] Mail 31"), "{m}");
        assert!(m.contains("[77] Mail 77: Weekly"), "the named mark is in, {m}");
        assert!(!m.contains("toolbar:") && !m.contains("sidebar:"), "the page before the chrome, {m}");
        assert_eq!(ag.shown.len(), 40);
        // the next order of the same conversation, the screen unchanged: only its new marks
        let (seen, shown) = (ag.seen.clone(), ag.shown.clone());
        drop(ag);
        let mut ag = Agent::new("click Label 20", false, &mut ask, &mut d, &mut mem);
        ag.first_message();
        (ag.seen, ag.shown) = (seen, shown);
        let m = ag.order_text();
        assert!(m.contains("The marks you got last, plus:\n") && m.contains("[20] Label 20 (link)"), "{m}");
        assert!(!m.contains("[77]"), "sent already, {m}");
        assert!(ag.order_text().ends_with("The marks are the ones you got last."));
        // list_elements: all of them, whatever was sent
        assert!(ag.call("list_elements", &json!({})).text.contains("120 marks:\nmain: [1]"));
    }

    #[test]
    fn computer_use_sees_the_whole_screen_after_every_action() {
        // a window in the middle of the display: computer use points over the display, not the window
        let (mut d, mut mem, mut ask) = (paint(), Memory::new(), no_jev);
        d.window = Some(PtRect { x: 400.0, y: 200.0, w: 800.0, h: 600.0 });
        let mut ag = Agent::new("dibuja una línea", false, &mut ask, &mut d, &mut mem);
        ag.vision = true;
        let m = ag.first_message();
        assert_eq!(ag.first_image.as_deref(), Some(&b"\xff\xd8jpeg"[..]), "the order carries the screen");
        assert!(m.contains("Screenshot frame: 1280x800 px.") && m.ends_with("The screenshot shows the screen now."), "no marks: {m}");
        assert!(!m.contains("[1]"), "{m}");
        let c = ag.call("click", &json!({ "x": 640, "y": 400 }));
        assert!(!c.error && c.image.is_some() && !c.text.contains("marks"), "an action answers with a screenshot: {}", c.text);
        let w = ag.call("wait", &json!({ "ms": 300 }));
        assert!(w.image.is_some());
        let s = ag.call("screenshot", &json!({}));
        assert!(s.image.is_some() && s.text.contains("whole screen") && !s.text.contains("marks"), "{}", s.text);
        assert!(!ag.call("key", &json!({ "combo": "cmd+t" })).image.is_none());
        assert!(!ag.call("type", &json!({ "text": "hola" })).error);
        assert!(!ag.call("scroll", &json!({ "dir": "down" })).error);
        drop(ag);
        let drawn: Vec<_> = d.steps.iter().filter(|st| st.status == CmdStatus::Marks).map(|st| st.instruction.clone()).collect();
        assert!(drawn.is_empty(), "no numbers drawn on screen: {drawn:?}");
        let k = 1568.0 / 1280.0;
        assert_eq!(d.acts.first(), Some(&Act::Click(Target::Point(640.0 * k, 400.0 * k))), "x,y over the display");
        // the tool list: no marks, no list_elements
        let t = tools_for(true).to_string();
        assert!(!t.contains("list_elements") && !t.contains("\"mark\"") && t.contains("whole screen"), "{t}");
        assert!(tools_for(false).to_string().contains("list_elements"));
        assert!(system_for(true).contains("computer use") && !system_for(false).contains("computer use"));
    }

    #[test]
    fn computer_use_clicks_still_ask_before_a_risky_button() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        d.els.push(el(4, "Delete", "button"));
        d.deny = true;
        let mut ag = Agent::new("borra", true, &mut ask, &mut d, &mut mem);
        ag.vision = true;
        ag.first_message();
        let r = ag.el(4).map(|e| e.mark.rect).unwrap();
        let k = 1280.0 / 1568.0;
        let o = ag.call("click", &json!({ "x": (r.x + r.w / 2.0) * k, "y": (r.y + r.h / 2.0) * k }));
        assert!(o.error && o.text.contains("said no"), "{}", o.text);
        drop(ag);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn mouse_tools() {
        let (mut d, mut mem, mut ask) = (paint(), Memory::new(), no_jev);
        let mut ag = Agent::new("dibuja una línea", false, &mut ask, &mut d, &mut mem);
        assert!(ag.call("drag", &json!({ "path": [[200, 300], [600, 300]] })).error, "no screenshot yet: no frame");
        ag.call("screenshot", &json!({}));
        let k = 1568.0 / 1280.0;
        let line = ag.call("drag", &json!({ "path": [[200, 300], [600, 300]] }));
        assert!(!line.error && line.text.starts_with("dragged from 200,300 to 600,300"), "{}", line.text);
        assert!(line.text.contains("Screen changed: no (a drawing on a canvas does not show here)"), "{}", line.text);
        assert!(ag.call("drag", &json!({ "path": [[1, 1]] })).error, "one point is no drag");
        assert!(!ag.call("drag", &json!({ "path": [[10, 10], [20, 10], [20, 20]], "modifiers": "shift" })).error);
        assert!(!ag.call("double_click", &json!({ "mark": 3 })).error);
        assert!(!ag.call("right_click", &json!({ "x": 100, "y": 50 })).error);
        assert!(!ag.call("mouse_move", &json!({ "mark": 1 })).error);
        assert!(!ag.call("click", &json!({ "mark": 2, "modifiers": "cmd" })).error);
        assert!(!ag.call("key", &json!({ "combo": "space", "hold_ms": 300 })).error);
        assert!(!ag.call("scroll", &json!({ "dir": "down", "x": 640, "y": 400 })).error);
        assert!(ag.call("click", &json!({ "mark": 2, "modifiers": "hyper" })).error);
        assert!(ag.call("key", &json!({ "combo": "cmd+shift+q", "hold_ms": 300 })).error, "blocked combos stay blocked");
        drop(ag);
        let p = |x: f64, y: f64| (x * k, y * k);
        assert_eq!(
            d.acts,
            [
                Act::Drag(vec![p(200.0, 300.0), p(600.0, 300.0)], 0),
                Act::Drag(vec![p(10.0, 10.0), p(20.0, 10.0), p(20.0, 20.0)], crate::paste::SHIFT),
                Act::ClickAt { at: (340.0, 512.0), right: false, count: 2, flags: 0 },
                Act::ClickAt { at: p(100.0, 50.0), right: true, count: 1, flags: 0 },
                Act::MouseMove(140.0, 512.0),
                Act::ClickAt { at: (240.0, 512.0), right: false, count: 1, flags: crate::paste::CMD },
                Act::KeyHold(49, 0, 300),
                Act::Scroll { down: true, at: Some(Target::Point(640.0 * k, 400.0 * k)) },
            ]
        );
    }

    #[test]
    fn risky_raw_clicks_ask_and_secure_points_block() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        d.els.push(el(4, "Delete", "button"));
        d.deny = true;
        let mut ag = Agent::new("x", true, &mut ask, &mut d, &mut mem);
        ag.call("list_elements", &json!({}));
        let r = ag.call("double_click", &json!({ "mark": 4 }));
        assert!(r.error && r.text.starts_with("The user said no"), "{}", r.text);
        drop(ag);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert!(d.acts.is_empty());
        d.secure = true;
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        ag.call("list_elements", &json!({}));
        assert!(ag.call("right_click", &json!({ "mark": 1 })).text.starts_with("Blocked"));
        drop(ag);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn point_clicks_snap_to_marks_and_secure_fields_block() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        assert!(ag.call("click", &json!({ "x": 10, "y": 10 })).error, "no screenshot yet");
        ag.call("list_elements", &json!({}));
        // (278, 418) px x 1.225 = (340.5, 512.05) pt, inside Attach (300..380, 500..524): its mark, its checks
        assert!(!ag.call("click", &json!({ "x": 278, "y": 418 })).error);
        assert!(!ag.call("click", &json!({ "x": 800, "y": 100 })).error);
        drop(ag);
        let (x, y) = to_points(PtRect { x: 0.0, y: 0.0, w: 1568.0, h: 980.0 }, (800.0, 100.0));
        assert_eq!(d.acts, [Act::Click(Target::Mark(3)), Act::Click(Target::Point(x, y))]);
        assert!((x - 980.0).abs() < 1e-9 && (y - 122.5).abs() < 1e-9);
        d.secure = true;
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        ag.call("list_elements", &json!({}));
        let t = ag.call("type", &json!({ "text": "hunter2" }));
        assert!(t.error && t.text.starts_with("Blocked"), "{}", t.text);
        drop(ag);
        assert!(d.typed.is_empty());
    }

    #[test]
    fn pablo_taking_over_stops_the_agent() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        d.paused = true;
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        let r = ag.call("click", &json!({ "mark": 2 }));
        assert!(r.error && r.text.contains("took over"));
        let end = finish(&ag, None);
        assert_eq!((end.status, end.message.as_deref()), (CmdStatus::Stopped, Some("Paused: you took over")));
        drop(ag);
        assert!(d.acts.is_empty());
    }

    #[test]
    fn step_cap() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        for _ in 0..STEP_CAP {
            assert!(!ag.call("key", &json!({ "combo": "down" })).error);
        }
        assert!(ag.call("key", &json!({ "combo": "down" })).error);
        assert!(ag.capped);
        assert_eq!(finish(&ag, None).status, CmdStatus::Unsure);
    }

    fn order(at: i64, said: &str, end: &str, steps: usize) -> Order {
        let steps = (0..steps).map(|i| Step { at: at + i as i64, app: "Slack".into(), did: format!("clicked “{i}”"), ok: i != 1 }).collect();
        Order { at, said: said.into(), end: end.into(), steps }
    }

    #[test]
    fn the_ledger_keeps_the_last_orders_with_actions() {
        let p = std::env::temp_dir().join(format!("susurro-agent-mem-{}.json", std::process::id()));
        std::fs::write(&p, r#"[{"at":1,"app":"Edge","did":"clicked","ok":true}]"#).unwrap();
        assert_eq!(load_memory(&p), vec![], "the old list of actions reads as empty");
        save_order(&p, order(1, "hola", "done", 0));
        assert!(load_memory(&p).is_empty(), "an order that did nothing has nothing to redo");
        for i in 0..35 {
            save_order(&p, order(i, "x", "done", 1));
        }
        let m = load_memory(&p);
        assert_eq!((m.len(), m[0].at, m[MEMORY_MAX - 1].at), (MEMORY_MAX, 5, 34));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_session_ends_at_a_long_pause() {
        let min = 60_000;
        let all = vec![order(0, "this morning", "done", 1), order(100 * min, "a", "done", 1), order(120 * min, "b", "done", 1)];
        let at = |v: Vec<Order>| v.iter().map(|o| o.at).collect::<Vec<_>>();
        assert_eq!(at(session(&all, 125 * min)), [100 * min, 120 * min], "the 100 min pause cut this morning off");
        assert_eq!(at(session(&all, 151 * min)), Vec::<i64>::new(), "31 min since the last order: a new session");
        assert_eq!(at(session(&all[..1], 29 * min)), [0]);
        assert!(session(&[], 0).is_empty());
    }

    #[test]
    fn the_ledger_text_names_each_order_its_end_and_its_actions() {
        assert_eq!(ledger_text(&[], 0), "");
        let t = ledger_text(&[order(0, " abre Slack ", "done", 1), order(60_000, "escribe hola en diseño", "stopped", 12)], 150_000);
        assert!(t.starts_with("Done earlier in this session (oldest first; these actions happened, do not redo them):\n"), "{t}");
        assert!(t.contains("\n- 2 min ago: “abre Slack” (done): [Slack] clicked “0”\n"), "{t}");
        assert!(ledger_text(&[order(0, "x", "done", 1)], 59_000).contains("- 59 s ago:"));
        assert!(t.contains("- 1 min ago: “escribe hola en diseño” (stopped): 4 earlier actions, then [Slack] clicked “4”; "), "{t}");
        assert!(t.ends_with("[Slack] clicked “11”\n\n"), "{t}");
        let many: Vec<Order> = (0..20).map(|i| order(i, &format!("o{i}"), "done", 1)).collect();
        let t = ledger_text(&many, 20);
        assert!(!t.contains("“o7”") && t.contains("“o8”") && t.contains("“o19”"), "the newest LEDGER_ORDERS: {t}");
        let first = ledger_text(&[order(0, "x", "done", 2)], 0);
        assert!(first.contains("[Slack] clicked “1” (failed)"), "{first}");
    }

    #[test]
    fn outcomes() {
        let e = |status, message: Option<&str>| End { status, summary: String::new(), message: message.map(String::from), steps: 1 };
        assert_eq!(outcome(&e(CmdStatus::Done, None)), "done");
        assert_eq!(outcome(&e(CmdStatus::Stopped, None)), "stopped");
        assert_eq!(outcome(&e(CmdStatus::Stopped, Some("Paused: you took over"))), "stopped: Paused: you took over");
        assert_eq!(outcome(&e(CmdStatus::Unsure, Some("Stopped at the 25-step cap"))), "not finished: Stopped at the 25-step cap");
        assert_eq!(outcome(&e(CmdStatus::Error, None)), "not finished: error");
    }

    #[test]
    fn mcp_over_http() {
        let (tx, rx) = mpsc::channel();
        let srv = Server::start(tx, false).unwrap();
        // the exec side: answer one call
        std::thread::spawn(move || {
            if let Ok(Msg::Call(name, args, reply)) = rx.recv() {
                let _ = reply.send(Out::ok(format!("{name} {args}")).mcp());
            }
        });
        let post = |body: Value, auth: bool| {
            let mut rb = crate::stt::http().post(&srv.url).json(&body);
            if auth {
                rb = rb.bearer_auth(&srv.token);
            }
            let r = rb.send().unwrap();
            (r.status().as_u16(), r.text().unwrap())
        };
        assert_eq!(post(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }), false).0, 401);
        let (s, b) = post(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }), true);
        assert_eq!(s, 200);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), true).0, 202);
        let v: Value = serde_json::from_str(&post(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }), true).1).unwrap();
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 13);
        let v: Value = serde_json::from_str(&post(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "click", "arguments": { "mark": 2 } } }), true).1).unwrap();
        assert_eq!(v["id"], 3);
        assert_eq!(v["result"]["content"][0]["text"], "click {\"mark\":2}");
        let v: Value = serde_json::from_str(&post(json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" }), true).1).unwrap();
        assert_eq!(v["error"]["code"], -32601);
        let cfg: Value = serde_json::from_str(&srv.config()).unwrap();
        assert_eq!(cfg["mcpServers"]["susurro"]["type"], "http");
        assert_eq!(srv.token.len(), 32);
    }

    #[test]
    fn claude_flags_lock_it_down() {
        let a = claude_args("sonnet", "SYS", "{}");
        let after = |f: &str| a[a.iter().position(|x| x == f).unwrap() + 1].clone();
        assert_eq!(a[0], "-p");
        assert_eq!(after("--input-format"), "stream-json", "one process, many orders");
        assert_eq!(after("--output-format"), "stream-json");
        assert_eq!(after("--system-prompt"), "SYS");
        assert_eq!(after("--tools"), "", "no built-in tools");
        assert_eq!(after("--allowedTools"), "mcp__susurro");
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--model"), "sonnet");
        assert_eq!(after("--setting-sources"), "", "no user/project settings, hooks, CLAUDE.md");
        assert!(!a.iter().any(|x| x == "--safe-mode"), "safe mode drops the --mcp-config server too (measured)");
        for f in ["--strict-mcp-config", "--verbose", "--no-session-persistence"] {
            assert!(a.iter().any(|x| x == f), "{f}");
        }
        assert!(!claude_args("", "S", "{}").iter().any(|x| x == "--model"));
    }

    /// A fake `claude`: reads its --mcp-config, then one stream-json user message per order on stdin, calls the tools
    /// over HTTP like Claude Code does, prints stream-json. Every order goes to <script>.log with its pid and system
    /// prompt. The plan depends on the order's words.
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/python3
import json, os, sys, urllib.request
a = sys.argv
cfg = json.loads(a[a.index("--mcp-config") + 1])["mcpServers"]["susurro"]
system = a[a.index("--system-prompt") + 1]
def rpc(i, method, params=None):
    body = {"jsonrpc": "2.0", "method": method, "params": params or {}}
    if i is not None: body["id"] = i
    rq = urllib.request.Request(cfg["url"], json.dumps(body).encode(), {"Content-Type": "application/json", **cfg["headers"]})
    r = urllib.request.urlopen(rq).read()
    return json.loads(r) if r else None
def say(o): print(json.dumps(o), flush=True)
say({"type": "system", "subtype": "init", "tools": [t["name"] for t in rpc(1, "tools/list")["result"]["tools"]]})
rpc(None, "notifications/initialized")
n = 0
for line in sys.stdin:
    text = json.loads(line)["message"]["content"][0]["text"]
    with open(a[0] + ".log", "a") as f:
        f.write(json.dumps({"pid": os.getpid(), "system": system, "text": text}) + "\n")
    order = text.split("\n")[0]
    if "line" in order:
        plan = [("screenshot", {}), ("drag", {"path": [[200, 300], [600, 300]]}), ("done", {"summary": "Drew a line"})]
    elif "Send" in order:
        plan = [("click", {"mark": 2}), ("done", {"summary": "Clicked Send"})]
    else:
        plan = []
    for name, args in plan:
        n += 1
        say({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t%d" % n, "name": "mcp__susurro__" + name, "input": args}]}})
        r = rpc(10 + n, "tools/call", {"name": name, "arguments": args})["result"]
        say({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t%d" % n, "content": r["content"][0]["text"]}]}})
    say({"type": "result", "subtype": "success", "is_error": False, "result": "Hola" if not plan else "ok"})
"#;

    fn script(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("susurro-{name}-{}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(p.with_extension("log"));
        p
    }

    fn opts(claude: Option<PathBuf>, base: &str) -> Opts {
        Opts { claude, model: "sonnet".into(), fallback_model: "google/gemini-3.5-flash-lite".into(), key: "sk-or".into(), base: base.into(), timeout: Duration::from_secs(20), memory: None, vision: false }
    }

    /// The fake's log: (pid, system prompt, message) per order.
    fn orders(bin: &Path) -> Vec<(u64, String, String)> {
        let log = std::fs::read_to_string(format!("{}.log", bin.display())).unwrap_or_default();
        log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).map(|v| (v["pid"].as_u64().unwrap(), v["system"].as_str().unwrap().into(), v["text"].as_str().unwrap().into())).collect()
    }

    #[test]
    fn fake_claude_drives_the_desk() {
        let bin = script("fake-claude", FAKE_CLAUDE);
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let end = run("click the Send button", &opts(Some(bin.clone()), "http://127.0.0.1:9"), true, &mut ask, &mut d, &mut mem);
        assert_eq!(end.status, CmdStatus::Done, "{end:?}");
        assert_eq!(end.summary, "agent: Clicked Send");
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert!(d.steps.iter().any(|s| s.instruction == "agent: click the Send button"));
        let _ = std::fs::remove_file(bin);
    }

    #[test]
    fn one_warm_process_takes_every_order_and_a_new_one_gets_the_session_ledger() {
        let bin = script("fake-warm", FAKE_CLAUDE);
        let mem_file = std::env::temp_dir().join(format!("susurro-agent-warm-mem-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&mem_file);
        let o = Opts { memory: Some(mem_file.clone()), ..opts(Some(bin.clone()), "http://127.0.0.1:9") };
        // F11 down: the process starts before the order is heard
        prewarm(o.clone());
        let t = Instant::now();
        while !POOL.lock().unwrap().idle.iter().any(|w| w.bin == bin) {
            assert!(t.elapsed() < Duration::from_secs(5), "prewarm never spawned");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut mem = Memory::new();
        let mut ask = no_jev;
        let mut d = chat();
        let e1 = run("click the Send button", &o, false, &mut ask, &mut d, &mut mem);
        let mut p = paint();
        let e2 = run("draw a line", &o, false, &mut ask, &mut p, &mut mem);
        assert_eq!((&e1.status, &e2.status), (&CmdStatus::Done, &CmdStatus::Done), "{e1:?} {e2:?}");
        assert_eq!(e2.summary, "agent: Drew a line");
        assert!(matches!(&p.acts[..], [Act::Drag(path, 0)] if path.len() == 2), "JS Paint: a line is a drag on the canvas, {:?}", p.acts);
        // the same window again: the conversation has its marks, the order does not repeat them
        run("draw a line", &o, false, &mut ask, &mut p, &mut mem);
        let o2 = orders(&bin);
        assert_eq!(o2.len(), 3);
        assert!(o2[2].2.ends_with("The marks are the ones you got last."), "{}", o2[2].2);
        assert_eq!(o2[0].0, o2[1].0, "the same process: it remembers the first order");
        assert!(o2[1].2.starts_with("Order: draw a line\n\nThe screen now:\nApp: Microsoft Edge."), "{}", o2[1].2);
        assert_eq!(o2[0].1, format!("{SYSTEM}{}", browser_note(default_browser())));
        assert!(o2[0].2.starts_with("Order: click the Send button"), "nothing done yet: no ledger, {}", o2[0].2);
        let st = status();
        assert!(st["warm"].as_array().unwrap().iter().any(|w| w["orders"] == 3), "{st}");
        // Esc / idle: killed; the next process gets what this session did before its order
        kill();
        let e3 = run("say hi", &o, false, &mut ask, &mut chat(), &mut mem);
        assert_eq!((e3.status, e3.summary.as_str()), (CmdStatus::Done, "agent: Hola"), "a prose answer is an answer, no fallback");
        let o3 = orders(&bin);
        assert_ne!(o3[3].0, o3[0].0, "a new process");
        assert!(o3[3].2.contains("3 marks:\nmain:"), "a new conversation gets the marks, {}", o3[3].2);
        let m = &o3[3].2;
        assert!(m.starts_with("Done earlier in this session"), "{m}");
        assert!(m.contains("“click the Send button” (done): [Chat] clicked “Send” (button, main)"), "{m}");
        assert!(m.contains("“draw a line” (done): [Microsoft Edge] dragged from 200,300 to 600,300"), "{m}");
        assert!(m.contains("\n\nOrder: say hi\n"), "{m}");
        assert_eq!(o3[3].1, o3[0].1, "the system prompt stays the same (cached)");
        // the same process again: it knows those orders now, the next one carries no ledger
        run("say hi", &o, false, &mut ask, &mut chat(), &mut mem);
        let o4 = orders(&bin);
        assert_eq!(o4[4].0, o4[3].0);
        assert!(o4[4].2.starts_with("Order: say hi"), "{}", o4[4].2);
        let saved = load_memory(&mem_file);
        assert_eq!(saved.iter().map(|o| o.said.as_str()).collect::<Vec<_>>(), ["click the Send button", "draw a line", "draw a line"], "say hi did nothing");
        kill();
        let _ = (std::fs::remove_file(&bin), std::fs::remove_file(bin.with_extension("log")), std::fs::remove_file(&mem_file));
    }

    #[test]
    fn claude_not_logged_in_falls_back_to_openrouter() {
        let bin = script("logged-out", "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n");
        let mut srv = mockito::Server::new();
        let call = |id: &str, name: &str, args: &str| {
            json!({ "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": [{ "id": id, "type": "function", "function": { "name": name, "arguments": args } }] } }] }).to_string()
        };
        let seen = Arc::new(std::sync::Mutex::new(vec![]));
        let bodies = VecDeque::from([call("a", "screenshot", "{}"), call("b", "click", "{\"mark\":2}"), call("c", "done", "{\"summary\":\"Listo\"}")]);
        let bodies = std::sync::Mutex::new(bodies);
        let s = seen.clone();
        let m = srv
            .mock("POST", "/api/v1/chat/completions")
            .match_header("authorization", "Bearer sk-or")
            .with_body_from_request(move |r| {
                s.lock().unwrap().push(serde_json::from_slice::<Value>(r.body().unwrap()).unwrap());
                bodies.lock().unwrap().pop_front().unwrap().into_bytes()
            })
            .expect(3)
            .create();
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let end = run("click the Send button", &opts(Some(bin.clone()), &format!("{}/api/v1", srv.url())), true, &mut ask, &mut d, &mut mem);
        let _ = std::fs::remove_file(bin);
        m.assert();
        assert_eq!((end.status, end.summary.as_str()), (CmdStatus::Done, "agent: Listo"));
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["model"], "google/gemini-3.5-flash-lite");
        assert_eq!(seen[0]["tools"].as_array().unwrap().len(), 13);
        assert_eq!(seen[0]["tools"][2]["function"]["name"], "click");
        assert!(seen[0]["messages"][1]["content"].as_str().unwrap().contains("[2] Send"), "the first message has the screen");
        // the second request carries the tool answer and the screenshot as a user image
        let msgs = seen[1]["messages"].as_array().unwrap();
        assert_eq!(msgs[3]["role"], "tool");
        assert!(msgs[3]["content"].as_str().unwrap().contains("[2] Send"));
        assert!(msgs[4]["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn no_claude_no_key_is_setup() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let o = Opts { key: String::new(), ..opts(None, "http://127.0.0.1:9") };
        let end = run("x", &o, true, &mut ask, &mut d, &mut mem);
        assert_eq!((end.status, end.message.as_deref()), (CmdStatus::Setup, Some("key")));
    }

    #[test]
    fn abort_kills_claude() {
        let bin = script("slow-claude", "#!/bin/sh\nsleep 30\n");
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        d.abort = true;
        let t = Instant::now();
        let end = run("x", &opts(Some(bin.clone()), "http://127.0.0.1:9"), true, &mut ask, &mut d, &mut mem);
        let _ = std::fs::remove_file(&bin);
        assert_eq!(end.status, CmdStatus::Stopped);
        assert!(t.elapsed() < Duration::from_secs(3), "killed at once");
        assert!(!POOL.lock().unwrap().idle.iter().any(|w| w.bin == bin), "an aborted order never goes back warm");
    }

    /// A Gmail-sized window: the chat's 3 marks, then 117 more (toolbar, sidebar, mail rows), as a real page has.
    fn gmail() -> Fake {
        let mut d = chat();
        let names = ["Back", "Forward", "Reload", "Address and search bar", "Inbox", "Starred", "Snoozed", "Sent", "Drafts", "More labels"];
        for id in 4..=120u32 {
            let (name, role, region) = match id {
                4..=13 => (names[(id - 4) as usize].to_string(), "button", "toolbar"),
                14..=30 => (format!("Label {id}"), "link", "sidebar"),
                _ => (format!("Mail {id}: Weekly report from the team, unread, starred"), "link", "main"),
            };
            let rect = PtRect { x: 50.0 + 90.0 * (id % 15) as f64, y: 40.0 + 30.0 * (id / 15) as f64, w: 80.0, h: 24.0 };
            d.els.push(El { mark: Mark { id, rect, name, role: role.into(), region: region.into() }, state: None, h: id as usize, url: None, value: None });
        }
        d
    }

    /// The real smoke run: Pablo's claude CLI against the MCP server, a FAKE desk only (no real clicks). Five orders
    /// in one warm process on a 120-mark window; prints each order's first tool call and total, and the medians.
    /// `cargo test --lib agent::tests::smoke_real_claude -- --ignored --nocapture` (SMOKE_MODEL=haiku to compare)
    #[test]
    #[ignore]
    fn smoke_real_claude() {
        let bin = find_claude().expect("no claude binary");
        let mut d = gmail();
        // a real JPEG (the API rejects a fake one): the app icon through sips
        let icon = concat!(env!("CARGO_MANIFEST_DIR"), "/icons/128x128.png");
        let jpg = std::env::temp_dir().join("susurro-smoke.jpg");
        std::process::Command::new("/usr/bin/sips").args(["-s", "format", "jpeg", icon, "--out"]).arg(&jpg).output().unwrap();
        d.jpeg = std::fs::read(&jpg).unwrap();
        let (mut mem, mut ask) = (Memory::new(), no_jev);
        let model = std::env::var("SMOKE_MODEL").unwrap_or("sonnet".into());
        let o = Opts { timeout: Duration::from_secs(120), model, ..opts(Some(bin.clone()), "http://127.0.0.1:9") };
        prewarm(o.clone());
        std::thread::sleep(Duration::from_secs(5)); // Pablo still talks; the process is up
        let (mut first, mut total) = (vec![], vec![]);
        for task in ["click the Send button", "now click the Attach button", "click the Message field", "click Send again", "click Attach again"] {
            // Pablo speaks the next order: the turn of the last one ends meanwhile (its reply to the done answer)
            std::thread::sleep(Duration::from_secs(4));
            let t = Instant::now();
            let mut ag = Agent::new(task, false, &mut ask, &mut d, &mut mem);
            let msg = ag.first_message();
            let end = run_claude(&bin, &msg, &[], crate::history::now_ms(), &o, &mut ag).unwrap();
            let f = ag.first_tool.unwrap_or(0);
            let ms = t.elapsed().as_millis();
            println!("SMOKE {task:?}: {:?} first tool {f} ms, total {ms} ms, sent {} chars: {}", end.status, msg.len(), end.summary);
            first.push(f);
            total.push(ms);
        }
        kill();
        let med = |v: &mut Vec<u128>| {
            v.sort();
            v[v.len() / 2]
        };
        println!("SMOKE median first tool {} ms, total {} ms", med(&mut first), med(&mut total));
        println!("SMOKE acts {:?}", d.acts);
        assert_eq!(d.acts, [2, 3, 1, 2, 3].map(|m| Act::Click(Target::Mark(m))));
    }
}
