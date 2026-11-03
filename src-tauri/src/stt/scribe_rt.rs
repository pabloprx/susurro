//! ElevenLabs Scribe v2 Realtime over a WebSocket (next-queue 1). Docs read 2026-10-17:
//! elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime and
//! .../realtime/transcripts-and-commit-strategies.
//! - `wss://api.elevenlabs.io/v1/speech-to-text/realtime?model_id=scribe_v2_realtime&audio_format=pcm_<rate>&commit_strategy=manual`
//!   (+ `language_code`, `keyterms` repeated, max 50, +20% cost).
//! - Auth: the `xi-api-key` header. Single-use tokens are for browsers; this desktop app holds the key itself.
//! - Send `{"message_type":"input_audio_chunk","audio_base_64":..,"commit":bool,"sample_rate":rate}`; `previous_text`
//!   only on the first chunk of a connection (a reconnect sends the last committed words, < 50 chars).
//! - Receive `session_started`, `partial_transcript {text}`, `committed_transcript {text}`, or an error type with `error`.
//! - Manual commits at susurro's own pause cuts (audio.rs Chunker): one commit = one `committed_transcript`.
//!   The server also commits by itself after ~36 s of audio; the dictation cuts commit well before that.
//!
//! The same session runs OpenAI realtime transcription (`Proto::OpenAi`). Docs read 2026-10-17:
//! developers.openai.com/api/docs/guides/realtime-transcription and .../models/gpt-live-transcribe.
//! - `wss://api.openai.com/v1/realtime?intent=transcription`, `Authorization: Bearer <key>`.
//! - First `session.update` with `session.type = "transcription"`, model `gpt-live-transcribe`, `audio/pcm` at
//!   24 kHz (the only rate), `turn_detection: null` (the model has no server VAD: susurro's pause cuts commit),
//!   `keywords`, `languages`, `delay`, and `prompt` = the last committed words after a reconnect.
//! - Send `input_audio_buffer.append {audio}` and `input_audio_buffer.commit`. Receive
//!   `input_audio_buffer.committed {item_id}` (commit order), `conversation.item.input_audio_transcription.delta
//!   {item_id, delta}` and `.completed {item_id, transcript}` (`.failed` = an empty answer), or `error {error}`.
//!   Completions of different turns may come out of order: `Order` puts them back in commit order.
//! - GPT-Live-1 (`/v1/live/sessions`) is a speaking voice agent, not a transcriber; OpenRouter has neither (2026-10-17).

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub const URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";
pub const OPENAI_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
const OPENAI_MODEL: &str = "gpt-live-transcribe";
/// `audio/pcm` in a transcription session is 24 kHz only.
const OPENAI_RATE: u32 = 24000;
/// ponytail: "low" picked from the docs' list ("low latency live captions"), not measured; benchmark minimal/low/medium.
const OPENAI_DELAY: &str = "low";
const MODEL: &str = "scribe_v2_realtime";
/// The `pcm_<rate>` formats the API takes. Another device rate is resampled to 16 kHz.
const RATES: [u32; 6] = [8000, 16000, 22050, 24000, 44100, 48000];
const MAX_KEYTERMS: usize = 50;
/// Reconnects in a row before the session gives up (the caller falls back to batch STT).
const MAX_RETRIES: u32 = 2;
/// The read poll: outgoing audio waits at most this long.
const POLL: Duration = Duration::from_millis(15);

#[derive(Debug, Clone, PartialEq)]
pub enum Ev {
    /// Interim words of the open segment (they may still change).
    Partial(String),
    /// The final words of one commit.
    Committed(String),
    /// The socket failed and the reconnects failed too: fall back to batch STT.
    Failed(String),
    /// After `finish`: every commit answered, the socket closed.
    Closed,
}

/// The wire protocol of the realtime server.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Proto {
    Scribe,
    OpenAi,
}

#[derive(Debug, Clone)]
pub struct Opts {
    pub proto: Proto,
    pub url: String,
    pub key: String,
    /// The device rate of the samples given to `send`.
    pub rate: u32,
    /// None or "auto" = detect (Scribe: among `AUTO_LANGUAGES`).
    pub language: Option<String>,
    pub keyterms: Vec<String>,
}

enum Out {
    Audio(Vec<u8>, bool),
    Finish,
}

/// One realtime session. Dropping it abandons the socket at once (Esc): no wait for the server.
pub struct Live {
    tx: mpsc::Sender<Out>,
    rate: u32,
    wire: u32,
}

impl Live {
    /// Connects on its own thread (the recording never waits for the handshake); audio sent before the socket is
    /// up waits in the channel. `on` gets every event, on that thread.
    pub fn start(o: Opts, on: impl Fn(Ev) + Send + 'static) -> Live {
        let (tx, rx) = mpsc::channel();
        let wire = match o.proto {
            Proto::OpenAi => OPENAI_RATE,
            Proto::Scribe if RATES.contains(&o.rate) => o.rate,
            Proto::Scribe => crate::audio::RATE,
        };
        let rate = o.rate;
        std::thread::Builder::new().name("scribe-rt".into()).spawn(move || run(&o, wire, &rx, &on)).expect("spawn scribe-rt");
        Live { tx, rate, wire }
    }

    /// Samples at the device rate. `commit` = a pause cut: close the segment.
    /// ponytail: a rate the API lacks is resampled per piece (edge clicks at 100 ms joins); no Mac mic seen with one.
    pub fn send(&self, samples: &[f32], commit: bool) {
        let s = crate::audio::resample_to(samples, self.rate, self.wire);
        let _ = self.tx.send(Out::Audio(super::pcm_s16le(&s), commit));
    }

    /// The recording stopped: commit the open segment, wait for every answer, then Ev::Closed.
    pub fn finish(&self) {
        let _ = self.tx.send(Out::Finish);
    }
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn name(p: Proto) -> &'static str {
    match p {
        Proto::Scribe => "ElevenLabs realtime",
        Proto::OpenAi => "OpenAI realtime",
    }
}

/// Scribe detects among these: `language_code` + `secondary_languages` (ElevenLabs realtime API reference).
/// ponytail: Pablo's two languages hardcoded; a settings list when someone else speaks others.
pub const AUTO_LANGUAGES: [&str; 2] = ["es", "en"];

/// The handshake URL: Scribe takes its options in the query, OpenAI in the first `session.update`.
fn request(o: &Opts, wire: u32) -> anyhow::Result<reqwest::Url> {
    if o.proto == Proto::OpenAi {
        return Ok(reqwest::Url::parse(&o.url)?);
    }
    let mut q: Vec<(&str, String)> = vec![("model_id", MODEL.into()), ("audio_format", format!("pcm_{wire}")), ("commit_strategy", "manual".into())];
    // Detection among Pablo's languages only (live log 2026-10-18: "Busca em YouTube", "Boa-" = Portuguese). "auto" = es
    // first; "es" or "en" keeps the other one as secondary, so English orders in a Spanish session stay English.
    let main = o.language.as_deref().filter(|l| !l.is_empty() && *l != "auto").unwrap_or(AUTO_LANGUAGES[0]);
    q.push(("language_code", main.into()));
    if AUTO_LANGUAGES.contains(&main) {
        q.extend(AUTO_LANGUAGES.iter().filter(|l| **l != main).map(|l| ("secondary_languages", l.to_string())));
    }
    q.extend(o.keyterms.iter().take(MAX_KEYTERMS).map(|k| ("keyterms", k.clone())));
    Ok(reqwest::Url::parse_with_params(&o.url, &q)?)
}

/// The first `session.update` of an OpenAI transcription session. `previous` = the last committed words.
fn session_update(o: &Opts, previous: &str) -> Message {
    // The API rejects a keyword with `<`, `>` or a line break.
    let keywords: Vec<&str> = o.keyterms.iter().map(|k| k.trim()).filter(|k| !k.is_empty() && !k.contains(['<', '>', '\r', '\n'])).take(MAX_KEYTERMS).collect();
    let mut tr = serde_json::json!({ "model": OPENAI_MODEL, "delay": OPENAI_DELAY });
    if !keywords.is_empty() {
        tr["keywords"] = keywords.into();
    }
    if let Some(l) = o.language.as_deref().filter(|l| !l.is_empty() && *l != "auto") {
        tr["languages"] = serde_json::json!([l]);
    }
    if !previous.is_empty() {
        tr["prompt"] = previous.into();
    }
    let v = serde_json::json!({
        "type": "session.update",
        "session": { "type": "transcription", "audio": { "input": {
            "format": { "type": "audio/pcm", "rate": OPENAI_RATE },
            "transcription": tr,
            "turn_detection": null,
        }}},
    });
    Message::text(v.to_string())
}

fn connect(o: &Opts, wire: u32, previous: &str) -> anyhow::Result<Ws> {
    let who = name(o.proto);
    let url = request(o, wire)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{who}: no host in {}", o.url))?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let mut req = url.as_str().into_client_request()?;
    match o.proto {
        Proto::Scribe => req.headers_mut().insert("xi-api-key", o.key.parse()?),
        Proto::OpenAi => req.headers_mut().insert("authorization", format!("Bearer {}", o.key).parse()?),
    };
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))?.next().ok_or_else(|| anyhow::anyhow!("{who}: {host} did not resolve"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| anyhow::anyhow!("Cannot connect to {who}: {e}"))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (mut ws, _) = tungstenite::client_tls(req, tcp).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
            let body = r.body().as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            anyhow::anyhow!("{who} rejected the connection (HTTP {}): {}", r.status().as_u16(), body.chars().take(200).collect::<String>())
        }
        e => anyhow::anyhow!("{who} handshake failed: {e}"),
    })?;
    if o.proto == Proto::OpenAi {
        ws.send(session_update(o, previous))?;
    }
    let tcp = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => anyhow::bail!("{who}: unknown stream"),
    };
    tcp.set_read_timeout(Some(POLL))?;
    Ok(ws)
}

fn chunk(pcm: &[u8], commit: bool, wire: u32, previous: Option<&str>) -> Message {
    use base64::Engine;
    let mut v = serde_json::json!({
        "message_type": "input_audio_chunk",
        "audio_base_64": base64::engine::general_purpose::STANDARD.encode(pcm),
        "commit": commit,
        "sample_rate": wire,
    });
    if let Some(p) = previous.filter(|p| !p.is_empty()) {
        v["previous_text"] = p.into();
    }
    Message::text(v.to_string())
}

/// The messages for one piece of audio. Scribe: one chunk (with `previous` on the first of a connection);
/// OpenAI: an append, then a commit.
fn frames(p: Proto, pcm: &[u8], commit: bool, wire: u32, previous: Option<&str>) -> Vec<Message> {
    use base64::Engine;
    if p == Proto::Scribe {
        return vec![chunk(pcm, commit, wire, previous)];
    }
    let mut v = vec![];
    if !pcm.is_empty() {
        let audio = base64::engine::general_purpose::STANDARD.encode(pcm);
        v.push(Message::text(serde_json::json!({ "type": "input_audio_buffer.append", "audio": audio }).to_string()));
    }
    if commit {
        v.push(Message::text(r#"{"type":"input_audio_buffer.commit"}"#));
    }
    v
}

/// The last words of `t`, under 50 chars (the docs' advice for previous_text).
fn tail_words(t: &str) -> String {
    let mut out = String::new();
    for w in t.split_whitespace().rev() {
        if out.chars().count() + w.chars().count() + 1 > 48 {
            break;
        }
        out = if out.is_empty() { w.to_string() } else { format!("{w} {out}") };
    }
    out
}

/// What one server message means for the session.
#[derive(Debug, PartialEq)]
enum In {
    Partial(String),
    Committed(String),
    /// A server error: true = worth a reconnect (throttled, rate limit, transient).
    Error(String, bool),
    Other,
    /// OpenAI: the server took a commit as this item (commit order).
    Item(String),
    /// OpenAI: new words of an item.
    Delta(String, String),
    /// OpenAI: the final words of an item.
    Done(String, String),
}

fn parse(p: Proto, t: &str) -> In {
    if p == Proto::OpenAi {
        return parse_openai(t);
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else { return In::Other };
    let text = || v["text"].as_str().unwrap_or("").trim().to_string();
    match v["message_type"].as_str().unwrap_or("") {
        "partial_transcript" => In::Partial(text()),
        "committed_transcript" => In::Committed(text()),
        "session_started" | "committed_transcript_with_timestamps" | "warning" => In::Other,
        ty => match v["error"].as_str().or(v["message"].as_str()) {
            Some(e) => {
                let retry = ["throttled", "rate_limited", "resource_exhausted", "queue_overflow", "transcriber_error"].iter().any(|k| ty.contains(k));
                In::Error(format!("ElevenLabs realtime {ty}: {e}"), retry)
            }
            None => In::Other,
        },
    }
}

fn parse_openai(t: &str) -> In {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else { return In::Other };
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    match v["type"].as_str().unwrap_or("") {
        "input_audio_buffer.committed" => In::Item(s("item_id")),
        "conversation.item.input_audio_transcription.delta" => In::Delta(s("item_id"), s("delta")),
        "conversation.item.input_audio_transcription.completed" => In::Done(s("item_id"), s("transcript").trim().to_string()),
        // One turn failed: an empty answer (after the release the caller sends that audio to the batch STT).
        "conversation.item.input_audio_transcription.failed" => {
            log::warn!("openai-rt: transcription of {} failed: {}", s("item_id"), v["error"]);
            In::Done(s("item_id"), String::new())
        }
        "error" => {
            let e = &v["error"];
            let (ty, code) = (e["type"].as_str().unwrap_or(""), e["code"].as_str().unwrap_or(""));
            let retry = ty == "server_error" || code.contains("rate_limit");
            In::Error(format!("OpenAI realtime {}: {}", if code.is_empty() { ty } else { code }, e["message"].as_str().unwrap_or("")), retry)
        }
        _ => In::Other,
    }
}

/// OpenAI answers per item, maybe out of order: back to commit order, and deltas summed into the open item's words.
#[derive(Default)]
struct Order {
    items: VecDeque<String>,
    done: std::collections::HashMap<String, String>,
    partial: (String, String),
}

impl Order {
    /// One parsed event in, the Scribe-shaped events out (Partial = the item's words so far, Committed in order).
    fn feed(&mut self, i: In) -> Vec<In> {
        match i {
            In::Item(id) => {
                self.items.push_back(id);
                vec![]
            }
            In::Delta(id, d) => {
                if self.partial.0 != id {
                    self.partial = (id, String::new());
                }
                self.partial.1.push_str(&d);
                vec![In::Partial(self.partial.1.trim().to_string())]
            }
            In::Done(id, t) => {
                self.done.insert(id, t);
                let mut out = vec![];
                while let Some(t) = self.items.front().and_then(|f| self.done.remove(f)) {
                    self.items.pop_front();
                    out.push(In::Committed(t));
                }
                out
            }
            i => vec![i],
        }
    }
}

/// The socket thread: connect, replay after a reconnect, pump audio out, events in.
fn run(o: &Opts, wire: u32, rx: &mpsc::Receiver<Out>, on: &dyn Fn(Ev)) {
    // Committed segments still waiting for their transcript, and the open one: a reconnect sends them again.
    let mut sent: VecDeque<Vec<u8>> = VecDeque::new();
    let mut open: Vec<u8> = vec![];
    let (mut finishing, mut last_text, mut fails) = (false, String::new(), 0u32);
    let t0 = Instant::now();
    loop {
        let mut order = Order::default(); // item ids live in one connection
        let mut ws = match connect(o, wire, &tail_words(&last_text)) {
            Ok(ws) => ws,
            Err(e) => {
                fails += 1;
                log::warn!("scribe-rt: connect failed ({fails}): {e:#}");
                if fails > MAX_RETRIES {
                    return on(Ev::Failed(format!("{e:#}")));
                }
                std::thread::sleep(Duration::from_millis(200 * fails as u64));
                continue;
            }
        };
        log::info!("scribe-rt: {} connected after {} ms", name(o.proto), t0.elapsed().as_millis());
        let mut first = Some(tail_words(&last_text));
        let mut out = |ws: &mut Ws, pcm: &[u8], commit: bool| frames(o.proto, pcm, commit, wire, first.take().as_deref()).into_iter().try_for_each(|m| ws.send(m));
        let replay = sent.iter().map(|s| (s.as_slice(), true)).chain((!open.is_empty()).then_some((open.as_slice(), false)));
        let mut err: Option<String> = replay.into_iter().find_map(|(s, c)| out(&mut ws, s, c).err().map(|e| e.to_string()));
        while err.is_none() {
            // outgoing
            loop {
                match rx.try_recv() {
                    Ok(Out::Audio(pcm, commit)) => {
                        // Nothing to commit: an empty commit gets no answer, and the finish would wait for it.
                        let commit = commit && !(open.is_empty() && pcm.is_empty());
                        if pcm.is_empty() && !commit {
                            continue;
                        }
                        open.extend_from_slice(&pcm);
                        if let Err(e) = out(&mut ws, &pcm, commit) {
                            err = Some(e.to_string());
                        }
                        if commit {
                            sent.push_back(std::mem::take(&mut open));
                        }
                    }
                    Ok(Out::Finish) => {
                        finishing = true;
                        if !open.is_empty() {
                            if let Err(e) = out(&mut ws, &[], true) {
                                err = Some(e.to_string());
                            }
                            sent.push_back(std::mem::take(&mut open));
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    // The session dropped us (Esc, or it moved on): abandon the socket now.
                    Err(mpsc::TryRecvError::Disconnected) => return close(ws),
                }
                if err.is_some() {
                    break;
                }
            }
            if err.is_some() {
                break;
            }
            if finishing && sent.is_empty() {
                close(ws);
                return on(Ev::Closed);
            }
            // incoming
            match ws.read() {
                Ok(Message::Text(t)) => {
                    for i in order.feed(parse(o.proto, &t)) {
                        match i {
                            In::Partial(p) => on(Ev::Partial(p)),
                            In::Committed(c) => {
                                sent.pop_front();
                                fails = 0;
                                if !c.is_empty() {
                                    last_text = c.clone();
                                }
                                on(Ev::Committed(c));
                            }
                            In::Error(e, true) => err = Some(e),
                            In::Error(e, false) => {
                                close(ws);
                                return on(Ev::Failed(e));
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Message::Close(f)) => err = Some(format!("closed by the server: {f:?}")),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => err = Some(e.to_string()),
            }
        }
        let e = err.unwrap_or_default();
        fails += 1;
        log::warn!("scribe-rt: socket error ({fails}), {} segments to replay: {e}", sent.len() + usize::from(!open.is_empty()));
        if fails > MAX_RETRIES {
            return on(Ev::Failed(format!("{}: {e}", name(o.proto))));
        }
    }
}

fn close(mut ws: Ws) {
    let _ = ws.close(None);
    let _ = ws.flush();
}

/// The whole clip at once (Providers "Test", the latency bench): send, commit, wait for the text.
/// `pace` = real-time pacing (100 ms of audio per 100 ms), as the mic would send it.
pub fn transcribe(o: Opts, audio16k: &[f32], pace: bool, timeout: Duration) -> anyhow::Result<(String, Duration)> {
    let (tx, rx) = mpsc::channel();
    let who = name(o.proto);
    let live = Live::start(Opts { rate: crate::audio::RATE, ..o }, move |e| {
        let _ = tx.send(e);
    });
    for piece in audio16k.chunks(1600) {
        live.send(piece, false);
        if pace {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let released = Instant::now();
    live.finish();
    let mut texts = vec![];
    loop {
        match rx.recv_timeout(timeout.saturating_sub(released.elapsed())) {
            Ok(Ev::Committed(t)) if !t.is_empty() => texts.push(t),
            Ok(Ev::Closed) => return Ok((texts.join(" "), released.elapsed())),
            Ok(Ev::Failed(e)) => anyhow::bail!(e),
            Ok(_) => {}
            Err(_) => anyhow::bail!("{who} timed out"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake Scribe server: answers every commit with "seg<n>" (n counts commits over all connections).
    /// `drop_after` = close the first connection after this many commits, without answering the last one.
    fn server(drop_after: Option<usize>) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/v1/speech-to-text/realtime", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let s = seen.clone();
        std::thread::spawn(move || {
            let mut n = 0;
            for (conn, stream) in l.incoming().enumerate() {
                let mut ws = tungstenite::accept_hdr(stream.unwrap(), |r: &tungstenite::handshake::server::Request, resp| {
                    s.lock().unwrap().push(format!("connect {} key={}", r.uri(), r.headers().get("xi-api-key").map(|v| v.to_str().unwrap()).unwrap_or("")));
                    Ok(resp)
                })
                .unwrap();
                let mut commits = 0;
                while let Ok(m) = ws.read() {
                    let Message::Text(t) = m else { continue };
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    s.lock().unwrap().push(format!("chunk {} commit={} prev={}", v["audio_base_64"].as_str().unwrap().len(), v["commit"], v["previous_text"].as_str().unwrap_or("-")));
                    ws.send(Message::text(r#"{"message_type":"partial_transcript","text":"par"}"#)).unwrap();
                    if v["commit"] == true {
                        commits += 1;
                        if conn == 0 && Some(commits) == drop_after {
                            break;
                        }
                        n += 1;
                        ws.send(Message::text(format!(r#"{{"message_type":"committed_transcript","text":"seg{n}"}}"#))).unwrap();
                    }
                }
            }
        });
        (url, seen)
    }

    fn opts(url: String) -> Opts {
        Opts { proto: Proto::Scribe, url, key: "xi".into(), rate: 16000, language: Some("es".into()), keyterms: vec!["Contaflow".into(), "Jev".into()] }
    }

    fn collect(rx: &mpsc::Receiver<Ev>) -> Vec<Ev> {
        let mut v = vec![];
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            let end = matches!(e, Ev::Closed | Ev::Failed(_));
            v.push(e);
            if end {
                break;
            }
        }
        v
    }

    #[test]
    fn commits_map_to_committed_texts_and_finish_closes() {
        let (url, seen) = server(None);
        let (tx, rx) = mpsc::channel();
        let live = Live::start(opts(url), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], false);
        live.send(&[0.1; 1600], true);
        live.send(&[], true); // nothing new: no empty commit
        live.send(&[0.1; 800], false);
        live.finish();
        let ev: Vec<Ev> = collect(&rx).into_iter().filter(|e| !matches!(e, Ev::Partial(_))).collect();
        assert_eq!(ev, [Ev::Committed("seg1".into()), Ev::Committed("seg2".into()), Ev::Closed]);
        let s = seen.lock().unwrap().clone();
        assert!(s[0].starts_with("connect /v1/speech-to-text/realtime?model_id=scribe_v2_realtime&audio_format=pcm_16000&commit_strategy=manual&language_code=es&secondary_languages=en&keyterms=Contaflow&keyterms=Jev key=xi"), "{}", s[0]);
        assert_eq!(s.iter().filter(|l| l.contains("commit=true")).count(), 2, "{s:?}");
    }

    #[test]
    fn auto_language_keeps_scribe_on_spanish_and_english() {
        // live log 2026-10-18: on auto Scribe heard "Busca en YouTube" as Portuguese ("Busca em YouTube", "Boa-")
        let q = |l: Option<&str>| request(&Opts { language: l.map(Into::into), keyterms: vec![], ..opts("wss://x/v1/speech-to-text/realtime".into()) }, 16000).unwrap().query().unwrap().to_string();
        for auto in [None, Some("auto"), Some("")] {
            assert!(q(auto).ends_with("commit_strategy=manual&language_code=es&secondary_languages=en"), "{}", q(auto));
        }
        assert!(q(Some("es")).ends_with("language_code=es&secondary_languages=en"), "{}", q(Some("es")));
        assert!(q(Some("en")).ends_with("language_code=en&secondary_languages=es"), "{}", q(Some("en")));
        assert!(q(Some("fr")).ends_with("language_code=fr"), "another language is the only one");
    }

    #[test]
    fn a_dropped_socket_reconnects_and_replays_the_unanswered_audio() {
        let (url, seen) = server(Some(2));
        let (tx, rx) = mpsc::channel();
        let live = Live::start(opts(url), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], true);
        std::thread::sleep(Duration::from_millis(200));
        live.send(&[0.2; 3200], true); // the server drops the socket before answering this one
        live.finish();
        let ev: Vec<Ev> = collect(&rx).into_iter().filter(|e| !matches!(e, Ev::Partial(_))).collect();
        assert_eq!(ev, [Ev::Committed("seg1".into()), Ev::Committed("seg2".into()), Ev::Closed]);
        let s = seen.lock().unwrap().clone();
        assert_eq!(s.iter().filter(|l| l.starts_with("connect")).count(), 2, "{s:?}");
        let replay = s.iter().skip_while(|l| !l.starts_with("connect")).skip(1).skip_while(|l| !l.starts_with("connect")).nth(1).unwrap();
        assert!(replay.contains("commit=true prev=seg1"), "the replay carries the last words: {replay}");
    }

    #[test]
    fn a_dead_server_fails_so_the_caller_falls_back() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let live = Live::start(opts(format!("ws://127.0.0.1:{port}/x")), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], true);
        assert!(matches!(collect(&rx).last(), Some(Ev::Failed(e)) if e.contains("Cannot connect")));
    }

    #[test]
    fn server_messages() {
        assert_eq!(parse(Proto::Scribe, r#"{"message_type":"partial_transcript","text":"hola "}"#), In::Partial("hola".into()));
        assert_eq!(parse(Proto::Scribe, r#"{"message_type":"committed_transcript","text":"Hola."}"#), In::Committed("Hola.".into()));
        assert_eq!(parse(Proto::Scribe, r#"{"message_type":"session_started","session_id":"x"}"#), In::Other);
        assert!(matches!(parse(Proto::Scribe, r#"{"message_type":"auth_error","error":"bad key"}"#), In::Error(e, false) if e.contains("bad key")));
        assert!(matches!(parse(Proto::Scribe, r#"{"message_type":"rate_limited","error":"slow down"}"#), In::Error(_, true)));
        assert_eq!(tail_words("one two three four five six seven eight nine ten eleven twelve"), "four five six seven eight nine ten eleven twelve", "under 50 chars");
    }

    /// How the fake OpenAI server behaves.
    #[derive(Clone, Copy, Default)]
    struct Fake {
        /// Close the first connection at this commit, without answering it.
        drop_at: Option<usize>,
        /// Answer each odd commit only after the next one (the completions come out of order).
        swap: bool,
        /// Answer the first message with this error event.
        error: Option<&'static str>,
        /// The transcription of commit 2 fails.
        fail_second: bool,
    }

    /// A fake OpenAI transcription server. Item n (over all connections) answers "seg<n>"; every append gets a
    /// delta "w " for the open item. `seen` logs the handshake and each client event.
    fn openai_server(f: Fake) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/v1/realtime?intent=transcription", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let s = seen.clone();
        std::thread::spawn(move || {
            let mut n = 0;
            for (conn, stream) in l.incoming().enumerate() {
                let mut ws = tungstenite::accept_hdr(stream.unwrap(), |r: &tungstenite::handshake::server::Request, resp| {
                    s.lock().unwrap().push(format!("connect {} {}", r.uri(), r.headers().get("authorization").map(|v| v.to_str().unwrap()).unwrap_or("")));
                    Ok(resp)
                })
                .unwrap();
                let (mut commits, mut held): (usize, Option<String>) = (0, None);
                let done = |n: usize| format!(r#"{{"type":"conversation.item.input_audio_transcription.completed","item_id":"it{n}","content_index":0,"transcript":" seg{n} "}}"#);
                while let Ok(m) = ws.read() {
                    let Message::Text(t) = m else { continue };
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    let ty = v["type"].as_str().unwrap().to_string();
                    s.lock().unwrap().push(if ty == "session.update" { t.to_string() } else { ty.clone() });
                    if let Some(e) = f.error {
                        ws.send(Message::text(e)).unwrap();
                        continue;
                    }
                    match ty.as_str() {
                        "input_audio_buffer.append" => ws.send(Message::text(format!(r#"{{"type":"conversation.item.input_audio_transcription.delta","item_id":"it{}","delta":"w "}}"#, n + 1))).unwrap(),
                        "input_audio_buffer.commit" => {
                            commits += 1;
                            if conn == 0 && Some(commits) == f.drop_at {
                                break;
                            }
                            n += 1;
                            ws.send(Message::text(format!(r#"{{"type":"input_audio_buffer.committed","item_id":"it{n}"}}"#))).unwrap();
                            if f.fail_second && n == 2 {
                                ws.send(Message::text(r#"{"type":"conversation.item.input_audio_transcription.failed","item_id":"it2","error":{"message":"x"}}"#)).unwrap();
                            } else if f.swap && n % 2 == 1 {
                                held = Some(done(n));
                            } else {
                                ws.send(Message::text(done(n))).unwrap();
                                if let Some(h) = held.take() {
                                    ws.send(Message::text(h)).unwrap();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        (url, seen)
    }

    fn oa(url: String) -> Opts {
        Opts { proto: Proto::OpenAi, url, key: "sk".into(), rate: 16000, language: Some("es".into()), keyterms: vec!["Contaflow".into(), "a<b".into()] }
    }

    fn finals(rx: &mpsc::Receiver<Ev>) -> Vec<Ev> {
        collect(rx).into_iter().filter(|e| !matches!(e, Ev::Partial(_))).collect()
    }

    #[test]
    fn openai_sets_up_the_session_and_answers_in_commit_order() {
        let (url, seen) = openai_server(Fake { swap: true, ..Default::default() });
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(url), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], false);
        live.send(&[0.1; 1600], true);
        live.send(&[], true); // nothing new: no empty commit
        live.send(&[0.1; 800], false);
        live.finish(); // the open tail is the last commit
        let ev = collect(&rx);
        assert!(ev.contains(&Ev::Partial("w w".into())), "deltas add up per item: {ev:?}");
        let ev: Vec<Ev> = ev.into_iter().filter(|e| !matches!(e, Ev::Partial(_))).collect();
        assert_eq!(ev, [Ev::Committed("seg1".into()), Ev::Committed("seg2".into()), Ev::Closed], "seg2 came first on the wire");
        let s = seen.lock().unwrap().clone();
        assert_eq!(s[0], "connect /v1/realtime?intent=transcription Bearer sk");
        let su: serde_json::Value = serde_json::from_str(&s[1]).unwrap();
        let input = &su["session"]["audio"]["input"];
        assert_eq!(su["session"]["type"], "transcription");
        assert_eq!(input["format"], serde_json::json!({"type": "audio/pcm", "rate": 24000}));
        assert!(input["turn_detection"].is_null() && input.as_object().unwrap().contains_key("turn_detection"));
        assert_eq!(input["transcription"]["model"], "gpt-live-transcribe");
        assert_eq!(input["transcription"]["keywords"], serde_json::json!(["Contaflow"]), "a keyword with < is dropped");
        assert_eq!(input["transcription"]["languages"], serde_json::json!(["es"]));
        assert!(input["transcription"].get("prompt").is_none());
        assert_eq!(s.iter().filter(|l| *l == "input_audio_buffer.commit").count(), 2, "{s:?}");
    }

    #[test]
    fn openai_audio_is_24k_pcm() {
        let (url, _) = openai_server(Fake::default());
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(url), move |e| drop(tx.send(e)));
        assert_eq!(live.wire, 24000);
        live.send(&[0.1; 1600], true); // 100 ms at 16 kHz
        live.finish();
        assert_eq!(finals(&rx), [Ev::Committed("seg1".into()), Ev::Closed]);
        let m = &frames(Proto::OpenAi, &super::super::pcm_s16le(&crate::audio::resample_to(&[0.1; 1600], 16000, 24000)), true, 24000, None);
        assert_eq!(m.len(), 2);
        let v: serde_json::Value = serde_json::from_str(m[0].to_text().unwrap()).unwrap();
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(v["audio"].as_str().unwrap()).unwrap().len();
        assert!((4700..=4900).contains(&bytes), "100 ms at 24 kHz s16 = 4800 bytes, got {bytes}");
        assert_eq!(m[1].to_text().unwrap(), r#"{"type":"input_audio_buffer.commit"}"#);
    }

    #[test]
    fn openai_reconnects_and_replays_with_the_last_words_as_prompt() {
        let (url, seen) = openai_server(Fake { drop_at: Some(2), ..Default::default() });
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(url), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], true);
        std::thread::sleep(Duration::from_millis(200));
        live.send(&[0.2; 3200], true); // the server drops the socket before answering this one
        live.finish();
        assert_eq!(finals(&rx), [Ev::Committed("seg1".into()), Ev::Committed("seg2".into()), Ev::Closed]);
        let s = seen.lock().unwrap().clone();
        let connects: Vec<usize> = s.iter().enumerate().filter(|(_, l)| l.starts_with("connect")).map(|(i, _)| i).collect();
        assert_eq!(connects.len(), 2, "{s:?}");
        let su: serde_json::Value = serde_json::from_str(&s[connects[1] + 1]).unwrap();
        assert_eq!(su["session"]["audio"]["input"]["transcription"]["prompt"], "seg1");
        assert_eq!(&s[connects[1] + 2..connects[1] + 4], ["input_audio_buffer.append", "input_audio_buffer.commit"], "the unanswered audio again");
    }

    #[test]
    fn openai_failures_fall_back() {
        // a rejected key / bad session: Failed at once (the caller runs the batch STT)
        let bad = r#"{"type":"error","error":{"type":"invalid_request_error","code":"invalid_api_key","message":"Incorrect API key"}}"#;
        let (url, _) = openai_server(Fake { error: Some(bad), ..Default::default() });
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(url), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], true);
        assert!(matches!(collect(&rx).last(), Some(Ev::Failed(e)) if e.contains("invalid_api_key") && e.contains("Incorrect API key")));
        // one failed turn: an empty commit, the session goes on
        let (url, _) = openai_server(Fake { fail_second: true, ..Default::default() });
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(url), move |e| drop(tx.send(e)));
        for _ in 0..3 {
            live.send(&[0.1; 1600], true);
        }
        live.finish();
        assert_eq!(finals(&rx), [Ev::Committed("seg1".into()), Ev::Committed("".into()), Ev::Committed("seg3".into()), Ev::Closed]);
        // no server
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let live = Live::start(oa(format!("ws://127.0.0.1:{port}/v1/realtime")), move |e| drop(tx.send(e)));
        live.send(&[0.1; 1600], true);
        assert!(matches!(collect(&rx).last(), Some(Ev::Failed(e)) if e.contains("Cannot connect to OpenAI realtime")));
    }

    #[test]
    fn openai_events_and_order() {
        let p = |t| parse(Proto::OpenAi, t);
        assert_eq!(p(r#"{"type":"input_audio_buffer.committed","item_id":"a"}"#), In::Item("a".into()));
        assert_eq!(p(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"a","delta":"Ho"}"#), In::Delta("a".into(), "Ho".into()));
        assert_eq!(p(r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"a","transcript":" Hola. "}"#), In::Done("a".into(), "Hola.".into()));
        assert_eq!(p(r#"{"type":"session.updated","session":{}}"#), In::Other);
        assert!(matches!(p(r#"{"type":"error","error":{"type":"server_error","message":"x"}}"#), In::Error(_, true)));
        assert!(matches!(p(r#"{"type":"error","error":{"type":"invalid_request_error","code":"rate_limit_exceeded","message":"x"}}"#), In::Error(_, true)));
        assert!(matches!(p(r#"{"type":"error","error":{"type":"invalid_request_error","code":"invalid_value","message":"bad keyword"}}"#), In::Error(e, false) if e.contains("bad keyword")));
        let mut o = Order::default();
        assert_eq!(o.feed(In::Item("a".into())), []);
        assert_eq!(o.feed(In::Item("b".into())), []);
        assert_eq!(o.feed(In::Delta("c".into(), "x ".into())), [In::Partial("x".into())]);
        assert_eq!(o.feed(In::Delta("c".into(), "y".into())), [In::Partial("x y".into())]);
        assert_eq!(o.feed(In::Done("b".into(), "two".into())), [], "waits for a");
        assert_eq!(o.feed(In::Done("a".into(), "one".into())), [In::Committed("one".into()), In::Committed("two".into())]);
        assert_eq!(o.feed(In::Committed("scribe".into())), [In::Committed("scribe".into())], "Scribe events pass through");
    }
}
