//! Usage events kept in a file on this computer (`telemetry.jsonl` in the program's data folder).
//! Nothing is sent over the network.

use std::io::Write;
use std::path::PathBuf;

use rustcode_application::Telemetry;

/// Appends one JSON line per event.
pub struct LocalTelemetry {
    file: PathBuf,
    version: String,
}

impl LocalTelemetry {
    pub fn new(version: &str) -> Self {
        let file = dirs::data_dir().map(|dir| dir.join("rustcode")).unwrap_or_else(std::env::temp_dir).join("telemetry.jsonl");
        Self::with_file(version, file)
    }

    pub fn with_file(version: &str, file: PathBuf) -> Self {
        Self { file, version: version.to_owned() }
    }
}

impl Telemetry for LocalTelemetry {
    fn record(&self, event: &str) {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let line = serde_json::json!({
            "event": event, "time": at, "version": self.version, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        });
        if let Some(parent) = self.file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&self.file) {
            let _ = writeln!(out, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_one_json_line_without_personal_data() {
        let dir = std::env::temp_dir().join(format!("rustcode-telemetry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("telemetry.jsonl");
        let telemetry = LocalTelemetry::with_file("0.1.0", file.clone());
        telemetry.record("app_start");
        telemetry.record("update_check");
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "app_start");
        assert_eq!(lines[0]["version"], "0.1.0");
        let mut keys: Vec<_> = lines[0].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["arch", "event", "os", "time", "version"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
