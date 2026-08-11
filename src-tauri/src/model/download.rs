use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ModelError, ModelErrorCode, ModelResult,
    manifest::ModelFile,
};

pub const DISK_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
const BUFFER_BYTES: usize = 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_OVERALL_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const CANCELLATION_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStage {
    Checking,
    Downloading,
    Verifying,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetupProgress {
    pub stage: SetupStage,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait DiskSpace: Clone + Send + Sync + 'static {
    fn available_bytes(&self, path: &Path) -> ModelResult<u64>;
}

#[derive(Clone, Copy, Default)]
pub struct SystemDiskSpace;

impl DiskSpace for SystemDiskSpace {
    fn available_bytes(&self, path: &Path) -> ModelResult<u64> {
        fs4::available_space(path).map_err(|_| {
            ModelError::new(ModelErrorCode::DownloadFailed, "available disk space could not be determined")
        })
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub content_range: Option<String>,
    pub body: Box<dyn Read + Send>,
}

pub trait HttpTransport: Clone + Send + Sync + 'static {
    fn get(
        &self,
        url: &str,
        range_start: Option<u64>,
        cancellation: &CancellationToken,
    ) -> ModelResult<HttpResponse>;
}

#[derive(Clone)]
pub struct ReqwestHttpTransport {
    client: reqwest::Client,
}

impl ReqwestHttpTransport {
    pub fn new() -> ModelResult<Self> {
        Self::with_timeouts(DEFAULT_CONNECT_TIMEOUT, DEFAULT_READ_TIMEOUT, DEFAULT_OVERALL_TIMEOUT)
    }

    pub fn with_timeouts(
        connect_timeout: Duration,
        read_timeout: Duration,
        overall_timeout: Duration,
    ) -> ModelResult<Self> {
        if connect_timeout.is_zero() || read_timeout.is_zero() || overall_timeout.is_zero() {
            return Err(ModelError::new(
                ModelErrorCode::DownloadFailed,
                "download timeouts must be bounded and nonzero",
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .read_timeout(read_timeout)
            .timeout(overall_timeout)
            .build()
            .map_err(|_| {
                ModelError::new(ModelErrorCode::DownloadFailed, "download client could not be created")
            })?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestHttpTransport {
    fn get(
        &self,
        url: &str,
        range_start: Option<u64>,
        cancellation: &CancellationToken,
    ) -> ModelResult<HttpResponse> {
        let (sender, messages) = sync_channel(2);
        let abort = Arc::new(AtomicBool::new(false));
        let worker = spawn_network_worker(
            self.client.clone(),
            url.to_owned(),
            range_start,
            cancellation.clone(),
            Arc::clone(&abort),
            sender,
        )?;

        loop {
            if cancellation.is_canceled() {
                abort.store(true, Ordering::Release);
                drop(messages);
                join_network_worker(worker)?;
                return Err(canceled());
            }
            match messages.recv_timeout(CANCELLATION_POLL) {
                Ok(NetworkMessage::Headers { status, content_range }) => {
                    return Ok(HttpResponse {
                        status,
                        content_range,
                        body: Box::new(CancelableResponseBody {
                            messages: Some(messages),
                            worker: Some(worker),
                            cancellation: cancellation.clone(),
                            abort,
                            current: Vec::new(),
                            current_offset: 0,
                        }),
                    });
                }
                Ok(NetworkMessage::Canceled) => {
                    join_network_worker(worker)?;
                    return Err(canceled());
                }
                Ok(NetworkMessage::Failed) | Err(RecvTimeoutError::Disconnected) => {
                    join_network_worker(worker)?;
                    return Err(interrupted());
                }
                Ok(NetworkMessage::Chunk(_) | NetworkMessage::End) => {
                    abort.store(true, Ordering::Release);
                    drop(messages);
                    join_network_worker(worker)?;
                    return Err(interrupted());
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

enum NetworkMessage {
    Headers { status: u16, content_range: Option<String> },
    Chunk(Vec<u8>),
    End,
    Failed,
    Canceled,
}

struct CancelableResponseBody {
    messages: Option<Receiver<NetworkMessage>>,
    worker: Option<thread::JoinHandle<()>>,
    cancellation: CancellationToken,
    abort: Arc<AtomicBool>,
    current: Vec<u8>,
    current_offset: usize,
}

impl CancelableResponseBody {
    fn stop_and_join(&mut self) -> io::Result<()> {
        self.abort.store(true, Ordering::Release);
        self.messages.take();
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| io::Error::other("download worker panicked"))?;
        }
        Ok(())
    }
}

impl Read for CancelableResponseBody {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        if destination.is_empty() {
            return Ok(0);
        }
        if self.current_offset < self.current.len() {
            let copied = destination.len().min(self.current.len() - self.current_offset);
            destination[..copied]
                .copy_from_slice(&self.current[self.current_offset..self.current_offset + copied]);
            self.current_offset += copied;
            return Ok(copied);
        }

        loop {
            if self.cancellation.is_canceled() {
                self.stop_and_join()?;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "download canceled"));
            }
            let Some(messages) = self.messages.as_ref() else {
                return Ok(0);
            };
            match messages.recv_timeout(CANCELLATION_POLL) {
                Ok(NetworkMessage::Chunk(chunk)) => {
                    self.current = chunk;
                    self.current_offset = 0;
                    if self.current.is_empty() {
                        continue;
                    }
                    let copied = destination.len().min(self.current.len());
                    destination[..copied].copy_from_slice(&self.current[..copied]);
                    self.current_offset = copied;
                    return Ok(copied);
                }
                Ok(NetworkMessage::End) => {
                    self.stop_and_join()?;
                    return Ok(0);
                }
                Ok(NetworkMessage::Canceled) => {
                    self.stop_and_join()?;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "download canceled"));
                }
                Ok(NetworkMessage::Failed) | Err(RecvTimeoutError::Disconnected) => {
                    self.stop_and_join()?;
                    return Err(io::Error::other("response body failed"));
                }
                Ok(NetworkMessage::Headers { .. }) => {
                    self.stop_and_join()?;
                    return Err(io::Error::other("duplicate response headers"));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Drop for CancelableResponseBody {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn spawn_network_worker(
    client: reqwest::Client,
    url: String,
    range_start: Option<u64>,
    cancellation: CancellationToken,
    abort: Arc<AtomicBool>,
    sender: SyncSender<NetworkMessage>,
) -> ModelResult<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("intern-model-download".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                let _ = sender.send(NetworkMessage::Failed);
                return;
            };
            runtime.block_on(async move {
                let mut request = client.get(url);
                if let Some(start) = range_start {
                    request = request.header(reqwest::header::RANGE, format!("bytes={start}-"));
                }
                let response = tokio::select! {
                    _ = wait_for_network_cancel(&cancellation, &abort) => {
                        let _ = sender.send(NetworkMessage::Canceled);
                        return;
                    }
                    response = request.send() => match response {
                        Ok(response) => response,
                        Err(_) => {
                            let _ = sender.send(NetworkMessage::Failed);
                            return;
                        }
                    }
                };
                let status = response.status().as_u16();
                let content_range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|header| header.to_str().ok())
                    .map(ToOwned::to_owned);
                if sender.send(NetworkMessage::Headers { status, content_range }).is_err() {
                    return;
                }

                let mut response = response;
                loop {
                    let chunk = tokio::select! {
                        _ = wait_for_network_cancel(&cancellation, &abort) => {
                            let _ = sender.send(NetworkMessage::Canceled);
                            return;
                        }
                        chunk = response.chunk() => chunk,
                    };
                    match chunk {
                        Ok(Some(chunk)) => {
                            if sender.send(NetworkMessage::Chunk(chunk.to_vec())).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = sender.send(NetworkMessage::End);
                            return;
                        }
                        Err(_) => {
                            let _ = sender.send(NetworkMessage::Failed);
                            return;
                        }
                    }
                }
            });
        })
        .map_err(|_| download_failed())
}

async fn wait_for_network_cancel(cancellation: &CancellationToken, abort: &AtomicBool) {
    while !cancellation.is_canceled() && !abort.load(Ordering::Acquire) {
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

fn join_network_worker(worker: thread::JoinHandle<()>) -> ModelResult<()> {
    worker.join().map_err(|_| interrupted())
}

pub struct Downloader<H = ReqwestHttpTransport, D = SystemDiskSpace> {
    http: H,
    disk: D,
}

impl<H: HttpTransport, D: DiskSpace> Downloader<H, D> {
    pub fn new(http: H, disk: D) -> Self {
        Self { http, disk }
    }

    pub fn download<F>(
        &self,
        expected: &ModelFile,
        destination_directory: &Path,
        cancellation: &CancellationToken,
        mut progress: F,
    ) -> ModelResult<PathBuf>
    where
        F: FnMut(SetupProgress),
    {
        if !safe_install_name(&expected.name) {
            return Err(invalid_file());
        }
        fs::create_dir_all(destination_directory).map_err(|_| download_failed())?;
        let final_path = destination_directory.join(&expected.name);
        let partial_path = destination_directory.join(format!("{}.partial", expected.name));

        progress(SetupProgress { stage: SetupStage::Checking, completed_bytes: 0, total_bytes: expected.size });
        if final_path.exists() {
            validate_file_cancelable(&final_path, expected, cancellation, |checked| {
                progress(SetupProgress {
                    stage: SetupStage::Checking,
                    completed_bytes: checked,
                    total_bytes: expected.size,
                });
            })?;
            if cancellation.is_canceled() {
                return Err(canceled());
            }
            progress(SetupProgress {
                stage: SetupStage::Complete,
                completed_bytes: expected.size,
                total_bytes: expected.size,
            });
            return Ok(final_path);
        }
        if cancellation.is_canceled() {
            return Err(canceled());
        }

        let existing = partial_length(&partial_path)?;
        if existing == expected.size {
            match validate_file_cancelable(&partial_path, expected, cancellation, |checked| {
                progress(SetupProgress {
                    stage: SetupStage::Checking,
                    completed_bytes: checked,
                    total_bytes: expected.size,
                });
            }) {
                Ok(()) => {
                    if cancellation.is_canceled() {
                        return Err(canceled());
                    }
                    fs::rename(&partial_path, &final_path).map_err(|_| download_failed())?;
                    progress(SetupProgress {
                        stage: SetupStage::Complete,
                        completed_bytes: expected.size,
                        total_bytes: expected.size,
                    });
                    return Ok(final_path);
                }
                Err(error) if error.code() == ModelErrorCode::DownloadCanceled => return Err(error),
                Err(_) => {}
            }
        }
        let requested_start = (existing > 0 && existing < expected.size).then_some(existing);
        let initial_remaining = expected.size.saturating_sub(requested_start.unwrap_or(0));
        require_disk(&self.disk, destination_directory, initial_remaining)?;

        if let Some(resume_length) = requested_start {
            hash_prefix_cancelable(&partial_path, resume_length, cancellation, |checked| {
                progress(SetupProgress {
                    stage: SetupStage::Checking,
                    completed_bytes: checked,
                    total_bytes: expected.size,
                });
            })?;
        }

        let mut response = match self.http.get(&expected.url, requested_start, cancellation) {
            Ok(response) => response,
            Err(_) if cancellation.is_canceled() => return Err(canceled()),
            Err(error) => return Err(error),
        };
        let append_from = match (requested_start, response.status) {
            (Some(start), 206) if confirmed_content_range(response.content_range.as_deref(), start, expected.size) => start,
            (Some(_), 200) => 0,
            (None, 200) => 0,
            (None, 206) if confirmed_content_range(response.content_range.as_deref(), 0, expected.size) => 0,
            _ => return Err(download_failed()),
        };
        require_disk(
            &self.disk,
            destination_directory,
            expected.size.saturating_sub(append_from),
        )?;

        let mut output = open_partial(&partial_path, append_from > 0)?;
        let mut completed = append_from;
        progress(SetupProgress {
            stage: SetupStage::Downloading,
            completed_bytes: completed,
            total_bytes: expected.size,
        });

        let mut buffer = vec![0_u8; BUFFER_BYTES];
        loop {
            if cancellation.is_canceled() {
                sync_partial(&mut output)?;
                return Err(canceled());
            }
            let read = match response.body.read(&mut buffer) {
                Ok(read) => read,
                Err(_) if cancellation.is_canceled() => {
                    sync_partial(&mut output)?;
                    return Err(canceled());
                }
                Err(_) => {
                    sync_partial(&mut output)?;
                    return Err(interrupted());
                }
            };
            if read == 0 {
                break;
            }
            if completed.saturating_add(read as u64) > expected.size {
                return Err(invalid_file());
            }
            output.write_all(&buffer[..read]).map_err(|_| download_failed())?;
            completed += read as u64;
            progress(SetupProgress {
                stage: SetupStage::Downloading,
                completed_bytes: completed,
                total_bytes: expected.size,
            });
        }
        output.flush().map_err(|_| download_failed())?;
        output.sync_all().map_err(|_| download_failed())?;
        drop(output);

        progress(SetupProgress {
            stage: SetupStage::Verifying,
            completed_bytes: 0,
            total_bytes: expected.size,
        });
        if completed != expected.size {
            return Err(invalid_file());
        }
        validate_file_cancelable(&partial_path, expected, cancellation, |verified| {
            progress(SetupProgress {
                stage: SetupStage::Verifying,
                completed_bytes: verified,
                total_bytes: expected.size,
            });
        })?;
        if cancellation.is_canceled() {
            return Err(canceled());
        }
        if final_path.exists() {
            return Err(download_failed());
        }
        fs::rename(&partial_path, &final_path).map_err(|_| download_failed())?;
        progress(SetupProgress {
            stage: SetupStage::Complete,
            completed_bytes: expected.size,
            total_bytes: expected.size,
        });
        Ok(final_path)
    }
}

pub fn default_model_directory() -> ModelResult<PathBuf> {
    let local_app_data = env::var_os("LOCALAPPDATA").ok_or_else(|| {
        ModelError::new(ModelErrorCode::DownloadFailed, "LocalAppData is unavailable")
    })?;
    Ok(PathBuf::from(local_app_data).join("Intern").join("models"))
}

pub fn validate_selected_file(path: &Path, expected: &ModelFile) -> ModelResult<()> {
    if !safe_install_name(&expected.name) {
        return Err(invalid_file());
    }
    let metadata = fs::metadata(path).map_err(|_| invalid_file())?;
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(invalid_file());
    }
    let mut input = File::open(path).map_err(|_| invalid_file())?;
    let mut hasher = Sha256::new();
    hash_reader(&mut input, &mut hasher).map_err(|_| invalid_file())?;
    let digest = hasher.finalize();
    if hex_digest(&digest) != expected.sha256 {
        return Err(invalid_file());
    }
    Ok(())
}

pub fn install_selected_file<D, F>(
    selected: &Path,
    expected: &ModelFile,
    destination_directory: &Path,
    disk: &D,
    cancellation: &CancellationToken,
    mut progress: F,
) -> ModelResult<PathBuf>
where
    D: DiskSpace,
    F: FnMut(SetupProgress),
{
    if !safe_install_name(&expected.name) {
        return Err(invalid_file());
    }
    fs::create_dir_all(destination_directory).map_err(|_| download_failed())?;
    let final_path = destination_directory.join(&expected.name);
    progress(SetupProgress { stage: SetupStage::Checking, completed_bytes: 0, total_bytes: expected.size });
    if cancellation.is_canceled() {
        return Err(canceled());
    }
    if selected == final_path {
        validate_file_cancelable(selected, expected, cancellation, |checked| {
            progress(SetupProgress {
                stage: SetupStage::Checking,
                completed_bytes: checked,
                total_bytes: expected.size,
            });
        })?;
        if cancellation.is_canceled() {
            return Err(canceled());
        }
        progress(SetupProgress {
            stage: SetupStage::Complete,
            completed_bytes: expected.size,
            total_bytes: expected.size,
        });
        return Ok(final_path);
    }
    let partial_path = destination_directory.join(format!("{}.partial", expected.name));
    if selected == partial_path {
        return Err(invalid_file());
    }
    if final_path.exists() {
        validate_file_cancelable(&final_path, expected, cancellation, |checked| {
            progress(SetupProgress {
                stage: SetupStage::Checking,
                completed_bytes: checked,
                total_bytes: expected.size,
            });
        })?;
        if cancellation.is_canceled() {
            return Err(canceled());
        }
        progress(SetupProgress {
            stage: SetupStage::Complete,
            completed_bytes: expected.size,
            total_bytes: expected.size,
        });
        return Ok(final_path);
    }
    require_disk(disk, destination_directory, expected.size)?;
    validate_file_cancelable(selected, expected, cancellation, |checked| {
        progress(SetupProgress {
            stage: SetupStage::Checking,
            completed_bytes: checked,
            total_bytes: expected.size,
        });
    })?;
    if cancellation.is_canceled() {
        return Err(canceled());
    }

    let mut input = File::open(selected).map_err(|_| invalid_file())?;
    let mut output = open_partial(&partial_path, false)?;
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    progress(SetupProgress {
        stage: SetupStage::Downloading,
        completed_bytes: 0,
        total_bytes: expected.size,
    });
    loop {
        if cancellation.is_canceled() {
            sync_partial(&mut output)?;
            return Err(canceled());
        }
        let read = input.read(&mut buffer).map_err(|_| invalid_file())?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|_| download_failed())?;
        copied += read as u64;
        progress(SetupProgress {
            stage: SetupStage::Downloading,
            completed_bytes: copied,
            total_bytes: expected.size,
        });
    }
    sync_partial(&mut output)?;
    drop(output);
    if copied != expected.size {
        return Err(invalid_file());
    }
    progress(SetupProgress {
        stage: SetupStage::Verifying,
        completed_bytes: 0,
        total_bytes: expected.size,
    });
    validate_file_cancelable(&partial_path, expected, cancellation, |verified| {
        progress(SetupProgress {
            stage: SetupStage::Verifying,
            completed_bytes: verified,
            total_bytes: expected.size,
        });
    })?;
    if final_path.exists() {
        return Err(download_failed());
    }
    if cancellation.is_canceled() {
        return Err(canceled());
    }
    fs::rename(&partial_path, &final_path).map_err(|_| download_failed())?;
    progress(SetupProgress {
        stage: SetupStage::Complete,
        completed_bytes: expected.size,
        total_bytes: expected.size,
    });
    Ok(final_path)
}

fn require_disk<D: DiskSpace>(disk: &D, path: &Path, remaining: u64) -> ModelResult<()> {
    let required = remaining.checked_add(DISK_RESERVE_BYTES).ok_or_else(download_failed)?;
    if disk.available_bytes(path)? < required {
        return Err(ModelError::new(
            ModelErrorCode::InsufficientDisk,
            "not enough disk space for the verified model download",
        ));
    }
    Ok(())
}

fn partial_length(path: &Path) -> ModelResult<u64> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => Err(download_failed()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(_) => Err(download_failed()),
    }
}

fn safe_install_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && Path::new(name).file_name() == Some(std::ffi::OsStr::new(name))
        && Path::new(name).components().count() == 1
}

fn confirmed_content_range(header: Option<&str>, start: u64, total: u64) -> bool {
    let Some(value) = header else { return false };
    let Some(value) = value.strip_prefix("bytes ") else { return false };
    let Some((range, received_total)) = value.split_once('/') else { return false };
    let Some((received_start, received_end)) = range.split_once('-') else { return false };
    received_start.parse::<u64>().ok() == Some(start)
        && received_total.parse::<u64>().ok() == Some(total)
        && received_end.parse::<u64>().ok() == total.checked_sub(1)
}

fn open_partial(path: &Path, append: bool) -> ModelResult<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .map_err(|_| download_failed())
}

fn hash_prefix_cancelable<F>(
    path: &Path,
    expected_length: u64,
    cancellation: &CancellationToken,
    progress: F,
) -> ModelResult<()>
where
    F: FnMut(u64),
{
    let mut file = File::open(path).map_err(|_| download_failed())?;
    let (copied, _) = hash_reader_cancelable(
        &mut file.by_ref().take(expected_length),
        cancellation,
        progress,
    )?;
    if copied != expected_length {
        return Err(invalid_file());
    }
    Ok(())
}

fn hash_reader(reader: &mut impl Read, hasher: &mut Sha256) -> io::Result<u64> {
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(total);
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
}

fn hash_reader_cancelable<F>(
    reader: &mut impl Read,
    cancellation: &CancellationToken,
    mut progress: F,
) -> ModelResult<(u64, Vec<u8>)>
where
    F: FnMut(u64),
{
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    loop {
        if cancellation.is_canceled() {
            return Err(canceled());
        }
        let read = reader.read(&mut buffer).map_err(|_| invalid_file())?;
        if read == 0 {
            let digest = hasher.finalize();
            return Ok((total, digest.to_vec()));
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
        progress(total);
    }
}

fn validate_file_cancelable<F>(
    path: &Path,
    expected: &ModelFile,
    cancellation: &CancellationToken,
    progress: F,
) -> ModelResult<()>
where
    F: FnMut(u64),
{
    if cancellation.is_canceled() {
        return Err(canceled());
    }
    let metadata = fs::metadata(path).map_err(|_| invalid_file())?;
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(invalid_file());
    }
    let mut file = File::open(path).map_err(|_| invalid_file())?;
    let (size, digest) = hash_reader_cancelable(&mut file, cancellation, progress)?;
    if size != expected.size || hex_digest(&digest) != expected.sha256 {
        return Err(invalid_file());
    }
    Ok(())
}

fn sync_partial(output: &mut File) -> ModelResult<()> {
    output.flush().map_err(|_| download_failed())?;
    output.sync_all().map_err(|_| download_failed())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

const fn download_failed() -> ModelError {
    ModelError::new(ModelErrorCode::DownloadFailed, "model download failed")
}

const fn invalid_file() -> ModelError {
    ModelError::new(ModelErrorCode::ModelFileInvalid, "model file failed size or digest validation")
}

const fn canceled() -> ModelError {
    ModelError::new(ModelErrorCode::DownloadCanceled, "model download was canceled")
}

const fn interrupted() -> ModelError {
    ModelError::new(ModelErrorCode::DownloadInterrupted, "model download was interrupted")
}
