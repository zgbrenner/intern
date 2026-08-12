use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use intern_engine::{
    ModelFile, ModelRole,
    download::{
        CancellationToken, DISK_RESERVE_BYTES, DiskSpace, Downloader, ReqwestHttpTransport,
        SetupStage, install_selected_file, validate_selected_file,
    },
    error::EngineErrorCode as ModelErrorCode,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[derive(Clone)]
struct FixedDisk(u64);

impl DiskSpace for FixedDisk {
    fn available_bytes(&self, _path: &Path) -> Result<u64, intern_engine::EngineError> {
        Ok(self.0)
    }
}

struct FakeServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    join: Option<thread::JoinHandle<()>>,
}

enum StallPoint {
    BeforeHeaders,
    DuringBody { total: usize, prefix: Vec<u8> },
}

struct StallingServer {
    url: String,
    join: Option<thread::JoinHandle<()>>,
}

struct HeaderCancelServer {
    url: String,
    disconnected: mpsc::Receiver<()>,
    join: Option<thread::JoinHandle<()>>,
}

impl HeaderCancelServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (disconnected_sender, disconnected) = mpsc::channel();
        let join = thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = read_request(&mut stream);
            if stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .is_err()
            {
                return;
            }
            let mut byte = [0_u8; 1];
            loop {
                match std::io::Read::read(&mut stream, &mut byte) {
                    Ok(0) | Err(_) => {
                        let _ = disconnected_sender.send(());
                        return;
                    }
                    Ok(_) => {}
                }
            }
        });
        Self {
            url: format!("http://{address}/model.gguf"),
            disconnected,
            join: Some(join),
        }
    }

    fn wait_for_disconnect(&self) {
        self.disconnected
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
}

impl Drop for HeaderCancelServer {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            // Joining must not panic. A panic here can run while the test is
            // already unwinding, and a panic during a panic aborts the whole test
            // binary instead of failing one test - which is how a broken pipe in a
            // helper thread turned into STATUS_STACK_BUFFER_OVERRUN on CI after
            // every test had reported ok.
            let _ = join.join();
        }
    }
}

impl StallingServer {
    fn new(stall: StallPoint) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let join = thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = read_request(&mut stream);
            if let StallPoint::DuringBody { total, prefix } = stall {
                // This server exists to be cancelled mid-body, so the client
                // hanging up is the scenario under test, not a failure. Unwrapping
                // these writes panicked the thread on a broken pipe, and the
                // panic then propagated out of `join().unwrap()` in `Drop` - a
                // panic while panicking, which aborts the process. That is the
                // `STATUS_STACK_BUFFER_OVERRUN` the Windows runner reported after
                // every test had already printed `ok`.
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(&prefix);
                let _ = stream.flush();
                // Then stall until the client goes away, rather than for a fixed
                // 350 ms. The body test cancels as soon as the prefix reaches
                // disk, and a fixed sleep put that cancellation in a race with
                // the socket closing: whichever happened first decided whether
                // the download reported canceled or interrupted. Waiting for the
                // peer removes the competitor instead of tuning the odds.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                // Outlast the cancelling thread's own ten-second deadline, so the
                // socket closing can never be what ends the download.
                let deadline = Instant::now() + Duration::from_secs(25);
                let mut byte = [0_u8; 1];
                while Instant::now() < deadline {
                    match std::io::Read::read(&mut stream, &mut byte) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                            ) => {}
                        Err(_) => break,
                    }
                }
                return;
            }
            thread::sleep(Duration::from_millis(350));
        });
        Self {
            url: format!("http://{address}/model.gguf"),
            join: Some(join),
        }
    }
}

impl Drop for StallingServer {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            // Joining must not panic. A panic here can run while the test is
            // already unwinding, and a panic during a panic aborts the whole test
            // binary instead of failing one test - which is how a broken pipe in a
            // helper thread turned into STATUS_STACK_BUFFER_OVERRUN on CI after
            // every test had reported ok.
            let _ = join.join();
        }
    }
}

impl FakeServer {
    fn sequence(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let join = thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                if let Ok(mut seen) = seen.lock() {
                    seen.push(read_request(&mut stream));
                }
                // A client that cancelled, or a queued response the test never
                // asks for, must not panic this thread; see the note in
                // StallingServer for why a panic here aborts the whole binary.
                let _ = stream.write_all(&response);
            }
        });
        Self {
            url: format!("http://{address}/model.gguf"),
            requests,
            join: Some(join),
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            // Joining must not panic. A panic here can run while the test is
            // already unwinding, and a panic during a panic aborts the whole test
            // binary instead of failing one test - which is how a broken pipe in a
            // helper thread turned into STATUS_STACK_BUFFER_OVERRUN on CI after
            // every test had reported ok.
            let _ = join.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let Ok(clone) = stream.try_clone() else {
        return String::new();
    };
    let mut reader = BufReader::new(clone);
    let mut request = String::new();
    loop {
        let mut line = String::new();
        // A client that cancelled mid-request resets the connection; return what
        // arrived rather than panicking a helper thread.
        if reader.read_line(&mut line).is_err() {
            return request;
        }
        request.push_str(&line);
        if line == "\r\n" || line.is_empty() {
            return request;
        }
    }
}

fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut value = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, content) in headers {
        value.push_str(&format!("{name}: {content}\r\n"));
    }
    value.push_str("Connection: close\r\n\r\n");
    let mut bytes = value.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn interrupted_response(total_length: usize, received_body: &[u8]) -> Vec<u8> {
    let mut bytes =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {total_length}\r\nConnection: close\r\n\r\n")
            .into_bytes();
    bytes.extend_from_slice(received_body);
    bytes
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file(url: &str, bytes: &[u8]) -> ModelFile {
    ModelFile {
        name: "model.gguf".into(),
        role: ModelRole::Model,
        url: url.into(),
        size: bytes.len() as u64,
        sha256: digest(bytes),
    }
}

fn downloader(disk: u64) -> Downloader<ReqwestHttpTransport, FixedDisk> {
    Downloader::new(ReqwestHttpTransport::new().unwrap(), FixedDisk(disk))
}

/// Timeouts wide enough that cancellation is the only thing that can end the
/// download.
///
/// `short_timeout_downloader` caps the *whole* transfer at 180 ms, which is right
/// for asserting that a stalled server times out and wrong for asserting what
/// cancellation does: on a loaded runner the 180 ms clock beat the cancel and the
/// download reported `DownloadInterrupted` instead of `DownloadCanceled`.
///
/// The margins here are deliberately far larger than any plausible scheduling
/// slip, because the cancelling thread gives itself ten seconds to notice the
/// partial file. Every competing deadline has to sit outside that window, or the
/// test is only ever measuring which timer fired first.
fn patient_downloader(disk: u64) -> Downloader<ReqwestHttpTransport, FixedDisk> {
    Downloader::new(
        ReqwestHttpTransport::with_timeouts(
            Duration::from_secs(5),
            Duration::from_secs(20),
            Duration::from_secs(60),
        )
        .unwrap(),
        FixedDisk(disk),
    )
}

fn short_timeout_downloader(disk: u64) -> Downloader<ReqwestHttpTransport, FixedDisk> {
    Downloader::new(
        ReqwestHttpTransport::with_timeouts(
            Duration::from_millis(80),
            Duration::from_millis(80),
            Duration::from_millis(180),
        )
        .unwrap(),
        FixedDisk(disk),
    )
}

#[test]
fn fresh_download_is_verified_then_published() {
    let bytes = b"complete model bytes";
    let server = FakeServer::sequence(vec![response("200 OK", &[], bytes)]);
    let directory = tempdir().unwrap();
    let result = downloader(u64::MAX)
        .download(
            &file(&server.url, bytes),
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();

    assert_eq!(fs::read(result).unwrap(), bytes);
    assert!(!directory.path().join("model.gguf.partial").exists());
}

#[test]
fn confirmed_range_resumes_a_partial_download() {
    let bytes = b"resume these bytes";
    let split = 7;
    let directory = tempdir().unwrap();
    let server = FakeServer::sequence(vec![
        interrupted_response(bytes.len(), &bytes[..split]),
        response(
            "206 Partial Content",
            &[(
                "Content-Range",
                format!("bytes {split}-{}/{}", bytes.len() - 1, bytes.len()),
            )],
            &bytes[split..],
        ),
    ]);

    let expected = file(&server.url, bytes);
    let first_error = downloader(u64::MAX)
        .download(
            &expected,
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap_err();
    assert_eq!(first_error.code(), ModelErrorCode::DownloadInterrupted);
    assert_eq!(
        fs::read(directory.path().join("model.gguf.partial")).unwrap(),
        &bytes[..split]
    );

    downloader(u64::MAX)
        .download(
            &expected,
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();

    assert!(
        server.requests.lock().unwrap()[1]
            .to_ascii_lowercase()
            .contains(&format!("range: bytes={split}-"))
    );
    assert_eq!(
        fs::read(directory.path().join("model.gguf")).unwrap(),
        bytes
    );
}

#[test]
fn server_ignoring_range_restarts_instead_of_appending() {
    let bytes = b"authoritative full body";
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("model.gguf.partial"), b"stale").unwrap();
    let server = FakeServer::sequence(vec![response("200 OK", &[], bytes)]);

    downloader(u64::MAX)
        .download(
            &file(&server.url, bytes),
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap();

    assert!(
        server.requests.lock().unwrap()[0]
            .to_ascii_lowercase()
            .contains("range: bytes=5-")
    );
    assert_eq!(
        fs::read(directory.path().join("model.gguf")).unwrap(),
        bytes
    );
}

#[test]
fn wrong_digest_keeps_partial_and_never_publishes() {
    let bytes = b"corrupted";
    let expected = b"different";
    let server = FakeServer::sequence(vec![response("200 OK", &[], bytes)]);
    let directory = tempdir().unwrap();
    let error = downloader(u64::MAX)
        .download(
            &file(&server.url, expected),
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::ModelFileInvalid);
    assert!(directory.path().join("model.gguf.partial").exists());
    assert!(!directory.path().join("model.gguf").exists());
}

#[test]
fn insufficient_disk_is_rejected_before_request() {
    let bytes = b"model";
    let directory = tempdir().unwrap();
    let error = downloader(512 * 1024 * 1024 + bytes.len() as u64 - 1)
        .download(
            &file("http://127.0.0.1:1/model", bytes),
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::InsufficientDisk);
}

#[test]
fn cancellation_retains_reusable_partial_file() {
    let bytes = b"resume later";
    let directory = tempdir().unwrap();
    let partial = directory.path().join("model.gguf.partial");
    fs::write(&partial, &bytes[..4]).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = downloader(u64::MAX)
        .download(
            &file("http://127.0.0.1:1/model", bytes),
            directory.path(),
            &cancellation,
            |_| {},
        )
        .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    assert_eq!(fs::read(partial).unwrap(), &bytes[..4]);
}

#[test]
fn stalled_response_headers_time_out_without_hanging() {
    let bytes = b"model bytes";
    let server = StallingServer::new(StallPoint::BeforeHeaders);
    let directory = tempdir().unwrap();
    let started = Instant::now();
    let error = short_timeout_downloader(u64::MAX)
        .download(
            &file(&server.url, bytes),
            directory.path(),
            &CancellationToken::new(),
            |_| {},
        )
        .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::DownloadInterrupted);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn cancellation_during_a_stalled_body_preserves_received_partial() {
    let bytes = b"prefix then a stalled response";
    let prefix = bytes[..7].to_vec();
    let server = StallingServer::new(StallPoint::DuringBody {
        total: bytes.len(),
        prefix: prefix.clone(),
    });
    let directory = tempdir().unwrap();
    let cancellation = CancellationToken::new();
    let cancel_from_thread = cancellation.clone();
    // Cancel once the prefix is actually on disk rather than after a fixed
    // sleep. A timer races the download loop, and under load the cancel can
    // arrive before anything has been written, leaving no partial file to
    // assert on.
    let partial = directory.path().join("model.gguf.partial");
    let expected_prefix = prefix.len();
    let cancel_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if fs::metadata(&partial).is_ok_and(|info| info.len() as usize >= expected_prefix) {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        cancel_from_thread.cancel();
    });

    let error = patient_downloader(u64::MAX)
        .download(
            &file(&server.url, bytes),
            directory.path(),
            &cancellation,
            |_| {},
        )
        .unwrap_err();
    cancel_thread.join().unwrap();

    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    assert_eq!(
        fs::read(directory.path().join("model.gguf.partial")).unwrap(),
        prefix
    );
}

#[test]
fn default_policy_cancels_header_acquisition_and_joins_network_work() {
    let bytes = b"model bytes";
    let server = HeaderCancelServer::new();
    let directory = tempdir().unwrap();
    let cancellation = CancellationToken::new();
    let cancel_from_thread = cancellation.clone();
    let cancel_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        cancel_from_thread.cancel();
    });
    let started = Instant::now();

    let error = downloader(u64::MAX)
        .download(
            &file(&server.url, bytes),
            directory.path(),
            &cancellation,
            |_| {},
        )
        .unwrap_err();
    cancel_thread.join().unwrap();

    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    assert!(started.elapsed() < Duration::from_secs(2));
    server.wait_for_disconnect();
}

#[test]
fn cancellation_while_hashing_a_resume_prefix_skips_the_network_request() {
    let bytes = vec![7_u8; 2 * 1024 * 1024 + 31];
    let directory = tempdir().unwrap();
    let partial = directory.path().join("model.gguf.partial");
    fs::write(&partial, &bytes[..2 * 1024 * 1024]).unwrap();
    let cancellation = CancellationToken::new();
    let cancel_from_progress = cancellation.clone();

    let error = downloader(u64::MAX)
        .download(
            &file("http://127.0.0.1:1/model.gguf", &bytes),
            directory.path(),
            &cancellation,
            move |event| {
                if event.stage == SetupStage::Checking && event.completed_bytes > 0 {
                    cancel_from_progress.cancel();
                }
            },
        )
        .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    assert_eq!(fs::metadata(partial).unwrap().len(), 2 * 1024 * 1024);
}

#[test]
fn selected_existing_file_uses_identical_size_and_digest_validation() {
    let bytes = b"selected model";
    let directory = tempdir().unwrap();
    let selected = directory.path().join("selected.gguf");
    fs::write(&selected, bytes).unwrap();
    let expected = file("https://example.invalid/model.gguf", bytes);
    validate_selected_file(&selected, &expected).unwrap();

    fs::write(&selected, b"wrong contents!").unwrap();
    assert_eq!(
        validate_selected_file(&selected, &expected)
            .unwrap_err()
            .code(),
        ModelErrorCode::ModelFileInvalid,
    );
}

#[test]
fn selected_file_install_requires_file_size_plus_disk_reserve() {
    let bytes = b"selected model";
    let directory = tempdir().unwrap();
    let selected = directory.path().join("selected.gguf");
    let install = directory.path().join("install");
    fs::write(&selected, bytes).unwrap();
    let error = install_selected_file(
        &selected,
        &file("https://example.invalid/model.gguf", bytes),
        &install,
        &FixedDisk(DISK_RESERVE_BYTES + bytes.len() as u64 - 1),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap_err();

    assert_eq!(error.code(), ModelErrorCode::InsufficientDisk);
    assert!(!install.join("model.gguf").exists());
}

#[test]
fn selected_file_cancel_sync_and_revalidation_use_partial_publication() {
    let bytes = vec![42_u8; 2 * 1024 * 1024 + 17];
    let directory = tempdir().unwrap();
    let selected = directory.path().join("selected.gguf");
    let install = directory.path().join("install");
    fs::write(&selected, &bytes).unwrap();
    let expected = file("https://example.invalid/model.gguf", &bytes);
    let cancellation = CancellationToken::new();
    let cancel_from_progress = cancellation.clone();

    let error = install_selected_file(
        &selected,
        &expected,
        &install,
        &FixedDisk(u64::MAX),
        &cancellation,
        move |event| {
            if event.stage == SetupStage::Downloading && event.completed_bytes > 0 {
                cancel_from_progress.cancel();
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), ModelErrorCode::DownloadCanceled);
    let partial = install.join("model.gguf.partial");
    assert!(partial.exists());
    assert!(fs::metadata(&partial).unwrap().len() < expected.size);

    let mut events = Vec::new();
    let final_path = install_selected_file(
        &selected,
        &expected,
        &install,
        &FixedDisk(u64::MAX),
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .unwrap();
    assert_eq!(fs::read(final_path).unwrap(), bytes);
    assert!(!partial.exists());
    assert_eq!(events.last().unwrap().stage, SetupStage::Complete);
}
