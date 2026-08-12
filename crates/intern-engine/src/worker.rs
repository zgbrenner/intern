//! Client for the out-of-process parser worker.
//!
//! Extraction runs in a separate process so that a malformed PDF can only take
//! down the parser, never the app. This module owns the JSONL protocol, the
//! supervision, and the translation from worker pages into a [`DocumentSource`].

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
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::domain::{DocumentSource, PageImage, PageOrigin, ParserWarning, SourcePage};

pub const PROTOCOL_VERSION: u32 = 1;
pub const EXTRACTION_TIMEOUT_SECONDS: u64 = 30 * 60;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STALE_WORKSPACE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// OCR below this mean confidence is reported as a fact-affecting warning.
const LOW_OCR_CONFIDENCE: f32 = 75.0;

/// Progress from the extraction stage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractProgress {
    pub stage: String,
    pub current: usize,
    pub total: Option<usize>,
}

/// Why extraction did not produce a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractFailure {
    pub code: String,
    pub retryable: bool,
    pub crashed: bool,
    pub canceled: bool,
}

impl ExtractFailure {
    pub fn new(code: impl Into<String>, retryable: bool, crashed: bool) -> Self {
        Self {
            code: code.into(),
            retryable,
            crashed,
            canceled: false,
        }
    }

    pub fn crashed() -> Self {
        Self::new("WORKER_CRASHED", true, true)
    }

    pub fn canceled() -> Self {
        Self {
            code: "CANCELED".into(),
            retryable: false,
            crashed: false,
            canceled: true,
        }
    }
}

/// Turns a path into extracted pages. Implemented by the supervised worker and
/// by test doubles.
pub trait DocumentExtractor: Send + Sync {
    fn extract(
        &self,
        request_id: &str,
        path: &Path,
        progress: &mut dyn FnMut(ExtractProgress),
    ) -> Result<DocumentSource, ExtractFailure>;
    fn cancel(&self, request_id: &str) -> Result<(), ExtractFailure>;
    fn restart(&self) -> Result<(), ExtractFailure>;
    fn shutdown(&self) -> Result<(), ExtractFailure> {
        Ok(())
    }
}

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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum WorkerPageSource {
    Native,
    Ocr,
    AnyDoc,
    Text,
}

impl From<WorkerPageSource> for PageOrigin {
    fn from(value: WorkerPageSource) -> Self {
        match value {
            WorkerPageSource::Native => Self::Native,
            WorkerPageSource::Ocr => Self::Ocr,
            WorkerPageSource::AnyDoc => Self::Office,
            WorkerPageSource::Text => Self::PlainText,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct WorkerImage {
    page_number: usize,
    mime_type: String,
    data_base64: String,
}

pub fn decode_worker_response(line: &str) -> Result<WorkerResponse, ExtractFailure> {
    let response: WorkerResponse = serde_json::from_str(line)
        .map_err(|_| ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(ExtractFailure::new(
            "PROTOCOL_VERSION_UNSUPPORTED",
            false,
            false,
        ));
    }
    Ok(response)
}

/// Converts a worker reply into the engine's input type, keeping page
/// boundaries intact so distillation can reason about position.
pub fn adapt_document(document: WorkerDocument) -> Result<DocumentSource, ExtractFailure> {
    let numbers = document
        .pages
        .iter()
        .map(|page| page.page_number)
        .collect::<HashSet<_>>();
    if document.pages.iter().any(|page| page.page_number == 0)
        || numbers.len() != document.pages.len()
    {
        return Err(ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false));
    }

    let mut parser_warnings = document
        .warnings
        .into_iter()
        .map(|code| ParserWarning::new(code, true))
        .collect::<Vec<_>>();
    let low_confidence = document.pages.iter().any(|page| {
        page.source == WorkerPageSource::Ocr
            && page
                .ocr_confidence
                .is_some_and(|confidence| confidence < LOW_OCR_CONFIDENCE)
    });
    if low_confidence
        && !parser_warnings
            .iter()
            .any(|warning| warning.code == "LOW_OCR_CONFIDENCE")
    {
        parser_warnings.push(ParserWarning::new("LOW_OCR_CONFIDENCE", true));
    }
    if document.truncated
        && !parser_warnings
            .iter()
            .any(|warning| warning.code == "TEXT_TRUNCATED")
    {
        parser_warnings.push(ParserWarning::new("TEXT_TRUNCATED", true));
    }

    let page_image = document
        .optional_image
        .map(|image| {
            if !numbers.contains(&image.page_number) || image.mime_type != "image/png" {
                return Err(ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image.data_base64)
                .map_err(|_| ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
            Ok(PageImage {
                page_number: image.page_number,
                media_type: image.mime_type,
                bytes,
            })
        })
        .transpose()?;

    Ok(DocumentSource {
        pages: document
            .pages
            .into_iter()
            .map(|page| SourcePage {
                page_number: page.page_number,
                text: page.text,
                origin: page.source.into(),
                ocr_confidence: page
                    .ocr_confidence
                    .map(|confidence| confidence.round().clamp(0.0, 100.0) as u32),
            })
            .collect(),
        parser_warnings,
        page_image,
    })
}

struct WorkerProcess {
    child: Mutex<Child>,
    input: Mutex<ChildStdin>,
    output: Mutex<Receiver<Result<String, ()>>>,
}

impl WorkerProcess {
    fn write(&self, value: serde_json::Value) -> Result<(), ExtractFailure> {
        let mut input = self.input.lock().map_err(|_| ExtractFailure::crashed())?;
        serde_json::to_writer(&mut *input, &value)
            .map_err(|_| ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false))?;
        input
            .write_all(b"\n")
            .map_err(|_| ExtractFailure::crashed())?;
        input.flush().map_err(|_| ExtractFailure::crashed())
    }

    fn receive(&self, timeout: Duration) -> Result<String, ExtractFailure> {
        match self
            .output
            .lock()
            .map_err(|_| ExtractFailure::crashed())?
            .recv_timeout(timeout)
        {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(())) | Err(RecvTimeoutError::Disconnected) => Err(ExtractFailure::crashed()),
            Err(RecvTimeoutError::Timeout) => {
                Err(ExtractFailure::new("WORKER_POLL_TIMEOUT", true, false))
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
    temp_root: Option<PathBuf>,
    handshake_timeout: Duration,
    extraction_timeout: Duration,
    running: Mutex<Option<Arc<WorkerProcess>>>,
    active: Mutex<Option<String>>,
    canceled: Mutex<HashSet<String>>,
}

impl SupervisedWorker {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            temp_root: None,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            extraction_timeout: Duration::from_secs(EXTRACTION_TIMEOUT_SECONDS),
            running: Mutex::new(None),
            active: Mutex::new(None),
            canceled: Mutex::new(HashSet::new()),
        }
    }

    pub fn with_temp_root(executable: impl Into<PathBuf>, temp_root: impl Into<PathBuf>) -> Self {
        let mut worker = Self::new(executable);
        worker.temp_root = Some(temp_root.into());
        worker
    }

    #[doc(hidden)]
    pub fn with_timeouts(
        executable: impl Into<PathBuf>,
        handshake_timeout: Duration,
        extraction_timeout: Duration,
    ) -> Self {
        let mut worker = Self::new(executable);
        worker.handshake_timeout = handshake_timeout;
        worker.extraction_timeout = extraction_timeout;
        worker
    }

    fn ensure_running(&self) -> Result<Arc<WorkerProcess>, ExtractFailure> {
        let mut running = self.running.lock().map_err(|_| ExtractFailure::crashed())?;
        if let Some(process) = running.as_ref() {
            return Ok(Arc::clone(process));
        }
        let process = Arc::new(launch(&self.executable, self.temp_root.as_deref())?);
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

impl DocumentExtractor for SupervisedWorker {
    fn extract(
        &self,
        request_id: &str,
        path: &Path,
        progress: &mut dyn FnMut(ExtractProgress),
    ) -> Result<DocumentSource, ExtractFailure> {
        {
            let mut active = self.active.lock().map_err(|_| ExtractFailure::crashed())?;
            if active.is_some() {
                return Err(ExtractFailure::new("WORKER_BUSY", true, false));
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
            let deadline = Instant::now() + self.extraction_timeout;
            loop {
                if Instant::now() >= deadline {
                    process.terminate();
                    self.clear_running(&process);
                    return Err(ExtractFailure::new("RESOURCE_LIMIT", false, false));
                }
                let line = match process.receive(POLL_INTERVAL) {
                    Err(error) if error.code == "WORKER_POLL_TIMEOUT" => continue,
                    Err(error) => {
                        self.clear_running(&process);
                        if self.was_canceled(request_id) {
                            return Err(ExtractFailure::canceled());
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
                    } => progress(ExtractProgress {
                        stage,
                        current,
                        total,
                    }),
                    WorkerEvent::Parsed { document } => {
                        return match adapt_document(document) {
                            Ok(document) => Ok(document),
                            Err(error) => {
                                process.terminate();
                                self.clear_running(&process);
                                Err(error)
                            }
                        };
                    }
                    WorkerEvent::Error { code, .. } if code == "CANCELED" => {
                        return Err(ExtractFailure::canceled());
                    }
                    WorkerEvent::Error {
                        code, retryable, ..
                    } => return Err(ExtractFailure::new(code, retryable, false)),
                    WorkerEvent::Hello { .. } => {
                        process.terminate();
                        self.clear_running(&process);
                        return Err(ExtractFailure::new("WORKER_PROTOCOL_INVALID", false, false));
                    }
                }
            }
        })();
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
        result
    }

    fn cancel(&self, request_id: &str) -> Result<(), ExtractFailure> {
        let active_matches = self
            .active
            .lock()
            .map_err(|_| ExtractFailure::crashed())?
            .as_deref()
            == Some(request_id);
        if !active_matches {
            return Err(ExtractFailure::new("ITEM_NOT_ACTIVE", false, false));
        }
        self.canceled
            .lock()
            .map_err(|_| ExtractFailure::crashed())?
            .insert(request_id.to_owned());
        let process = self
            .running
            .lock()
            .map_err(|_| ExtractFailure::crashed())?
            .take()
            .ok_or_else(ExtractFailure::crashed)?;
        let _ = process.write(json!({
            "protocol_version": PROTOCOL_VERSION,
            "request_id": format!("cancel-{request_id}"),
            "command": {"type": "cancel", "target_request_id": request_id},
        }));
        process.terminate();
        Ok(())
    }

    fn restart(&self) -> Result<(), ExtractFailure> {
        if let Some(process) = self
            .running
            .lock()
            .map_err(|_| ExtractFailure::crashed())?
            .take()
        {
            process.terminate();
        }
        let process = Arc::new(launch(&self.executable, self.temp_root.as_deref())?);
        handshake(&process, self.handshake_timeout)?;
        *self.running.lock().map_err(|_| ExtractFailure::crashed())? = Some(process);
        Ok(())
    }

    fn shutdown(&self) -> Result<(), ExtractFailure> {
        self.stop();
        Ok(())
    }
}

impl Drop for SupervisedWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn launch(executable: &Path, temp_root: Option<&Path>) -> Result<WorkerProcess, ExtractFailure> {
    let mut command = Command::new(executable);
    if let Some(temp_root) = temp_root {
        command.env("INTERN_TEMP_ROOT", temp_root);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|_| ExtractFailure::crashed())?;
    let input = match child.stdin.take() {
        Some(input) => input,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExtractFailure::crashed());
        }
    };
    let output = match child.stdout.take() {
        Some(output) => output,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExtractFailure::crashed());
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
        return Err(ExtractFailure::crashed());
    }
    Ok(WorkerProcess {
        child: Mutex::new(child),
        input: Mutex::new(input),
        output: Mutex::new(receiver),
    })
}

pub fn prepare_worker_temp_root(root: &Path, max_entries: usize) -> std::io::Result<usize> {
    std::fs::create_dir_all(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(root)? {
        if removed >= max_entries {
            break;
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        #[cfg(windows)]
        let metadata = std::fs::symlink_metadata(entry.path())?;
        let owned_name = entry
            .file_name()
            .to_str()
            .is_some_and(is_stale_owned_workspace_name);
        #[cfg(windows)]
        let reparse_point = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let reparse_point = false;
        if owned_name && file_type.is_dir() && !file_type.is_symlink() && !reparse_point {
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn is_stale_owned_workspace_name(name: &str) -> bool {
    if !name.starts_with("intern-worker-") {
        return false;
    }
    let mut suffix = name.rsplit('-');
    let Some(_nonce) = suffix.next().and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    let Some(created_nanos) = suffix.next().and_then(|value| value.parse::<u128>().ok()) else {
        return false;
    };
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    now_nanos.saturating_sub(created_nanos) >= STALE_WORKSPACE_AGE.as_nanos()
}

fn handshake(process: &WorkerProcess, timeout: Duration) -> Result<(), ExtractFailure> {
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
        Err(ExtractFailure::new("WORKER_HANDSHAKE_FAILED", false, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(json: &str) -> Result<DocumentSource, ExtractFailure> {
        let response = decode_worker_response(json).unwrap();
        match response.event {
            WorkerEvent::Parsed { document } => adapt_document(document),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn page_structure_survives_the_protocol() {
        let source = parsed(
            r#"{"protocol_version":1,"request_id":"r","event":{"type":"parsed","document":{
                "pages":[
                  {"page_number":1,"text":"First page.","source":"native","ocr_confidence":null,"vision_escalated":false},
                  {"page_number":2,"text":"Second page.","source":"native","ocr_confidence":null,"vision_escalated":false}
                ],"warnings":[],"truncated":false,"optional_image":null}}}"#,
        )
        .unwrap();
        assert_eq!(source.pages.len(), 2);
        assert_eq!(source.pages[1].page_number, 2);
        assert_eq!(source.pages[0].origin, PageOrigin::Native);
        assert!(source.parser_warnings.is_empty());
    }

    #[test]
    fn low_confidence_ocr_becomes_a_fact_affecting_warning() {
        let source = parsed(
            r#"{"protocol_version":1,"request_id":"r","event":{"type":"parsed","document":{
                "pages":[{"page_number":1,"text":"scan","source":"ocr","ocr_confidence":41.5,"vision_escalated":true}],
                "warnings":[],"truncated":false,"optional_image":null}}}"#,
        )
        .unwrap();
        assert!(
            source
                .parser_warnings
                .iter()
                .any(|warning| warning.code == "LOW_OCR_CONFIDENCE" && warning.field_affecting)
        );
        assert_eq!(source.pages[0].ocr_confidence, Some(42));
    }

    #[test]
    fn a_duplicate_page_number_is_a_protocol_violation() {
        assert!(parsed(
            r#"{"protocol_version":1,"request_id":"r","event":{"type":"parsed","document":{
                "pages":[
                  {"page_number":1,"text":"a","source":"native","ocr_confidence":null,"vision_escalated":false},
                  {"page_number":1,"text":"b","source":"native","ocr_confidence":null,"vision_escalated":false}
                ],"warnings":[],"truncated":false,"optional_image":null}}}"#
        )
        .is_err());
    }

    #[test]
    fn an_image_for_a_page_that_does_not_exist_is_refused() {
        assert!(parsed(
            r#"{"protocol_version":1,"request_id":"r","event":{"type":"parsed","document":{
                "pages":[{"page_number":1,"text":"a","source":"native","ocr_confidence":null,"vision_escalated":false}],
                "warnings":[],"truncated":false,
                "optional_image":{"page_number":9,"mime_type":"image/png","data_base64":"AAA="}}}}"#
        )
        .is_err());
    }

    #[test]
    fn an_unsupported_protocol_version_is_rejected() {
        assert_eq!(
            decode_worker_response(
                r#"{"protocol_version":2,"request_id":"r","event":{"type":"hello","worker_version":"x"}}"#
            )
            .unwrap_err()
            .code,
            "PROTOCOL_VERSION_UNSUPPORTED"
        );
    }
}
