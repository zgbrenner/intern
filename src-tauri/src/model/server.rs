use std::{
    fmt,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use rand::{RngCore, rngs::OsRng};
use reqwest::blocking::Client;

use super::{ModelError, ModelErrorCode, ModelResult};

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_CONFIRMATION_DELAY: Duration = Duration::from_millis(500);
const MAX_SPAWN_ATTEMPTS: usize = 3;

pub trait ProcessControl: Send {
    fn has_exited(&mut self) -> ModelResult<bool>;
    fn terminate_and_wait(&mut self) -> ModelResult<()>;
}

pub trait ProcessLauncher: Send + Sync {
    fn launch(&self, executable: &Path, arguments: &[String]) -> ModelResult<Box<dyn ProcessControl>>;
}

pub trait PortAllocator: Send + Sync {
    fn next_port(&self) -> ModelResult<u16>;
}

pub trait HealthProbe: Send + Sync {
    fn is_healthy(&self, endpoint: &str, api_key: &str) -> bool;
}

pub struct LlamaServer {
    process: Mutex<Box<dyn ProcessControl>>,
    endpoint: String,
    api_key: String,
    health_probe: Arc<dyn HealthProbe>,
}

impl fmt::Debug for LlamaServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LlamaServer")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl LlamaServer {
    pub fn start(
        executable: &Path,
        model: &Path,
        projector: &Path,
        startup_timeout: Duration,
    ) -> ModelResult<Self> {
        Self::start_with(
            executable,
            model,
            projector,
            startup_timeout,
            HEALTH_CONFIRMATION_DELAY,
            &StdProcessLauncher,
            &EphemeralPortAllocator,
            Arc::new(ReqwestHealthProbe::new()?),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_with(
        executable: &Path,
        model: &Path,
        projector: &Path,
        startup_timeout: Duration,
        health_confirmation_delay: Duration,
        launcher: &dyn ProcessLauncher,
        ports: &dyn PortAllocator,
        health_probe: Arc<dyn HealthProbe>,
    ) -> ModelResult<Self> {
        require_file(executable)?;
        require_file(model)?;
        require_file(projector)?;
        if startup_timeout.is_zero() {
            return Err(start_failed());
        }

        let deadline = Instant::now() + startup_timeout;
        for attempt in 0..MAX_SPAWN_ATTEMPTS {
            if Instant::now() >= deadline {
                return Err(unhealthy());
            }
            let port = ports.next_port()?;
            let api_key = random_api_key();
            let arguments = Self::arguments_for(model, projector, port, &api_key);
            let mut process = match launcher.launch(executable, &arguments) {
                Ok(process) => process,
                Err(_) if attempt + 1 < MAX_SPAWN_ATTEMPTS => continue,
                Err(_) => return Err(start_failed()),
            };
            let endpoint = format!("http://127.0.0.1:{port}");

            let startup_state = match confirm_startup(
                process.as_mut(),
                health_probe.as_ref(),
                &endpoint,
                &api_key,
                deadline,
                health_confirmation_delay,
            ) {
                Ok(state) => state,
                Err(error) => {
                    process.terminate_and_wait()?;
                    return Err(error);
                }
            };

            match startup_state {
                StartupState::Ready => {
                    return Ok(Self {
                        process: Mutex::new(process),
                        endpoint,
                        api_key,
                        health_probe,
                    });
                }
                StartupState::Exited => {
                    process.terminate_and_wait()?;
                    if attempt + 1 == MAX_SPAWN_ATTEMPTS {
                        return Err(start_failed());
                    }
                }
                StartupState::TimedOut => {
                    process.terminate_and_wait()?;
                    return Err(unhealthy());
                }
            }
        }
        Err(start_failed())
    }

    pub fn arguments_for(model: &Path, projector: &Path, port: u16, api_key: &str) -> Vec<String> {
        vec![
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--api-key".into(),
            api_key.into(),
            "--model".into(),
            model.to_string_lossy().into_owned(),
            "--mmproj".into(),
            projector.to_string_lossy().into_owned(),
            "--parallel".into(),
            "1".into(),
            "--ctx-size".into(),
            "8192".into(),
            "--n-gpu-layers".into(),
            "0".into(),
        ]
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn completion_endpoint(&self) -> String {
        format!("{}/v1/chat/completions", self.endpoint)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn health(&self) -> bool {
        let Ok(mut process) = self.process.lock() else { return false };
        if process.has_exited().unwrap_or(true) {
            return false;
        }
        let healthy = self.health_probe.is_healthy(&self.endpoint, &self.api_key);
        healthy && !process.has_exited().unwrap_or(true)
    }

    pub fn stop(&mut self) -> ModelResult<()> {
        self.process.lock().map_err(|_| start_failed())?.terminate_and_wait()
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

enum StartupState {
    Ready,
    Exited,
    TimedOut,
}

fn confirm_startup(
    process: &mut dyn ProcessControl,
    health: &dyn HealthProbe,
    endpoint: &str,
    api_key: &str,
    deadline: Instant,
    confirmation_delay: Duration,
) -> ModelResult<StartupState> {
    loop {
        if process.has_exited()? {
            return Ok(StartupState::Exited);
        }
        let healthy = health.is_healthy(endpoint, api_key);
        if process.has_exited()? {
            return Ok(StartupState::Exited);
        }
        if healthy {
            if !confirmation_delay.is_zero() {
                thread::sleep(confirmation_delay.min(deadline.saturating_duration_since(Instant::now())));
            }
            if process.has_exited()? {
                return Ok(StartupState::Exited);
            }
            let confirmed = health.is_healthy(endpoint, api_key);
            if process.has_exited()? {
                return Ok(StartupState::Exited);
            }
            if confirmed {
                return Ok(StartupState::Ready);
            }
        }
        if Instant::now() >= deadline {
            return Ok(StartupState::TimedOut);
        }
        thread::sleep(HEALTH_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

struct StdChildProcess(Child);

impl ProcessControl for StdChildProcess {
    fn has_exited(&mut self) -> ModelResult<bool> {
        self.0.try_wait().map(|status| status.is_some()).map_err(|_| start_failed())
    }

    fn terminate_and_wait(&mut self) -> ModelResult<()> {
        if self.0.try_wait().map_err(|_| start_failed())?.is_some() {
            return Ok(());
        }
        let kill_result = self.0.kill();
        let wait_result = self.0.wait();
        if wait_result.is_ok() {
            return Ok(());
        }
        kill_result.map_err(|_| start_failed())?;
        Err(start_failed())
    }
}

struct StdProcessLauncher;

impl ProcessLauncher for StdProcessLauncher {
    fn launch(&self, executable: &Path, arguments: &[String]) -> ModelResult<Box<dyn ProcessControl>> {
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.spawn().map_err(|_| start_failed())?;
        Ok(Box::new(StdChildProcess(child)))
    }
}

struct EphemeralPortAllocator;

impl PortAllocator for EphemeralPortAllocator {
    fn next_port(&self) -> ModelResult<u16> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| start_failed())?;
        listener.local_addr().map(|address| address.port()).map_err(|_| start_failed())
    }
}

struct ReqwestHealthProbe {
    client: Client,
}

impl ReqwestHealthProbe {
    fn new() -> ModelResult<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(1))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| start_failed())?;
        Ok(Self { client })
    }
}

impl HealthProbe for ReqwestHealthProbe {
    fn is_healthy(&self, endpoint: &str, api_key: &str) -> bool {
        self.client
            .get(format!("{endpoint}/health"))
            .bearer_auth(api_key)
            .send()
            .is_ok_and(|response| response.status().is_success())
    }
}

fn random_api_key() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut key = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn require_file(path: &Path) -> ModelResult<()> {
    path.is_file().then_some(()).ok_or_else(start_failed)
}

const fn start_failed() -> ModelError {
    ModelError::new(ModelErrorCode::ModelServerStartFailed, "local model server could not be started")
}

const fn unhealthy() -> ModelError {
    ModelError::new(ModelErrorCode::ModelServerUnhealthy, "local model server did not become healthy")
}
