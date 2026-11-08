use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

// Public IDs omit effort; CLIProxyAPI still needs the upstream's actual IDs.
const GEMINI_MODEL_ALIASES: [(&str, &str, &str); 2] = [
    ("gemini-4-argon-high", "gemini-4-argon", "Gemini 4 Argon"),
    (
        "gemini-3.8-flash-high",
        "gemini-3.8-flash",
        "Gemini 3.8 Flash",
    ),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub backend_path: PathBuf,
    pub state_dir: PathBuf,
    pub port: u16,
    pub model: String,
    #[serde(default = "enabled")]
    pub auto_start: bool,
}
fn enabled() -> bool {
    true
}

pub fn root() -> Result<PathBuf, String> {
    Ok(dirs::home_dir()
        .ok_or("Cannot resolve home directory")?
        .join(".cockpit_tools/local-tools/antigravity-upstream"))
}

pub fn normalize_gemini_model_id(model: &str) -> String {
    // Only rewrite models with a configured alias, so an unknown model cannot
    // become an ID the backend does not serve. rsplit_once is also UTF-8 safe.
    if let Some((base, effort)) = model.rsplit_once('-') {
        if ["low", "medium", "high", "xhigh"]
            .iter()
            .any(|level| effort.eq_ignore_ascii_case(level))
        {
            if let Some((_, alias, _)) = GEMINI_MODEL_ALIASES
                .iter()
                .find(|(_, alias, _)| base.eq_ignore_ascii_case(alias))
            {
                return (*alias).to_owned();
            }
        }
    }
    model.to_owned()
}

impl Config {
    pub fn defaults() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("Cannot resolve home directory")?;
        let backend_path = [
            "/opt/homebrew/bin/cliproxyapi",
            "/usr/local/bin/cliproxyapi",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .unwrap_or_default();
        Ok(Self {
            backend_path,
            state_dir: home.join(".cli-proxy-api/antigravity-codex"),
            port: 8317,
            model: "gemini-4-argon".into(),
            auto_start: true,
        })
    }

    pub fn from_value(mut value: Value) -> Result<Self, String> {
        // Only the two retired fields are accepted for migration; never execute them.
        if let Some(object) = value.as_object_mut() {
            object.remove("script_path");
            object.remove("python_path");
        }
        let mut config: Self = serde_json::from_value(value)
            .map_err(|_| String::from("Invalid tool configuration"))?;
        config.model = normalize_gemini_model_id(&config.model);
        Ok(config)
    }

    pub fn read(root: &Path) -> Result<Self, String> {
        let file = root.join("config.json");
        if !file.exists() {
            return Self::defaults();
        }
        reject_symlink(&file)?;
        if file.metadata().map_err(|e| e.to_string())?.len() > 16_384 {
            return Err("Configuration is too large".into());
        }
        Self::from_value(
            serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
                .map_err(|_| "Invalid tool configuration JSON")?,
        )
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.port < 1024 {
            return Err("Port must be between 1024 and 65535".into());
        }
        if !self.backend_path.is_absolute() || !self.backend_path.is_file() {
            return Err("Select an existing absolute CLIProxyAPI executable path".into());
        }
        if !self.state_dir.is_absolute() || !self.state_dir.is_dir() {
            return Err("Select the existing account state directory".into());
        }
        reject_symlink(&self.state_dir)?;
        let state = self.state_dir.canonicalize().map_err(|e| e.to_string())?;
        if state.parent().is_none() || dirs::home_dir().as_deref() == Some(state.as_path()) {
            return Err("Use a dedicated account state directory".into());
        }
        if self.model.is_empty()
            || self.model.len() > 128
            || !self
                .model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._/-".contains(&c))
            || self.model.starts_with('-')
        {
            return Err("Invalid model ID".into());
        }
        Ok(())
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        self.validate()?;
        private_dir(root)?;
        private_write(
            &root.join("config.json"),
            &serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
    }

    pub fn backend_config(&self, port: u16, key: &str) -> Value {
        let aliases: Vec<Value> = GEMINI_MODEL_ALIASES
            .iter()
            .map(|(name, alias, display)| {
                json!({
                    "name": name, "alias": alias, "display-name": display,
                    "force-mapping": true
                })
            })
            .collect();
        json!({
            "config-version": 8,
            "server": {"host": "127.0.0.1", "port": port, "commercial-mode": true, "discovery": {"enabled": false}},
            "management": {"allow-remote": false, "secret-key": "", "disable-control-panel": true},
            "access": {"api-keys": [key]},
            "oauth": {
                "auth-dir": self.state_dir.join("auth"),
                "providers": {"antigravity": {"antigravity-credits": false}},
                "model-alias": {
                    "antigravity": aliases
                }
            },
            "routing": {"strategy": "fill-first"},
            "observability": {"logs": {"debug": false, "logging-to-file": false, "request-log": false}, "usage": {"usage-statistics-enabled": false}},
            "plugins": {"enabled": false}
        })
    }

    pub fn backend_document(&self, port: u16, key: &str) -> Result<Vec<u8>, String> {
        // JSON is valid YAML. Serialize through serde_json so arbitrary_precision
        // numbers stay scalars, not serde_json's private Number marker objects.
        serde_json::to_vec_pretty(&self.backend_config(port, key)).map_err(|e| e.to_string())
    }
}

pub fn reject_symlink(path: &Path) -> Result<(), String> {
    if path.is_symlink() {
        Err("Refusing a symlink in private service state".into())
    } else {
        Ok(())
    }
}

pub fn private_dir(path: &Path) -> Result<(), String> {
    reject_symlink(path)?;
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    reject_symlink(path)?;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing parent directory")?)
            .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_key(state: &Path) -> Result<String, String> {
    let path = state.join("api-key");
    reject_symlink(&path)?;
    let meta =
        fs::metadata(&path).map_err(|_| "Local API key missing; authorize CLIProxyAPI first")?;
    if !meta.is_file() || meta.len() > 128 {
        return Err("Invalid local API key file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Local API key must have permissions 0600".into());
        }
    }
    let key = fs::read_to_string(path).map_err(|_| "Cannot read local API key")?;
    let key = key.trim();
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid local API key".into());
    }
    Ok(key.to_owned())
}

pub fn state_lock(state: &Path) -> Result<File, String> {
    let path = state.join("service.lock");
    reject_symlink(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| "Account state is already owned by another service")?;
    Ok(file)
}
