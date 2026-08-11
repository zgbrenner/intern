use intern_app::{
    pipeline::{PipelineProgress, WorkerBoundary},
    worker::{SupervisedWorker, WorkerEvent, adapt_parsed_document, decode_worker_response},
};

#[test]
fn owned_worker_wire_requires_protocol_v1_and_preserves_progress_strings() {
    let response = decode_worker_response(
        r#"{"protocol_version":1,"request_id":"queue-7-1","event":{"type":"progress","stage":"extracting","current":2,"total":9}}"#,
    ).unwrap();
    assert_eq!(response.request_id, "queue-7-1");
    assert_eq!(response.event, WorkerEvent::Progress {
        stage: "extracting".into(), current: 2, total: Some(9),
    });
    assert!(decode_worker_response(
        r#"{"protocol_version":2,"request_id":"queue-7-1","event":{"type":"hello","worker_version":"wrong"}}"#,
    ).is_err());
}

#[test]
fn worker_document_is_explicitly_adapted_to_page_marked_core_text_and_image_bytes() {
    let response = decode_worker_response(
        r#"{"protocol_version":1,"request_id":"queue-4-1","event":{"type":"parsed","document":{"pages":[{"page_number":1,"text":"Title","source":"native","ocr_confidence":null,"vision_escalated":false},{"page_number":2,"text":"Signature","source":"ocr","ocr_confidence":71.0,"vision_escalated":true}],"warnings":["LOW_OCR_CONFIDENCE"],"truncated":true,"optional_image":{"page_number":2,"mime_type":"image/png","data_base64":"aW1hZ2U="}}}}"#,
    ).unwrap();
    let WorkerEvent::Parsed { document } = response.event else { panic!("expected parsed event"); };

    let adapted = adapt_parsed_document(document).unwrap();

    assert_eq!(adapted.extracted.text, "[Page 1]\nTitle\n\n[Page 2]\nSignature");
    assert_eq!(adapted.extracted.parser_warnings.len(), 2);
    assert!(adapted.extracted.parser_warnings.iter().all(|warning| warning.field_affecting));
    assert_eq!(adapted.image.unwrap().bytes, b"image");
}

#[test]
fn worker_rejects_unknown_sources_and_invalid_image_references() {
    assert!(decode_worker_response(
        r#"{"protocol_version":1,"request_id":"bad-source","event":{"type":"parsed","document":{"pages":[{"page_number":1,"text":"x","source":"guess","ocr_confidence":null,"vision_escalated":false}],"warnings":[],"truncated":false,"optional_image":null}}}"#,
    ).is_err());
    let response = decode_worker_response(
        r#"{"protocol_version":1,"request_id":"bad-image","event":{"type":"parsed","document":{"pages":[{"page_number":1,"text":"x","source":"native","ocr_confidence":null,"vision_escalated":false}],"warnings":[],"truncated":false,"optional_image":{"page_number":2,"mime_type":"image/jpeg","data_base64":"eA=="}}}}"#,
    ).unwrap();
    let WorkerEvent::Parsed { document } = response.event else { panic!("expected parsed event"); };
    assert!(adapt_parsed_document(document).is_err());
}

#[cfg(target_os = "linux")]
mod process_fixture {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn fixture(root: &Path, body: &str) -> (PathBuf, PathBuf, PathBuf) {
        let executable = root.join("worker-fixture.sh");
        let pid = root.join("worker.pid");
        let parsed = root.join("parse.received");
        fs::write(&executable, format!(
            "#!/bin/sh\necho $$ > '{}'\n{}\n",
            pid.display(), body.replace("$PARSED", &format!("'{}'", parsed.display())),
        )).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();
        (executable, pid, parsed)
    }

    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(5)); }
        assert!(path.exists(), "fixture marker was not written");
    }

    fn assert_reaped(pid_file: &Path) {
        wait_for(pid_file);
        let pid = fs::read_to_string(pid_file).unwrap();
        let process = PathBuf::from(format!("/proc/{}", pid.trim()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while process.exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(5)); }
        assert!(!process.exists(), "worker child was not waited after termination");
    }

    const HELLO: &str = "read hello\nprintf '%s\\n' '{\"protocol_version\":1,\"request_id\":\"hello\",\"event\":{\"type\":\"hello\",\"worker_version\":\"fixture\"}}'";

    #[test]
    fn malformed_handshake_kills_and_reaps_the_fixture() {
        let temp = tempdir().unwrap();
        let (executable, pid, _) = fixture(temp.path(), "read hello\necho not-json\nsleep 30");
        let worker = SupervisedWorker::with_timeouts(executable, Duration::from_millis(200), Duration::from_secs(1));
        let error = worker.parse("bad-handshake", Path::new("x.pdf"), &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "WORKER_PROTOCOL_INVALID");
        assert_reaped(&pid);
    }

    #[test]
    fn parser_timeout_kills_and_reaps_the_fixture() {
        let temp = tempdir().unwrap();
        let body = format!("{HELLO}\nread parse\necho yes > $PARSED\nsleep 30");
        let (executable, pid, parsed) = fixture(temp.path(), &body);
        let worker = SupervisedWorker::with_timeouts(executable, Duration::from_secs(1), Duration::from_millis(60));
        let error = worker.parse("timeout", Path::new("x.pdf"), &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "RESOURCE_LIMIT");
        wait_for(&parsed);
        assert_reaped(&pid);
    }

    #[test]
    fn cancel_kills_and_reaps_the_fixture_and_reports_canceled() {
        let temp = tempdir().unwrap();
        let body = format!("{HELLO}\nread parse\necho yes > $PARSED\nsleep 30");
        let (executable, pid, parsed) = fixture(temp.path(), &body);
        let worker = Arc::new(SupervisedWorker::with_timeouts(executable, Duration::from_secs(1), Duration::from_secs(5)));
        let parsing = Arc::clone(&worker);
        let join = thread::spawn(move || {
            let mut progress = |_progress: PipelineProgress| {};
            parsing.parse("cancel-me", Path::new("x.pdf"), &mut progress)
        });
        wait_for(&parsed);
        worker.cancel("cancel-me").unwrap();
        assert!(join.join().unwrap().unwrap_err().canceled);
        assert_reaped(&pid);
    }

    #[test]
    fn crash_is_observed_and_the_exited_child_is_waited() {
        let temp = tempdir().unwrap();
        let body = format!("{HELLO}\nread parse\necho yes > $PARSED\nexit 17");
        let (executable, pid, parsed) = fixture(temp.path(), &body);
        let worker = SupervisedWorker::with_timeouts(executable, Duration::from_secs(1), Duration::from_secs(1));
        let error = worker.parse("crash", Path::new("x.pdf"), &mut |_| {}).unwrap_err();
        assert!(error.crashed);
        wait_for(&parsed);
        assert_reaped(&pid);
    }

    #[test]
    fn unexpected_hello_during_parse_terminates_and_reaps_the_worker() {
        let temp = tempdir().unwrap();
        let body = format!(
            "{HELLO}\nread parse\necho yes > $PARSED\nprintf '%s\\n' '{{\"protocol_version\":1,\"request_id\":\"unexpected\",\"event\":{{\"type\":\"hello\",\"worker_version\":\"again\"}}}}'\nsleep 30",
        );
        let (executable, pid, parsed) = fixture(temp.path(), &body);
        let worker = SupervisedWorker::with_timeouts(executable, Duration::from_secs(1), Duration::from_secs(1));
        let error = worker.parse("unexpected", Path::new("x.pdf"), &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "WORKER_PROTOCOL_INVALID");
        wait_for(&parsed);
        assert_reaped(&pid);
    }
}
