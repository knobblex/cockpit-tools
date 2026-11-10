// Keep protocol adaptation and model aliases identical to the desktop service.
#[allow(dead_code)]
#[path = "../../../src-tauri/src/local_tools/config.rs"]
mod config;
mod egress;
#[allow(dead_code)]
#[path = "../../../src-tauri/src/local_tools/relay.rs"]
mod relay;

use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfig {
    backend: config::Config,
    backend_port: u16,
    proxy_url: String,
    #[serde(default)]
    egress_port: Option<u16>,
}

impl ServerConfig {
    fn validate(&self) -> Result<(), String> {
        self.backend.validate()?;
        if let Some(port) = self.egress_port {
            if port < 1024 || port == self.backend.port || port == self.backend_port {
                return Err("Egress port must be unprivileged and distinct".into());
            }
            let proxy = url::Url::parse(&self.proxy_url).map_err(|_| "Invalid proxy URL")?;
            if proxy.scheme() != "http" || proxy.host_str() != Some("127.0.0.1") {
                return Err("DNS adapter requires a local IPv4 HTTP parent proxy".into());
            }
        }
        if self.backend_port < 1024 || self.backend_port == self.backend.port {
            return Err("Backend port must be unprivileged and distinct from relay port".into());
        }
        let proxy = url::Url::parse(&self.proxy_url).map_err(|_| "Invalid proxy URL")?;
        if !matches!(proxy.scheme(), "http" | "socks5" | "socks5h")
            || !matches!(proxy.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
            || !proxy.username().is_empty()
            || proxy.password().is_some()
        {
            return Err("Use a local HTTP or SOCKS proxy without embedded credentials".into());
        }
        Ok(())
    }

    fn backend_document(&self, key: &str) -> serde_json::Value {
        let mut document = self.backend.backend_config(self.backend_port, key);
        let proxy = self
            .egress_port
            .map(|port| format!("http://127.0.0.1:{port}"))
            .unwrap_or_else(|| self.proxy_url.clone());
        document["requests"] = serde_json::json!({"proxy-url": proxy});
        document
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mode = args
        .next()
        .ok_or("Usage: cockpit-antigravity-server prepare|serve CONFIG")?;
    let path = PathBuf::from(args.next().ok_or("Missing configuration path")?);
    if args.next().is_some() || !matches!(mode.as_str(), "prepare" | "serve") {
        return Err("Usage: cockpit-antigravity-server prepare|serve CONFIG".into());
    }
    config::reject_symlink(&path)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.len() > 16_384 {
        return Err("Configuration exceeds 16 KiB".into());
    }
    let settings: ServerConfig =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid server configuration")?;
    settings.validate()?;
    let key = config::read_key(&settings.backend.state_dir)?;
    if mode == "prepare" {
        let _lock = config::state_lock(&settings.backend.state_dir)?;
        config::private_write(
            &settings.backend.state_dir.join("backend.generated.yaml"),
            &serde_json::to_vec_pretty(&settings.backend_document(&key))
                .map_err(|e| e.to_string())?,
        )?;
        return Ok(());
    }
    let client = relay::client()?;
    // Backend discovery can be temporarily empty during OAuth refresh or startup.
    // Keep the listener alive so recovery does not exhaust systemd's start limit.
    let cancel = CancellationToken::new();
    if let Some(port) = settings.egress_port {
        let proxy = url::Url::parse(&settings.proxy_url).map_err(|_| "Invalid proxy URL")?;
        let parent = format!(
            "127.0.0.1:{}",
            proxy.port_or_known_default().ok_or("Missing proxy port")?
        )
        .parse()
        .map_err(|_| "Invalid parent proxy address")?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(egress::serve(listener, parent, cancel.clone()));
    }
    let router = relay::router(relay::Relay {
        client,
        backend_port: settings.backend_port,
        key: Arc::from(key),
        cancel: cancel.clone(),
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", settings.backend.port))
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("Relay ready on 127.0.0.1:{}", settings.backend.port);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Cannot register SIGTERM handler");
            tokio::select! {
                _ = term.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
            cancel.cancel();
        })
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_backend_uses_local_proxy_and_shared_aliases() {
        let settings = ServerConfig {
            backend: config::Config {
                backend_path: "/opt/antigravity-upstream/cliproxyapi".into(),
                state_dir: "/var/lib/antigravity-upstream".into(),
                port: 8317,
                model: "gemini-4-argon".into(),
                auto_start: false,
            },
            backend_port: 8318,
            proxy_url: "http://127.0.0.1:7890".into(),
            egress_port: None,
        };
        let document = settings.backend_document("test-key");
        assert_eq!(document["requests"]["proxy-url"], settings.proxy_url);
        assert_eq!(document["server"]["host"], "127.0.0.1");
        assert_eq!(document["server"]["port"], 8318);
        assert_eq!(document["management"]["allow-remote"], false);
        assert_eq!(document["plugins"]["enabled"], false);
        assert_eq!(
            document["oauth"]["model-alias"],
            settings.backend.backend_config(8318, "test-key")["oauth"]["model-alias"]
        );
    }
}
