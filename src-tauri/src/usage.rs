//! 当日日志的会话聚合、真实请求状态与统计快照；不读取凭据或推测账号冷却时间。

use chrono::{DateTime, Local, NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
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

/// 本地当日的一个小时段，金额未知与真实零值分别表达。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyStats {
    pub tokens: u64,
    pub cost: Option<f64>,
    /// 该小时只记录了部分观测区间，不能当作完整小时账单。
    #[serde(default)]
    pub cost_partial: bool,
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
    /// 已完成请求的缓存命中率，cache_read / (in + cache_read)，0 到 1。分母为 0 时未知。
    pub cache_hit_rate: Option<f64>,
    /// 加权分子与分母，仅用于聚合，不发给前端。
    #[serde(skip)]
    cache_read_tokens: u64,
    #[serde(skip)]
    cache_denominator_tokens: u64,
    /// Magpie 费用除以已定价请求数。全部未定价、费用未知或没有已定价请求时为 None。
    pub cost_per_call: Option<f64>,
    /// 按本地请求开始时间分为 24 段，与当日 Token 采用同一口径。
    pub hourly: [HourlyStats; 24],
}

impl DailyStats {
    /// 将一个完成请求计入统计；路由分类器调用在入口处排除，不重复累计。
    fn add(&mut self, record: &Record) {
        self.requests += 1;
        self.tokens += record.tokens;
        self.hourly[record.hour].tokens = self.hourly[record.hour].tokens.saturating_add(record.tokens);
        let denominator = record.input.saturating_add(record.cache_read);
        if denominator > 0 {
            self.cache_read_tokens = self.cache_read_tokens.saturating_add(record.cache_read);
            self.cache_denominator_tokens = self.cache_denominator_tokens.saturating_add(denominator);
            self.cache_hit_rate = Some(self.cache_read_tokens as f64 / self.cache_denominator_tokens as f64);
        }
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
    /// 子 Agent 身份。根会话请求为空；不读取正文，只用于归属与上轮指标。
    subagent: String,
    timestamp_ms: i64,
    hour: usize,
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
    input: u64,
    cache_read: u64,
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

    /// 日志的 t 是请求开始时间，ms 是本请求耗时；完成时刻为二者之和。
    /// 不随轮询变化，刷新不会给已结束的卡续期。
    fn updated_at(&self) -> Option<String> {
        // 只用有限的非负耗时；超大值饱和转换，不截断成 24 小时。
        let duration = self.duration_ms
            .filter(|ms| ms.is_finite() && *ms >= 0.0)
            .map(|ms| if ms >= i64::MAX as f64 { i64::MAX } else { ms as i64 })
            .unwrap_or(0);
        let finished = self.timestamp_ms.saturating_add(duration);
        DateTime::from_timestamp_millis(finished).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
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
    #[serde(default)]
    pub metrics_are_previous: bool,
    /// 子 Agent 请求身份。根会话请求为 None；前端用它挂到父会话下方。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    /// 不含正文的通用标签。缺省时前端用「子 Agent」加短 ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_label: Option<String>,
    /// 实际命中的组链，从父组到子组，不含最终模型。只有实时 trace 的选中尝试
    /// 能确定；日志与无法唯一判定时为空，不用候选全集拼链。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_groups: Vec<String>,
    /// 最近真实请求活动时刻（RFC3339 UTC）。活跃卡随进度推进；完成卡固定为
    /// 真实完成时刻，刷新不会续期。旧快照与无法确定时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
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
    /// 当前正在跑的子 Agent 请求。已完成的子请求不在这里，也不进 routes。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<RouteCard>,
    /// 每个子 Agent 最近一次已完成请求行。前端只在当前选中会话里与进行中行按子 ID 合并。
    #[serde(default, rename = "completedSubagents", skip_serializing_if = "Vec::is_empty")]
    pub completed_subagents: Vec<RouteCard>,
    /// 每个子 Agent 最近一次已完成记录，供进行中行引用上轮指标；不发给前端。
    #[serde(skip)]
    pub subagent_history: HashMap<String, RouteCard>,
    /// 根请求与子请求按账号各留最新一条，供额度匹配。统计仍只来自 daily。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quota_routes: Vec<RouteCard>,
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
        // 无会话身份的记录无法准确归属会话，因此不进入账本与 HUD。
        if string(&value, "session").unwrap_or("").is_empty() {
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
            subagent: string(&value, "subagent").unwrap_or("").into(),
            timestamp_ms: timestamp.timestamp_millis(),
            hour: local.hour() as usize,
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
            input: number("in"),
            cache_read: number("cache_read"),
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
            let latest = *records.iter().max_by_key(|record| record.sequence).unwrap();
            let root_records: Vec<_> = records.iter().copied().filter(|record| record.subagent.is_empty()).collect();
            let latest_root = root_records.iter().copied().max_by_key(|record| record.sequence);
            let label: String = if session.is_empty() {
                "未标记会话".into()
            } else {
                "未命名会话".into()
            };
            let mut account_history: HashMap<(String, &str), Vec<&Record>> = HashMap::new();
            let mut child_history: HashMap<&str, Vec<&Record>> = HashMap::new();
            let mut stats = DailyStats::default();
            for record in &records {
                stats.add(record);
                let key = (record.provider.to_ascii_lowercase(), record.account.as_str());
                account_history.entry(key).or_default().push(record);
                if !record.subagent.is_empty() {
                    child_history
                        .entry(&record.subagent)
                        .or_default()
                        .push(record);
                }
            }
            let card_for = |record: &Record, current: bool, history: &[&Record]| {
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
                    tokens_per_second: record
                        .generation_ms
                        .filter(|_| record.success())
                        .map(|ms| record.output as f64 * 1000.0 / ms),
                    metrics_are_previous: false,
                    subagent_id: (!record.subagent.is_empty()).then(|| record.subagent.clone()),
                    subagent_label: (!record.subagent.is_empty()).then(|| crate::live::fallback_child_label(&record.subagent)),
                    route_groups: Vec::new(),
                    updated_at: record.updated_at(),
                    note: record.note(recovered),
                }
            };
            let mut routes = Vec::new();
            if let Some(latest_root) = latest_root {
                // 较新以文件追加顺序为准。current 永远保留；非 current 只保留比最新成功根更晚的
                // 失败/取消/未知根卡片，跨账号与供应商。子请求不参与；失败后成功会清掉更早留档。
                let latest_success = root_records
                    .iter()
                    .filter(|record| record.success())
                    .map(|record| record.sequence)
                    .max();
                for history in account_history.values() {
                    let Some(record) = history.iter().rev().find(|record| record.subagent.is_empty()) else {
                        continue;
                    };
                    let current = record.identity == latest_root.identity;
                    let retained = current
                        || (record.outcome() != "success"
                            && latest_success.is_none_or(|sequence| record.sequence > sequence));
                    if !retained {
                        continue;
                    }
                    let history_refs: Vec<_> = history.iter().copied().filter(|item| item.subagent.is_empty()).collect();
                    routes.push((record.sequence, card_for(record, current, &history_refs)));
                }
            }
            routes.sort_by(|a, b| b.0.cmp(&a.0));
            let routes: Vec<_> = routes.into_iter().map(|(_, route)| route).collect();
            let mut subagent_history = HashMap::new();
            let mut completed_subagents = Vec::new();
            for (child, history) in &child_history {
                let record = *history.last().unwrap();
                let history_refs: Vec<_> = history.iter().copied().collect();
                let card = card_for(record, false, &history_refs);
                completed_subagents.push((record.sequence, card.clone()));
                subagent_history.insert((*child).to_owned(), card);
            }
            completed_subagents.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
            let completed_subagents: Vec<_> = completed_subagents.into_iter().map(|(_, card)| card).collect();
            let mut quota_by_account: HashMap<(String, String), (u64, RouteCard)> = HashMap::new();
            for record in &records {
                let key = (record.provider.to_ascii_lowercase(), record.account.clone());
                if quota_by_account.get(&key).is_some_and(|(sequence, _)| *sequence >= record.sequence) {
                    continue;
                }
                let history = account_history.get(&(record.provider.to_ascii_lowercase(), record.account.as_str()));
                let history_refs: Vec<_> = history.into_iter().flatten().copied()
                    .filter(|item| item.subagent.is_empty() == record.subagent.is_empty())
                    .collect();
                quota_by_account.insert(key, (record.sequence, card_for(record, false, &history_refs)));
            }
            let mut quota_routes: Vec<_> = quota_by_account.into_values().collect();
            quota_routes.sort_by(|a, b| b.0.cmp(&a.0));
            let quota_routes: Vec<_> = quota_routes.into_iter().map(|(_, route)| route).collect();
            let (anchor, anchor_card) = if let Some(latest_root) = latest_root {
                let card = routes.iter().find(|route| route.current).expect("根请求当前卡片");
                (latest_root, card.note.clone())
            } else {
                (
                    latest,
                    Note {
                        time: latest.time.clone(),
                        kind: "子 Agent".into(),
                        text: "子 Agent 请求中".into(),
                    },
                )
            };
            let second = if let Some(failed) = routes
                .iter()
                .find(|route| !route.current && route.state == "failed")
            {
                failed.note.clone()
            } else if latest_root.is_some() {
                Note {
                    time: anchor.time.clone(),
                    kind: "会话".into(),
                    text: format!(
                        "{label} · {} · {} · {}",
                        anchor.model,
                        anchor.effort,
                        if anchor.tries > 1 {
                            format!("尝试 {} 次；此前失败账号和冷却时长未记录", anchor.tries)
                        } else {
                            "日志不包含其它账号的排除原因或恢复倒计时".into()
                        }
                    ),
                }
            } else {
                Note {
                    time: anchor.time.clone(),
                    kind: "来源".into(),
                    text: "该会话只有子 Agent 记录，父会话尚无已完成请求".into(),
                }
            };
            sessions.push(SessionView {
                id: session.into(),
                label,
                timestamp_ms: latest.timestamp_ms,
                latest_sequence: latest.sequence,
                agent: anchor.agent.clone(),
                request: if latest_root.is_some() { anchor.request.clone() } else { String::new() },
                notes: [anchor_card, second],
                routes,
                subagents: Vec::new(),
                completed_subagents,
                subagent_history,
                quota_routes,
                daily: stats,
            });
        }
        sessions.sort_by(|a, b| b.latest_sequence.cmp(&a.latest_sequence).then(a.id.cmp(&b.id)));
        // 全局 TPS 只看最近 10 个会话，不把当天更早会话的速度混入。
        daily.output_tokens = 0;
        daily.generation_ms = 0.0;
        daily.speed_samples = 0;
        daily.tokens_per_second = None;
        for session in sessions.iter().take(10) {
            daily.output_tokens += session.daily.output_tokens;
            daily.generation_ms += session.daily.generation_ms;
            daily.speed_samples += session.daily.speed_samples;
        }
        if daily.generation_ms > 0.0 {
            daily.tokens_per_second = Some(daily.output_tokens as f64 * 1000.0 / daily.generation_ms);
        }
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

/// 读取非空字符串字段；缺失和空值都保持未知。
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
    /// 小时桶与日总量一致；同一条完成记录去重，子请求计入所属会话。
    #[test]
    fn hourly_tokens_follow_local_time_and_session_scope() {
        let mut ledger = ledger();
        let mut first = fixture(1, "one", 200);
        let stamp = DateTime::parse_from_rfc3339(first["t"].as_str().unwrap()).unwrap().with_timezone(&Local);
        let hour = stamp.hour() as usize;
        first["cache_read"] = json!(100);
        ledger.ingest(first.to_string().as_bytes());
        ledger.ingest(first.to_string().as_bytes());
        let mut child = fixture(2, "one", 200);
        child["subagent"] = json!("child-one");
        ledger.ingest(child.to_string().as_bytes());
        ledger.ingest(fixture(3, "two", 200).to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.daily.hourly[hour].tokens, 2300);
        assert_eq!(view.daily.hourly.iter().map(|bucket| bucket.tokens).sum::<u64>(), view.daily.tokens);
        let one = view.sessions.iter().find(|session| session.id == "one").unwrap();
        assert_eq!(one.daily.hourly[hour].tokens, 1300);
        assert_eq!(view.daily.hourly.iter().filter(|bucket| bucket.tokens > 0).count(), 1);
        assert!(view.daily.hourly.iter().all(|bucket| bucket.cost.is_none()));
        ledger.change_day(ledger.day + chrono::Duration::days(1));
        assert!(ledger.snapshot("h", "ready", None).daily.hourly.iter().all(|bucket| bucket.tokens == 0));
    }

    /// 全局速度只纳入最近 10 个会话；第 11 个更早会话不影响结果。
    #[test]
    fn global_speed_uses_ten_latest_sessions() {
        let mut ledger = ledger();
        for id in 1..=11 {
            let mut value = fixture(id, &format!("session-{id:02}"), 200);
            value["out"] = json!(if id == 1 { 1000 } else { 100 });
            ledger.ingest(value.to_string().as_bytes());
        }
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions.len(), 11);
        assert_eq!(view.daily.speed_samples, 10);
        assert_eq!(view.daily.tokens_per_second, Some(50.0));
        assert_eq!(view.sessions[10].daily.tokens_per_second, Some(500.0));
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
        let recovered = ledger.snapshot("h", "ready", None);
        assert_eq!(recovered.sessions[0].routes.len(), 1);
        assert_eq!(recovered.sessions[0].routes[0].state, "recovered");
        ledger.ingest(fixture(3, "beta", 499).to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions[0].routes[0].state, "cancelled");
        assert!(!view.sessions[0].notes[0].text.contains("完成"));
    }
    /// 缺费用和生成时间保持未知；无 session 的记录彻底排除，保留已标记的短记录。
    #[test]
    fn missing_fields_and_out_of_order_records_remain_truthful() {
        let mut ledger = ledger();
        let mut anonymous = fixture(1, "", 200);
        anonymous.as_object_mut().unwrap().remove("ttft_ms");
        anonymous["out"] = json!(531);
        ledger.ingest(anonymous.to_string().as_bytes());
        assert!(ledger.snapshot("h", "ready", None).sessions.is_empty());

        let mut identified = fixture(2, "alpha", 200);
        identified.as_object_mut().unwrap().remove("ttft_ms");
        ledger.ingest(fixture(3, "alpha", 503).to_string().as_bytes());
        ledger.ingest(identified.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions[0].label, "未命名会话");
        assert_eq!(view.sessions[0].routes[0].state, "recovered");
        assert_eq!(view.daily.credits, None);
        assert_eq!(view.daily.tokens_per_second, None);
        assert_eq!(view.daily.requests, 2);
    }

    /// 所有无会话身份的记录都过滤，不受输出量影响；有身份短记录照常统计。
    #[test]
    fn drops_all_sessionless_records_and_keeps_identified_short_output() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "", 200).to_string().as_bytes());
        let mut long = fixture(2, "", 200);
        long["out"] = json!(531);
        ledger.ingest(long.to_string().as_bytes());
        let mut identified = fixture(3, "alpha", 200);
        identified["out"] = json!(18);
        ledger.ingest(identified.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].id, "alpha");
        assert_eq!(view.daily.requests, 1);
    }

    /// 子 Agent 只计入会话与全局统计一次，不占据父会话的完成卡片。
    #[test]
    fn completed_children_stay_out_of_root_routes_and_count_once() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 200).to_string().as_bytes());
        let mut child = fixture(2, "alpha", 200);
        child["subagent"] = json!("agent-child-1");
        child["providerAccount"] = json!("child@example.com");
        child["out"] = json!(200);
        ledger.ingest(child.to_string().as_bytes());
        let mut only_child = fixture(3, "beta", 503);
        only_child["subagent"] = json!("agent-only");
        ledger.ingest(only_child.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        assert_eq!(view.daily.requests, 3);
        let alpha = view.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.routes.len(), 1);
        assert!(alpha.routes[0].subagent_id.is_none());
        assert!(alpha.routes[0].current);
        assert!(alpha.subagents.is_empty());
        assert_eq!(alpha.subagent_history["agent-child-1"].tokens, Some(1_100));
        assert_eq!(alpha.daily.requests, 2);
        let accounts: Vec<_> = alpha.quota_routes.iter().map(|card| card.account.as_str()).collect();
        assert!(accounts.contains(&"one@example.com") && accounts.contains(&"child@example.com"));
        assert_eq!(alpha.completed_subagents.len(), 1);
        assert_eq!(alpha.completed_subagents[0].subagent_id.as_deref(), Some("agent-child-1"));
        assert!(!alpha.completed_subagents[0].current);
        assert_eq!(alpha.completed_subagents[0].subagent_label.as_deref(), Some("子 Agent agent-ch"));
        let mut same_name = fixture(4, "alpha", 200);
        same_name["subagent"] = json!("agent-child-2");
        same_name["provider"] = json!("other");
        same_name["providerAccount"] = json!("one@example.com");
        ledger.ingest(same_name.to_string().as_bytes());
        let split = ledger.snapshot("h", "ready", None);
        let alpha = split.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.quota_routes.iter().filter(|card| card.account == "one@example.com").count(), 2);
        let mut other_root = fixture(5, "alpha", 200);
        other_root["provider"] = json!("ProviderB");
        other_root["providerAccount"] = json!("shared@example.com");
        ledger.ingest(other_root.to_string().as_bytes());
        let mut first_root = fixture(6, "alpha", 200);
        first_root["provider"] = json!("providerA");
        first_root["providerAccount"] = json!("shared@example.com");
        ledger.ingest(first_root.to_string().as_bytes());
        let roots = ledger.snapshot("h", "ready", None);
        let shared: Vec<_> = roots.sessions.iter().find(|session| session.id == "alpha").unwrap()
            .quota_routes.iter().filter(|card| card.account == "shared@example.com")
            .map(|card| card.provider.to_ascii_lowercase()).collect();
        assert!(shared.contains(&"providera".into()) && shared.contains(&"providerb".into()));
        let beta = view.sessions.iter().find(|session| session.id == "beta").unwrap();
        assert!(beta.routes.is_empty());
        assert_eq!(beta.notes[0].text, "子 Agent 请求中");
        assert_eq!(beta.quota_routes.len(), 1);
        assert_eq!(beta.daily.requests, 1);
    }

    /// 每个子 Agent 只暴露最新完成行；缓存命中率按全部完成记录加权，分母为零保持未知。
    #[test]
    fn completed_children_are_unique_and_cache_rate_is_weighted() {
        let mut ledger = ledger();
        let mut first = fixture(1, "11111111-1111-1111-1111-111111111111", 200);
        first["subagent"] = json!("child-one");
        first["in"] = json!(100);
        first["cache_read"] = json!(300);
        ledger.ingest(first.to_string().as_bytes());
        let mut newer = fixture(2, "11111111-1111-1111-1111-111111111111", 503);
        newer["subagent"] = json!("child-one");
        newer["in"] = json!(0);
        newer["out"] = json!(0);
        newer["cache_read"] = json!(0);
        ledger.ingest(newer.to_string().as_bytes());
        let mut other = fixture(3, "11111111-1111-1111-1111-111111111111", 200);
        other["subagent"] = json!("child-two");
        other["in"] = json!(100);
        other["cache_read"] = json!(100);
        ledger.ingest(other.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        let session = &view.sessions[0];
        assert_eq!(session.completed_subagents.len(), 2);
        assert_eq!(session.subagent_history.len(), 2);
        let latest = session.completed_subagents.iter().find(|card| card.subagent_id.as_deref() == Some("child-one")).unwrap();
        assert_eq!(latest.state, "failed");
        assert_eq!(latest.tokens, Some(0));
        assert_eq!(session.daily.requests, 3);
        assert_eq!(session.daily.cache_hit_rate, Some(400.0 / 600.0));
        assert_eq!(view.daily.cache_hit_rate, Some(400.0 / 600.0));
        let empty = DailyStats::default();
        assert_eq!(empty.cache_hit_rate, None);
    }

    /// 从未成功的会话仍保留各账号最新失败、取消和未知根卡片。
    #[test]
    fn never_successful_session_keeps_failed_cancelled_and_unknown_roots() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 503).to_string().as_bytes());
        let mut cancelled = fixture(2, "alpha", 499);
        cancelled["providerAccount"] = json!("cancel@example.com");
        ledger.ingest(cancelled.to_string().as_bytes());
        let mut unknown = fixture(3, "alpha", 200);
        unknown["provider"] = json!("other");
        unknown["providerAccount"] = json!("unknown@example.com");
        unknown.as_object_mut().unwrap().remove("status");
        ledger.ingest(unknown.to_string().as_bytes());
        let mut child = fixture(4, "alpha", 200);
        child["subagent"] = json!("child-only-success");
        child["providerAccount"] = json!("child@example.com");
        ledger.ingest(child.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        let routes = &view.sessions[0].routes;
        assert_eq!(routes.len(), 3);
        assert!(routes.iter().any(|card| card.state == "unknown" && card.current));
        assert!(routes.iter().any(|card| card.state == "failed" && !card.current));
        assert!(routes.iter().any(|card| card.state == "cancelled" && !card.current));
        assert!(routes.iter().all(|card| card.subagent_id.is_none()));
        assert_eq!(view.sessions[0].completed_subagents.len(), 1);
    }

    /// 较新成功根请求清掉同会话更早的失败根卡片，不跨会话；子请求成功不能清。
    #[test]
    fn newer_successful_root_clears_earlier_failed_roots_only_in_session() {
        let mut ledger = ledger();
        let mut earlier = fixture(1, "alpha", 200);
        earlier["providerAccount"] = json!("early@example.com");
        earlier["t"] = json!("2026-10-20T00:00:30+00:00");
        ledger.ingest(earlier.to_string().as_bytes());
        ledger.ingest(fixture(2, "alpha", 503).to_string().as_bytes());
        let mut cancelled = fixture(3, "alpha", 499);
        cancelled["providerAccount"] = json!("cancel@example.com");
        ledger.ingest(cancelled.to_string().as_bytes());
        let mut unknown = fixture(4, "alpha", 200);
        unknown["provider"] = json!("other");
        unknown["providerAccount"] = json!("unknown@example.com");
        unknown.as_object_mut().unwrap().remove("status");
        ledger.ingest(unknown.to_string().as_bytes());
        let mut other_session = fixture(5, "beta", 503);
        other_session["providerAccount"] = json!("stay@example.com");
        ledger.ingest(other_session.to_string().as_bytes());
        let held = ledger.snapshot("h", "ready", None);
        let alpha = held.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.routes.len(), 3);
        assert!(alpha.routes.iter().any(|card| card.state == "unknown" && card.current));
        assert!(alpha.routes.iter().any(|card| card.state == "failed"));
        assert!(alpha.routes.iter().any(|card| card.state == "cancelled"));
        assert!(!alpha.routes.iter().any(|card| card.state == "success" || card.state == "recovered"));

        let mut child = fixture(6, "alpha", 200);
        child["subagent"] = json!("child-success");
        child["providerAccount"] = json!("child@example.com");
        ledger.ingest(child.to_string().as_bytes());
        let after_child = ledger.snapshot("h", "ready", None);
        let alpha = after_child.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.routes.len(), 3);
        assert_eq!(alpha.completed_subagents.len(), 1);

        let mut success = fixture(7, "alpha", 200);
        success["providerAccount"] = json!("recovered@example.com");
        // 开始时间早于失败行，完成顺序仍以文件追加为准。
        success["t"] = json!("2026-10-20T00:00:00+00:00");
        ledger.ingest(success.to_string().as_bytes());
        let cleared = ledger.snapshot("h", "ready", None);
        let alpha = cleared.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.routes.len(), 1);
        assert_eq!(alpha.routes[0].account, "recovered@example.com");
        assert!(alpha.routes[0].current);
        assert!(matches!(alpha.routes[0].state.as_str(), "success" | "recovered"));
        let beta = cleared.sessions.iter().find(|session| session.id == "beta").unwrap();
        assert_eq!(beta.routes.len(), 1);
        assert_eq!(beta.routes[0].state, "failed");

        let mut later = fixture(8, "alpha", 503);
        later["provider"] = json!("later");
        later["providerAccount"] = json!("again@example.com");
        ledger.ingest(later.to_string().as_bytes());
        let after_later = ledger.snapshot("h", "ready", None);
        let alpha = after_later.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(alpha.routes.len(), 1);
        assert_eq!(alpha.routes[0].state, "failed");
        assert!(alpha.routes[0].current);
        assert_eq!(alpha.daily.requests, 7);
    }

    /// 较新成功根请求清掉更早失败对照；成功之后的失败仍留下，且不编造冷却秒数。
    #[test]
    fn keeps_failed_account_when_another_account_succeeds() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 503).to_string().as_bytes());
        let mut other = fixture(2, "alpha", 200);
        other["providerAccount"] = json!("another@example.com");
        other["tries"] = json!(2);
        ledger.ingest(other.to_string().as_bytes());
        let cleared = ledger.snapshot("h", "ready", None);
        assert_eq!(cleared.sessions[0].routes.len(), 1);
        assert_eq!(cleared.sessions[0].routes[0].state, "recovered");
        let mut later = fixture(3, "alpha", 503);
        later["providerAccount"] = json!("later@example.com");
        ledger.ingest(later.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        let routes = &view.sessions[0].routes;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].state, "failed");
        assert!(routes[0].current);
        assert!(routes[0].detail.contains("恢复时间未记录"));
        assert_eq!(view.sessions[0].notes[1].kind, "会话");
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

    /// 完成卡的 updatedAt 是 t + ms 的真实完成时刻，刷新不随轮询续期。
    #[test]
    fn completed_card_updated_at_uses_finish_time_and_stays_fixed() {
        let mut ledger = ledger();
        ledger.ingest(fixture(1, "alpha", 200).to_string().as_bytes());
        let first = ledger.snapshot("h", "ready", None);
        let card = &first.sessions[0].routes[0];
        assert_eq!(card.updated_at.as_deref(), Some("2026-10-20T00:00:04.000Z"));
        // 现有 time 槽仍是日志里的开始时间 HH:mm:ss（本地时区）。
        let expected_time = DateTime::parse_from_rfc3339("2026-10-20T00:00:01Z").unwrap()
            .with_timezone(&Local).format("%H:%M:%S").to_string();
        assert_eq!(card.time, expected_time);
        assert!(card.route_groups.is_empty(), "日志没有完整组链，不能猜");
        // 重复快照与毫秒缺失都不改变已结束卡的时间。
        let second = ledger.snapshot("h", "ready", None);
        assert_eq!(second.sessions[0].routes[0].updated_at, card.updated_at);
        let mut no_duration = fixture(2, "beta", 200);
        no_duration.as_object_mut().unwrap().remove("ms");
        ledger.ingest(no_duration.to_string().as_bytes());
        let view = ledger.snapshot("h", "ready", None);
        let beta = view.sessions.iter().find(|session| session.id == "beta").unwrap();
        assert_eq!(beta.routes[0].updated_at.as_deref(), Some("2026-10-20T00:00:02.000Z"));
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
