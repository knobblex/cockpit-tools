use super::{
    config::{self, Config},
    relay, Service,
};
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const SENTENCE: &str = "You are Codex, an agent based on GPT-5.";
const CORRECTED: &str = "You are Codex, a coding assistant powered by Gemini.";

#[test]
fn legacy_model_names_resolve_without_changing_explicit_effort() {
    for model in ["gemini-4-argon", "gemini-3.8-flash"] {
        for suffix in ["low", "medium", "high", "xhigh"] {
            let mut request = json!({"model": format!("{model}-{suffix}"),
                "input": "hello", "reasoning": {"effort": "low"}});
            let result: Value = serde_json::from_slice(&relay::normalize_identity(Bytes::from(
                request.to_string(),
            )))
            .unwrap();
            request["model"] = json!(model);
            assert_eq!(result, request);
        }
    }
    for model in [
        "gemini-3-flash",
        "gemini-3.1-flash-lite",
        "gemini-future-high",
        "gemini-测试",
        "gemini-🦀",
        "claude-opus-5-5-high",
        "gpt-oss-120b-medium",
    ] {
        let request = Bytes::from(json!({"model": model, "input": "hello"}).to_string());
        assert_eq!(relay::normalize_identity(request.clone()), request);
    }
}

#[test]
fn identity_correction_is_scoped_and_preserves_tools_and_reasoning() {
    let original = json!({"model":"gemini-4-argon-high", "instructions":SENTENCE,
        "reasoning":{"effort":"high", "summary":"detailed"},
        "tools":[{"type":"function", "description":SENTENCE}],
        "input":[{"role":"developer", "content":[{"type":"input_text", "text":format!("{SENTENCE}\nKeep all permissions.")} ]},
        {"role":"user", "content":SENTENCE}, {"type":"function_call_output", "output":SENTENCE},
        {"type":"reasoning", "encrypted_content":"opaque"}]});
    let result: Value = serde_json::from_slice(&relay::normalize_identity(Bytes::from(
        original.to_string(),
    )))
    .unwrap();
    assert_eq!(result["model"], "gemini-4-argon");
    assert_eq!(result["instructions"], CORRECTED);
    assert_eq!(
        result["input"][0]["content"][0]["text"],
        format!("{CORRECTED}\nKeep all permissions.")
    );
    for field in ["reasoning", "tools"] {
        assert_eq!(result[field], original[field]);
    }
    for index in 1..4 {
        assert_eq!(result["input"][index], original["input"][index]);
    }
}

#[test]
fn identity_handles_chat_and_versions_but_not_embedded_user_quotes() {
    for sentence in [
        "You are Codex, based on GPT-5.6.",
        "You are Codex, an agent based on GPT-5.6-sol.",
    ] {
        let value =
            json!({"model":"GEMINI-test", "messages":[{"role":"system", "content":sentence}]});
        let result: Value =
            serde_json::from_slice(&relay::normalize_identity(Bytes::from(value.to_string())))
                .unwrap();
        assert_eq!(result["messages"][0]["content"], CORRECTED);
    }
    for input in [
        b"not json".to_vec(),
        json!({"model":"gpt-5", "instructions":SENTENCE})
            .to_string()
            .into_bytes(),
        json!({"model":"gemini-test", "messages":[{"role":"user", "content":SENTENCE}]})
            .to_string()
            .into_bytes(),
    ] {
        assert_eq!(relay::normalize_identity(Bytes::from(input.clone())), input);
    }
}

#[test]
fn legacy_config_migrates_without_retaining_python_and_rejects_unknown_fields() {
    let value = json!({"backend_path":"/bin/test", "state_dir":"/private/state", "port":8317, "model":"gemini-4-argon-high", "python_path":"ignored", "script_path":"ignored"});
    let config = Config::from_value(value.clone()).unwrap();
    assert!(config.auto_start);
    assert_eq!(config.model, "gemini-4-argon");
    let saved = serde_json::to_value(&config).unwrap();
    assert!(saved.get("python_path").is_none() && saved.get("script_path").is_none());
    for change in [
        json!({"port":true}),
        json!({"port":65536}),
        json!({"secret":"not allowed"}),
    ] {
        let mut invalid = value.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert!(Config::from_value(invalid).is_err());
    }
}

#[test]
fn backend_config_disables_remote_access_credits_plugins_and_request_logs() {
    let config = Config::defaults().unwrap();
    assert_eq!(config.model, "gemini-4-argon");
    let yaml = config.backend_config(12345, "test-key");
    assert_eq!(yaml["server"]["host"], "127.0.0.1");
    assert_eq!(yaml["server"]["port"], 12345);
    assert_eq!(yaml["management"]["allow-remote"], false);
    assert_eq!(
        yaml["oauth"]["providers"]["antigravity"]["antigravity-credits"],
        false
    );
    let aliases = yaml["oauth"]["model-alias"]["antigravity"]
        .as_array()
        .unwrap();
    assert_eq!(aliases[0]["name"], "gemini-4-argon-high");
    assert_eq!(aliases[0]["alias"], "gemini-4-argon");
    assert_eq!(aliases[1]["name"], "gemini-3.8-flash-high");
    assert_eq!(aliases[1]["alias"], "gemini-3.8-flash");
    assert_eq!(yaml["observability"]["logs"]["request-log"], false);
    assert_eq!(yaml["plugins"]["enabled"], false);
}

#[test]
fn generated_backend_document_uses_plain_numeric_scalars_with_arbitrary_precision() {
    let bytes = Config::defaults()
        .unwrap()
        .backend_document(12345, "test-key")
        .unwrap();
    // Use YAML's value type, not serde_json::Value (which hides the marker bug
    // by round-tripping its own private arbitrary_precision representation).
    let document: serde_yaml::Value = serde_yaml::from_slice(&bytes).unwrap();
    assert_eq!(document["config-version"].as_u64(), Some(8));
    assert_eq!(document["server"]["port"].as_u64(), Some(12345));
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains("$serde_json::private::Number"));
}

#[test]
fn state_lock_is_exclusive_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let first = config::state_lock(dir.path()).unwrap();
    assert!(config::state_lock(dir.path()).is_err());
    drop(first);
    assert!(config::state_lock(dir.path()).is_ok());
}

#[cfg(unix)]
#[test]
fn credentials_and_writes_reject_symlinks_and_public_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api-key");
    config::private_write(&key, "a".repeat(64).as_bytes()).unwrap();
    assert_eq!(config::read_key(dir.path()).unwrap(), "a".repeat(64));
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(config::read_key(dir.path()).is_err());
    let link = dir.path().join("link");
    symlink(&key, &link).unwrap();
    assert!(config::private_write(&link, b"overwrite").is_err());
    assert_eq!(std::fs::read_to_string(key).unwrap(), "a".repeat(64));
}

async fn serve(app: Router) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        port,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}

async fn proxy(app: Router) -> (String, CancellationToken, Vec<tokio::task::JoinHandle<()>>) {
    let (backend_port, backend) = serve(app).await;
    let cancel = CancellationToken::new();
    let (port, proxy) = serve(relay::router(relay::Relay {
        backend_port,
        client: relay::client().unwrap(),
        key: "test-key".into(),
        cancel: cancel.clone(),
    }))
    .await;
    (
        format!("http://127.0.0.1:{port}"),
        cancel,
        vec![backend, proxy],
    )
}

fn finish(cancel: CancellationToken, handles: Vec<tokio::task::JoinHandle<()>>) {
    cancel.cancel();
    for handle in handles {
        handle.abort();
    }
}

#[tokio::test]
async fn proxy_authenticates_corrects_body_and_removes_hop_headers() {
    let app = Router::new().route(
        "/v1/responses",
        post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert!(headers.get("x-private").is_none());
            Json(body)
        }),
    );
    let (url, cancel, handles) = proxy(app).await;
    let client = relay::client().unwrap();
    assert_eq!(
        client
            .post(format!("{url}/v1/responses"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(format!("{url}/v1/responses"))
        .bearer_auth("test-key")
        .header("connection", "x-private")
        .header("x-private", "remove")
        .json(&json!({"model":"gemini-test", "instructions":SENTENCE, "input":"hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["instructions"],
        CORRECTED
    );
    finish(cancel, handles);
}

#[tokio::test]
async fn proxy_preserves_error_status_retry_after_and_body() {
    let app = Router::new().fallback(|| async {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "37")],
            "quota exhausted",
        )
    });
    let (url, cancel, handles) = proxy(app).await;
    let response = relay::client()
        .unwrap()
        .get(url)
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "37");
    assert_eq!(response.text().await.unwrap(), "quota exhausted");
    finish(cancel, handles);
}

#[tokio::test]
async fn proxy_disables_websocket_catalog_preference_and_rejects_upgrade() {
    let app = Router::new().route(
        "/v1/models",
        get(|| async {
            Json(json!({"models":[{"slug":"gemini-test", "prefer_websockets":true}]}))
        }),
    );
    let (url, cancel, handles) = proxy(app).await;
    let client = relay::client().unwrap();
    let result: Value = client
        .get(format!("{url}/v1/models?client_version=1.0.0"))
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["models"][0]["prefer_websockets"], false);
    assert_eq!(
        client
            .get(url)
            .bearer_auth("test-key")
            .header("upgrade", "websocket")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    finish(cancel, handles);
}

#[tokio::test]
async fn proxy_does_not_follow_redirects_with_the_local_key() {
    let app = Router::new().fallback(|| async {
        (
            StatusCode::FOUND,
            [("location", "http://127.0.0.1:1/never")],
        )
    });
    let (url, cancel, handles) = proxy(app).await;
    let response = relay::client()
        .unwrap()
        .get(url)
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 302);
    finish(cancel, handles);
}

#[tokio::test]
async fn sse_first_event_arrives_before_completion_and_stop_cancels_stream() {
    let release = Arc::new(tokio::sync::Notify::new());
    let app = Router::new().fallback({
        let release = release.clone();
        move || {
            let release = release.clone();
            async move {
                let chunks = stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from_static(b"data: first\n\n"))
                })
                .chain(stream::once(async move {
                    release.notified().await;
                    Ok(Bytes::from_static(b"data: last\n\n"))
                }));
                (
                    [("content-type", "text/event-stream")],
                    Body::from_stream(chunks),
                )
                    .into_response()
            }
        }
    });
    let (url, cancel, handles) = proxy(app).await;
    let mut response = relay::client()
        .unwrap()
        .get(url)
        .bearer_auth("test-key")
        .send()
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first, "data: first\n\n");
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), response.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    release.notify_one();
    finish(cancel, handles);
}

async fn fixture() -> Option<(tempfile::TempDir, Arc<Service>, Value)> {
    // The isolated harness builds this helper; normal host unit tests need not.
    let executable = option_env!("CARGO_BIN_EXE_fixture-backend")?;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("accounts");
    std::fs::create_dir_all(state.join("auth")).unwrap();
    std::fs::write(state.join("auth/account.json"), "{}").unwrap();
    config::private_write(&state.join("api-key"), "a".repeat(64).as_bytes()).unwrap();
    let port = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let value = json!({"backend_path":executable, "state_dir":state, "port":port, "model":"gemini-test", "auto_start":true});
    let service = Arc::new(Service::with_root_result(Ok(dir.path().join("tool"))));
    service.perform("save", Some(value.clone())).await.unwrap();
    Some((dir, service, value))
}

#[tokio::test]
async fn start_stop_and_duplicate_start_own_only_the_spawned_child() {
    let Some((_dir, service, config)) = fixture().await else {
        return;
    };
    let first = service.perform("start", None).await.unwrap();
    assert_eq!(first["state"], "running");
    assert_eq!(first["healthy"], true);
    assert_eq!(
        service.perform("start", None).await.unwrap()["pid"],
        first["pid"]
    );
    assert!(service.perform("save", Some(config.clone())).await.is_err());
    let stopped = service.perform("stop", None).await.unwrap();
    assert_eq!(stopped["managed"], false);
    assert_eq!(stopped["state"], "stopped");
    assert!(std::path::Path::new(config["state_dir"].as_str().unwrap())
        .join("api-key")
        .exists());
    #[cfg(unix)]
    assert_eq!(
        unsafe { libc::kill(first["pid"].as_i64().unwrap() as i32, 0) },
        -1
    );
}

#[tokio::test]
async fn public_port_collision_never_stops_external_listener() {
    let Some((_dir, service, config)) = fixture().await else {
        return;
    };
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", config["port"].as_u64().unwrap() as u16))
            .await
            .unwrap();
    assert!(service.perform("start", None).await.is_err());
    assert_eq!(service.status().await.unwrap()["state"], "external");
    service.perform("stop", None).await.unwrap();
    assert_eq!(service.status().await.unwrap()["state"], "external");
    drop(listener);
}

#[tokio::test]
async fn failed_readiness_stops_child_and_releases_state_lock() {
    let Some((_dir, service, mut config)) = fixture().await else {
        return;
    };
    config["model"] = json!("missing-model");
    service.perform("save", Some(config.clone())).await.unwrap();
    assert!(service.perform("start", None).await.is_err());
    assert!(service.status().await.unwrap()["pid"].is_null());
    assert!(
        config::state_lock(std::path::Path::new(config["state_dir"].as_str().unwrap())).is_ok()
    );
    service.perform("stop", None).await.unwrap();
}

#[tokio::test]
async fn app_lifecycle_autostarts_and_shutdown_reaps_child() {
    let Some((_dir, service, _config)) = fixture().await else {
        return;
    };
    let worker = tokio::spawn(service.clone().run_lifecycle());
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = service.status().await.unwrap();
            if status["healthy"] == true {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    service.shutdown();
    assert_eq!(service.status().await.unwrap()["managed"], false);
    #[cfg(unix)]
    assert_eq!(
        unsafe { libc::kill(status["pid"].as_i64().unwrap() as i32, 0) },
        -1
    );
    assert!(service.perform("start", None).await.is_err());
    worker.abort();
}

#[tokio::test]
async fn disabled_autostart_does_not_spawn_backend() {
    let Some((_dir, service, mut config)) = fixture().await else {
        return;
    };
    config["auto_start"] = json!(false);
    service.perform("save", Some(config)).await.unwrap();
    let worker = tokio::spawn(service.clone().run_lifecycle());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(service.status().await.unwrap()["state"], "stopped");
    service.shutdown();
    worker.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn lifecycle_restarts_crashed_backend_without_replaying_requests() {
    let Some((_dir, service, _config)) = fixture().await else {
        return;
    };
    let first = service.perform("start", None).await.unwrap();
    let worker = tokio::spawn(service.clone().run_lifecycle());
    assert_eq!(
        unsafe { libc::kill(first["pid"].as_i64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let status = service.status().await.unwrap();
            if status["healthy"] == true && status["pid"] != first["pid"] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    service.shutdown();
    worker.abort();
}
