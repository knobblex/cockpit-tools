use axum::{
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args[1], "-config");
    let document: serde_yaml::Value =
        serde_yaml::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
    assert_eq!(document["config-version"].as_u64(), Some(8));
    assert!(document["server"]["port"].as_u64().is_some());
    let config: Value = serde_json::to_value(document).unwrap();
    let key = format!(
        "Bearer {}",
        config["access"]["api-keys"][0].as_str().unwrap()
    );
    let app = Router::new().route(
        "/v1/models",
        get(move |headers: HeaderMap| {
            let key = key.clone();
            async move {
                if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&key) {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                Ok(Json(
                    json!({"object":"list", "data":[{"id":"gemini-test", "object":"model"}]}),
                ))
            }
        }),
    );
    let port = config["server"]["port"].as_u64().unwrap() as u16;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
