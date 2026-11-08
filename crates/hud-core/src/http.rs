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
pub fn read_json<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    classify: bool,
) -> Result<T, String> {
    if !classify {
        return crate::live::read_json(agent, url);
    }
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(error) => return Err(classify_ureq(error)),
    };
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| "响应格式无效".to_string())?;
    serde_json::from_str(&body).map_err(|_| "响应格式无效".to_string())
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
}
