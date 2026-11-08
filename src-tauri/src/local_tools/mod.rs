//! Isolated local tool integration; no changes to the main gateway's routing.
mod core;

use std::sync::{Arc, LazyLock};
static SERVICE: LazyLock<Arc<core::Service>> = LazyLock::new(|| Arc::new(core::Service::new()));

pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("local-upstream-lifecycle")
        .setup(|_, _| {
            let service = SERVICE.clone();
            tauri::async_runtime::spawn(async move { service.run_lifecycle().await });
            Ok(())
        })
        .on_event(|_, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                SERVICE.shutdown();
            }
        })
        .build()
}

#[tauri::command]
pub async fn local_tools_antigravity(
    window: tauri::WebviewWindow,
    action: String,
    config: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    if window.label() != "main" {
        return Err("This tool is only available in the main window".into());
    }
    SERVICE.perform(&action, config).await
}
