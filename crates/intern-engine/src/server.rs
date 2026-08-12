//! Supervision of the local llama.cpp server process.
//!
//! The server binds a random loopback port with a random bearer key, is kept
//! warm between documents, and is stopped when Intern exits.

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

use crate::error::{EngineError, EngineErrorCode, EngineResult};

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_CONFIRMATION_DELAY: Duration = Duration::from_millis(500);
const MAX_SPAWN_ATTEMPTS: usize = 3;

/// Context window. Large enough for the distillation budget plus the prompt,
/// small enough that the KV cache stays a few hundred megabytes on CPU.
pub const CONTEXT_TOKENS: u32 = 8_192;

/// Threads to give the model.
///
/// On a hyper-threaded laptop, llama.cpp scales with physical cores, not
/// logical ones, and taking every core makes the rest of Windows stutter.
/// Half the logical count is the physical count on the machines Intern targets,
/// and it leaves the other half for whatever else the user is doing.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get() / 2)
        .unwrap_or(4)
        .clamp(2, 12)
}

/// How the model process should be launched.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerOptions {
    pub threads: usize,
    pub context_tokens: u32,
    pub startup_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            threads: default_threads(),
            context_tokens: CONTEXT_TOKENS,
            startup_timeout: Duration::from_secs(180),
        }
    }
}

pub trait ProcessControl: Send {
    fn has_exited(&mut self) -> EngineResult<bool>;
    fn terminate_and_wait(&mut self) -> EngineResult<()>;
}

pub trait ProcessLauncher: Send + Sync {
    fn launch(
        &self,
        executable: &Path,
        arguments: &[String],
    ) -> EngineResult<Box<dyn ProcessControl>>;
}

pub trait PortAllocator: Send + Sync {
    fn next_port(&self) -> EngineResult<u16>;
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
        projector: Option<&Path>,
        options: &ServerOptions,
    ) -> EngineResult<Self> {
        Self::start_with(
            executable,
            model,
            projector,
            options,
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
        projector: Option<&Path>,
        options: &ServerOptions,
        health_confirmation_delay: Duration,
        launcher: &dyn ProcessLauncher,
        ports: &dyn PortAllocator,
        health_probe: Arc<dyn HealthProbe>,
    ) -> EngineResult<Self> {
        require_file(executable)?;
        require_file(model)?;
        if let Some(projector) = projector {
            require_file(projector)?;
        }
        if options.startup_timeout.is_zero() {
            return Err(start_failed());
        }

        let deadline = Instant::now() + options.startup_timeout;
        for attempt in 0..MAX_SPAWN_ATTEMPTS {
            if Instant::now() >= deadline {
                return Err(unhealthy());
            }
            let port = ports.next_port()?;
            let api_key = random_api_key();
            let arguments = Self::arguments_for(model, projector, port, &api_key, options);
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

    pub fn arguments_for(
        model: &Path,
        projector: Option<&Path>,
        port: u16,
        api_key: &str,
        options: &ServerOptions,
    ) -> Vec<String> {
        let mut arguments = vec![
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--api-key".into(),
            api_key.into(),
            "--model".into(),
            model.to_string_lossy().into_owned(),
            "--parallel".into(),
            "1".into(),
            "--ctx-size".into(),
            options.context_tokens.to_string(),
            "--n-gpu-layers".into(),
            "0".into(),
            "--threads".into(),
            options.threads.to_string(),
            "--threads-batch".into(),
            options.threads.to_string(),
            // The model's own chat template is required for the
            // enable_thinking switch to reach hybrid-reasoning models.
            "--jinja".into(),
            "--no-webui".into(),
        ];
        match projector {
            Some(projector) => {
                arguments.push("--mmproj".into());
                arguments.push(projector.to_string_lossy().into_owned());
            }
            // Loading a vision projector Intern will not use costs hundreds of
            // megabytes of resident memory on every document.
            None => arguments.push("--no-mmproj".into()),
        }
        arguments
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
        let Ok(mut process) = self.process.lock() else {
            return false;
        };
        if process.has_exited().unwrap_or(true) {
            return false;
        }
        let healthy = self.health_probe.is_healthy(&self.endpoint, &self.api_key);
        healthy && !process.has_exited().unwrap_or(true)
    }

    pub fn stop(&self) -> EngineResult<()> {
        self.process
            .lock()
            .map_err(|_| start_failed())?
            .terminate_and_wait()
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
) -> EngineResult<StartupState> {
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
                thread::sleep(
                    confirmation_delay.min(deadline.saturating_duration_since(Instant::now())),
                );
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
    fn has_exited(&mut self) -> EngineResult<bool> {
        self.0
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|_| start_failed())
    }

    fn terminate_and_wait(&mut self) -> EngineResult<()> {
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
    fn launch(
        &self,
        executable: &Path,
        arguments: &[String],
    ) -> EngineResult<Box<dyn ProcessControl>> {
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
    fn next_port(&self) -> EngineResult<u16> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| start_failed())?;
        listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|_| start_failed())
    }
}

struct ReqwestHealthProbe {
    client: Client,
}

impl ReqwestHealthProbe {
    fn new() -> EngineResult<Self> {
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

fn require_file(path: &Path) -> EngineResult<()> {
    path.is_file().then_some(()).ok_or_else(start_failed)
}

const fn start_failed() -> EngineError {
    EngineError::new(
        EngineErrorCode::ModelServerStartFailed,
        "local model server could not be started",
    )
}

const fn unhealthy() -> EngineError {
    EngineError::new(
        EngineErrorCode::ModelServerUnhealthy,
        "local model server did not become healthy",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ServerOptions {
        ServerOptions {
            threads: 6,
            context_tokens: 8_192,
            startup_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn a_text_only_launch_refuses_to_load_a_projector() {
        let arguments =
            LlamaServer::arguments_for(Path::new("m.gguf"), None, 51_000, "key", &options());
        assert!(arguments.contains(&"--no-mmproj".to_owned()));
        assert!(!arguments.contains(&"--mmproj".to_owned()));
        assert!(arguments.contains(&"--threads".to_owned()));
        assert!(arguments.contains(&"6".to_owned()));
        assert!(arguments.contains(&"127.0.0.1".to_owned()));
    }

    #[test]
    fn a_vision_launch_loads_the_projector() {
        let arguments = LlamaServer::arguments_for(
            Path::new("m.gguf"),
            Some(Path::new("p.gguf")),
            51_000,
            "key",
            &options(),
        );
        assert!(arguments.contains(&"--mmproj".to_owned()));
        assert!(!arguments.contains(&"--no-mmproj".to_owned()));
    }

    #[test]
    fn the_thread_default_leaves_the_machine_usable() {
        let threads = default_threads();
        assert!((2..=12).contains(&threads));
    }
}
