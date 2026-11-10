use super::config::normalize_model_id;
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::State,
    http::{HeaderMap, Request, Response, StatusCode},
    routing::any,
    Router,
};
use futures_util::StreamExt;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

static IDENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^You are Codex, (?:an agent )?based on GPT-[A-Za-z0-9]+(?:[.-][A-Za-z0-9]+)*\.",
    )
    .unwrap()
});
const MAX_BODY: usize = 128 * 1024 * 1024;
const MAX_CATALOG: usize = 1024 * 1024;

pub fn normalize_request(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(model) = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return body;
    };
    let normalized_model = normalize_model_id(&model);
    let mut changed = false;
    if normalized_model != model {
        value["model"] = Value::String(normalized_model);
        changed = true;
    }
    let family = model.to_ascii_lowercase();
    let identity = if family.starts_with("gemini-") {
        Some("You are Codex, a coding assistant powered by Gemini.")
    } else if family.starts_with("claude-") {
        Some("You are Codex, a coding assistant powered by Claude.")
    } else {
        None
    };
    if let Some(identity) = identity {
        changed |= correct_model_identity(&mut value, identity);
    }
    if changed {
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
    } else {
        body
    }
}

fn correct_model_identity(value: &mut Value, identity: &str) -> bool {
    fn correct(value: &mut Value, identity: &str) -> bool {
        let Some(text) = value.as_str() else {
            return false;
        };
        let corrected = IDENTITY.replace_all(text, identity);
        if corrected == text {
            return false;
        }
        *value = Value::String(corrected.into_owned());
        true
    }
    let mut changed = value
        .get_mut("instructions")
        .is_some_and(|value| correct(value, identity));
    for field in ["input", "messages"] {
        if let Some(messages) = value.get_mut(field).and_then(Value::as_array_mut) {
            for message in messages {
                if !matches!(
                    message.get("role").and_then(Value::as_str),
                    Some("system" | "developer")
                ) {
                    continue;
                }
                if let Some(content) = message.get_mut("content") {
                    if let Some(parts) = content.as_array_mut() {
                        for part in parts {
                            if matches!(
                                part.get("type").and_then(Value::as_str),
                                Some("text" | "input_text")
                            ) {
                                changed |= part
                                    .get_mut("text")
                                    .is_some_and(|value| correct(value, identity));
                            }
                        }
                    } else {
                        changed |= correct(content, identity);
                    }
                }
            }
        }
    }
    changed
}

#[derive(Clone)]
pub struct Relay {
    pub client: reqwest::Client,
    pub backend_port: u16,
    pub key: Arc<str>,
    pub cancel: CancellationToken,
}

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

fn error(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(json!({"error":{"message":message}}).to_string()))
        .unwrap()
}

pub fn clean_headers(headers: &mut HeaderMap) {
    let connection_names: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|n| n.trim().to_owned()))
        .collect();
    for name in connection_names {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

pub fn router(state: Relay) -> Router {
    Router::new().fallback(any(forward)).with_state(state)
}

async fn limited_response(response: reqwest::Response) -> Result<Bytes, String> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| "Cannot read backend response")?;
        if body.len() + part.len() > MAX_CATALOG {
            return Err("Model catalog exceeds 1 MiB".into());
        }
        body.extend_from_slice(&part);
    }
    Ok(Bytes::from(body))
}

pub async fn models(client: &reqwest::Client, port: u16, key: &str) -> Result<Vec<String>, String> {
    let response = client
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth(key)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| "Local model backend unavailable")?;
    if !response.status().is_success() {
        return Err(format!(
            "Model health check returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let value: Value = serde_json::from_slice(&limited_response(response).await?)
        .map_err(|_| "Invalid model catalog")?;
    if value["object"] != "list" {
        return Err("Invalid model catalog".into());
    }
    value["data"]
        .as_array()
        .ok_or("Invalid model catalog")?
        .iter()
        .map(|m| {
            m["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| "Invalid model ID".into())
        })
        .collect()
}

async fn forward(State(state): State<Relay>, request: Request<Body>) -> Response<Body> {
    let expected = format!("Bearer {}", state.key);
    #[allow(deprecated)]
    let authorized = request.headers().get("authorization").is_some_and(|v| {
        ring::constant_time::verify_slices_are_equal(v.as_bytes(), expected.as_bytes()).is_ok()
    });
    if !authorized {
        return error(StatusCode::UNAUTHORIZED, "Invalid local API key");
    }
    if request
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "Request exceeds 128 MiB");
    }
    if request.headers().contains_key("upgrade")
        || request.headers().contains_key("transfer-encoding")
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Use HTTP Responses with Content-Length; streaming SSE is supported",
        );
    }
    if request.uri().scheme().is_some() || request.uri().path().starts_with("//") {
        return error(StatusCode::BAD_REQUEST, "Invalid request path");
    }
    let (parts, body) = request.into_parts();
    let body = tokio::select! {
        _ = state.cancel.cancelled() => return error(StatusCode::SERVICE_UNAVAILABLE, "Service stopping"),
        result = to_bytes(body, MAX_BODY) => match result { Ok(body) => body, Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "Invalid body or request exceeds 128 MiB") }
    };
    let is_catalog = parts.method == "GET"
        && parts.uri.path() == "/v1/models"
        && parts.uri.query().is_some_and(|q| {
            url::form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "client_version")
        });
    let body = if parts.method == "POST"
        && matches!(parts.uri.path(), "/v1/responses" | "/v1/chat/completions")
    {
        normalize_request(body)
    } else {
        body
    };
    let mut headers = parts.headers;
    clean_headers(&mut headers);
    headers.remove("host");
    headers.remove("content-length");
    if is_catalog {
        headers.remove("accept-encoding");
    }
    // Fixed loopback destination; never follow redirects or inherit system proxy settings.
    let url = format!(
        "http://127.0.0.1:{}{}",
        state.backend_port,
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let response = tokio::select! {
        _ = state.cancel.cancelled() => return error(StatusCode::SERVICE_UNAVAILABLE, "Service stopping"),
        response = state.client.request(parts.method, url).headers(headers).body(body).send() => match response {
            Ok(response) => response,
            Err(_) => return error(StatusCode::BAD_GATEWAY, "Local model backend unavailable"),
        }
    };
    let status = response.status();
    let mut headers = response.headers().clone();
    clean_headers(&mut headers);
    let body = if is_catalog && status == StatusCode::OK {
        let bytes = tokio::select! {
            _ = state.cancel.cancelled() => return error(StatusCode::SERVICE_UNAVAILABLE, "Service stopping"),
            result = limited_response(response) => match result { Ok(bytes) => bytes, Err(_) => return error(StatusCode::BAD_GATEWAY, "Invalid model catalog") }
        };
        let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
            return error(StatusCode::BAD_GATEWAY, "Invalid model catalog");
        };
        if let Some(models) = value.get_mut("models").and_then(Value::as_array_mut) {
            for model in models {
                if let Some(model) = model.as_object_mut() {
                    model.insert("prefer_websockets".into(), Value::Bool(false));
                }
            }
        }
        headers.remove("content-length");
        Body::from(value.to_string())
    } else {
        Body::from_stream(
            response
                .bytes_stream()
                .take_until(state.cancel.cancelled_owned()),
        )
    };
    let mut outgoing = Response::new(body);
    *outgoing.status_mut() = status;
    *outgoing.headers_mut() = headers;
    outgoing
}
