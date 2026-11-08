//! Rust-only HUD connection. Viewing credentials never leave this module except
//! for the key initially entered by the user; settings return only a presence bit.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionConfig {
    /// "local" is the backwards-compatible default.
    pub mode: String,
    pub remote_url: String,
    pub credential_ref: Option<String>,
}
impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            mode: "local".into(),
            remote_url: String::new(),
            credential_ref: None,
        }
    }
}

pub struct RemoteClient {
    client: reqwest::blocking::Client,
    stream: reqwest::blocking::Client,
    base: String,
    key: String,
}
impl RemoteClient {
    pub fn new(base: &str, key: String) -> Result<Arc<Self>, String> {
        let url = reqwest::Url::parse(base.trim()).map_err(|_| "HUD 地址格式无效")?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err("请填写 HUD 的 HTTPS 站点地址；本机测试可使用回环 HTTP".into());
        }
        if key.trim().is_empty() {
            return Err("请填写 HUD 查看密钥".into());
        }
        let build = || {
            reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
        };
        let client = build()
            .timeout(Duration::from_secs(40))
            .build()
            .map_err(|_| "无法初始化 HTTPS 客户端")?;
        // Reopen the stream periodically to bound cancellation latency. Every
        // subscription starts with a full snapshot, so no replay log is needed.
        let stream = build()
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|_| "无法初始化 HTTPS 客户端")?;
        Ok(Arc::new(Self {
            client,
            stream,
            base: url.as_str().trim_end_matches('/').to_owned(),
            key,
        }))
    }
    fn response(
        &self,
        client: &reqwest::blocking::Client,
        path: &str,
        args: Option<&Value>,
    ) -> Result<reqwest::blocking::Response, String> {
        let request = if let Some(args) = args {
            client.post(format!("{}{path}", self.base)).json(args)
        } else {
            client.get(format!("{}{path}", self.base))
        };
        let response = request
            .bearer_auth(&self.key)
            .send()
            .map_err(|_| "无法连接远端 HUD，请检查网络、证书和服务器状态")?;
        match response.status().as_u16() {
            200 => Ok(response),
            401 | 403 => Err("HUD 查看密钥无效、已撤销或访问被拒绝".into()),
            429 => Err("远端 HUD 查询繁忙，请稍后重试".into()),
            _ => Err("远端 HUD 返回错误，请检查服务版本和状态".into()),
        }
    }
    pub fn query(&self, command: &str, args: Value) -> Result<Value, String> {
        if !matches!(
            command,
            "query_date_stats"
                | "query_month_usage"
                | "query_route_history"
                | "query_usage_details"
                | "query_model_costs"
        ) {
            return Err("未知远端查询".into());
        }
        let response = self.response(
            &self.client,
            &format!("/api/v1/query/{command}"),
            Some(&args),
        )?;
        read_json(response)
    }
    pub fn capabilities(&self) -> Result<Value, String> {
        let value = read_json(self.response(&self.client, "/api/v1/capabilities", None)?)?;
        if value["protocolVersion"].as_u64() != Some(1) || value["instanceId"].as_str().is_none() {
            return Err("HUD 协议版本不兼容，请升级服务端或客户端".into());
        }
        Ok(value)
    }
    /// A streaming connection has a bounded lifetime so a stopped source never
    /// leaves an unbounded blocked worker. The server sends a full state on every
    /// reconnect, making this safe even without an event replay buffer.
    pub fn subscribe(
        self: Arc<Self>,
        stop: Arc<AtomicBool>,
        receive: impl Fn(Value) + Send + 'static,
        disconnected: impl Fn() + Send + 'static,
    ) {
        std::thread::spawn(move || {
            let mut delay = 1u64;
            while !stop.load(Ordering::Acquire) {
                let result = self.read_events(&stop, &receive);
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if result.is_err() {
                    disconnected();
                    delay = (delay * 2).min(30);
                } else {
                    delay = 1;
                }
                for _ in 0..delay * 10 {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }
    fn read_events(&self, stop: &AtomicBool, receive: &impl Fn(Value)) -> Result<(), String> {
        let response = self.response(&self.stream, "/api/v1/events", None)?;
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Err("远端未提供实时事件流".into());
        }
        let mut reader = BufReader::new(response.take(64 * 1024 * 1024));
        let mut line = String::new();
        let mut event = String::new();
        let mut data = String::new();
        let mut received = false;
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    return if received {
                        Ok(())
                    } else {
                        Err("远端实时连接已关闭".into())
                    }
                }
                Err(_) => {
                    return if received {
                        Ok(())
                    } else {
                        Err("远端实时连接中断".into())
                    }
                }
                Ok(_) => {}
            }
            if line.len() + data.len() > 8 * 1024 * 1024 {
                return Err("远端快照超过大小限制".into());
            }
            let value = line.trim_end_matches(['\r', '\n']);
            if value.is_empty() {
                if event == "auth-expired" {
                    return Err("HUD 查看密钥已失效".into());
                }
                if event == "snapshot" && !data.is_empty() {
                    let parsed: Value =
                        serde_json::from_str(&data).map_err(|_| "远端快照格式无效")?;
                    receive(parsed);
                    received = true;
                }
                event.clear();
                data.clear();
            } else if let Some(value) = value.strip_prefix("event:") {
                event = value.trim().to_owned();
            } else if let Some(value) = value.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
    }
}
fn read_json(response: reqwest::blocking::Response) -> Result<Value, String> {
    serde_json::from_reader(response.take(16 * 1024 * 1024))
        .map_err(|_| "远端 HUD 数据格式无效或超过大小限制".into())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn entry(reference: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new("com.smallcat.magpie-hud.remote", reference)
        .map_err(|_| "系统凭据库不可用".to_string())
}
pub fn load_key(reference: &str) -> Result<String, String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        entry(reference)?
            .get_password()
            .map_err(|_| "未找到远端 HUD 查看密钥，请重新输入".into())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = reference;
        Err("当前桌面平台尚未配置系统凭据库".into())
    }
}
pub fn save_key(reference: &str, key: &str) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        entry(reference)?
            .set_password(key)
            .map_err(|_| "无法保存 HUD 查看密钥到系统凭据库".into())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (reference, key);
        Err("当前桌面平台尚未配置系统凭据库".into())
    }
}
pub fn forget_key(reference: &str) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        match entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("无法从系统凭据库删除密钥".into()),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = reference;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_addresses_require_tls_and_never_embed_credentials() {
        for bad in [
            "http://example.com",
            "https://user:password@example.com",
            "https://example.com/?key=secret",
            "file:///tmp/key",
            "https://example.com/path",
        ] {
            assert!(RemoteClient::new(bad, "test".into()).is_err(), "{bad}");
        }
        assert!(RemoteClient::new("https://example.com", "test".into()).is_ok());
        assert!(RemoteClient::new("http://127.0.0.1:3435", "test".into()).is_ok());
    }

    #[test]
    fn authenticated_queries_and_sse_use_the_hud_protocol() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            for route in [
                "/api/v1/capabilities",
                "/api/v1/query/query_date_stats",
                "/api/v1/events",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                input.read_line(&mut line).unwrap();
                assert!(line.contains(route), "{line}");
                let mut authorized = false;
                let mut length = 0usize;
                loop {
                    line.clear();
                    input.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if line.to_lowercase().starts_with("authorization:") {
                        authorized = line.trim().ends_with("Bearer hud-test-key");
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!(authorized);
                let mut body = vec![0u8; length];
                input.read_exact(&mut body).unwrap();
                let (mime, response) = if route.ends_with("capabilities") {
                    (
                        "application/json",
                        r#"{"protocolVersion":1,"instanceId":"instance"}"#.to_owned(),
                    )
                } else if route.ends_with("query_date_stats") {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&body).unwrap()["date"],
                        "2026-11-08"
                    );
                    ("application/json", r#"{"daily":{"tokens":14}}"#.to_owned())
                } else {
                    ("text/event-stream", "event: snapshot\ndata: {\"instanceId\":\"instance\",\"bootId\":\"boot\",\"revision\":3,\"snapshot\":{\"date\":\"2026-11-08\"}}\n\n".to_owned())
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        let client =
            RemoteClient::new(&format!("http://{address}"), "hud-test-key".into()).unwrap();
        assert_eq!(client.capabilities().unwrap()["protocolVersion"], 1);
        assert_eq!(
            client
                .query("query_date_stats", serde_json::json!({"date":"2026-11-08"}))
                .unwrap()["daily"]["tokens"],
            14
        );
        let stop = Arc::new(AtomicBool::new(false));
        let (send, receive) = std::sync::mpsc::channel();
        client.subscribe(
            stop.clone(),
            move |value| {
                let _ = send.send(value);
            },
            || {},
        );
        let value = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        stop.store(true, Ordering::Release);
        assert_eq!(value["revision"], 3);
        worker.join().unwrap();
    }
}
