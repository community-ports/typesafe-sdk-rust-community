//! A tiny scripted HTTP/1.1 server for exercising the client without the network.
//!
//! Responses are queued with [`MockServer::enqueue`] and served in order; every request is
//! recorded for assertions. Runs on plain `std` threads so it works from both async and
//! blocking tests.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

/// A scripted response.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Option<Duration>,
    pub drop_connection: bool,
}

impl Reply {
    pub fn empty(status: u16) -> Self {
        Reply { status, headers: Vec::new(), body: Vec::new(), delay: None, drop_connection: false }
    }

    pub fn json(status: u16, value: Value) -> Self {
        Reply::empty(status).header("content-type", "application/json").with_body(serde_json::to_vec(&value).unwrap())
    }

    pub fn text(status: u16, text: &str) -> Self {
        Reply::empty(status).header("content-type", "text/plain").with_body(text.as_bytes().to_vec())
    }

    /// Close the connection without writing a response.
    pub fn drop() -> Self {
        Reply { drop_connection: true, ..Reply::empty(0) }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

/// A request as received by the server.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The first value of a header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<Reply>,
    fallback: Option<Reply>,
    requests: Vec<RecordedRequest>,
}

pub struct MockServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

impl MockServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let state = Arc::clone(&shared);
                thread::spawn(move || handle(stream, state));
            }
        });
        MockServer { addr, state }
    }

    /// The server's base URL, e.g. `http://127.0.0.1:12345`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Queue the next response.
    pub fn enqueue(&self, reply: Reply) -> &Self {
        self.state.lock().unwrap().queue.push_back(reply);
        self
    }

    /// The response served when the queue is empty.
    pub fn fallback(&self, reply: Reply) -> &Self {
        self.state.lock().unwrap().fallback = Some(reply);
        self
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.lock().unwrap().requests.len()
    }

    pub fn last_request(&self) -> RecordedRequest {
        self.requests().pop().expect("at least one request was received")
    }
}

fn handle(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let length: usize =
        headers.iter().find(|(n, _)| n == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; length];
    if length > 0 {
        reader.read_exact(&mut body).expect("read body");
    }

    let reply = {
        let mut state = state.lock().unwrap();
        state.requests.push(RecordedRequest { method, path, headers, body });
        state.queue.pop_front().or_else(|| state.fallback.clone())
    };
    let reply = reply.unwrap_or_else(|| Reply::text(599, "mock server: no scripted response"));

    if let Some(delay) = reply.delay {
        thread::sleep(delay);
    }
    if reply.drop_connection {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let mut stream = stream;
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\nconnection: close\r\n\r\n", reply.body.len()));
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// A well-formed System One response body with one answer of each type.
pub fn system_one_body() -> Value {
    serde_json::json!({
        "model": "jev-2026-09-15",
        "usage": {"input_tokens": 120, "output_tokens": 12},
        "answers": {
            "billing": {"type": "noul", "noul": 0.98},
            "tone": {
                "type": "choice",
                "choice": "angry",
                "confidence": 0.9,
                "probabilities": {"angry": 0.8, "calm": 0.1, "excited": 0.1}
            },
            "urgency": {
                "type": "score",
                "score": 1.7,
                "confidence": 0.9,
                "legend": {"0": "Can wait", "1": "This week", "2": "Today"},
                "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8}
            }
        }
    })
}

pub fn models_body() -> Value {
    serde_json::json!({
        "models": [
            {"name": "jev-latest", "description": "General-purpose system one model.", "release_date": "2026-09-15"},
            {"name": "jev-2026-09-15", "description": "Pinned release.", "release_date": "2026-09-15"}
        ]
    })
}
