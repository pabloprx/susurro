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

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub const URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";
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

#[derive(Debug, Clone)]
pub struct Opts {
    pub url: String,
    pub key: String,
    /// The device rate of the samples given to `send`.
    pub rate: u32,
    /// None or "auto" = detect.
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
        let (rate, wire) = (o.rate, if RATES.contains(&o.rate) { o.rate } else { crate::audio::RATE });
        std::thread::Builder::new().name("scribe-rt".into()).spawn(move || run(&o, wire, &rx, &on)).expect("spawn scribe-rt");
        Live { tx, rate, wire }
    }

    /// Samples at the device rate. `commit` = a pause cut: close the segment.
    /// ponytail: a rate the API lacks is resampled per piece (edge clicks at 100 ms joins); no Mac mic seen with one.
    pub fn send(&self, samples: &[f32], commit: bool) {
        let s = if self.wire == self.rate { samples.to_vec() } else { crate::audio::resample(samples, self.rate) };
        let _ = self.tx.send(Out::Audio(super::pcm_s16le(&s), commit));
    }

    /// The recording stopped: commit the open segment, wait for every answer, then Ev::Closed.
    pub fn finish(&self) {
        let _ = self.tx.send(Out::Finish);
    }
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(o: &Opts, wire: u32) -> anyhow::Result<Ws> {
    let mut q: Vec<(&str, String)> = vec![("model_id", MODEL.into()), ("audio_format", format!("pcm_{wire}")), ("commit_strategy", "manual".into())];
    if let Some(l) = o.language.as_deref().filter(|l| !l.is_empty() && *l != "auto") {
        q.push(("language_code", l.into()));
    }
    q.extend(o.keyterms.iter().take(MAX_KEYTERMS).map(|k| ("keyterms", k.clone())));
    let url = reqwest::Url::parse_with_params(&o.url, &q)?;
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("ElevenLabs realtime: no host in {}", o.url))?.to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let mut req = url.as_str().into_client_request()?;
    req.headers_mut().insert("xi-api-key", o.key.parse()?);
    let addr = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))?.next().ok_or_else(|| anyhow::anyhow!("ElevenLabs realtime: {host} did not resolve"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| anyhow::anyhow!("Cannot connect to ElevenLabs realtime: {e}"))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (ws, _) = tungstenite::client_tls(req, tcp).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
            let body = r.body().as_ref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            anyhow::anyhow!("ElevenLabs realtime rejected the connection (HTTP {}): {}", r.status().as_u16(), body.chars().take(200).collect::<String>())
        }
        e => anyhow::anyhow!("ElevenLabs realtime handshake failed: {e}"),
    })?;
    let tcp = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => anyhow::bail!("ElevenLabs realtime: unknown stream"),
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
}

fn parse(t: &str) -> In {
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

/// The socket thread: connect, replay after a reconnect, pump audio out, events in.
fn run(o: &Opts, wire: u32, rx: &mpsc::Receiver<Out>, on: &dyn Fn(Ev)) {
    // Committed segments still waiting for their transcript, and the open one: a reconnect sends them again.
    let mut sent: VecDeque<Vec<u8>> = VecDeque::new();
    let mut open: Vec<u8> = vec![];
    let (mut finishing, mut last_text, mut fails) = (false, String::new(), 0u32);
    let t0 = Instant::now();
    loop {
        let mut ws = match connect(o, wire) {
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
        log::info!("scribe-rt: connected after {} ms", t0.elapsed().as_millis());
        let mut first = Some(tail_words(&last_text));
        let mut out = |ws: &mut Ws, pcm: &[u8], commit: bool| ws.send(chunk(pcm, commit, wire, first.take().as_deref()));
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
                Ok(Message::Text(t)) => match parse(&t) {
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
                    In::Other => {}
                },
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
            return on(Ev::Failed(format!("ElevenLabs realtime: {e}")));
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
            Err(_) => anyhow::bail!("ElevenLabs realtime timed out"),
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
        Opts { url, key: "xi".into(), rate: 16000, language: Some("es".into()), keyterms: vec!["Contaflow".into(), "Jev".into()] }
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
        assert!(s[0].starts_with("connect /v1/speech-to-text/realtime?model_id=scribe_v2_realtime&audio_format=pcm_16000&commit_strategy=manual&language_code=es&keyterms=Contaflow&keyterms=Jev key=xi"), "{}", s[0]);
        assert_eq!(s.iter().filter(|l| l.contains("commit=true")).count(), 2, "{s:?}");
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
        assert_eq!(parse(r#"{"message_type":"partial_transcript","text":"hola "}"#), In::Partial("hola".into()));
        assert_eq!(parse(r#"{"message_type":"committed_transcript","text":"Hola."}"#), In::Committed("Hola.".into()));
        assert_eq!(parse(r#"{"message_type":"session_started","session_id":"x"}"#), In::Other);
        assert!(matches!(parse(r#"{"message_type":"auth_error","error":"bad key"}"#), In::Error(e, false) if e.contains("bad key")));
        assert!(matches!(parse(r#"{"message_type":"rate_limited","error":"slow down"}"#), In::Error(_, true)));
        assert_eq!(tail_words("one two three four five six seven eight nine ten eleven twelve"), "four five six seven eight nine ten eleven twelve", "under 50 chars");
    }
}
