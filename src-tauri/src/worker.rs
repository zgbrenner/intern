use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;

use crate::{
    model::client::ImageInput,
    pipeline::{
        PARSER_TIMEOUT_SECONDS, ParsedDocument, PipelineProgress, WorkerBoundary, WorkerFailure,
    },
};
use intern_core::{ExtractedDocument, ParserWarning};

const PROTOCOL_VERSION: u32 = 1;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkerResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub event: WorkerEvent,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerEvent {
    Hello {
        worker_version: String,
    },
    Progress {
        stage: String,
        current: usize,
        total: Option<usize>,
    },
    Parsed {
        document: WorkerDocument,
    },
    Error {
        code: String,
        message: String,
        retryable: bool,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkerDocument {
    pages: Vec<WorkerPage>,
    warnings: Vec<String>,
    truncated: bool,
    optional_image: Option<WorkerImage>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkerPage {
    page_number: usize,
    text: String,
    source: WorkerPageSource,
    ocr_confidence: Option<f32>,
    vision_escalated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum WorkerPageSource {
    Native,
    Ocr,
    AnyDoc,
    Text,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkerImage {
    page_number: usize,
    mime_type: String,
    data_base64: String,
}

pub fn decode_worker_response(line: &str) -> Result<WorkerResponse, WorkerFailure> {
    let response: WorkerResponse = serde_json::from_str(line)
        .map_err(|_| WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerFailure::new(
            "PROTOCOL_VERSION_UNSUPPORTED",
            false,
            false,
        ));
    }
    Ok(response)
}

pub fn adapt_parsed_document(document: WorkerDocument) -> Result<ParsedDocument, WorkerFailure> {
    if document.pages.iter().any(|page| page.page_number == 0)
        || document
            .pages
            .iter()
            .map(|page| page.page_number)
            .collect::<HashSet<_>>()
            .len()
            != document.pages.len()
    {
        return Err(WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false));
    }
    let page_numbers = document
        .pages
        .iter()
        .map(|page| page.page_number)
        .collect::<HashSet<_>>();
    let text = document
        .pages
        .iter()
        .map(|page| format!("[Page {}]\n{}", page.page_number, page.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut parser_warnings = document
        .warnings
        .into_iter()
        .map(|code| ParserWarning {
            code,
            field_affecting: true,
        })
        .collect::<Vec<_>>();
    if document.pages.iter().any(|page| {
        page.source == WorkerPageSource::Ocr
            && (page.vision_escalated
                || page
                    .ocr_confidence
                    .is_some_and(|confidence| confidence < 75.0))
    }) && !parser_warnings
        .iter()
        .any(|warning| warning.code == "LOW_OCR_CONFIDENCE")
    {
        parser_warnings.push(ParserWarning {
            code: "LOW_OCR_CONFIDENCE".into(),
            field_affecting: true,
        });
    }
    if document.truncated
        && !parser_warnings
            .iter()
            .any(|warning| warning.code == "TEXT_TRUNCATED")
    {
        parser_warnings.push(ParserWarning {
            code: "TEXT_TRUNCATED".into(),
            field_affecting: true,
        });
    }
    let image = document
        .optional_image
        .map(|image| {
            if !page_numbers.contains(&image.page_number) || image.mime_type != "image/png" {
                return Err(WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image.data_base64)
                .map_err(|_| WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
            Ok(ImageInput {
                media_type: image.mime_type,
                bytes,
            })
        })
        .transpose()?;
    Ok(ParsedDocument {
        extracted: ExtractedDocument {
            text,
            parser_warnings,
        },
        image,
    })
}

struct WorkerProcess {
    child: Mutex<Child>,
    input: Mutex<ChildStdin>,
    output: Mutex<Receiver<Result<String, ()>>>,
}

impl WorkerProcess {
    fn write(&self, value: serde_json::Value) -> Result<(), WorkerFailure> {
        let mut input = self.input.lock().map_err(|_| WorkerFailure::crashed())?;
        serde_json::to_writer(&mut *input, &value)
            .map_err(|_| WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
        input
            .write_all(b"\n")
            .map_err(|_| WorkerFailure::crashed())?;
        input.flush().map_err(|_| WorkerFailure::crashed())
    }

    fn receive(&self, timeout: Duration) -> Result<String, WorkerFailure> {
        match self
            .output
            .lock()
            .map_err(|_| WorkerFailure::crashed())?
            .recv_timeout(timeout)
        {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(())) | Err(RecvTimeoutError::Disconnected) => Err(WorkerFailure::crashed()),
            Err(RecvTimeoutError::Timeout) => {
                Err(WorkerFailure::new("WORKER_POLL_TIMEOUT", true, false))
            }
        }
    }

    fn terminate(&self) {
        if let Ok(mut child) = self.child.lock() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub struct SupervisedWorker {
    executable: PathBuf,
    handshake_timeout: Duration,
    parser_timeout: Duration,
    running: Mutex<Option<Arc<WorkerProcess>>>,
    active: Mutex<Option<String>>,
    canceled: Mutex<HashSet<String>>,
}

impl SupervisedWorker {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            handshake_timeout: HANDSHAKE_TIMEOUT,
            parser_timeout: Duration::from_secs(PARSER_TIMEOUT_SECONDS),
            running: Mutex::new(None),
            active: Mutex::new(None),
            canceled: Mutex::new(HashSet::new()),
        }
    }

    #[doc(hidden)]
    pub fn with_timeouts(
        executable: impl Into<PathBuf>,
        handshake_timeout: Duration,
        parser_timeout: Duration,
    ) -> Self {
        let mut worker = Self::new(executable);
        worker.handshake_timeout = handshake_timeout;
        worker.parser_timeout = parser_timeout;
        worker
    }

    fn ensure_running(&self) -> Result<Arc<WorkerProcess>, WorkerFailure> {
        let mut running = self.running.lock().map_err(|_| WorkerFailure::crashed())?;
        if let Some(process) = running.as_ref() {
            return Ok(Arc::clone(process));
        }
        let process = Arc::new(launch(&self.executable)?);
        handshake(&process, self.handshake_timeout)?;
        *running = Some(Arc::clone(&process));
        Ok(process)
    }

    fn clear_running(&self, expected: &Arc<WorkerProcess>) {
        if let Ok(mut running) = self.running.lock() {
            if running
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            {
                *running = None;
            }
        }
    }

    pub fn stop(&self) {
        let process = self
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.take());
        if let Some(process) = process {
            let _ = process.write(json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": "shutdown",
                "command": {"type": "shutdown"},
            }));
            process.terminate();
        }
    }

    fn was_canceled(&self, request_id: &str) -> bool {
        self.canceled
            .lock()
            .map(|mut canceled| canceled.remove(request_id))
            .unwrap_or(false)
    }
}

impl WorkerBoundary for SupervisedWorker {
    fn parse(
        &self,
        request_id: &str,
        path: &Path,
        progress: &mut dyn FnMut(PipelineProgress),
    ) -> Result<ParsedDocument, WorkerFailure> {
        {
            let mut active = self.active.lock().map_err(|_| WorkerFailure::crashed())?;
            if active.is_some() {
                return Err(WorkerFailure::new("WORKER_BUSY", true, false));
            }
            *active = Some(request_id.to_owned());
        }
        let result = (|| {
            let process = self.ensure_running()?;
            if let Err(error) = process.write(json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": request_id,
                "command": {"type": "parse", "path": path},
            })) {
                process.terminate();
                self.clear_running(&process);
                return Err(error);
            }
            let deadline = Instant::now() + self.parser_timeout;
            loop {
                if Instant::now() >= deadline {
                    process.terminate();
                    self.clear_running(&process);
                    return Err(WorkerFailure::new("RESOURCE_LIMIT", false, false));
                }
                let line = match process.receive(POLL_INTERVAL) {
                    Err(error) if error.code == "WORKER_POLL_TIMEOUT" => continue,
                    Err(error) => {
                        self.clear_running(&process);
                        if self.was_canceled(request_id) {
                            return Err(WorkerFailure::canceled());
                        }
                        return Err(error);
                    }
                    Ok(line) => line,
                };
                let response = match decode_worker_response(&line) {
                    Ok(response) => response,
                    Err(error) => {
                        process.terminate();
                        self.clear_running(&process);
                        return Err(error);
                    }
                };
                if response.request_id != request_id {
                    continue;
                }
                match response.event {
                    WorkerEvent::Progress {
                        stage,
                        current,
                        total,
                    } => progress(PipelineProgress {
                        item_id: 0,
                        stage,
                        current,
                        total,
                    }),
                    WorkerEvent::Parsed { document } => {
                        return match adapt_parsed_document(document) {
                            Ok(document) => Ok(document),
                            Err(error) => {
                                process.terminate();
                                self.clear_running(&process);
                                Err(error)
                            }
                        };
                    }
                    WorkerEvent::Error {
                        code, retryable: _, ..
                    } if code == "CANCELED" => {
                        return Err(WorkerFailure::canceled());
                    }
                    WorkerEvent::Error {
                        code, retryable, ..
                    } => {
                        return Err(WorkerFailure::new(code, retryable, false));
                    }
                    WorkerEvent::Hello { .. } => {
                        process.terminate();
                        self.clear_running(&process);
                        return Err(WorkerFailure::new("WORKER_PROTOCOL_INVALID", false, false));
                    }
                }
            }
        })();
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
        result
    }

    fn cancel(&self, request_id: &str) -> Result<(), WorkerFailure> {
        let active_matches = self
            .active
            .lock()
            .map_err(|_| WorkerFailure::crashed())?
            .as_deref()
            == Some(request_id);
        if !active_matches {
            return Err(WorkerFailure::new("ITEM_NOT_ACTIVE", false, false));
        }
        self.canceled
            .lock()
            .map_err(|_| WorkerFailure::crashed())?
            .insert(request_id.to_owned());
        let process = self
            .running
            .lock()
            .map_err(|_| WorkerFailure::crashed())?
            .take()
            .ok_or_else(WorkerFailure::crashed)?;
        let _ = process.write(json!({
            "protocol_version": PROTOCOL_VERSION,
            "request_id": format!("cancel-{request_id}"),
            "command": {"type": "cancel", "target_request_id": request_id},
        }));
        process.terminate();
        Ok(())
    }

    fn restart(&self) -> Result<(), WorkerFailure> {
        if let Some(process) = self
            .running
            .lock()
            .map_err(|_| WorkerFailure::crashed())?
            .take()
        {
            process.terminate();
        }
        let process = Arc::new(launch(&self.executable)?);
        handshake(&process, self.handshake_timeout)?;
        *self.running.lock().map_err(|_| WorkerFailure::crashed())? = Some(process);
        Ok(())
    }

    fn shutdown(&self) -> Result<(), WorkerFailure> {
        self.stop();
        Ok(())
    }
}

impl Drop for SupervisedWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn launch(executable: &Path) -> Result<WorkerProcess, WorkerFailure> {
    let mut command = Command::new(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|_| WorkerFailure::crashed())?;
    let input = match child.stdin.take() {
        Some(input) => input,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerFailure::crashed());
        }
    };
    let output = match child.stdout.take() {
        Some(output) => output,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerFailure::crashed());
        }
    };
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("intern-worker-jsonl".into())
        .spawn(move || {
            let reader = BufReader::new(output);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(Ok(line)).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(Err(()));
                        return;
                    }
                }
            }
            let _ = sender.send(Err(()));
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(WorkerFailure::crashed());
    }
    Ok(WorkerProcess {
        child: Mutex::new(child),
        input: Mutex::new(input),
        output: Mutex::new(receiver),
    })
}

fn handshake(process: &WorkerProcess, timeout: Duration) -> Result<(), WorkerFailure> {
    process.write(json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "hello",
        "command": {"type": "hello"},
    }))?;
    let response = decode_worker_response(&process.receive(timeout)?)?;
    if response.request_id == "hello" && matches!(response.event, WorkerEvent::Hello { .. }) {
        Ok(())
    } else {
        process.terminate();
        Err(WorkerFailure::new("WORKER_HANDSHAKE_FAILED", false, false))
    }
}
