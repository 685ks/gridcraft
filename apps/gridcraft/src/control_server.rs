//! Localhost JSON-lines control server. Each connection may send many requests; each line
//! `{"id":…, "method":…, "params":…}` gets one reply line `{"id":…, "ok":…, …}`.
//!
//! The server only reads requests: a line that is not a JSON object with a string `method` (an
//! HTTP request line, a stray `nc`, binary data) gets one error reply and the connection is
//! closed, so nothing sent after it runs.
//!
//! Only clients that hold the per-launch token are served (see
//! `gridcraft_engine::control_token`): a connection's first request must carry it as `token`
//! (later requests on that connection may too). Anything else gets one `unauthorized` reply and
//! the connection is closed before the request reaches the app. The listener binds the loopback
//! address only, and peers that are not loopback addresses are refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use gridcraft_ui_egui::ControlRequest;
use serde_json::{Value as Json, json};

const MAX_LINE: usize = 64 * 1024 * 1024;

pub fn start(port: u16, token: Arc<str>, ctx: egui::Context) -> std::io::Result<Receiver<ControlRequest>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let rx = serve_listener(listener, token, ctx)?;
    log::info!("control channel listening on 127.0.0.1:{port} (token required)");
    Ok(rx)
}

/// Accept connections on `listener`; requests arrive on the returned receiver.
fn serve_listener(listener: TcpListener, token: Arc<str>, ctx: egui::Context) -> std::io::Result<Receiver<ControlRequest>> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
                continue;
            }
            let tx = tx.clone();
            let ctx = ctx.clone();
            let token = Arc::clone(&token);
            let _ = std::thread::Builder::new().name("control-conn".into()).spawn(move || serve(stream, tx, ctx, &token));
        }
    })?;
    Ok(rx)
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("request too large (over {max} bytes); closing the connection"));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// One request line, parsed.
enum Line {
    Request {
        id: Json,
        method: String,
        params: Json,
        token: Option<String>,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    if is_http_request_line(text) {
        return Line::Reject("HTTP is not served here: this port takes JSON lines; closing the connection".into());
    }
    let msg = match serde_json::from_str::<Json>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("invalid JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Json::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    Line::Request {
        id: obj.get("id").cloned().unwrap_or(Json::Null),
        method: method.to_string(),
        params: obj.get("params").cloned().unwrap_or(json!({})),
        token: obj.get("token").and_then(Json::as_str).map(str::to_string),
    }
}

/// An HTTP/1 request line (`POST / HTTP/1.1`) or the HTTP/2 preface (`PRI * HTTP/2.0`).
fn is_http_request_line(text: &str) -> bool {
    const METHODS: [&str; 10] = ["GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH", "PRI"];
    text.split_once(' ').is_some_and(|(method, _)| METHODS.contains(&method))
        || text.rsplit_once(' ').is_some_and(|(_, version)| version.starts_with("HTTP/"))
}

/// The reply to a request without the right token; the connection closes after it.
const UNAUTHORIZED: &str = "unauthorized: send the control token as \"token\" (gridcraft-cli and the MCP client read it from the app's control-<PORT>.token file or GRIDCRAFT_CONTROL_TOKEN); closing the connection";

fn serve(stream: TcpStream, tx: Sender<ControlRequest>, ctx: egui::Context, token: &str) {
    use gridcraft_engine::control_token::matches;
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    let mut authorized = false;
    loop {
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => break,
            // The peer is gone before sending a line: nobody to tell.
            Err(_) if buf.is_empty() => break,
            Err(e) => e,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params, token: given } => {
                    let ok = given.as_deref().map(|g| matches(token, g));
                    if ok == Some(false) || (!authorized && ok.is_none()) {
                        let _ = writeln!(out, "{}", json!({"ok": false, "error": UNAUTHORIZED, "id": id}));
                        break;
                    }
                    authorized = true;
                    if method == "auth" {
                        // Authenticate without running anything in the app.
                        if writeln!(out, "{}", json!({"ok": true, "result": "authorized", "id": id})).is_err() {
                            break;
                        }
                        continue;
                    }
                    let (req, rx) = ControlRequest::new(method, params);
                    if tx.send(req).is_err() {
                        break;
                    }
                    // Wake the UI thread: egui only runs frames when something happens.
                    ctx.request_repaint();
                    let mut reply = rx
                        .recv_timeout(Duration::from_secs(60))
                        .unwrap_or_else(|_| json!({"ok": false, "error": "timed out waiting for the UI (is the window minimised?)"}));
                    if let Some(o) = reply.as_object_mut() {
                        o.insert("id".into(), id);
                    }
                    if writeln!(out, "{reply}").is_err() {
                        break;
                    }
                    continue;
                }
            },
        };
        // The connection closes either way; a peer that can't read the reason loses nothing.
        let _ = writeln!(out, "{}", json!({"ok": false, "error": reject}));
        break;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Send `input` on a new connection and read reply lines until the server closes it.
    fn exchange(addr: std::net::SocketAddr, input: &[u8]) -> Vec<Json> {
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(input).unwrap();
        // A read timeout ends the list too: the server kept the connection open.
        BufReader::new(c).lines().map_while(Result::ok).map(|l| serde_json::from_str(&l).unwrap()).collect()
    }

    fn serve_on_loopback() -> (std::net::SocketAddr, Receiver<ControlRequest>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        (addr, serve_listener(listener, Arc::from(TOKEN), egui::Context::default()).unwrap())
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// `{"id":ID,"method":METHOD,"token":TOKEN}` as a line.
    fn authed(id: u32, method: &str) -> String {
        format!("{{\"id\":{id},\"method\":\"{method}\",\"token\":\"{TOKEN}\"}}\n")
    }

    /// A browser can POST to the port: its request line closes the connection before the JSON
    /// line in its body is read, so nothing is dispatched.
    #[test]
    fn an_http_request_closes_the_connection_before_its_body_is_dispatched() {
        let (addr, rx) = serve_on_loopback();
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:7979\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"cell.set\",\"params\":{\"cell\":\"A1\",\"input\":\"x\"}}}\n";
        let replies = exchange(addr, http);
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert!(replies[0]["error"].as_str().unwrap().contains("closing the connection"), "{replies:?}");
        assert!(rx.try_recv().is_err(), "the body was dispatched");
    }

    #[test]
    fn a_line_that_is_not_a_request_closes_the_connection() {
        let (addr, rx) = serve_on_loopback();
        std::thread::spawn(move || {
            for req in rx {
                let _ = req.reply.send(json!({"ok": true}));
            }
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // CRLF, a blank line, then a request that relies on the first one's token.
        c.write_all(format!("{}\r\n\n{{\"id\":2,\"method\":\"b\"}}\n", authed(1, "a").trim_end()).as_bytes()).unwrap();
        let mut r = BufReader::new(c);
        for id in [1, 2] {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            assert_eq!(serde_json::from_str::<Json>(&line).unwrap()["id"], id, "{line}");
        }
        for junk in [&b"hello\n"[..], b"[1,2]\n", b"{\"id\":1}\n", b"{\"method\":5}\n", b"\xff\xfe\n", b"GET /x HTTP/1.1\n"] {
            let mut input = authed(0, "auth").into_bytes();
            input.extend_from_slice(junk);
            input.extend_from_slice(b"{\"id\":2,\"method\":\"b\"}\n");
            let replies = exchange(addr, &input);
            assert_eq!(replies.len(), 2, "{replies:?}");
            assert_eq!(replies[0]["result"], "authorized");
            assert_eq!(replies[1]["ok"], false);
        }
        assert!(is_http_request_line("OPTIONS * HTTP/1.1"));
        assert!(is_http_request_line("PRI * HTTP/2.0"));
        assert!(!is_http_request_line("{\"method\":\"GET /\"}"));
    }

    #[test]
    fn an_overlong_line_is_refused_without_reading_the_rest() {
        let mut buf = Vec::new();
        let long = [b'x'; 100];
        let mut input: &[u8] = &long;
        assert!(read_bounded_line(&mut input, &mut buf, 10).is_err());
        assert_eq!(input.len(), 100 - 11, "the rest of the line is left unread");
    }

    /// Without the token nothing reaches the app: one `unauthorized` reply, then the connection closes.
    #[test]
    fn a_request_without_the_token_is_refused_and_not_dispatched() {
        let (addr, rx) = serve_on_loopback();
        for first in [
            r#"{"id":1,"method":"engine.execute","params":{"command":"cell.set","params":{"cell":"A1","input":"x"}}}"#.to_string(),
            format!(r#"{{"id":1,"method":"engine.execute","token":"{}"}}"#, "f".repeat(64)),
            format!(r#"{{"id":1,"method":"auth","token":"{}"}}"#, &TOKEN[..63]),
        ] {
            let replies = exchange(addr, format!("{first}\n{}", authed(2, "m")).as_bytes());
            assert_eq!(replies.len(), 1, "{replies:?}");
            assert_eq!(replies[0]["ok"], false);
            assert!(replies[0]["error"].as_str().unwrap().starts_with("unauthorized"), "{replies:?}");
            assert_eq!(replies[0]["id"], 1);
        }
        assert!(rx.try_recv().is_err(), "an unauthorized request was dispatched");
    }

    /// The token on the first request authorizes the connection; it may repeat on later ones, and
    /// a wrong one later still closes the connection.
    #[test]
    fn the_token_on_a_request_authorizes_the_connection() {
        let (addr, rx) = serve_on_loopback();
        std::thread::spawn(move || {
            for req in rx {
                let m = req.method.clone();
                let _ = req.reply.send(json!({"ok": true, "result": m}));
            }
        });
        let input = format!(
            "{}{{\"id\":2,\"method\":\"document.inspect\"}}\n{}{{\"id\":4,\"method\":\"n\",\"token\":\"{}\"}}\n{{\"id\":5,\"method\":\"o\"}}\n",
            authed(1, "ui.inspect"),
            authed(3, "m"),
            "0".repeat(64)
        );
        let replies = exchange(addr, input.as_bytes());
        assert_eq!(replies.len(), 4, "{replies:?}");
        assert_eq!(replies[0]["result"], "ui.inspect");
        assert_eq!(replies[1]["result"], "document.inspect");
        assert_eq!(replies[2]["result"], "m");
        assert!(replies[3]["error"].as_str().unwrap().starts_with("unauthorized"), "{replies:?}");
    }
}
