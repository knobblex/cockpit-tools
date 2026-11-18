//! Small diagnostic summaries, never a second copy of conversation history.
use axum::http::HeaderMap;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub(crate) fn allowed_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "content-type"
            | "content-encoding"
            | "content-length"
            | "accept"
            | "user-agent"
            | "openai-beta"
            | "x-request-id"
            | "x-oai-request-id"
            | "x-openai-request-id"
            | "openai-request-id"
            | "session-id"
            | "thread-id"
            | "session_id"
            | "thread_id"
            | "date"
            | "server"
            | "cf-ray"
            | "cf-mitigated"
            | "retry-after"
            | "openai-processing-ms"
    ) || name.to_ascii_lowercase().starts_with("x-ratelimit-")
}

pub(crate) fn headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter(|(key, _)| allowed_header(key.as_str()))
        .take(32)
        .filter_map(|(key, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (key.to_string(), prefix(value, 256).to_owned()))
        })
        .collect()
}

fn prefix(value: &str, bytes: usize) -> &str {
    let mut end = bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn sensitive_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_', ' '], "");
    matches!(
        key.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "xapikey"
            | "accesskey"
            | "accesskeyid"
            | "secretaccesskey"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "password"
            | "passwd"
            | "clientsecret"
            | "secret"
            | "privatekey"
            | "agentprivatekey"
            | "credential"
            | "credentials"
            | "assertion"
            | "clientassertion"
            | "encryptedcontent"
            | "b64json"
            | "imageurl"
            | "audiodata"
    )
}

struct Budget {
    nodes: usize,
    text: usize,
    omitted: bool,
}
impl Budget {
    fn value(&mut self, value: &Value, depth: usize) -> Value {
        if self.nodes == 0 || depth >= 6 {
            self.omitted = true;
            return json!("[omitted]");
        }
        self.nodes -= 1;
        match value {
            Value::String(text) => {
                let preview = prefix(text, self.text.min(256));
                self.text -= preview.len();
                if preview.len() < text.len() {
                    self.omitted = true;
                    json!(format!("{preview}…"))
                } else {
                    json!(preview)
                }
            }
            Value::Array(items) => {
                self.omitted |= items.len() > 2;
                // The newest messages are more useful than the start of a long chat.
                json!({"count":items.len(), "last":items.iter().rev().take(2)
                    .collect::<Vec<_>>().into_iter().rev()
                    .map(|item| self.value(item, depth + 1)).collect::<Vec<_>>()})
            }
            Value::Object(fields) => {
                let mut result = Map::new();
                // Keep errors and routing metadata ahead of large input/tool schemas.
                let priority = [
                    "type",
                    "id",
                    "model",
                    "status",
                    "error",
                    "detail",
                    "code",
                    "message",
                    "usage",
                    "stream",
                    "reasoning",
                    "input",
                    "messages",
                    "output",
                    "tools",
                ];
                let keys = priority
                    .iter()
                    .filter_map(|key| fields.get_key_value(*key))
                    .chain(
                        fields
                            .iter()
                            .filter(|(key, _)| !priority.contains(&key.as_str())),
                    );
                for (key, value) in keys {
                    if result.len() >= 16 || self.nodes == 0 {
                        break;
                    }
                    let value = if sensitive_field(key) {
                        self.nodes -= 1;
                        self.omitted = true;
                        json!("[REDACTED]")
                    } else {
                        self.value(value, depth + 1)
                    };
                    result.insert(prefix(key, 64).to_owned(), value);
                }
                if result.len() < fields.len() {
                    self.omitted = true;
                    result.insert("_omittedFields".into(), json!(fields.len() - result.len()));
                }
                Value::Object(result)
            }
            _ => value.clone(),
        }
    }
}

pub(crate) fn summarize(body: &str) -> (String, bool) {
    let mut budget = Budget {
        nodes: 48,
        text: 2048,
        omitted: false,
    };
    let summary = match serde_json::from_str::<Value>(body) {
        Ok(value) => budget.value(&value, 0),
        // Errors may be plain text or HTML. Save a short preview as valid JSON.
        Err(_) => json!({"textPreview":budget.value(&json!(prefix(body, 1024)), 0)}),
    };
    let encoded = summary.to_string();
    // JSON escaping can expand text. Keep even hostile input bounded and parseable.
    if encoded.len() > 8 * 1024 {
        return (
            json!({"preview":prefix(&encoded, 1024), "omitted":true}).to_string(),
            true,
        );
    }
    (
        encoded,
        budget.omitted || body.len() > 256 && !body.trim_start().starts_with(['{', '[']),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_chat_keeps_model_counts_latest_input_and_error_without_credentials() {
        let body = json!({"model":"gpt-6.1-sol", "input":(0..1000).map(|i|
            json!({"role":"user","content":format!("message-{i} {}", "字".repeat(1000))}))
            .collect::<Vec<_>>(), "api_key":"secret", "error":{"code":"bad_request","message":"Rejected"}}).to_string();
        let (summary, omitted) = summarize(&body);
        let value: Value = serde_json::from_str(&summary).unwrap();
        assert!(omitted);
        assert!(summary.len() <= 8192);
        assert_eq!(value["model"], "gpt-6.1-sol");
        assert_eq!(value["input"]["count"], 1000);
        assert!(summary.contains("message-999"));
        assert_eq!(value["error"]["code"], "bad_request");
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn error_headers_and_non_json_body_are_short_and_auth_free() {
        let source = HeaderMap::from_iter([
            (
                "x-oai-request-id".parse().unwrap(),
                "req-400".parse().unwrap(),
            ),
            ("retry-after".parse().unwrap(), "10".parse().unwrap()),
            (
                "authorization".parse().unwrap(),
                "Bearer secret".parse().unwrap(),
            ),
            ("set-cookie".parse().unwrap(), "secret".parse().unwrap()),
        ]);
        let safe = headers(&source);
        assert_eq!(safe.len(), 2);
        assert_eq!(safe["x-oai-request-id"], "req-400");
        let (summary, _) = summarize(&"<html>Bad Request</html>".repeat(1000));
        assert!(summary.len() < 1024);
        assert!(summary.contains("Bad Request"));
        assert!(serde_json::from_str::<Value>(&summary).is_ok());
    }
}
