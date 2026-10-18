//! Agent mode (`.lo/next-queue.md` 9b): "modo agente: <task>", or an order whose fast path failed twice, goes to
//! Pablo's local Claude Code (`claude -p`, his subscription) as the agent loop. susurro serves its own tools over MCP
//! (HTTP on 127.0.0.1, a random token per run); every tool call runs on the exec thread through `command::exec`
//! (verify + real-click fallback, risky confirm by voice/F11, password fields blocked). No claude binary, or claude
//! fails before its first tool call (not logged in): the same tools over OpenRouter function calling.

use crate::command::{self, Act, Ask, Cfg, Desk, End, Memory, Screen, Target};
use crate::contract::{CmdStatus, CommandSettings, CommandStep, PtRect};
use crate::planner::Action;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Actions per run (screenshots, list_elements and wait do not count).
pub const STEP_CAP: u32 = 25;
pub const TIMEOUT: Duration = Duration::from_secs(300);
const WAIT_MAX_MS: u64 = 5000;
/// An MCP tool call waits this long for the exec thread (a risky confirm alone waits 10 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const BODY_MAX: usize = 1 << 20;
const PROTOCOL: &str = "2025-06-18";
/// The OpenRouter fallback's request timeout.
const OR_TIMEOUT: Duration = Duration::from_secs(60);

pub const SYSTEM: &str = "You control the user's Mac with the susurro tools, and only with them. \
Start with screenshot. The numbered marks are the clickable elements of the front window: prefer click {mark}. \
Click a text field before you type into it. After an action that changes the screen, take a screenshot to check it. \
Mark numbers change after every action: use the ones from the last screenshot or list_elements. \
Risky actions (send, delete, buy) ask the user first; if a tool says the user said no, stop. \
Never type passwords. Use as few steps as you can. \
When the task is done, or cannot be done, call done with one short sentence for the user, in the user's language.";

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
/// a pause or the release, so the task can span chunks. "modo agente" alone waits for the task. None = not agent mode.
pub fn plan(text: &str, last: bool) -> Option<crate::planner::Plan> {
    let task = task_in(text)?;
    let actions = if task.is_empty() { vec![] } else { vec![Action { op: "agent".into(), text: Some(task), ..Default::default() }] };
    Some(crate::planner::Plan { complete: last, rest: if last { String::new() } else { text.to_string() }, actions, ..Default::default() })
}

/// The tool schema: MCP `tools/list`, and the OpenRouter fallback's functions.
pub fn tools() -> Value {
    let mark = json!({ "type": "integer", "description": "A mark number from the last screenshot or list_elements" });
    let none = json!({ "type": "object", "properties": {} });
    json!([
        { "name": "screenshot", "description": "The front window as an image with the numbered marks drawn on it, and the marks as text. Call it first, and after actions to check them.", "inputSchema": none },
        { "name": "list_elements", "description": "The marks of the front window as text: [N] name (role, region). Faster than a screenshot.", "inputSchema": none },
        { "name": "click", "description": "Click a mark (preferred), or a point x,y in screenshot pixels.", "inputSchema": { "type": "object", "properties": { "mark": mark, "x": { "type": "number" }, "y": { "type": "number" } } } },
        { "name": "type", "description": "Type text into the focused field. Click the field first.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } },
        { "name": "key", "description": "Press a key or a combo: return, escape, tab, down, cmd+t, cmd+shift+n.", "inputSchema": { "type": "object", "properties": { "combo": { "type": "string" } }, "required": ["combo"] } },
        { "name": "scroll", "description": "Scroll the front window, or the area of a mark.", "inputSchema": { "type": "object", "properties": { "dir": { "type": "string", "enum": ["up", "down"] }, "mark": mark }, "required": ["dir"] } },
        { "name": "open_app", "description": "Open or bring an app to the front by its name.", "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] } },
        { "name": "wait", "description": "Wait for the screen to settle (a page load). At most 5000 ms.", "inputSchema": { "type": "object", "properties": { "ms": { "type": "integer" } }, "required": ["ms"] } },
        { "name": "done", "description": "The task is finished, or cannot be done: one short sentence for the user, in the user's language.", "inputSchema": { "type": "object", "properties": { "summary": { "type": "string" } }, "required": ["summary"] } }
    ])
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

/// The screenshot size `capture(r, false)` returns: sips fits the longest side to IMG_MAX (it also scales up).
pub fn img_size(r: PtRect) -> (f64, f64) {
    let k = command::IMG_MAX / r.w.max(r.h).max(1.0);
    ((r.w * k).round(), (r.h * k).round())
}

/// Screenshot pixels -> global points.
pub fn to_points(r: PtRect, px: (f64, f64)) -> (f64, f64) {
    let k = r.w.max(r.h).max(1.0) / command::IMG_MAX;
    (r.x + px.0 * k, r.y + px.1 * k)
}

/// One run's tool handlers over the executor.
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
}

impl<'a, 'b> Agent<'a, 'b> {
    pub fn new(task: &str, confirm_risky: bool, ask: &'a mut Ask<'b>, desk: &'a mut dyn Desk, mem: &'a mut Memory) -> Self {
        // the agent picks its own targets: no "Which one?" per click
        let cfg = Cfg { confirm_risky, choose: false, max: STEP_CAP };
        Agent { cfg, task: task.into(), ask, desk, mem, step: 0, calls: 0, shot: None, done: None, capped: false, did: vec![] }
    }

    /// A pill line between actions.
    pub fn say(&mut self, text: &str) {
        let st = CommandStep { instruction: text.chars().take(120).collect(), step: self.step, max: self.cfg.max, status: CmdStatus::Thinking, ..Default::default() };
        self.desk.emit(&st);
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Out {
        self.calls += 1;
        if self.desk.aborted() {
            return Out::err("Stopped by the user.");
        }
        let s = |k: &str| args[k].as_str().map(String::from);
        let mark = |k: &str| args[k].as_u64().map(|m| m as u32);
        match name {
            "screenshot" => self.look(true),
            "list_elements" => self.look(false),
            "wait" => {
                self.desk.sleep(args["ms"].as_u64().unwrap_or(500).min(WAIT_MAX_MS));
                Out::ok("waited")
            }
            "done" => {
                self.done = Some(s("summary").unwrap_or_default());
                Out::ok("ok")
            }
            "click" => match (mark("mark"), args["x"].as_f64(), args["y"].as_f64()) {
                (Some(m), _, _) => self.act(Action { op: "click".into(), target: Some(self.label(m)), ..Default::default() }),
                (None, Some(x), Some(y)) => self.click_point(x, y),
                _ => Out::err("click needs mark, or x and y"),
            },
            "type" => self.act(Action { op: "type".into(), text: s("text"), ..Default::default() }),
            "key" => self.act(Action { op: "key".into(), keys: s("combo"), ..Default::default() }),
            "scroll" => {
                let target = mark("mark").map(|m| self.label(m));
                self.act(Action { op: "scroll".into(), dir: s("dir"), target, ..Default::default() })
            }
            "open_app" => self.act(Action { op: "open_app".into(), app: s("name"), ..Default::default() }),
            n => Out::err(format!("unknown tool {n}")),
        }
    }

    /// "[12] Send (button, main)": the target text the executor grounds (planner style), else the bare number.
    fn label(&self, id: u32) -> String {
        match self.shot.as_ref().and_then(|s| s.els.iter().find(|e| e.mark.id == id)) {
            Some(e) => format!("[{id}] {}", crate::ax::describe(e)),
            None => id.to_string(),
        }
    }

    fn frame(s: &Screen) -> PtRect {
        if s.window.w > 0.0 { s.window } else { s.display }
    }

    fn look(&mut self, image: bool) -> Out {
        let scr = match self.desk.read() {
            Ok(s) => s,
            Err(e) if e == "accessibility" => return Out::err("susurro has no Accessibility permission. Call done."),
            Err(e) => return Out::err(e),
        };
        let win = Self::frame(&scr);
        let marks: Vec<String> = scr.els.iter().map(|e| format!("[{}] {}", e.mark.id, crate::ax::describe(e))).collect();
        let mut head = format!("App: {}. {} marks.", scr.app, marks.len());
        let mut out = Out::default();
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
                match self.desk.capture(win, false) {
                    Ok(b) => {
                        let (w, h) = img_size(win);
                        head += &format!(" Image: the front window, {w}x{h} px (click x,y are in these pixels).");
                        out.image = Some(b);
                    }
                    Err(e) => head += &format!(" No image: {e:#}."),
                }
            } else {
                head += " No image: susurro has no Screen Recording permission. Use the marks.";
            }
        }
        self.shot = Some(scr);
        out.text = format!("{head}\n{}", marks.join("\n"));
        out
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
        let end = command::exec(&a, self.step, 0, &self.cfg, self.ask, self.desk, self.mem);
        self.report(end)
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

    /// A point click: the mark under it when there is one (its checks, its verify), else a raw click.
    fn click_point(&mut self, x: f64, y: f64) -> Out {
        let Some(scr) = self.shot.clone() else { return Out::err("Take a screenshot first.") };
        let p = to_points(Self::frame(&scr), (x, y));
        if let Some(id) = command::snap(p, &scr.els).map(|e| e.mark.id) {
            return self.act(Action { op: "click".into(), target: Some(self.label(id)), ..Default::default() });
        }
        if !self.next_step() {
            return Out::err("Step cap reached. Call done.");
        }
        if self.desk.secure_at(p) {
            return Out::err("Blocked: a password field. susurro won't touch it.");
        }
        let st = CommandStep { instruction: format!("step {}: click at {x:.0},{y:.0}", self.step), step: self.step, max: self.cfg.max, status: CmdStatus::Done, ..Default::default() };
        if let Err(e) = self.desk.act(&Act::Click(Target::Point(p.0, p.1))) {
            return Out::err(format!("error: {e:#}"));
        }
        self.desk.sleep(command::SETTLE_MS);
        self.desk.emit(&st);
        self.did.push(format!("clicked at {x:.0},{y:.0}"));
        Out::ok(format!("clicked at {x:.0},{y:.0}"))
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
    pub fn start(tx: mpsc::Sender<Msg>) -> std::io::Result<Server> {
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
                    if let Err(e) = serve(c, &t, &tx) {
                        log::debug!("agent mcp: {e}");
                    }
                });
            }
        })?;
        Ok(Server { url: format!("http://{addr}/mcp"), token: tok, addr, stop })
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

fn serve(c: TcpStream, token: &str, tx: &mpsc::Sender<Msg>) -> std::io::Result<()> {
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
                Ok(req) => match rpc(&req, tx) {
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
pub fn rpc(req: &Value, tx: &mpsc::Sender<Msg>) -> Option<Value> {
    let id = req.get("id")?.clone();
    let p = &req["params"];
    let result: Result<Value, (i32, String)> = match req["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": p["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "susurro", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
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

pub struct Opts {
    /// The claude binary. None = straight to the fallback.
    pub claude: Option<PathBuf>,
    pub model: String,
    pub fallback_model: String,
    /// The OpenRouter key (fallback). Empty = no fallback.
    pub key: String,
    pub base: String,
    pub timeout: Duration,
}

impl Opts {
    pub fn from(s: &CommandSettings, key: &str) -> Self {
        Opts { claude: find_claude(), model: s.agent_model.clone(), fallback_model: s.agent_fallback.clone(), key: key.into(), base: crate::stt::OPENROUTER_BASE.into(), timeout: TIMEOUT }
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

/// `claude -p` locked down to susurro's tools: no built-in tools (no Bash, Read, Write, Edit, WebFetch), no other MCP
/// server, no user/project settings (hooks, permissions; CLAUDE.md measured not loaded: ~2k prompt tokens), no
/// prompts (dontAsk), nothing saved. Not --safe-mode: it drops the --mcp-config server too (measured on 2.1.296), and
/// not --bare: it never reads the subscription login.
pub fn claude_args(task: &str, model: &str, mcp_config: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p", task, "--output-format", "stream-json", "--verbose", "--model", model, "--system-prompt", SYSTEM, "--mcp-config", mcp_config,
        "--strict-mcp-config", "--tools", "", "--allowedTools", "mcp__susurro", "--permission-mode", "dontAsk", "--permission-prompts", "none",
        "--setting-sources", "", "--disable-slash-commands", "--no-session-persistence",
    ]
    .map(String::from)
    .to_vec();
    if model.trim().is_empty() {
        let i = a.iter().position(|x| x == "--model").unwrap();
        a.drain(i..i + 2);
    }
    a
}

/// Runs one agent task to its end. `confirm_risky`: the Voice control setting.
pub fn run(task: &str, o: &Opts, confirm_risky: bool, ask: &mut Ask, desk: &mut dyn Desk, mem: &mut Memory) -> End {
    let t0 = Instant::now();
    let mut ag = Agent::new(task, confirm_risky, ask, desk, mem);
    ag.say(&format!("agent: {task}"));
    let claude = o.claude.as_deref().map(|b| run_claude(b, task, o, &mut ag));
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
                run_openrouter(task, o, &mut ag)
            }
        }
    };
    log::info!("agent: {:?} in {} ms, {} steps, {} calls: {} {:?}", end.status, t0.elapsed().as_millis(), ag.step, ag.calls, end.summary, end.message);
    End { summary: if end.summary.is_empty() { end.summary } else { format!("agent: {}", end.summary) }, ..end }
}

/// How a run ended, before the End.
fn finish(ag: &Agent, why: Option<String>) -> End {
    let mk = |status, summary: String, message: Option<String>| End { status, summary, message, steps: ag.step };
    if ag.desk.aborted() {
        return mk(CmdStatus::Stopped, String::new(), None);
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

/// Err = claude failed before any tool call (missing login, a bad flag): the fallback may run.
fn run_claude(bin: &Path, task: &str, o: &Opts, ag: &mut Agent) -> Result<End, String> {
    use std::process::{Command, Stdio};
    let (tx, rx) = mpsc::channel();
    let srv = Server::start(tx.clone()).map_err(|e| format!("mcp server: {e}"))?;
    // An empty folder: no project CLAUDE.md, .mcp.json or settings to pick up.
    let dir = std::env::temp_dir().join("susurro-agent");
    let _ = std::fs::create_dir_all(&dir);
    let t0 = Instant::now();
    let mut child = Command::new(bin)
        .args(claude_args(task, &o.model, &srv.config()))
        .current_dir(&dir)
        .env_remove("ANTHROPIC_API_KEY") // his subscription login, never a stray key
        .stdin(Stdio::null())
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
    let mut result: Option<(bool, String)> = None;
    let mut first_tool: Option<u128> = None;
    let mut why = None;
    loop {
        if ag.desk.aborted() || ag.done.is_some() || ag.capped {
            break;
        }
        if t0.elapsed() > o.timeout {
            why = Some(format!("No answer in {} s", o.timeout.as_secs()));
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Call(name, args, reply)) => {
                log::info!("agent: {} ms {name} {args}", t0.elapsed().as_millis());
                let out = ag.call(&name, &args);
                let _ = reply.send(out.mcp());
            }
            Ok(Msg::Line(l)) => {
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                match v["type"].as_str() {
                    Some("assistant") => {
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
                        let bad = v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success");
                        result = Some((bad, v["result"].as_str().unwrap_or("").to_string()));
                    }
                    _ => {}
                }
            }
            Ok(Msg::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(srv);
    log::info!("agent: claude ran {} ms, first tool call at {first_tool:?} ms", t0.elapsed().as_millis());
    if ag.calls == 0 && !ag.desk.aborted() && why.is_none() {
        return Err(result.map(|r| r.1).filter(|t| !t.is_empty()).unwrap_or_else(|| "no tool call".into()));
    }
    let why = why.or_else(|| match &result {
        Some((true, t)) => Some(if t.is_empty() { "Claude failed".into() } else { t.clone() }),
        None if ag.done.is_none() && !ag.capped => Some("Claude stopped with no result".into()),
        _ => None,
    });
    if ag.done.is_none() && let Some((false, t)) = &result && !t.trim().is_empty() {
        ag.done = Some(t.trim().to_string()); // finished in prose, without the done tool
    }
    Ok(finish(ag, why))
}

/// The same tools over OpenRouter function calling (a vision model; screenshots go in as user images).
fn run_openrouter(task: &str, o: &Opts, ag: &mut Agent) -> End {
    use crate::stt::{http, send_json, url};
    let t0 = Instant::now();
    let fns: Vec<Value> = tools()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| json!({ "type": "function", "function": { "name": t["name"], "description": t["description"], "parameters": t["inputSchema"] } }))
        .collect();
    let mut msgs = vec![json!({ "role": "system", "content": SYSTEM }), json!({ "role": "user", "content": task })];
    // ponytail: every screenshot stays in the history; drop old ones if long runs get slow or costly.
    for _ in 0..STEP_CAP * 3 {
        if ag.desk.aborted() || ag.done.is_some() || ag.capped {
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
        pub els: Vec<El>,
        pub acts: Vec<Act>,
        pub typed: Vec<String>,
        pub steps: Vec<CommandStep>,
        pub waits: Vec<Wait>,
        pub board: Vec<El>,
        pub jpeg: Vec<u8>,
        pub abort: bool,
        pub secure: bool,
    }

    fn el(id: u32, name: &str, role: &str) -> El {
        let r = PtRect { x: 100.0 * id as f64, y: 500.0, w: 80.0, h: 24.0 };
        El { mark: Mark { id, rect: r, name: name.into(), role: role.into(), region: "main".into() }, state: None, h: id as usize, url: None, value: None }
    }

    pub fn chat() -> Fake {
        Fake { els: vec![el(1, "Message", "text field"), el(2, "Send", "button"), el(3, "Attach", "button")], jpeg: b"\xff\xd8jpeg".to_vec(), ..Default::default() }
    }

    impl Desk for Fake {
        fn read(&mut self) -> Result<Screen, String> {
            let window = PtRect { x: 0.0, y: 0.0, w: 1568.0, h: 980.0 };
            Ok(Screen { app: "Chat".into(), display: window, scale: 2.0, els: self.els.clone(), window, ..Default::default() })
        }
        fn focused(&mut self) -> Field {
            Field { app: "Chat".into(), role: "AXTextField".into(), secure: self.secure, ..Default::default() }
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
        fn sleep(&mut self, _: u64) {}
        fn wait(&mut self, w: Wait, _: Duration) -> Answer {
            self.waits.push(w);
            Answer::Yes
        }
        fn aborted(&self) -> bool {
            self.abort
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
    fn schema_names_and_required() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["screenshot", "list_elements", "click", "type", "key", "scroll", "open_app", "wait", "done"]);
        for x in t.as_array().unwrap() {
            assert_eq!(x["inputSchema"]["type"], "object", "{}", x["name"]);
            assert!(x["description"].as_str().unwrap().len() > 10);
        }
        assert_eq!(t[3]["inputSchema"]["required"], json!(["text"]));
    }

    #[test]
    fn pixels_to_points() {
        let r = PtRect { x: 100.0, y: 50.0, w: 784.0, h: 490.0 };
        assert_eq!(img_size(r), (1568.0, 980.0));
        assert_eq!(to_points(r, (1568.0, 980.0)), (884.0, 540.0));
        assert_eq!(to_points(r, (0.0, 0.0)), (100.0, 50.0));
    }

    #[test]
    fn handlers_go_through_the_executor() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("send hi", true, &mut ask, &mut d, &mut mem);
        let shot = ag.call("screenshot", &json!({}));
        assert!(shot.text.contains("[2] Send (button, main)"), "{}", shot.text);
        assert!(shot.text.contains("1568x980 px"));
        assert_eq!(shot.image.as_deref(), Some(&b"\xff\xd8jpeg"[..]));
        assert_eq!(shot.mcp()["content"][1]["mimeType"], "image/jpeg");
        assert_eq!(ag.step, 0, "a look is not a step");
        assert!(!ag.call("click", &json!({ "mark": 1 })).error);
        assert!(!ag.call("type", &json!({ "text": "hola" })).error);
        // Send is risky: the executor asks (the fake says yes) before the click
        let send = ag.call("click", &json!({ "mark": 2 }));
        assert!(!send.error, "{}", send.text);
        assert!(!ag.call("key", &json!({ "combo": "cmd+t" })).error);
        assert!(ag.call("click", &json!({})).error);
        ag.call("done", &json!({ "summary": "Sent hola" }));
        assert_eq!(ag.done.as_deref(), Some("Sent hola"));
        assert_eq!(ag.step, 4);
        drop(ag);
        let steps = &d.steps;
        assert_eq!(d.waits, [Wait::Confirm], "the risky Send asked first");
        assert_eq!(d.typed, ["hola"]);
        assert_eq!(d.acts, [Act::Click(Target::Mark(1)), Act::Click(Target::Mark(2)), Act::Combo(17, crate::paste::CMD)]);
        assert!(steps.iter().any(|s| s.instruction.starts_with("step 3: click") && s.max == STEP_CAP), "{steps:?}");
    }

    #[test]
    fn point_clicks_snap_to_marks_and_secure_fields_block() {
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        assert!(ag.call("click", &json!({ "x": 10, "y": 10 })).error, "no screenshot yet");
        ag.call("list_elements", &json!({}));
        // (340, 512) is inside Attach (300..380, 500..524): its mark, its checks
        assert!(!ag.call("click", &json!({ "x": 340, "y": 512 })).error);
        assert!(!ag.call("click", &json!({ "x": 900, "y": 100 })).error);
        drop(ag);
        assert_eq!(d.acts, [Act::Click(Target::Mark(3)), Act::Click(Target::Point(900.0, 100.0))]);
        d.secure = true;
        let mut ag = Agent::new("x", false, &mut ask, &mut d, &mut mem);
        ag.call("list_elements", &json!({}));
        let t = ag.call("type", &json!({ "text": "hunter2" }));
        assert!(t.error && t.text.starts_with("Blocked"), "{}", t.text);
        drop(ag);
        assert!(d.typed.is_empty());
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

    #[test]
    fn mcp_over_http() {
        let (tx, rx) = mpsc::channel();
        let srv = Server::start(tx).unwrap();
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
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 9);
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
        let a = claude_args("click Send", "sonnet", "{}");
        let after = |f: &str| a[a.iter().position(|x| x == f).unwrap() + 1].clone();
        assert_eq!(after("-p"), "click Send");
        assert_eq!(after("--tools"), "", "no built-in tools");
        assert_eq!(after("--allowedTools"), "mcp__susurro");
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--output-format"), "stream-json");
        assert_eq!(after("--model"), "sonnet");
        assert_eq!(after("--setting-sources"), "", "no user/project settings, hooks, CLAUDE.md");
        assert!(!a.iter().any(|x| x == "--safe-mode"), "safe mode drops the --mcp-config server too (measured)");
        for f in ["--strict-mcp-config", "--verbose", "--no-session-persistence"] {
            assert!(a.iter().any(|x| x == f), "{f}");
        }
        assert!(!claude_args("x", "", "{}").iter().any(|x| x == "--model"));
    }

    /// A fake `claude`: reads its --mcp-config, calls the tools over HTTP like Claude Code does, prints stream-json.
    const FAKE_CLAUDE: &str = r#"#!/usr/bin/python3
import json, sys, urllib.request
a = sys.argv
cfg = json.loads(a[a.index("--mcp-config") + 1])["mcpServers"]["susurro"]
def rpc(i, method, params=None):
    body = {"jsonrpc": "2.0", "method": method, "params": params or {}}
    if i is not None: body["id"] = i
    rq = urllib.request.Request(cfg["url"], json.dumps(body).encode(), {"Content-Type": "application/json", **cfg["headers"]})
    r = urllib.request.urlopen(rq).read()
    return json.loads(r) if r else None
def say(o): print(json.dumps(o), flush=True)
say({"type": "system", "subtype": "init", "tools": [t["name"] for t in rpc(1, "tools/list")["result"]["tools"]]})
rpc(None, "notifications/initialized")
plan = [("screenshot", {}), ("click", {"mark": 2}), ("done", {"summary": "Clicked Send"})]
for n, (name, args) in enumerate(plan):
    say({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t%d" % n, "name": "mcp__susurro__" + name, "input": args}]}})
    r = rpc(10 + n, "tools/call", {"name": name, "arguments": args})["result"]
    say({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t%d" % n, "content": r["content"][0]["text"]}]}})
say({"type": "result", "subtype": "success", "is_error": False, "result": "Clicked Send"})
"#;

    fn script(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("susurro-{name}-{}", std::process::id()));
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn opts(claude: Option<PathBuf>, base: &str) -> Opts {
        Opts { claude, model: "sonnet".into(), fallback_model: "google/gemini-3.5-flash-lite".into(), key: "sk-or".into(), base: base.into(), timeout: Duration::from_secs(20) }
    }

    #[test]
    fn fake_claude_drives_the_desk() {
        let bin = script("fake-claude", FAKE_CLAUDE);
        let (mut d, mut mem, mut ask) = (chat(), Memory::new(), no_jev);
        let end = run("click the Send button", &opts(Some(bin.clone()), "http://127.0.0.1:9"), true, &mut ask, &mut d, &mut mem);
        let _ = std::fs::remove_file(bin);
        assert_eq!(end.status, CmdStatus::Done, "{end:?}");
        assert_eq!(end.summary, "agent: Clicked Send");
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        assert_eq!(d.waits, [Wait::Confirm]);
        assert!(d.steps.iter().any(|s| s.instruction == "agent: click the Send button"));
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
        assert_eq!(seen[0]["tools"].as_array().unwrap().len(), 9);
        assert_eq!(seen[0]["tools"][2]["function"]["name"], "click");
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
        let _ = std::fs::remove_file(bin);
        assert_eq!(end.status, CmdStatus::Stopped);
        assert!(t.elapsed() < Duration::from_secs(3), "killed at once");
    }

    /// The real smoke run: Pablo's claude CLI against the MCP server, a FAKE desk only (no real clicks).
    /// `cargo test --lib agent::tests::smoke_real_claude -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn smoke_real_claude() {
        let bin = find_claude().expect("no claude binary");
        let mut d = chat();
        // a real JPEG (the API rejects a fake one): the app icon through sips
        let icon = concat!(env!("CARGO_MANIFEST_DIR"), "/icons/128x128.png");
        let jpg = std::env::temp_dir().join("susurro-smoke.jpg");
        std::process::Command::new("/usr/bin/sips").args(["-s", "format", "jpeg", icon, "--out"]).arg(&jpg).output().unwrap();
        d.jpeg = std::fs::read(&jpg).unwrap();
        let (mut mem, mut ask) = (Memory::new(), no_jev);
        let o = Opts { timeout: Duration::from_secs(120), ..opts(Some(bin), "http://127.0.0.1:9") };
        let t = Instant::now();
        let end = run("click the Send button", &o, true, &mut ask, &mut d, &mut mem);
        println!("SMOKE {:?} in {} ms: {} {:?}", end.status, t.elapsed().as_millis(), end.summary, end.message);
        println!("SMOKE acts {:?}", d.acts);
        for s in &d.steps {
            println!("SMOKE step {} {:?} {}", s.step, s.status, s.instruction);
        }
        assert_eq!(d.acts, [Act::Click(Target::Mark(2))]);
        assert_eq!(end.status, CmdStatus::Done);
    }
}
