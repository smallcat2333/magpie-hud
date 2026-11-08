//! 回环网关读取。服务端只保留分类后的短错误，不带回上游正文或请求路径。

use serde::de::DeserializeOwned;
use std::io::ErrorKind;
use std::time::Duration;

/// 分类后的网关失败。文本不含 URL、响应正文或本地路径。
pub fn classify_ureq(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("网关返回 HTTP {code}"),
        ureq::Error::Timeout(_) => "连接超时".into(),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => "无法连接网关".into(),
        ureq::Error::Io(error) => match error.kind() {
            ErrorKind::ConnectionRefused => "连接被拒绝".into(),
            ErrorKind::TimedOut => "连接超时".into(),
            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe => {
                "连接中断".into()
            }
            _ => "无法连接网关".into(),
        },
        ureq::Error::Protocol(_) | ureq::Error::Http(_) | ureq::Error::BadUri(_) => {
            "网关响应无效".into()
        }
        _ => "网关请求失败".into(),
    }
}

/// 读取 JSON。`classify` 为假时保留桌面端原有的详细错误文本。
/// 两条路径共用 8 MiB 上限、成功状态校验与回环地址解析，分类只改变错误文本。
pub fn read_json<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    classify: bool,
) -> Result<T, String> {
    if !classify {
        return crate::live::read_json(agent, url);
    }
    // 分类路径同样只接受回环地址，避免服务端把配置里的远端地址发出去。
    if !is_loopback_url(url) {
        return Err("实时路由地址必须是本机回环 HTTP 地址和端口".into());
    }
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(error) => return Err(classify_ureq(error)),
    };
    if !response.status().is_success() {
        return Err(format!("网关返回 HTTP {}", response.status()));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(crate::live::MAX_JSON_BYTES)
        .read_to_string()
        .map_err(|_| "响应超出 8 MiB 上限".to_string())?;
    serde_json::from_str(&body).map_err(|_| "响应格式无效".to_string())
}

/// 只允许解析后落到回环的 http URL，与 `live::gateway_url` 同一规则。
fn is_loopback_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    crate::live::gateway_url(authority).is_ok()
}

pub fn client(timeout: Duration) -> ureq::Agent {
    crate::live::client(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classified_errors_hide_transport_details() {
        let status = classify_ureq(ureq::Error::StatusCode(503));
        assert_eq!(status, "网关返回 HTTP 503");
        assert!(!status.contains('/'));
        assert!(!status.contains("http"));
        let timeout = classify_ureq(ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(timeout, "连接超时");
        let missing = classify_ureq(ureq::Error::HostNotFound);
        assert_eq!(missing, "无法连接网关");
        let refused = classify_ureq(ureq::Error::Io(std::io::Error::new(
            ErrorKind::ConnectionRefused,
            "connect 127.0.0.1:3425/v1/magpie/trace",
        )));
        assert_eq!(refused, "连接被拒绝");
        assert!(!refused.contains("magpie"));
        assert!(!refused.contains("3425"));
    }

    #[test]
    fn classified_reader_rejects_non_loopback_before_connecting() {
        let agent = client(std::time::Duration::from_millis(200));
        for url in [
            "https://127.0.0.1:9/v1/magpie/trace",
            "http://example.com:9/v1/magpie/trace",
            "http://8.8.8.8:9/v1/magpie/trace",
            "http://127.0.0.1/v1/magpie/trace",
        ] {
            let error = read_json::<serde_json::Value>(&agent, url, true).unwrap_err();
            assert!(error.contains("回环"), "{url} -> {error}");
            assert!(!error.contains("example.com"), "错误不能带回远端地址");
        }
    }
}
