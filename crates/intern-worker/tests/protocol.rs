use std::io::{Cursor, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use intern_worker::extract::ExtractedDocument;
use intern_worker::protocol::{
    MAX_PROTOCOL_LINE_BYTES, handle_line, run_concurrent_worker, run_control_loop,
};

#[test]
fn hello_reports_exact_protocol_version() {
    let response =
        handle_line(r#"{"protocol_version":1,"request_id":"r1","command":{"type":"hello"}}"#)
            .unwrap();

    assert_eq!(
        response,
        r#"{"protocol_version":1,"request_id":"r1","event":{"type":"hello","worker_version":"0.1.0-alpha.4"}}"#
    );
}

#[test]
fn malformed_json_is_an_error_event_and_the_next_request_is_processed() {
    let input = joined_lines([b"not json\n".to_vec(), hello_line("r2"), shutdown_line()]);
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();

    run_control_loop(
        Cursor::new(&input),
        &mut output,
        &mut diagnostics,
        |_request, _sink| unreachable!("no parse request was supplied"),
    )
    .unwrap();

    let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
    assert_eq!(lines.len(), 2);
    let error: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(error["event"]["type"], "error");
    assert_eq!(error["event"]["code"], "PARSE_FAILED");
    assert_eq!(error["request_id"], "");
    assert_eq!(
        lines[1],
        r#"{"protocol_version":1,"request_id":"r2","event":{"type":"hello","worker_version":"0.1.0-alpha.4"}}"#
    );
    assert!(
        std::str::from_utf8(&diagnostics)
            .unwrap()
            .contains("PARSE_FAILED")
    );
}

#[test]
fn invalid_utf8_is_rejected_and_the_next_request_is_processed() {
    let mut input = vec![0xff, b'\n'];
    input.extend_from_slice(&hello_line("after-utf8"));
    let mut output = Vec::new();

    run_control_loop(
        Cursor::new(input),
        &mut output,
        Vec::new(),
        |_request, _sink| unreachable!(),
    )
    .unwrap();

    let events: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"]["code"], "PARSE_FAILED");
    assert_eq!(events[1]["request_id"], "after-utf8");
    assert_eq!(events[1]["event"]["type"], "hello");
}

#[test]
fn oversized_line_is_drained_and_the_next_request_is_processed() {
    let mut input = vec![b'x'; MAX_PROTOCOL_LINE_BYTES + 1];
    input.push(b'\n');
    input.extend_from_slice(&hello_line("after-large"));
    let mut output = Vec::new();

    run_control_loop(
        Cursor::new(input),
        &mut output,
        Vec::new(),
        |_request, _sink| unreachable!(),
    )
    .unwrap();

    let events: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["event"]["code"], "PARSE_FAILED");
    assert_eq!(events[1]["request_id"], "after-large");
    assert_eq!(events[1]["event"]["type"], "hello");
}

#[test]
fn unsupported_protocol_version_returns_stable_version_error() {
    let response =
        handle_line(r#"{"protocol_version":2,"request_id":"r3","command":{"type":"hello"}}"#)
            .unwrap();
    let event: serde_json::Value = serde_json::from_str(&response).unwrap();

    assert_eq!(event["protocol_version"], 1);
    assert_eq!(event["request_id"], "r3");
    assert_eq!(event["event"]["type"], "error");
    assert_eq!(event["event"]["code"], "PROTOCOL_VERSION_UNSUPPORTED");
    assert_eq!(event["event"]["retryable"], false);
}

#[test]
fn stdout_contains_only_flushed_json_lines() {
    struct FlushCountingWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl std::io::Write for FlushCountingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    let input = joined_lines([hello_line("r4"), shutdown_line()]);
    let mut output = FlushCountingWriter {
        bytes: Vec::new(),
        flushes: 0,
    };
    let mut diagnostics = Vec::new();

    run_control_loop(
        Cursor::new(&input),
        &mut output,
        &mut diagnostics,
        |_request, _sink| unreachable!(),
    )
    .unwrap();

    assert_eq!(output.flushes, 1);
    assert!(
        std::str::from_utf8(&output.bytes)
            .unwrap()
            .lines()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
    );
    assert!(diagnostics.is_empty());
}

#[test]
fn cancel_interrupts_the_active_request_and_shutdown_joins_it() {
    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let input = br#"{"protocol_version":1,"request_id":"parse-1","command":{"type":"parse","path":"scan.pdf"}}
{"protocol_version":1,"request_id":"cancel-1","command":{"type":"cancel","target_request_id":"parse-1"}}
{"protocol_version":1,"request_id":"done","command":{"type":"shutdown"}}
"#;
    let output = SharedWriter::default();
    let captured = output.clone();
    let mut diagnostics = Vec::new();

    run_concurrent_worker(
        Cursor::new(input),
        output,
        &mut diagnostics,
        |_path, cancel| {
            loop {
                cancel.check()?;
                std::thread::sleep(Duration::from_millis(1));
            }
        },
    )
    .unwrap();

    let bytes = captured.0.lock().unwrap().clone();
    let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["event"]["stage"] == "cancel_requested")
    );
    assert!(
        events
            .iter()
            .any(|event| event["event"]["code"] == "CANCELED")
    );
    assert!(diagnostics.is_empty());
}

#[derive(Clone, Default)]
struct SignalingWriter(Arc<(Mutex<Vec<u8>>, Condvar)>);

impl Write for SignalingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let (output, changed) = &*self.0;
        output.lock().unwrap().extend_from_slice(bytes);
        changed.notify_all();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct TerminalGatedReader {
    chunks: Vec<Vec<u8>>,
    next: usize,
    output: SignalingWriter,
    first_terminal: &'static [u8],
}

impl std::io::Read for TerminalGatedReader {
    fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
        if self.next >= self.chunks.len() {
            return Ok(0);
        }
        if self.next == 1 {
            let (bytes, changed) = &*self.output.0;
            let mut bytes = bytes.lock().unwrap();
            while !bytes
                .windows(self.first_terminal.len())
                .any(|window| window == self.first_terminal)
            {
                bytes = changed.wait(bytes).unwrap();
            }
        }
        let chunk = &self.chunks[self.next];
        assert!(chunk.len() <= target.len());
        target[..chunk.len()].copy_from_slice(chunk);
        self.next += 1;
        Ok(chunk.len())
    }
}

fn empty_document() -> ExtractedDocument {
    ExtractedDocument {
        pages: vec![],
        warnings: vec![],
        truncated: false,
        optional_image: None,
    }
}

fn parse_line(request_id: &str, path: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "request_id": request_id,
        "command": { "type": "parse", "path": path },
    }))
    .unwrap();
    line.push(b'\n');
    line
}

fn hello_line(request_id: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 1,
        "request_id": request_id,
        "command": { "type": "hello" },
    }))
    .unwrap();
    line.push(b'\n');
    line
}

fn shutdown_line() -> Vec<u8> {
    let mut line =
        br#"{"protocol_version":1,"request_id":"done","command":{"type":"shutdown"}}"#.to_vec();
    line.push(b'\n');
    line
}

fn joined_lines(lines: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    lines.into_iter().flatten().collect()
}

#[test]
fn a_parse_sent_after_the_terminal_event_is_not_reported_busy() {
    let output = SignalingWriter::default();
    let captured = output.clone();
    let reader = TerminalGatedReader {
        chunks: vec![
            parse_line("first", "one.txt"),
            joined_lines([parse_line("second", "two.txt"), shutdown_line()]),
        ],
        next: 0,
        output,
        first_terminal: b"\"type\":\"parsed\"",
    };

    run_concurrent_worker(reader, captured.clone(), Vec::new(), |_path, _cancel| {
        Ok(empty_document())
    })
    .unwrap();
    let (bytes, _) = &*captured.0;
    let bytes = bytes.lock().unwrap().clone();
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.matches("\"type\":\"parsed\"").count(), 2);
    assert!(!text.contains("WORKER_BUSY"));
}

#[test]
fn extractor_panic_is_terminal_and_does_not_leak_the_active_slot() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let output = SignalingWriter::default();
    let captured = output.clone();
    let reader = TerminalGatedReader {
        chunks: vec![
            parse_line("panic", "one.txt"),
            joined_lines([parse_line("after", "two.txt"), shutdown_line()]),
        ],
        next: 0,
        output,
        first_terminal: b"WORKER_THREAD_PANIC",
    };
    let calls = Arc::new(AtomicUsize::new(0));

    run_concurrent_worker(
        reader,
        captured.clone(),
        Vec::new(),
        move |_path, _cancel| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("fixture panic");
            }
            Ok(empty_document())
        },
    )
    .unwrap();
    let (bytes, _) = &*captured.0;
    let bytes = bytes.lock().unwrap().clone();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("WORKER_THREAD_PANIC"));
    assert!(text.contains("\"request_id\":\"after\",\"event\":{\"type\":\"parsed\""));
    assert!(!text.contains("WORKER_BUSY"));
}

#[test]
fn nul_in_request_id_cannot_break_thread_start_or_leak_the_active_slot() {
    let output = SignalingWriter::default();
    let captured = output.clone();
    let reader = TerminalGatedReader {
        chunks: vec![
            parse_line("nul\0id", "one.txt"),
            joined_lines([parse_line("after-nul", "two.txt"), shutdown_line()]),
        ],
        next: 0,
        output,
        first_terminal: b"\"type\":\"parsed\"",
    };

    run_concurrent_worker(reader, captured.clone(), Vec::new(), |_path, _cancel| {
        Ok(empty_document())
    })
    .unwrap();

    let (bytes, _) = &*captured.0;
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches("\"type\":\"parsed\"").count(), 2);
    assert!(text.contains(r#""request_id":"nul\u0000id""#));
    assert!(!text.contains("WORKER_THREAD_START_FAILED"));
    assert!(!text.contains("WORKER_BUSY"));
}
