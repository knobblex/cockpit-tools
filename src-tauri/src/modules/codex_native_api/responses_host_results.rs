//! Codex clients can inject these host results without a model-issued call.
//! Recognize the envelope exactly; replay validation still checks its payload.
use serde_json::Value;

pub(super) fn is_host_result(item: &Value) -> bool {
    item["type"] == "function_call_output"
        && match item["namespace"].as_str() {
            Some("codex_app" | "codex_tui") => matches!(
                item["name"].as_str(),
                Some("create_thread" | "send_message_to_thread")
            ),
            Some("agent") => item["name"].as_str().is_some_and(is_page_context),
            _ => false,
        }
}

// Companion Page snapshots are injected by the client, not returned by a
// model-issued call. Recognize only their wire names, never arbitrary agent
// outputs; payload portability and caller provenance are checked separately.
fn is_page_context(name: &str) -> bool {
    if name == "codex_apps_open_page" {
        return true;
    }
    let Some((part, total)) = name
        .strip_prefix("codex_apps_selected_page_part_")
        .and_then(|suffix| suffix.split_once("_of_"))
    else {
        return false;
    };
    let number = |value: &str| {
        (!value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten()
    };
    matches!((number(part), number(total)), (Some(part), Some(total)) if part <= total)
}

pub(super) fn is_standalone(item: &Value) -> bool {
    is_host_result(item)
        && match item.get("call_id") {
            None => true,
            Some(Value::String(id)) => id.is_empty(),
            // Explicit malformed or stale IDs must follow normal pairing rules.
            _ => false,
        }
}
