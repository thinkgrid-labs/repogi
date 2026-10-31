//! Model Context Protocol server over stdio: newline-delimited JSON-RPC 2.0.
//!
//! stdout carries only protocol messages; diagnostics go to stderr. A reader
//! thread parses input and applies cancellations immediately, while one worker
//! answers requests in arrival order. Tools are read-only adapters over the
//! store/index/retrieval crates and never execute repository code.
mod artifacts;
mod tools;

use std::{
    collections::HashMap,
    io::{self, BufRead, Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    thread,
};

use anyhow::{Context, Result};
use code_intel_store::Interrupt;
use serde_json::{Value, json};

/// Newest first. A client requesting another version receives the newest.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING: usize = 64;
const MAX_ERROR_BYTES: usize = 4096;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const NOT_INITIALIZED: i64 = -32002;
const SERVER_BUSY: i64 = -32000;

pub use tools::Profile;

pub fn serve(roots: Vec<PathBuf>, profile: Profile, tools: Option<Vec<String>>) -> Result<()> {
    let server = tools::Server::new(roots, profile, tools)?;
    eprintln!(
        "repogi mcp {}: serving {} root(s) over stdio; tools: {}",
        env!("CARGO_PKG_VERSION"),
        server.roots().len(),
        server.tools().join(", ")
    );
    run(server, io::stdin().lock(), io::stdout())
}

enum Message {
    Request {
        id: Value,
        key: String,
        method: String,
        params: Value,
    },
}

/// A panicking request must not wedge the server, so poisoning is ignored.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Pending {
    cancelled: bool,
    cancellable: bool,
    interrupt: Option<Interrupt>,
}

struct Shared {
    output: Mutex<Box<dyn Write + Send>>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl Shared {
    fn send(&self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        let mut output = lock(&self.output);
        if output
            .write_all(line.as_bytes())
            .and_then(|()| output.flush())
            .is_err()
        {
            // The client closed our stdout; nobody can receive further replies.
            std::process::exit(0);
        }
    }

    fn error(&self, id: &Value, code: i64, message: &str) {
        let mut end = message.len().min(MAX_ERROR_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let message = &message[..end];
        self.send(&json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}));
    }

    fn cancel(&self, key: &str) {
        let mut pending = lock(&self.pending);
        if let Some(entry) = pending.get_mut(key) {
            if !entry.cancellable {
                return;
            }
            entry.cancelled = true;
            if let Some(interrupt) = &entry.interrupt {
                interrupt.interrupt();
            }
        }
    }
}

fn run(
    server: tools::Server,
    mut input: impl BufRead,
    output: impl Write + Send + 'static,
) -> Result<()> {
    let shared = Arc::new(Shared {
        output: Mutex::new(Box::new(output)),
        pending: Mutex::new(HashMap::new()),
    });
    let (sender, receiver) = mpsc::channel::<Message>();
    let worker = {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("mcp-worker".into())
            .spawn(move || work(&server, &shared, &receiver))
            .context("start MCP worker thread")?
    };
    let mut buffer = Vec::new();
    while let Some(line) = read_message(&mut input, &mut buffer).context("read MCP input")? {
        let Ok(line) = line else {
            shared.error(
                &Value::Null,
                PARSE_ERROR,
                &format!("message exceeds {MAX_MESSAGE_BYTES} bytes"),
            );
            continue;
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if let Some(message) = classify(&shared, line)
            && sender.send(message).is_err()
        {
            break;
        }
    }
    // stdin closed: the client is shutting down. Already-queued requests are
    // bounded and answered before exit, so piped sessions are not truncated.
    drop(sender);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("MCP worker thread panicked"))
}

/// One newline-terminated message without its terminator, `Err(())` when it
/// exceeded the size limit (the remainder is discarded), or `None` at EOF.
fn read_message<'a>(
    input: &mut impl BufRead,
    buffer: &'a mut Vec<u8>,
) -> io::Result<Option<Result<&'a [u8], ()>>> {
    buffer.clear();
    let limit = u64::try_from(MAX_MESSAGE_BYTES + 2).unwrap_or(u64::MAX);
    if input.by_ref().take(limit).read_until(b'\n', buffer)? == 0 {
        return Ok(None);
    }
    let terminated = buffer.ends_with(b"\n");
    if terminated {
        buffer.pop();
        if buffer.ends_with(b"\r") {
            buffer.pop();
        }
    }
    if buffer.len() > MAX_MESSAGE_BYTES {
        if !terminated {
            let mut rest = Vec::new();
            loop {
                rest.clear();
                let read = input.by_ref().take(65_536).read_until(b'\n', &mut rest)?;
                if read == 0 || rest.ends_with(b"\n") {
                    break;
                }
            }
        }
        return Ok(Some(Err(())));
    }
    Ok(Some(Ok(buffer.as_slice())))
}

/// Validate framing and route cancellations; returns work for the worker.
fn classify(shared: &Shared, line: &[u8]) -> Option<Message> {
    let value: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(error) => {
            shared.error(&Value::Null, PARSE_ERROR, &format!("invalid JSON: {error}"));
            return None;
        }
    };
    let Value::Object(object) = value else {
        let message = if value.is_array() {
            "JSON-RPC batches are not supported; send one message per line"
        } else {
            "a JSON-RPC message must be an object"
        };
        shared.error(&Value::Null, INVALID_REQUEST, message);
        return None;
    };
    let id = object.get("id").cloned();
    let valid_id = matches!(&id, Some(Value::String(_)))
        || id
            .as_ref()
            .is_some_and(|id| id.as_i64().is_some() || id.as_u64().is_some());
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        if object.contains_key("result") || object.contains_key("error") {
            return None; // A response to a request we never send; ignore it.
        }
        shared.error(
            if valid_id {
                id.as_ref().unwrap_or(&Value::Null)
            } else {
                &Value::Null
            },
            INVALID_REQUEST,
            "missing method",
        );
        return None;
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        if valid_id {
            shared.error(
                id.as_ref().unwrap_or(&Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            );
        }
        return None;
    }
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        if method == "notifications/cancelled" {
            if let Some(request) = params.get("requestId") {
                shared.cancel(&request.to_string());
            }
            return None;
        }
        // This passive server sends no client requests. Initialized and unknown
        // notifications need no work; never queue an unbounded notification flood.
        return None;
    };
    if !valid_id {
        shared.error(
            &Value::Null,
            INVALID_REQUEST,
            "id must be a string or integer",
        );
        return None;
    }
    let key = id.to_string();
    {
        let mut pending = lock(&shared.pending);
        if pending.contains_key(&key) {
            drop(pending);
            shared.error(&id, INVALID_REQUEST, "request id is already in flight");
            return None;
        }
        if pending.len() >= MAX_PENDING {
            drop(pending);
            shared.error(&id, SERVER_BUSY, "too many requests in flight; retry later");
            return None;
        }
        pending.insert(
            key.clone(),
            Pending {
                cancellable: method != "initialize",
                ..Pending::default()
            },
        );
    }
    Some(Message::Request {
        id,
        key,
        method: method.to_owned(),
        params,
    })
}

#[derive(Default)]
struct Session {
    initialized: bool,
}

fn work(server: &tools::Server, shared: &Shared, receiver: &mpsc::Receiver<Message>) {
    let mut session = Session::default();
    for message in receiver {
        let (id, key, method, params) = match message {
            Message::Request {
                id,
                key,
                method,
                params,
            } => (id, key, method, params),
        };
        let cancelled = || {
            lock(&shared.pending)
                .get(&key)
                .is_none_or(|entry| entry.cancelled)
        };
        let outcome = if cancelled() {
            None
        } else {
            let register = |interrupt: Interrupt| {
                if let Some(entry) = lock(&shared.pending).get_mut(&key) {
                    if entry.cancelled {
                        interrupt.interrupt();
                    }
                    entry.interrupt = Some(interrupt);
                }
            };
            Some(
                catch_unwind(AssertUnwindSafe(|| {
                    handle(server, &mut session, &method, &params, &register)
                }))
                .unwrap_or_else(|_| {
                    Err((
                        INTERNAL_ERROR,
                        format!("internal error while handling {method}; see server stderr"),
                    ))
                }),
            )
        };
        let was_cancelled = lock(&shared.pending)
            .remove(&key)
            .is_none_or(|entry| entry.cancelled);
        // Cancelled requests receive no response, per the MCP cancellation rules.
        if was_cancelled {
            continue;
        }
        match outcome {
            Some(Ok(result)) => shared.send(&json!({"jsonrpc":"2.0","id":id,"result":result})),
            Some(Err((code, message))) => shared.error(&id, code, &message),
            None => {}
        }
    }
}

fn handle(
    server: &tools::Server,
    session: &mut Session,
    method: &str,
    params: &Value,
    register: &dyn Fn(Interrupt),
) -> Result<Value, (i64, String)> {
    if !params.is_null() && !params.is_object() {
        return Err((INVALID_PARAMS, "params must be an object".into()));
    }
    match method {
        "initialize" => {
            if session.initialized {
                return Err((INVALID_REQUEST, "initialize was already received".into()));
            }
            let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
                return Err((
                    INVALID_PARAMS,
                    "initialize requires params.protocolVersion".into(),
                ));
            };
            if !params.get("capabilities").is_some_and(Value::is_object)
                || !params.get("clientInfo").is_some_and(|info| {
                    info.get("name").is_some_and(Value::is_string)
                        && info.get("version").is_some_and(Value::is_string)
                })
            {
                return Err((
                    INVALID_PARAMS,
                    "initialize requires capabilities and clientInfo.name/version".into(),
                ));
            }
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|version| **version == requested)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            session.initialized = true;
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "repogi",
                    "title": "Repogi code intelligence",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": server.instructions(),
            }))
        }
        "ping" => Ok(json!({})),
        _ if !session.initialized => Err((
            NOT_INITIALIZED,
            "server not initialized; send initialize first".into(),
        )),
        "tools/list" => Ok(json!({ "tools": server.definitions() })),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Err((INVALID_PARAMS, "tools/call requires params.name".into()));
            };
            let arguments = match params.get("arguments") {
                None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
                Some(value @ Value::Object(_)) => value.clone(),
                Some(_) => {
                    return Err((INVALID_PARAMS, "params.arguments must be an object".into()));
                }
            };
            let Some(output) = server.call(name, arguments, register) else {
                return Err((INVALID_PARAMS, format!("unknown tool: {name}")));
            };
            Ok(json!({
                "content": [{"type": "text", "text": output.text}],
                "isError": output.is_error,
            }))
        }
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    }
}
