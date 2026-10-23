//! Runs the production coordinator, HTTP adapter, persistence and locks without a desktop runtime.
pub mod modules {
    pub mod windows_operation {
        pub fn format_permission_io_error(
            _: &str,
            _: &str,
            _: &str,
            _: &std::io::Error,
        ) -> Option<String> {
            None
        }
    }
    pub mod logger {
        pub fn log_warn(_: &str) {}
    }
    pub mod atomic_write {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../src-tauri/src/modules/atomic_write.rs"
        ));
    }
}
#[path = "../../src-tauri/src/modules/codex_seat_rotation.rs"]
pub mod codex_seat_rotation;
