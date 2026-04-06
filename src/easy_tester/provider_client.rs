use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::Command,
};

use serde::{de::DeserializeOwned, Serialize};

use crate::easy_tester::error::EasyTesterError;

/// Synchronous JSON-RPC 2.0 client that shells out to `curl`.
/// When `log_file` is set, request/response details are appended there instead
/// of being printed to the terminal.
pub struct ProviderClient {
    url: String,
    auth_token: String,
    log_file: Option<PathBuf>,
}

impl ProviderClient {
    pub fn new(url: String, auth_token: String, log_file: Option<PathBuf>) -> Self {
        Self { url, auth_token, log_file }
    }

    fn log(&self, msg: &str) {
        if let Some(path) = &self.log_file {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "[rpc_client] {msg}");
            }
        }
    }

    pub fn call<Req, Resp>(&self, method: &str, params: Req) -> Result<Resp, EasyTesterError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        })
        .to_string();

        self.log(&format!("POST {} method={}", self.url, method));
        let out = Command::new("curl")
            .args([
                "-s",
                "-X", "POST",
                "-H", &format!("Authorization: Bearer {}", self.auth_token),
                "-H", "Content-Type: application/json",
                "-d", &body,
                &self.url,
            ])
            .output()
            .map_err(|e| EasyTesterError::runtime(format!("curl exec failed: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            return Err(EasyTesterError::runtime(format!(
                "curl failed (exit {}):\nstderr: {stderr}\nstdout: {stdout}",
                out.status
            )));
        }

        let text = String::from_utf8_lossy(&out.stdout);
        self.log(&format!("{method} response: {} bytes", text.len()));

        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            EasyTesterError::runtime(format!(
                "invalid JSON from provider: {e}\nBody: {text}"
            ))
        })?;

        if let Some(err_obj) = value.get("error") {
            let msg = match err_obj.get("message").and_then(|m| m.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => format!("provider error: {err_obj}"),
            };
            return Err(EasyTesterError::runtime(msg));
        }

        let result = value.get("result").ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "provider response missing 'result' field: {text}"
            ))
        })?;

        serde_json::from_value(result.clone()).map_err(|e| {
            EasyTesterError::runtime(format!(
                "failed to deserialize provider response: {e}\nResult: {result}"
            ))
        })
    }
}
