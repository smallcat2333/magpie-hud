//! Desktop connection settings. The HUD viewing key is stored in the OS
//! credential store; config carries only a reference and the HTTPS URL.
use crate::remote::{self, ConnectionConfig, RemoteClient};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Keep separate desktop profiles (including isolated verification profiles)
/// from overwriting one another's system credential.
pub fn credential_reference() -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    crate::config::config_path().hash(&mut hash);
    format!("remote-hud-{:016x}", hash.finish())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub mode: String,
    pub remote_url: String,
    pub has_credential: bool,
    /// Populated when the saved credential cannot be read; never the key itself.
    pub credential_error: Option<String>,
}
pub fn view(connection: &ConnectionConfig) -> ConnectionView {
    let has_reference = connection.credential_ref.is_some();
    let (has_credential, credential_error) = if has_reference {
        match remote::load_key(
            &connection
                .credential_ref
                .clone()
                .unwrap_or_else(credential_reference),
        ) {
            Ok(_) => (true, None),
            Err(error) => (false, Some(error)),
        }
    } else {
        (false, None)
    };
    ConnectionView {
        mode: if connection.mode == "remote" {
            "remote".into()
        } else {
            "local".into()
        },
        remote_url: connection.remote_url.clone(),
        has_credential,
        credential_error,
    }
}
/// Build the client used by queries and the event stream. The supplied key wins
/// when testing; otherwise the saved credential is read outside the WebView.
pub fn client(
    connection: &ConnectionConfig,
    key: Option<&str>,
) -> Result<Arc<RemoteClient>, String> {
    if connection.remote_url.trim().is_empty() {
        return Err("请先填写主 HUD 地址".into());
    }
    let key = match key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => key.to_owned(),
        None => remote::load_key(
            &connection
                .credential_ref
                .clone()
                .unwrap_or_else(credential_reference),
        )?,
    };
    RemoteClient::new(&connection.remote_url, key)
}
/// Per-capability result: a healthy process is not reported as a fully working
/// Magpie source when the log, trace, quotas or costs are unavailable.
pub fn test(url: &str, key: Option<&str>, reference: String) -> Result<Value, String> {
    let temporary = ConnectionConfig {
        mode: "remote".into(),
        remote_url: url.trim().to_owned(),
        credential_ref: Some(reference),
    };
    let client = client(&temporary, key)?;
    let capabilities = client.capabilities()?;
    let collectors = capabilities
        .get("collectors")
        .cloned()
        .unwrap_or(Value::Null);
    let mut checks = Vec::new();
    for (name, label) in [
        ("logs", "用量日志"),
        ("trace", "完整实时路由"),
        ("quotas", "账号额度"),
        ("usage", "费用与用量"),
        ("concurrency", "并发"),
    ] {
        let item = collectors.get(name).unwrap_or(&Value::Null);
        checks.push(json!({
            "name": name, "label": label,
            "ok": item.get("available").and_then(Value::as_bool).unwrap_or(false),
            "detail": item.get("error").and_then(Value::as_str).unwrap_or(if item.is_null() { "服务端未报告此项" } else { "" }),
        }));
    }
    Ok(
        json!({"instanceId": capabilities.get("instanceId"), "timezone": capabilities.get("timezone"), "checks": checks}),
    )
}
