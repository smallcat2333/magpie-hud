//! 当日日志的会话聚合、真实请求状态与统计快照；不读取凭据或推测账号冷却时间。

use chrono::{DateTime, Local, NaiveDate};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// 固定日志槽；没有真实时间时显示明确占位。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub time: String,
    pub kind: String,
    pub text: String,
}

/// 当日统计。费用来自 Magpie 按模型价格算出的美元金额，不是日志里的 Credits；速度按有效生成时间加权。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyStats {
    pub requests: u64,
    pub tokens: u64,
    /// Magpie `/v1/magpie/usage?period=today` 的美元费用；未取到时为 None，不拿 Credits 代替。
    pub cost: Option<f64>,
    /// 已计入费用的请求数；与 unpriced 一起说明覆盖范围。
    pub priced_requests: u64,
    /// Magpie 明确未定价、因此没有计入 cost 的请求数。
    pub unpriced_requests: u64,
    pub credits: Option<f64>,
    pub credited_requests: u64,
    pub output_tokens: u64,
    pub generation_ms: f64,
    pub speed_samples: u64,
    pub tokens_per_second: Option<f64>,
}

impl DailyStats {
    /// 将一个完成请求计入统计；路由分类器调用在入口处排除，不重复累计。
    fn add(&mut self, record: &Record) {
        self.requests += 1;
        self.tokens += record.tokens;
        if let Some(credits) = record.credits {
            self.credits = Some(self.credits.unwrap_or(0.0) + credits);
            self.credited_requests += 1;
        }
        if record.success() {
            if let Some(duration) = record.generation_ms {
                self.output_tokens += record.output;
                self.generation_ms += duration;
                self.speed_samples += 1;
                self.tokens_per_second =
                    Some(self.output_tokens as f64 * 1000.0 / self.generation_ms);
            }
        }
    }
}

/// 一次已完成的外部记录，仅保留展示和聚合所需字段。
#[derive(Clone, Debug)]
struct Record {
    identity: String,
    route_id: Option<i64>,
    timestamp_ms: i64,
    sequence: u64,
    time: String,
    session: String,
    agent: String,
    request: String,
    account: String,
    model: String,
    provider: String,
    effort: String,
    status: Option<u16>,
    error: Option<String>,
    tries: u64,
    tokens: u64,
    output: u64,
    duration_ms: Option<f64>,
    ttft_ms: Option<f64>,
    cache_hit_percent: Option<f64>,
    generation_ms: Option<f64>,
    credits: Option<f64>,
}

impl Record {
    /// 明确的 2xx 才算成功；取消和缺失状态不冒充成功。
    fn success(&self) -> bool {
        self.status
            .is_some_and(|status| (200..300).contains(&status))
    }

    /// 区分调用方取消和服务失败，防止把 499 变成账号冷却。
    fn outcome(&self) -> &'static str {
        if self.success() {
            "success"
        } else if self.status == Some(499) {
            "cancelled"
        } else if self.status.is_some() {
            "failed"
        } else {
            "unknown"
        }
    }

    /// 输出带原始状态码的简短中文说明，不虚构重试间隔。
    fn detail(&self, recovered: bool) -> String {
        match self.outcome() {
            "success" if self.tries > 1 => format!("重试 {} 次后完成", self.tries - 1),
            "success" if recovered => "后续请求成功 · 已恢复".into(),
            "success" => "请求已完成".into(),
            "cancelled" => "请求已取消 · 不是账号冷却".into(),
            "failed" => format!("失败 HTTP {} · 恢复时间未记录", self.status.unwrap()),
            _ => "日志未提供请求状态".into(),
        }
    }

    /// 第一行展示请求本身，包含失败原因；第二行在会话快照中补充恢复依据。
    fn note(&self, recovered: bool) -> Note {
        let kind = match self.outcome() {
            "success" => {
                if recovered || self.tries > 1 {
                    "恢复"
                } else {
                    "完成"
                }
            }
            "cancelled" => "取消",
            "failed" => "失败",
            _ => "状态",
        };
        let mut text = format!(
            "{} ({}) · {} · {}",
            self.account,
            self.model,
            self.effort,
            self.detail(recovered)
        );
        if let Some(duration) = self.duration_ms {
            text.push_str(&format!(" · {:.1}s", duration / 1000.0));
        }
        if self.tokens > 0 {
            text.push_str(&format!(" · {:.1}k token", self.tokens as f64 / 1000.0));
        }
        if let Some(ttft) = self.ttft_ms {
            text.push_str(&format!(" · {:.1}s 后首响", ttft / 1000.0));
        }
        if let Some(hit) = self.cache_hit_percent {
            text.push_str(&format!(" · 缓存命中率 {:.1}%（估算）", hit));
        }
        if let Some(error) = &self.error {
            text.push_str(&format!(" · {error}"));
        }
        Note {
            time: self.time.clone(),
            kind: kind.into(),
            text,
        }
    }
}

/// 每个真实账号的两行路由块；恢复只根据已记录失败后的成功或 tries 判定。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteCard {
    pub id: String,
    pub session_id: String,
    pub session_label: String,
    pub account: String,
    pub model: String,
    pub provider: String,
    pub effort: String,
    pub state: String,
    pub detail: String,
    pub error: Option<String>,
    pub time: String,
    pub current: bool,
    pub route_id: Option<i64>,
    pub tokens: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub note: Note,
}

/// 会话的路由块、最新两行说明和当日统计，前端可无需再次读文件直接切换。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: String,
    pub label: String,
    pub timestamp_ms: i64,
    pub latest_sequence: u64,
    pub agent: String,
    pub request: String,
    pub routes: Vec<RouteCard>,
    pub notes: [Note; 2],
    pub daily: DailyStats,
}

/// 图、会话列表、全局统计使用同一次原子快照，revision 控制前端去重。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSnapshot {
    pub revision: u64,
    pub date: String,
    pub state: String,
    pub watch_label: String,
    pub magpie_host: String,
    pub sessions: Vec<SessionView>,
    pub daily: DailyStats,
    pub quotas: Vec<crate::telemetry::Quota>,
    pub quota_error: Option<String>,
    pub cost_error: Option<String>,
    pub notes: [Note; 2],
    pub error: Option<String>,
}

/// 内存中的当日完成请求；启动流式读取历史，之后只追加新完整行。
pub struct UsageLedger {
    day: NaiveDate,
    records: Vec<Record>,
    seen: HashSet<String>,
    pub revision: u64,
    malformed: u64,
}

impl UsageLedger {
    /// 创建指定本地日期账本，支持测试跨日重置。
    pub fn new(day: NaiveDate) -> Self {
        Self {
            day,
            records: Vec::new(),
            seen: HashSet::new(),
            revision: 0,
            malformed: 0,
        }
    }

    /// 本地日期变更时清空当日数据；返回是否发生变更。
    pub fn change_day(&mut self, day: NaiveDate) -> bool {
        if self.day == day {
            return false;
        }
        let revision = self.revision + 1;
        *self = Self::new(day);
        self.revision = revision;
        true
    }

    /// 文件轮转后从头重建，避免旧文件累计值与新文件重复。
    pub fn reset(&mut self) {
        let revision = self.revision + 1;
        *self = Self::new(self.day);
        self.revision = revision;
    }

    /// 处理一行完整 JSON；外部坏行保留已有结果，分类器调用不算完成请求。
    pub fn ingest(&mut self, line: &[u8]) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            self.malformed += 1;
            self.revision += 1;
            return;
        };
        if !value.is_object() {
            self.malformed += 1;
            self.revision += 1;
            return;
        }
        if value.get("kind").and_then(Value::as_str) == Some("routing") {
            return;
        }
        let timestamp =
            string(&value, "t").and_then(|text| DateTime::parse_from_rfc3339(text).ok());
        // 无效日期不能正确归入当日，绝不将其计入今日金额或 token。
        let Some(timestamp) = timestamp else {
            self.malformed += 1;
            self.revision += 1;
            return;
        };
        let local = timestamp.with_timezone(&Local);
        if local.date_naive() != self.day {
            return;
        }
        let identity = format!(
            "{}|{}|{}|{}|{}",
            value
                .get("route_id")
                .map(Value::to_string)
                .unwrap_or_default(),
            timestamp.timestamp_millis(),
            string(&value, "providerAccount").unwrap_or(""),
            value
                .get("status")
                .map(Value::to_string)
                .unwrap_or_default(),
            string(&value, "rid").unwrap_or("")
        );
        if !self.seen.insert(identity.clone()) {
            return;
        }
        let number = |key| value.get(key).and_then(Value::as_u64).unwrap_or(0);
        let duration = finite_number(&value, "ms");
        let generation = duration
            .zip(finite_number(&value, "ttft_ms"))
            .map(|(ms, ttft)| ms - ttft)
            .filter(|ms| *ms > 0.0);
        let agent = string(&value, "agent").unwrap_or("Agent 未提供");
        self.records.push(Record {
            identity,
            route_id: value.get("route_id").and_then(Value::as_i64),
            timestamp_ms: timestamp.timestamp_millis(),
            sequence: self.records.len() as u64,
            time: local.format("%H:%M:%S").to_string(),
            session: string(&value, "session")
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .into(),
            agent: if agent == "claude-desktop" {
                "Claude Desktop".into()
            } else {
                agent.into()
            },
            request: string(&value, "req").unwrap_or("请求模型未提供").into(),
            account: string(&value, "providerAccount")
                .filter(|s| !s.is_empty())
                .or_else(|| string(&value, "host"))
                .unwrap_or("账号未提供")
                .into(),
            model: string(&value, "model")
                .or_else(|| string(&value, "served"))
                .unwrap_or("服务模型未提供")
                .into(),
            provider: string(&value, "provider").unwrap_or("").into(),
            effort: string(&value, "effort").unwrap_or("强度未提供").into(),
            status: value
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|s| u16::try_from(s).ok()),
            error: string(&value, "err").map(str::to_owned),
            tries: number("tries").max(1),
            tokens: number("in")
                .saturating_add(number("out"))
                .saturating_add(number("cache_read")),
            output: number("out"),
            duration_ms: duration,
            ttft_ms: finite_number(&value, "ttft_ms"),
            cache_hit_percent: finite_number(&value, "in").zip(finite_number(&value, "cache_read")).and_then(|(input, cache)| (input + cache > 0.0).then(|| cache / (input + cache) * 100.0)),
            generation_ms: generation,
            credits: finite_number(&value, "credits"),
        });
        self.malformed = 0;
        self.revision += 1;
    }

    /// 生成排序稳定的当日会话视图；仅展示会话最新请求和仍有失败依据的账号。
    pub fn snapshot(&self, host: &str, file_state: &str, error: Option<String>) -> RouteSnapshot {
        let mut grouped: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
        let mut daily = DailyStats::default();
        for record in &self.records {
            daily.add(record);
            grouped.entry(&record.session).or_default().push(record);
        }
        let mut sessions = Vec::new();
        for (session, records) in grouped {
            // t 是请求开始时间；完成顺序以完整日志行的追加顺序为准。
            let latest = *records.last().unwrap();
            let label: String = if session.is_empty() {
                "未标记会话".into()
            } else {
                "未命名会话".into()
            };
            let mut account_history: HashMap<&str, Vec<&Record>> = HashMap::new();
            let mut stats = DailyStats::default();
            for record in &records {
                stats.add(record);
                account_history
                    .entry(&record.account)
                    .or_default()
                    .push(record);
            }
            let mut routes = Vec::new();
            for history in account_history.values() {
                let record = *history.last().unwrap();
                let current = record.identity == latest.identity;
                if !current && record.outcome() != "failed" {
                    continue;
                }
                let recovered = record.success()
                    && history
                        .iter()
                        .rev()
                        .skip(1)
                        .find(|other| other.status != Some(499))
                        .is_some_and(|previous| previous.outcome() == "failed");
                let state = if record.success() && (recovered || record.tries > 1) {
                    "recovered"
                } else {
                    record.outcome()
                };
                routes.push((
                    record.sequence,
                    RouteCard {
                        id: record.identity.clone(),
                        session_id: session.into(),
                        session_label: label.clone(),
                        account: record.account.clone(),
                        model: record.model.clone(),
                        provider: record.provider.clone(),
                        effort: record.effort.clone(),
                        state: state.into(),
                        detail: record.detail(recovered),
                        error: record.error.clone(),
                        time: record.time.clone(),
                        current,
                        route_id: record.route_id,
                        tokens: Some(record.tokens),
                        tokens_per_second: record.generation_ms.filter(|_| record.success())
                            .map(|ms| record.output as f64 * 1000.0 / ms),
                        note: record.note(recovered),
                    },
                ));
            }
            routes.sort_by(|a, b| b.0.cmp(&a.0));
            let routes: Vec<_> = routes.into_iter().map(|(_, route)| route).collect();
            let latest_card = routes.iter().find(|route| route.current).unwrap();
            let second = if let Some(failed) = routes
                .iter()
                .find(|route| !route.current && route.state == "failed")
            {
                failed.note.clone()
            } else {
                Note {
                    time: latest.time.clone(),
                    kind: "会话".into(),
                    text: format!(
                        "{label} · {} · {} · {}",
                        latest.model,
                        latest.effort,
                        if latest.tries > 1 {
                            format!("尝试 {} 次；此前失败账号和冷却时长未记录", latest.tries)
                        } else {
                            "日志不包含其它账号的排除原因或恢复倒计时".into()
                        }
                    ),
                }
            };
            sessions.push(SessionView {
                id: session.into(),
                label,
                timestamp_ms: latest.timestamp_ms,
                latest_sequence: latest.sequence,
                agent: latest.agent.clone(),
                request: latest.request.clone(),
                notes: [latest_card.note.clone(), second],
                routes,
                daily: stats,
            });
        }
        sessions.sort_by(|a, b| b.latest_sequence.cmp(&a.latest_sequence).then(a.id.cmp(&b.id)));
        let mut notes = sessions
            .first()
            .map(|s| s.notes.clone())
            .unwrap_or_else(|| {
                [
                    Note {
                        time: "--:--:--".into(),
                        kind: "等待".into(),
                        text: "等待 Magpie 路由写入…".into(),
                    },
                    Note {
                        time: "--:--:--".into(),
                        kind: "来源".into(),
                        text: "只读 usage.jsonl".into(),
                    },
                ]
            });
        if file_state == "missing" {
            notes[0].kind = "文件".into();
            notes[0].text = "未检测到 Magpie usage.jsonl，请确认 Magpie 已运行过".into();
        }
        let warning = error.is_some() || self.malformed > 0;
        let state = if warning {
            "warning"
        } else if file_state == "missing" {
            "missing"
        } else if sessions.is_empty() {
            "waiting"
        } else {
            "ready"
        };
        RouteSnapshot {
            revision: self.revision,
            date: self.day.to_string(),
            state: state.into(),
            watch_label: match state {
                "warning" => "读取异常 · 保留有效数据",
                "missing" => "文件未检测到",
                "waiting" => "等待今日记录",
                _ => "当日记录 · 只读监视",
            }
            .into(),
            magpie_host: host.into(),
            sessions,
            daily,
            quotas: Vec::new(),
            quota_error: None,
            cost_error: None,
            notes,
            error: error
                .or_else(|| (self.malformed > 0).then(|| "日志中有无效行，已保留有效记录".into())),
        }
    }
}

/// 取非空字符串；仅对外部字段做必要检查。
fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}
/// 金额和耗时必须是非负有限数字，缺失值保持未知。
fn finite_number(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造固定日期样本，避免测试依赖机器时区和当前日期。
    fn fixture(id: u64, session: &str, status: u16) -> Value {
        json!({"route_id":id,"t":format!("2026-10-20T00:00:{:02}+00:00",id),"session":session,"providerAccount":"one@example.com","agent":"claude-desktop","model":"gpt-test","effort":"high","status":status,"in":100,"out":100,"cache_read":800,"ms":3000,"ttft_ms":1000})
    }
    /// 将样本加入与其本地时间相同日期的账本。
    fn ledger() -> UsageLedger {
        UsageLedger::new(
            DateTime::parse_from_rfc3339("2026-10-20T00:00:00Z")
                .unwrap()
                .with_timezone(&Local)
                .date_naive(),
        )
    }
    /// 检查当日分组、重复跳过、路由分类器排除和加权速度。
    #[test]
    fn aggregates_sessions_without_double_counting() {
        let mut ledger = ledger();
        let first = fixture(1, "alpha", 200);
        ledger.ingest(first.to_string().as_bytes());
        ledger.ingest(first.to_string().as_bytes());
        let mut second = fixture(2, "beta", 200);
        second["credits"] = json!(1.25);
        ledger.ingest(second.to_string().as_bytes());
        second["kind"] = json!("routing");
        ledger.ingest(second.to_string().as_bytes());
        let view = ledger.snapshot("localhost", "ready", None);
        assert_eq!(view.daily.requests, 2);
        assert_eq!(view.daily.tokens, 2000);
        assert_eq!(view.daily.credits, Some(1.25));
        assert_eq!(view.daily.credited_requests, 1);
        assert_eq!(view.daily.tokens_per_second, Some(50.0));
        assert_eq!(view.sessions.len(), 2);
    }
    /// 失败之后成功才显示恢复，取消不表示账号冷却，重试次数仅陈述日志事实。
    #[test]
    fn distinguishes_failure_cancel_and_recovery() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 503).to_string().as_bytes());
        assert_eq!(
            ledger.snapshot("h", "ready", None).sessions[0].routes[0].state,
            "failed"
        );
        ledger.ingest(fixture(2, "alpha", 200).to_string().as_bytes());
        assert_eq!(
            ledger.snapshot("h", "ready", None).sessions[0].routes[0].state,
            "recovered"
        );
        ledger.ingest(fixture(3, "beta", 499).to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions[0].routes[0].state, "cancelled");
        assert!(!view.sessions[0].notes[0].text.contains("完成"));
    }
    /// 缺费用和生成时间保持未知；无 session 的记录明确归入未标记组。
    #[test]
    fn missing_fields_and_out_of_order_records_remain_truthful() {
        let mut ledger = ledger();
        let mut latest = fixture(3, "", 200);
        latest.as_object_mut().unwrap().remove("ttft_ms");
        ledger.ingest(fixture(1, "", 503).to_string().as_bytes());
        ledger.ingest(latest.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions[0].label, "未标记会话");
        assert_eq!(view.sessions[0].routes[0].state, "recovered");
        assert_eq!(view.daily.credits, None);
        assert_eq!(view.daily.tokens_per_second, None);
        assert_eq!(view.daily.requests, 2);
    }

    /// 较旧失败账号仍可作为对照，但不会凭空创建冷却秒数。
    #[test]
    fn keeps_failed_account_when_another_account_succeeds() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 503).to_string().as_bytes());
        let mut other = fixture(2, "alpha", 200);
        other["providerAccount"] = json!("another@example.com");
        other["tries"] = json!(2);
        ledger.ingest(other.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        let routes = &view.sessions[0].routes;
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].state, "recovered");
        assert_eq!(routes[1].state, "failed");
        assert!(routes[1].detail.contains("恢复时间未记录"));
        assert_eq!(view.sessions[0].notes[1].kind, "失败");
    }

    /// 请求开始时间可逆序，最新与恢复必须按文件完成顺序判断。
    #[test]
    fn completion_order_wins_over_start_timestamp() {
        let mut ledger = ledger();
        ledger.ingest(fixture(10, "alpha", 503).to_string().as_bytes());
        ledger.ingest(fixture(2, "alpha", 200).to_string().as_bytes());
        ledger.ingest(fixture(1, "beta", 200).to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions[0].id, "beta");
        assert_eq!(view.sessions[1].routes[0].state, "recovered");
    }

    /// 跨日清空且坏行不清掉有效画面。
    #[test]
    fn resets_at_midnight_and_keeps_valid_on_bad_line() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 200).to_string().as_bytes());
        ledger.ingest(b"{");
        assert_eq!(ledger.snapshot("h", "ready", None).state, "warning");
        assert_eq!(ledger.snapshot("h", "ready", None).sessions.len(), 1);
        assert!(ledger.change_day(ledger.day.succ_opt().unwrap()));
        assert_eq!(ledger.snapshot("h", "ready", None).daily.requests, 0);
    }

    /// 文案公式只使用真实字段；缺字段、无效分母和非有限耗时不冒充 0。
    #[test]
    fn formats_time_tokens_and_cache_without_inventing_values() {
        let mut ledger = ledger();
        let mut value = fixture(8, "alpha", 200);
        value["req"] = json!("group/smart-split");
        value["cache_write"] = json!(50);
        ledger.ingest(value.to_string().as_bytes());
        let note = &ledger.snapshot("h", "ready", None).sessions[0].notes[0];
        assert_eq!(note.time.len(), 8);
        assert!(note.text.contains("1.0k token"));
        assert!(note.text.contains("缓存命中率 88.9%（估算）"));
        assert!(note.text.contains("3.0s"));
        assert!(note.text.contains("1.0s 后首响"));
        assert!(!note.text.contains("cache_write") && !note.text.contains("50"));

        let mut missing = fixture(9, "beta", 200);
        missing.as_object_mut().unwrap().remove("ms");
        missing["ttft_ms"] = json!(f64::NAN);
        missing["in"] = json!(0);
        missing["out"] = json!(0);
        missing["cache_read"] = json!(0);
        ledger.ingest(missing.to_string().as_bytes());
        let incomplete = &ledger.snapshot("h", "ready", None).sessions[0].notes[0];
        assert!(!incomplete.text.contains("token"));
        assert!(!incomplete.text.contains("缓存命中率"));
        assert!(!incomplete.text.contains("NaN"));
        assert_eq!(ledger.snapshot("h", "ready", None).daily.tokens_per_second, Some(50.0));
    }

    /// 非对象、空文件和未换行记录都不形成路由，也不伪造时间。
    #[test]
    fn ignores_empty_non_object_and_incomplete_records() {
        let mut ledger = ledger();
        ledger.ingest(b"");
        ledger.ingest(b"[]");
        ledger.ingest(br#"{"route_id":1,"status":200}"#);
        let view = ledger.snapshot("h", "ready", None);
        assert!(view.sessions.is_empty());
        assert_eq!(view.notes[0].time, "--:--:--");
        assert_eq!(view.daily.requests, 0);
        assert_eq!(view.state, "warning");
    }
}
