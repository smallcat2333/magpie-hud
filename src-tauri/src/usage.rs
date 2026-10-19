//! Magpie usage.jsonl 的尾部解析与中文文案生成。
//!
//! 输入是外部追加日志，字段可能缺失或半行未写完；输出是固定两个信息槽的结构化快照。
//! 只读取调用方传入的文本，不访问密钥文件或网络。

use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;

/// 前端一次渲染所需的完整路由快照；图与两行必须来自同一份数据。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSnapshot {
    /// 文件状态：ready、waiting、missing、warning。
    pub state: String,
    /// 顶部状态文案。
    pub watch_label: String,
    /// 是否存在可展示的命中记录。
    pub active: bool,
    /// 用于去重的 route_id 与时间组合；空表示当前不是有效新记录。
    pub identity: String,
    /// 请求来源名称。
    pub agent: String,
    /// 请求模型。
    pub request: String,
    /// 仅用于显示的 Magpie 地址。
    pub magpie_host: String,
    /// 命中账号；无记录时为明确占位。
    pub account: String,
    /// 实际服务模型。
    pub model: String,
    /// 第一行来源时间，缺失时为 `--:--:--`。
    pub line1_time: String,
    /// 第一行类型。
    pub line1_kind: String,
    /// 第一行说明。
    pub line1_text: String,
    /// 第二行来源时间。
    pub line2_time: String,
    /// 第二行类型。
    pub line2_kind: String,
    /// 第二行说明或明确占位。
    pub line2_text: String,
}

/// 从日志尾部提取最新有效记录。
///
/// `tail` 可以包含被 64KB 边界截断的首行；该行和末尾未换行半行都会跳过。
/// 返回 `None` 表示尾部没有完整且可解析的记录。
pub fn latest_record(tail: &str) -> Option<Value> {
    let mut lines = tail.lines().map(str::trim).filter(|line| !line.is_empty());
    if !tail.starts_with('{') {
        lines.next();
    }
    let complete = if tail.ends_with('\n') { lines.collect::<Vec<_>>() } else {
        let mut values = lines.collect::<Vec<_>>();
        values.pop();
        values
    };
    complete.into_iter().rev().find_map(|line| serde_json::from_str::<Value>(line).ok())
}

/// 把最新记录转成前端快照。
///
/// `warning` 为 true 时保留记录内容，但把顶部状态改为解析异常。
/// 成功记录按实机口径使用 HTTP `status == 200`；`model` 是带窗口后缀的服务模型。
pub fn snapshot_from_record(record: &Value, magpie_host: &str, warning: bool) -> RouteSnapshot {
    let account = text(record, &["providerAccount", "host"]).unwrap_or_else(|| "账号未提供".to_string());
    let model = text(record, &["model"]).unwrap_or_else(|| "服务模型未提供".to_string());
    let request = text(record, &["req"]).unwrap_or_else(|| "请求模型未提供".to_string());
    let agent = agent_label(&text(record, &["agent"]).unwrap_or_else(|| "Agent 未提供".to_string()));
    let time = record.get("t").and_then(Value::as_str).and_then(local_hms);
    let route_id = record.get("route_id").map(Value::to_string).unwrap_or_default();
    let stamp = record.get("t").and_then(Value::as_str).unwrap_or("");
    RouteSnapshot {
        state: if warning { "warning" } else { "ready" }.to_string(),
        watch_label: if warning { "解析异常 · 保留上一帧" } else { "最近一次路由" }.to_string(),
        active: true,
        identity: format!("{route_id}|{stamp}"),
        agent,
        request: request.clone(),
        magpie_host: magpie_host.to_string(),
        account: account.clone(),
        model: model.clone(),
        line1_time: time.clone().unwrap_or_else(|| "--:--:--".to_string()),
        line1_kind: "命中".to_string(),
        line1_text: hit_line(record, &account, &model, &request),
        line2_time: time.unwrap_or_else(|| "--:--:--".to_string()),
        line2_kind: "说明".to_string(),
        line2_text: "候补/其它账号未选中（占位说明，日志未提供完整排除原因）".to_string(),
    }
}

/// 生成无有效记录时的空态；`missing` 与 `waiting` 使用需求规定的不同文案。
pub fn empty_snapshot(state: &str, path: &str, magpie_host: &str) -> RouteSnapshot {
    let missing = state == "missing";
    RouteSnapshot {
        state: state.to_string(),
        watch_label: if missing { "文件未检测到" } else { "等待记录" }.to_string(),
        active: false,
        identity: String::new(),
        agent: "等待请求".to_string(),
        request: "请求模型待记录".to_string(),
        magpie_host: magpie_host.to_string(),
        account: "尚无命中账号".to_string(),
        model: "服务模型待记录".to_string(),
        line1_time: "--:--:--".to_string(),
        line1_kind: "等待".to_string(),
        line1_text: if missing { "未检测到 Magpie usage.jsonl，请确认 Magpie 已运行过" } else { "等待 Magpie 路由写入…" }.to_string(),
        line2_time: "--:--:--".to_string(),
        line2_kind: "路径".to_string(),
        line2_text: path.to_string(),
    }
}

/// 生成第一行命中说明。
///
/// 时长、token 与缓存命中率仅在字段类型和分母有效时出现；缺失值不显示为 0。
fn hit_line(record: &Value, account: &str, model: &str, request: &str) -> String {
    let mut line = format!("{account} ({model})");
    if let Some(effort) = text(record, &["effort"]) { line.push_str(&format!(" 以 {effort}")); }
    if let Some(ms) = number(record, "ms") { line.push_str(&format!(" 用 {:.1}s", ms / 1000.0)); }
    line.push_str(" 完成");
    let input = number(record, "in");
    let output = number(record, "out");
    let cache = number(record, "cache_read");
    if let (Some(input), Some(output), Some(cache)) = (input, output, cache) {
        line.push_str(&format!(" · {:.1}k token", (input + output + cache) / 1000.0));
    }
    if let Some(ttft) = number(record, "ttft_ms") { line.push_str(&format!(" · {:.1}s 后首响", ttft / 1000.0)); }
    if let (Some(input), Some(cache)) = (input, cache) {
        if input + cache > 0.0 { line.push_str(&format!(" · 缓存命中率 {:.1}%（估算）", cache / (input + cache) * 100.0)); }
    }
    line.push_str(&format!("。req={request}"));
    line
}

/// 按候选字段顺序读取非空字符串。
fn text(record: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| record.get(*key).and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_string))
}

/// 读取 JSON 数字；字符串数字属于外部脏数据，不在这里猜测转换。
fn number(record: &Value, key: &str) -> Option<f64> {
    record.get(key).and_then(Value::as_f64)
}

/// 把带偏移的 ISO 时间转换为记录自身偏移下的 `HH:mm:ss`；无偏移时按 UTC 显示。
fn local_hms(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value).ok().map(|time| time.format("%H:%M:%S").to_string())
}

/// 把已知 Agent 标识显示为可读名称，未知值原样保留。
fn agent_label(agent: &str) -> String {
    match agent { "claude-desktop" => "Claude Desktop".to_string(), other => other.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证成功记录的模型、统计公式、时间与第二行占位。
    #[test]
    fn formats_complete_record() {
        let tail = "{\"route_id\":7,\"t\":\"2026-10-19T20:33:19.5306096+08:00\",\"agent\":\"claude-desktop\",\"providerAccount\":\"relay@example.com\",\"model\":\"grok-4.7-build-fast@500k\",\"req\":\"group/smart-split\",\"effort\":\"low\",\"ms\":3800,\"ttft_ms\":3000,\"in\":1000,\"out\":2000,\"cache_read\":187000,\"status\":200}\n";
        let snapshot = snapshot_from_record(&latest_record(tail).unwrap(), "127.0.0.1:3425", false);
        assert_eq!(snapshot.line1_time, "20:33:19");
        assert_eq!(snapshot.model, "grok-4.7-build-fast@500k");
        assert!(snapshot.line1_text.contains("190.0k token"));
        assert!(snapshot.line1_text.contains("缓存命中率 99.5%（估算）"));
        assert!(snapshot.line2_text.contains("占位说明"));
    }

    /// 验证半行、坏行与截断首行都被跳过，且缺字段不显示为零。
    #[test]
    fn skips_partial_lines_and_missing_stats() {
        let tail = "uncated{\"bad\":true}\n{\"route_id\":9,\"providerAccount\":\"relay@example.com\"}\n{\"broken\":";
        let snapshot = snapshot_from_record(&latest_record(tail).unwrap(), "127.0.0.1:3425", false);
        assert_eq!(snapshot.identity, "9|");
        assert_eq!(snapshot.line1_time, "--:--:--");
        assert!(!snapshot.line1_text.contains("token"));
        assert!(latest_record("{\"only\":").is_none());
    }
}
