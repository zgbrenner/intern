use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::extract::{CancellationToken, ExtractedDocument, ExtractionError};

pub const PROTOCOL_VERSION: u32 = 1;
pub const WORKER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_PROTOCOL_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: String,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Hello,
    Parse { path: PathBuf },
    Cancel { target_request_id: String },
    Shutdown,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: String,
    pub event: Event,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Hello {
        worker_version: &'static str,
    },
    Progress {
        stage: &'static str,
        current: usize,
        total: Option<usize>,
    },
    Parsed {
        document: ExtractedDocument,
    },
    Error {
        code: String,
        message: String,
        retryable: bool,
    },
}

impl Response {
    pub fn new(request_id: impl Into<String>, event: Event) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.into(),
            event,
        }
    }

    pub fn extraction_error(request_id: impl Into<String>, error: ExtractionError) -> Self {
        Self::new(
            request_id,
            Event::Error {
                code: error.code().to_owned(),
                message: error.to_string(),
                retryable: error.retryable(),
            },
        )
    }
}

fn frame_error(message: impl Into<String>) -> Response {
    Response::new(
        "",
        Event::Error {
            code: "PARSE_FAILED".to_owned(),
            message: message.into(),
            retryable: false,
        },
    )
}

/// Reads one bounded JSONL frame. At most `MAX_PROTOCOL_LINE_BYTES + 2` bytes
/// are retained, and an oversized frame is drained before the next call.
fn read_protocol_line<B: BufRead>(reader: &mut B) -> io::Result<Option<Result<String, Response>>> {
    let mut bytes = Vec::with_capacity(8 * 1024);
    let read = {
        let mut bounded = (&mut *reader).take((MAX_PROTOCOL_LINE_BYTES + 2) as u64);
        bounded.read_until(b'\n', &mut bytes)?
    };
    if read == 0 {
        return Ok(None);
    }

    let terminated = bytes.last() == Some(&b'\n');
    let hit_bound = !terminated && bytes.len() == MAX_PROTOCOL_LINE_BYTES + 2;
    if hit_bound {
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                reader.consume(newline + 1);
                break;
            }
            let length = available.len();
            reader.consume(length);
        }
    }
    if terminated {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_PROTOCOL_LINE_BYTES || hit_bound {
        return Ok(Some(Err(frame_error(format!(
            "protocol line exceeds {MAX_PROTOCOL_LINE_BYTES} bytes"
        )))));
    }
    Ok(Some(match String::from_utf8(bytes) {
        Ok(line) => Ok(line),
        Err(_) => Err(frame_error("protocol line is not valid UTF-8")),
    }))
}

// Keeping the concrete wire error here lets frame parsing and request decoding
// share one error type without allocating every protocol failure.
#[allow(clippy::result_large_err)]
pub fn decode_request(line: &str) -> Result<Request, Response> {
    let request: Request = serde_json::from_str(line).map_err(|error| {
        Response::new(
            "",
            Event::Error {
                code: "PARSE_FAILED".to_owned(),
                message: error.to_string(),
                retryable: false,
            },
        )
    })?;
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(Response::new(
            request.request_id,
            Event::Error {
                code: "PROTOCOL_VERSION_UNSUPPORTED".to_owned(),
                message: format!("supported protocol version is {PROTOCOL_VERSION}"),
                retryable: false,
            },
        ));
    }
    Ok(request)
}

pub fn handle_line(line: &str) -> Result<String, serde_json::Error> {
    let response = match decode_request(line) {
        Ok(request) => match request.command {
            Command::Hello => Response::new(
                request.request_id,
                Event::Hello {
                    worker_version: WORKER_VERSION,
                },
            ),
            Command::Parse { .. } => Response::new(
                request.request_id,
                Event::Progress {
                    stage: "accepted",
                    current: 0,
                    total: None,
                },
            ),
            Command::Cancel { .. } => Response::new(
                request.request_id,
                Event::Progress {
                    stage: "cancel_requested",
                    current: 0,
                    total: None,
                },
            ),
            Command::Shutdown => Response::new(
                request.request_id,
                Event::Progress {
                    stage: "shutdown",
                    current: 0,
                    total: None,
                },
            ),
        },
        Err(response) => response,
    };
    serde_json::to_string(&response)
}

pub trait EventSink {
    fn emit(&mut self, response: &Response) -> io::Result<()>;
}

struct JsonLineSink<'a, W: Write> {
    writer: &'a mut W,
}

impl<W: Write> EventSink for JsonLineSink<'_, W> {
    fn emit(&mut self, response: &Response) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, response).map_err(io::Error::other)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

#[allow(clippy::result_large_err)]
pub fn run_control_loop<R, W, E, F>(
    reader: R,
    mut output: W,
    mut diagnostics: E,
    mut parse_handler: F,
) -> io::Result<()>
where
    R: Read,
    W: Write,
    E: Write,
    F: FnMut(Request, &mut dyn EventSink) -> Result<(), ExtractionError>,
{
    let mut reader = BufReader::new(reader);
    while let Some(frame) = read_protocol_line(&mut reader)? {
        let request = match frame.and_then(|line| decode_request(&line)) {
            Ok(request) => request,
            Err(response) => {
                let code = match &response.event {
                    Event::Error { code, .. } => code.as_str(),
                    _ => "PROTOCOL_ERROR",
                };
                writeln!(
                    diagnostics,
                    "{{\"level\":\"warning\",\"code\":{}}}",
                    serde_json::to_string(code).map_err(io::Error::other)?
                )?;
                JsonLineSink {
                    writer: &mut output,
                }
                .emit(&response)?;
                continue;
            }
        };
        match request.command.clone() {
            Command::Hello => JsonLineSink {
                writer: &mut output,
            }
            .emit(&Response::new(
                request.request_id,
                Event::Hello {
                    worker_version: WORKER_VERSION,
                },
            ))?,
            Command::Shutdown => break,
            Command::Parse { .. } => {
                let request_id = request.request_id.clone();
                let mut sink = JsonLineSink {
                    writer: &mut output,
                };
                if let Err(error) = parse_handler(request, &mut sink) {
                    sink.emit(&Response::extraction_error(request_id, error))?;
                }
            }
            Command::Cancel { .. } => JsonLineSink {
                writer: &mut output,
            }
            .emit(&Response::new(
                request.request_id,
                Event::Progress {
                    stage: "cancel_requested",
                    current: 0,
                    total: None,
                },
            ))?,
        }
    }
    Ok(())
}

fn emit_locked<W: Write>(output: &Arc<Mutex<W>>, response: &Response) -> io::Result<()> {
    let mut output = output
        .lock()
        .map_err(|_| io::Error::other("protocol output lock poisoned"))?;
    JsonLineSink {
        writer: &mut *output,
    }
    .emit(response)
}

struct ActiveRequestGuard {
    active: Arc<Mutex<HashMap<String, CancellationToken>>>,
    request_id: String,
    cleared: bool,
}

impl ActiveRequestGuard {
    fn clear(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.request_id);
            self.cleared = true;
        }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        if !self.cleared
            && let Ok(mut active) = self.active.lock()
        {
            active.remove(&self.request_id);
        }
    }
}

#[allow(clippy::result_large_err)]
pub fn run_concurrent_worker<R, W, E, X>(
    reader: R,
    output: W,
    mut diagnostics: E,
    extractor: X,
) -> io::Result<()>
where
    R: Read,
    W: Write + Send + 'static,
    E: Write,
    X: Fn(PathBuf, CancellationToken) -> Result<ExtractedDocument, ExtractionError>
        + Send
        + Sync
        + 'static,
{
    let output = Arc::new(Mutex::new(output));
    let extractor = Arc::new(extractor);
    let active: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    let mut reader = BufReader::new(reader);
    while let Some(frame) = read_protocol_line(&mut reader)? {
        let request = match frame.and_then(|line| decode_request(&line)) {
            Ok(request) => request,
            Err(response) => {
                let code = match &response.event {
                    Event::Error { code, .. } => code.as_str(),
                    _ => "PROTOCOL_ERROR",
                };
                writeln!(
                    diagnostics,
                    "{{\"level\":\"warning\",\"code\":{}}}",
                    serde_json::to_string(code).map_err(io::Error::other)?
                )?;
                emit_locked(&output, &response)?;
                continue;
            }
        };
        match request.command {
            Command::Hello => emit_locked(
                &output,
                &Response::new(
                    request.request_id,
                    Event::Hello {
                        worker_version: WORKER_VERSION,
                    },
                ),
            )?,
            Command::Cancel { target_request_id } => {
                if let Some(token) = active
                    .lock()
                    .map_err(|_| io::Error::other("active request lock poisoned"))?
                    .get(&target_request_id)
                {
                    token.cancel();
                }
                emit_locked(
                    &output,
                    &Response::new(
                        request.request_id,
                        Event::Progress {
                            stage: "cancel_requested",
                            current: 0,
                            total: None,
                        },
                    ),
                )?;
            }
            Command::Shutdown => break,
            Command::Parse { path } => {
                let mut active_guard = active
                    .lock()
                    .map_err(|_| io::Error::other("active request lock poisoned"))?;
                if !active_guard.is_empty() {
                    emit_locked(
                        &output,
                        &Response::new(
                            request.request_id,
                            Event::Error {
                                code: "WORKER_BUSY".to_owned(),
                                message: "the worker processes exactly one document at a time"
                                    .to_owned(),
                                retryable: true,
                            },
                        ),
                    )?;
                    continue;
                }
                let request_id = request.request_id;
                let token = CancellationToken::new();
                active_guard.insert(request_id.clone(), token.clone());
                drop(active_guard);
                emit_locked(
                    &output,
                    &Response::new(
                        request_id.clone(),
                        Event::Progress {
                            stage: "extracting",
                            current: 0,
                            total: None,
                        },
                    ),
                )?;
                let thread_output = Arc::clone(&output);
                let thread_active = Arc::clone(&active);
                let thread_extractor = Arc::clone(&extractor);
                let spawn_request_id = request_id.clone();
                match std::thread::Builder::new().spawn(move || {
                    let mut lifecycle = ActiveRequestGuard {
                        active: thread_active,
                        request_id: request_id.clone(),
                        cleared: false,
                    };
                    let response =
                        match catch_unwind(AssertUnwindSafe(|| thread_extractor(path, token))) {
                            Ok(Ok(document)) => {
                                Response::new(request_id.clone(), Event::Parsed { document })
                            }
                            Ok(Err(error)) => Response::extraction_error(request_id.clone(), error),
                            Err(_) => Response::new(
                                request_id.clone(),
                                Event::Error {
                                    code: "WORKER_THREAD_PANIC".to_owned(),
                                    message: "document extraction panicked".to_owned(),
                                    retryable: true,
                                },
                            ),
                        };
                    // The request is no longer active before its terminal event is visible.
                    lifecycle.clear();
                    let _ = emit_locked(&thread_output, &response);
                }) {
                    Ok(thread) => threads.push(thread),
                    Err(error) => {
                        if let Ok(mut active) = active.lock() {
                            active.remove(&spawn_request_id);
                        }
                        emit_locked(
                            &output,
                            &Response::new(
                                spawn_request_id,
                                Event::Error {
                                    code: "WORKER_THREAD_START_FAILED".to_owned(),
                                    message: error.to_string(),
                                    retryable: true,
                                },
                            ),
                        )?;
                    }
                }
            }
        }
    }
    if let Ok(active) = active.lock() {
        for token in active.values() {
            token.cancel();
        }
    }
    for thread in threads {
        if thread.join().is_err() {
            writeln!(
                diagnostics,
                "{{\"level\":\"error\",\"code\":\"WORKER_THREAD_PANIC\"}}"
            )?;
        }
    }
    Ok(())
}
