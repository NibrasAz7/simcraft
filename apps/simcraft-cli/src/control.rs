//! Control channel: JSON-lines over TCP on 127.0.0.1 — the same protocol
//! shape as MatCraft's (`apps/matcraft/src/control_server.rs`), so shared
//! clients drive either app.
//!
//! Wire format, one JSON object per line:
//!
//! ```text
//! {"id": 1, "method": "engine.execute", "params": {"command": "sim.run", "params": {"tstop": 5}}}
//! {"id": 2, "command": "model.load", "params": {"path": "pendulum.simcraft"}}
//! ```
//!
//! The flat `{command, params}` form is shorthand for `engine.execute`.
//! Methods: `engine.execute`/`command` (any engine command by id),
//! `engine.list`/`commands`, `ping`, `control.shutdown`. Replies are the
//! request's `id` plus `{ok, …}` or `{ok: false, error}`.
//!
//! The engine `Session` isn't `Send` (the linked `matlang::Session` holds
//! `Rc`s), so — as in MatCraft — one accept thread spawns a thread per
//! connection; connection threads parse bounded lines and hand
//! [`ControlRequest`]s over an mpsc channel to the thread that owns the
//! session ([`run`]), waiting on a condvar slot for the reply.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};

use simcraft_engine::Session;

/// Longest accepted request line (bytes, without the newline). Generous,
/// so commands that carry large parameter arrays still fit.
const MAX_LINE: usize = 8 * 1024 * 1024;
/// Cap on simultaneously open control connections.
const MAX_CONNECTIONS: usize = 8;
/// How long a connection thread waits for the session to reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// Shared slot between a [`ControlRequest`] (session side) and its
/// [`PendingReply`] (connection-thread side).
type Slot = Arc<(Mutex<Option<Json>>, Condvar)>;

/// One request handed to the session thread over the control channel.
pub struct ControlRequest {
    pub method: String,
    pub params: Json,
    slot: Slot,
}

/// The connection thread's handle for the reply.
pub struct PendingReply {
    slot: Slot,
}

impl ControlRequest {
    /// `new` splits a request into the session side and the connection side.
    pub fn new(method: String, params: Json) -> (Self, PendingReply) {
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        (ControlRequest { method, params, slot: slot.clone() }, PendingReply { slot })
    }

    /// Deliver the reply to the waiting connection thread.
    pub fn reply(self, v: Json) {
        let (m, cv) = &*self.slot;
        if let Ok(mut g) = m.lock() {
            *g = Some(v);
        }
        cv.notify_one();
    }
}

impl PendingReply {
    /// Block until the session thread replies or `dur` elapses.
    pub fn wait(&self, dur: Duration) -> Json {
        let (m, cv) = &*self.slot;
        let Ok(mut g) = m.lock() else {
            return json!({"ok": false, "error": "reply lock poisoned"});
        };
        loop {
            if let Some(v) = g.take() {
                return v;
            }
            match cv.wait_timeout(g, dur) {
                Ok((g2, timed_out)) => {
                    g = g2;
                    if timed_out.timed_out() {
                        return json!({"ok": false, "error": "control request timed out"});
                    }
                }
                Err(_) => return json!({"ok": false, "error": "reply lock poisoned"}),
            }
        }
    }
}

/// `simcraft control PORT` — bind, report the bound port, and drive the
/// session on this thread until the channel closes.
pub fn serve(port: u16) -> i32 {
    match spawn(port) {
        Ok((bound, rx)) => {
            eprintln!("simcraft control listening on 127.0.0.1:{bound}");
            let mut session = Session::new();
            // A linked MatCraft session is the model's compute context:
            // `sim.run` publishes `out` + `{t, y}` structs and draws scope
            // traces as matplot figures (`figure.list`).
            let _ = session.execute("session.link", json!({}));
            run(&mut session, rx);
            0
        }
        Err(e) => {
            eprintln!("simcraft control: cannot bind 127.0.0.1:{port}: {e}");
            1
        }
    }
}

/// Bind `127.0.0.1:port` (`0` picks an ephemeral port) and spawn the
/// accept loop; returns the bound port and the request receiver for
/// [`run`].
pub fn spawn(port: u16) -> std::io::Result<(u16, Receiver<ControlRequest>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let bound = listener.local_addr()?.port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || accept_loop(listener, tx));
    Ok((bound, rx))
}

/// Drive requests against `session` until every sender is gone or a
/// `control.shutdown` request arrives (its reply still goes out).
pub fn run(session: &mut Session, rx: Receiver<ControlRequest>) {
    while let Ok(req) = rx.recv() {
        let stop = matches!(req.method.as_str(), "control.shutdown" | "shutdown");
        handle(session, req);
        if stop {
            return;
        }
    }
}

/// Handle one control request: dispatch on the session thread and reply.
pub fn handle(session: &mut Session, req: ControlRequest) {
    let reply = dispatch(session, &req.method, req.params.clone());
    req.reply(reply);
}

fn dispatch(session: &mut Session, method: &str, params: Json) -> Json {
    match method {
        "ping" => json!({"ok": true, "pong": true}),
        "control.shutdown" | "shutdown" => json!({"ok": true}),
        "engine.list" | "commands" => exec(session, "engine.list", json!({})),
        "engine.execute" | "command" => {
            let cmd = params.get("command").and_then(Json::as_str).unwrap_or("");
            if cmd.is_empty() {
                return json!({"ok": false, "error": "engine.execute needs `command`"});
            }
            exec(session, cmd, params.get("params").cloned().unwrap_or(json!({})))
        }
        other => json!({"ok": false, "error": format!("unknown method `{other}`")}),
    }
}

/// Engine call → `{ok: true, …}` / `{ok: false, error}` — the MatCraft
/// convention (`ok` is added, never required of the command).
fn exec(session: &mut Session, cmd: &str, params: Json) -> Json {
    match session.execute(cmd, params) {
        Ok(mut v) => match v.as_object_mut() {
            Some(o) => {
                o.insert("ok".into(), json!(true));
                v
            }
            None => json!({"ok": true, "value": v}),
        },
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

/// A connection's place among [`MAX_CONNECTIONS`], given back on drop.
struct SlotGuard(Arc<AtomicUsize>);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(listener: TcpListener, tx: Sender<ControlRequest>) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = writeln!(stream, "{}", json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let guard = SlotGuard(Arc::clone(&open));
        let tx = tx.clone();
        let _ = std::thread::Builder::new().spawn(move || {
            let _guard = guard;
            serve_conn(stream, tx);
        });
    }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf`
/// (newline stripped). `Ok(false)` at end of stream; `Err` when the line
/// is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    loop {
        let avail = reader.fill_buf().map_err(|e| e.to_string())?;
        if avail.is_empty() {
            return Ok(!buf.is_empty());
        }
        let take = match avail.iter().position(|&b| b == b'\n') {
            Some(pos) => pos + 1,
            None => avail.len(),
        };
        buf.extend_from_slice(avail.get(..take).unwrap_or(&[]));
        let done = buf.last() == Some(&b'\n');
        reader.consume(take);
        if buf.len() > max {
            return Err(format!("request line longer than {max} bytes; closing the connection"));
        }
        if done {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(true);
        }
    }
}

fn serve_conn(stream: TcpStream, tx: Sender<ControlRequest>) {
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": e}));
                break;
            }
        }
        let Ok(line) = std::str::from_utf8(&buf) else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Json>(line) {
            Ok(msg) => {
                let id = msg.get("id").cloned().unwrap_or(Json::Null);
                // Flat `{command, params}` is `engine.execute` shorthand;
                // MatCraft-shaped `{method, params}` passes through.
                let (method, params) = match msg.get("command").and_then(Json::as_str) {
                    Some(cmd) => ("engine.execute".to_string(), json!({"command": cmd, "params": msg.get("params").cloned().unwrap_or(json!({}))})),
                    None => (msg.get("method").and_then(Json::as_str).unwrap_or("").to_string(), msg.get("params").cloned().unwrap_or(json!({}))),
                };
                let (req, pending) = ControlRequest::new(method, params);
                if tx.send(req).is_err() {
                    break;
                }
                let mut r = pending.wait(REPLY_TIMEOUT);
                if let Some(o) = r.as_object_mut() {
                    o.insert("id".into(), id);
                }
                r
            }
            Err(e) => json!({"ok": false, "error": format!("bad JSON: {e}")}),
        };
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn lines(input: &[u8], max: usize) -> Vec<Vec<u8>> {
        let mut r = Cursor::new(input);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        while let Ok(true) = read_bounded_line(&mut r, &mut buf, max) {
            out.push(buf.clone());
        }
        out
    }

    #[test]
    fn reads_lines_and_strips_crlf() {
        assert_eq!(lines(b"one\ntwo\r\nthree", 1024), vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn overlong_line_errors() {
        let mut r = Cursor::new(vec![b'x'; 64]);
        let mut buf = Vec::new();
        assert!(read_bounded_line(&mut r, &mut buf, 16).is_err());
    }

    #[test]
    fn pending_reply_times_out() {
        let (_req, pending) = ControlRequest::new("ping".to_string(), json!({}));
        let r = pending.wait(Duration::from_millis(5));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap_or_default().contains("timed out"));
    }

    /// Send `line`, read one reply line back as JSON.
    fn round_trip(out: &mut TcpStream, reader: &mut BufReader<TcpStream>, line: &str) -> Json {
        writeln!(out, "{line}").unwrap();
        out.flush().unwrap();
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap_or_else(|_| json!({"unparsed": reply}))
    }

    #[test]
    fn control_channel_round_trip() {
        let (port, rx) = spawn(0).unwrap();
        assert_ne!(port, 0);
        let worker = std::thread::spawn(move || {
            let mut session = Session::new();
            run(&mut session, rx);
        });
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());

        // ping
        let r = round_trip(&mut conn, &mut reader, r#"{"id":1,"method":"ping"}"#);
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pong"], json!(true));

        // MatCraft shape: engine.execute {command, params}
        let r = round_trip(&mut conn, &mut reader, r#"{"id":2,"method":"engine.execute","params":{"command":"engine.version","params":{}}}"#);
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["app"], json!("simcraft"));

        // Flat shorthand: {command, params}
        let r = round_trip(&mut conn, &mut reader, r#"{"id":3,"command":"block.list"}"#);
        assert_eq!(r["id"], json!(3));
        assert_eq!(r["ok"], json!(true));
        assert!(r["blocks"].as_array().unwrap().len() > 10);

        // engine.list method
        let r = round_trip(&mut conn, &mut reader, r#"{"id":4,"method":"engine.list"}"#);
        assert!(r["commands"].as_array().unwrap().iter().any(|c| c["id"] == json!("sim.run")));

        // Command-level error: unknown engine command → {ok:false,error}
        let r = round_trip(&mut conn, &mut reader, r#"{"id":5,"command":"nope.cmd"}"#);
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap_or_default().contains("unknown command"));

        // Unknown method
        let r = round_trip(&mut conn, &mut reader, r#"{"id":6,"method":"ui.screenshot"}"#);
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap_or_default().contains("unknown method"));

        // Malformed input → {ok:false,error}, connection stays usable.
        let r = round_trip(&mut conn, &mut reader, "this is not json");
        assert_eq!(r["ok"], json!(false));
        let r = round_trip(&mut conn, &mut reader, r#"{"id":7,"method":"ping"}"#);
        assert_eq!(r["ok"], json!(true));

        // Clean shutdown: reply still arrives, then the session loop ends.
        let r = round_trip(&mut conn, &mut reader, r#"{"id":8,"method":"control.shutdown"}"#);
        assert_eq!(r["ok"], json!(true));
        drop(conn);
        worker.join().unwrap();
    }
}
