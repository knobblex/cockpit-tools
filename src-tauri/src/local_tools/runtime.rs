use super::{
    config::{self, Config},
    relay,
};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct Runtime {
    child: Child,
    config: Config,
    key: Arc<str>,
    cancel: CancellationToken,
    server: tokio::task::JoinHandle<()>,
    _lock: File,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.server.abort();
        // Only this owned child is stopped; no PID file or port-based process killing.
        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = self.child.kill();
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct State {
    runtime: Option<Runtime>,
    desired: bool,
    detail: String,
    last_exit: Option<i32>,
    retry_at: Instant,
}

pub struct Service {
    root: Result<PathBuf, String>,
    operation: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    exiting: AtomicBool,
}

impl Service {
    pub fn new() -> Self {
        Self::with_root_result(config::root())
    }

    pub(super) fn with_root_result(root: Result<PathBuf, String>) -> Self {
        Self {
            root,
            operation: tokio::sync::Mutex::new(()),
            exiting: AtomicBool::new(false),
            state: Mutex::new(State {
                runtime: None,
                desired: false,
                detail: String::new(),
                last_exit: None,
                retry_at: Instant::now(),
            }),
        }
    }

    fn root(&self) -> Result<&Path, String> {
        self.root
            .as_ref()
            .map(PathBuf::as_path)
            .map_err(Clone::clone)
    }

    fn log(&self, message: &str) {
        let Ok(root) = self.root() else { return };
        if config::private_dir(root).is_err() {
            return;
        }
        let path = root.join("service.log");
        if config::reject_symlink(&path).is_err() {
            return;
        }
        if path
            .metadata()
            .map(|m| m.len() > 1_048_576)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&path, root.join("service.previous.log"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        if let Ok(mut file) = options.open(path) {
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(file, "{time} {message}");
        }
    }

    fn fail(&self, error: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.detail = error.to_owned();
        state.retry_at = Instant::now() + Duration::from_secs(30);
        drop(state);
        self.log(error);
    }

    fn stop_owned(&self, clear_desired: bool) {
        let runtime = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if clear_desired {
                state.desired = false;
                state.detail.clear();
            }
            state.runtime.take()
        };
        drop(runtime);
    }

    pub fn shutdown(&self) {
        self.exiting.store(true, Ordering::SeqCst);
        self.stop_owned(true);
        self.log("Cockpit exit: Rust relay and owned CLIProxyAPI stopped");
    }

    pub async fn perform(
        self: &Arc<Self>,
        action: &str,
        value: Option<Value>,
    ) -> Result<Value, String> {
        if action == "status" {
            return self.status().await;
        }
        let _operation = self.operation.lock().await;
        match action {
            "save" => {
                if self
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .runtime
                    .is_some()
                {
                    return Err("Stop the service before changing its configuration".into());
                }
                let value = value.ok_or("Missing configuration")?;
                if value.to_string().len() > 16_384 {
                    return Err("Configuration is too large".into());
                }
                Config::from_value(value)?.save(self.root()?)?;
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state.desired = false;
                state.detail.clear();
            }
            "start" => {
                self.state.lock().unwrap_or_else(|e| e.into_inner()).desired = true;
                if let Err(error) = self.start().await {
                    self.fail(&error);
                    return Err(error);
                }
            }
            "stop" => {
                let service = self.clone();
                tokio::task::spawn_blocking(move || service.stop_owned(true))
                    .await
                    .map_err(|e| e.to_string())?;
                self.log("Stopped Rust relay and owned CLIProxyAPI");
            }
            _ => return Err("Unknown local tool action".into()),
        }
        self.status().await
    }

    async fn start(self: &Arc<Self>) -> Result<(), String> {
        if self.exiting.load(Ordering::SeqCst) {
            return Err("Cockpit is exiting".into());
        }
        if self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtime
            .is_some()
        {
            return Ok(());
        }
        let config = Config::read(self.root()?)?;
        config.validate()?;
        let public = TcpListener::bind(("127.0.0.1", config.port)).await
            .map_err(|_| "Public port is occupied; stop the old service manually (including the retired launchd service)")?;
        let lock = config::state_lock(&config.state_dir)?;
        let key: Arc<str> = config::read_key(&config.state_dir)?.into();
        if !std::fs::read_dir(config.state_dir.join("auth"))
            .map_err(|_| "Missing CLIProxyAPI account directory")?
            .filter_map(Result::ok)
            .any(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        {
            return Err("No authorized CLIProxyAPI account; complete login first".into());
        }
        let reservation = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| e.to_string())?;
        let backend_port = reservation.local_addr().map_err(|e| e.to_string())?.port();
        let document = config.backend_document(backend_port, &key)?;
        let generated = config.state_dir.join("backend.generated.yaml");
        config::private_write(&generated, &document)?;
        config.save(self.root()?)?; // One-time normalization removes retired Python fields.
        let client = relay::client()?;
        let cancel = CancellationToken::new();
        let app = relay::router(relay::Relay {
            client: client.clone(),
            backend_port,
            key: key.clone(),
            cancel: cancel.clone(),
        });
        drop(reservation);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.exiting.load(Ordering::SeqCst) {
                return Err("Cockpit is exiting".into());
            }
            let mut command = Command::new(&config.backend_path);
            command
                .arg("-config")
                .arg(&generated)
                .current_dir(&config.state_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let child = command
                .spawn()
                .map_err(|e| format!("Cannot start CLIProxyAPI: {e}"))?;
            let shutdown = cancel.clone();
            let server = tokio::spawn(async move {
                let _ = axum::serve(public, app)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await;
            });
            state.runtime = Some(Runtime {
                child,
                config: config.clone(),
                key: key.clone(),
                cancel,
                server,
                _lock: lock,
            });
            state.detail.clear();
            state.last_exit = None;
        }
        let ready = async {
            let deadline = Instant::now() + Duration::from_secs(45);
            loop {
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let runtime = state
                        .runtime
                        .as_mut()
                        .ok_or("Service stopped during startup")?;
                    if let Some(exit) = runtime.child.try_wait().map_err(|e| e.to_string())? {
                        state.last_exit = exit.code();
                        return Err(format!("CLIProxyAPI exited during startup ({exit})"));
                    }
                }
                if let Ok(models) = relay::models(&client, backend_port, &key).await {
                    if models.contains(&config.model) {
                        return Ok(());
                    }
                    if !models.is_empty() {
                        return Err("Configured model is not offered by this account".into());
                    }
                }
                if Instant::now() >= deadline {
                    return Err(
                        "CLIProxyAPI startup timed out; check account login and backend version"
                            .into(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        .await;
        if ready.is_err() {
            let service = self.clone();
            let _ = tokio::task::spawn_blocking(move || service.stop_owned(false)).await;
        } else {
            self.log("Ready: in-process Rust relay with CLIProxyAPI child");
        }
        ready
    }

    pub async fn status(&self) -> Result<Value, String> {
        let (config, pid, key, detail, last_exit, desired) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let config = state
                .runtime
                .as_ref()
                .map(|r| r.config.clone())
                .map(Ok)
                .unwrap_or_else(|| Config::read(self.root()?))?;
            (
                config,
                state.runtime.as_ref().map(|r| r.child.id()),
                state.runtime.as_ref().map(|r| r.key.clone()),
                state.detail.clone(),
                state.last_exit,
                state.desired,
            )
        };
        let mut models = Vec::new();
        let mut detail = detail;
        let mut healthy = false;
        if let Some(key) = key {
            match relay::models(&relay::client()?, config.port, &key).await {
                Ok(found) => {
                    healthy = found.contains(&config.model);
                    models = found;
                }
                Err(error) => {
                    detail = error;
                }
            }
        }
        let external = if pid.is_none() {
            tokio::time::timeout(
                Duration::from_millis(300),
                tokio::net::TcpStream::connect(("127.0.0.1", config.port)),
            )
            .await
            .is_ok_and(|r| r.is_ok())
        } else {
            false
        };
        let state = if pid.is_some() {
            if healthy {
                "running"
            } else {
                "starting"
            }
        } else if external {
            "external"
        } else if desired {
            "starting"
        } else if self.root()?.join("config.json").exists() {
            "stopped"
        } else {
            "unconfigured"
        };
        Ok(
            json!({"config": config, "managed": pid.is_some() || desired, "pid": pid, "state": state,
            "healthy": healthy, "models": models, "detail": detail, "last_exit_code": last_exit,
            "base_url": format!("http://127.0.0.1:{}/v1", config.port), "log_path": self.root()?.join("service.log")}),
        )
    }

    pub async fn run_lifecycle(self: Arc<Self>) {
        let auto_start = self.root().is_ok_and(|root| {
            root.join("config.json").exists() && Config::read(root).is_ok_and(|c| c.auto_start)
        });
        if auto_start {
            let _ = self.perform("start", None).await;
        }
        while !self.exiting.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _operation = self.operation.lock().await;
            let (retired, retry) = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let exit = state
                    .runtime
                    .as_mut()
                    .and_then(|r| r.child.try_wait().ok().flatten());
                let server_failed = state
                    .runtime
                    .as_ref()
                    .is_some_and(|r| r.server.is_finished());
                let retired = if exit.is_some() || server_failed {
                    state.last_exit = exit.and_then(|s| s.code());
                    state.detail = "Backend or relay stopped; retrying in 30 seconds".into();
                    state.retry_at = Instant::now() + Duration::from_secs(30);
                    state.runtime.take()
                } else {
                    None
                };
                let retry =
                    state.desired && state.runtime.is_none() && Instant::now() >= state.retry_at;
                (retired, retry)
            };
            if let Some(retired) = retired {
                let _ = tokio::task::spawn_blocking(move || drop(retired)).await;
                self.log("Backend or relay stopped; scheduled retry");
            }
            if retry {
                if let Err(error) = self.start().await {
                    self.fail(&error);
                }
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop_owned(true);
    }
}
