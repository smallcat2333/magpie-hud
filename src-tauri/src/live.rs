//! 只读 Magpie 活跃请求/路由与本地会话标题；进行中数据只覆盖展示，不计入已完成账本。
//! 接口契约来自 yetone/magpie 的 gateway/{active,trace}.go 与 sessions/desktop_titles.go。

use crate::usage::{DailyStats, Note, RouteCard, RouteSnapshot, SessionView};
use chrono::{DateTime, Local};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 路由长轮询的增量响应；ids 是当前保留集合，用于移除已淘汰请求。
#[derive(Default, Deserialize)]
pub struct TraceUpdate {
    pub seq: i64,
    pub pid: u32,
    #[serde(default)]
    pub ids: Vec<i64>,
    #[serde(default)]
    pub routes: Vec<LiveRoute>,
}

/// 只接收展示需要的路由字段，不保存 prompt、响应正文或密钥。
#[derive(Clone, Default, Deserialize)]
pub struct LiveRoute {
    pub id: i64,
    #[serde(default)]
    pub session: String,
    /// 子 Agent 身份。与 usage.jsonl 的 subagent 相同；根请求没有该字段。
    #[serde(default, rename = "subagentId")]
    pub subagent_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub order: Option<Vec<Candidate>>,
    #[serde(default)]
    pub tries: Option<Vec<Attempt>>,
}

/// 路由候选的账号归属，仅按尝试 id 精确匹配。
#[derive(Clone, Default, Deserialize)]
pub struct Candidate {
    pub id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub who: String,
}

/// 当前尝试用于显示真实模型、强度以及是否已结束。
#[derive(Clone, Default, Deserialize)]
pub struct Attempt {
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub done: bool,
}

/// concurrency 完整快照，空列表表示当前没有请求。
#[derive(Default, Deserialize)]
pub struct Activity {
    #[serde(default)]
    pub active: Vec<Active>,
}

/// upstream 可能等待首字或正在输出，不能据此声称已收到流式 Token。
#[derive(Default, Deserialize)]
pub struct Active {
    pub id: i64,
    #[serde(default)]
    pub session: String,
    pub agent: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: String,
    pub started: String,
    pub ms: u64,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub route: i64,
}

/// 实时状态与已完成用量独立；失联后清空活跃态并显示提示，避免动画永远运行。
#[derive(Default)]
pub struct LiveState {
    pub seq: i64,
    pid: u32,
    routes: BTreeMap<i64, LiveRoute>,
    pub activity: Activity,
    pub names: HashMap<String, String>,
    pub error: Option<String>,
}

impl LiveState {
    /// 合并增量，服务重启或序号回退时清理旧数据。
    pub fn update(&mut self, update: TraceUpdate) {
        if self.pid != update.pid || update.seq < self.seq {
            self.routes.clear();
        }
        self.pid = update.pid;
        self.seq = update.seq;
        let ids: HashSet<_> = update.ids.into_iter().collect();
        self.routes.retain(|id, _| ids.contains(id));
        for route in update.routes {
            self.routes.insert(route.id, route);
        }
    }

    /// 用活跃请求覆盖展示，不改账本中的次数、费用与当日 Token。
    pub fn apply(&self, view: &mut RouteSnapshot) {
        let mut active_sessions = HashSet::new();
        let mut child_sessions = HashSet::new();
        // 根上轮只取根卡片；子 Agent 上轮按子 ID 单独保存，互不混用。
        let previous_metrics: HashMap<_, _> = view.sessions.iter().filter_map(|session| {
            session.routes.iter().find(|card| card.current && card.tokens.is_some()
                && !matches!(card.state.as_str(), "running" | "waiting"))
                .map(|card| (session.id.clone(), (card.tokens, card.tokens_per_second)))
        }).collect();
        let child_metrics: HashMap<_, _> = view.sessions.iter().flat_map(|session| {
            session.subagent_history.iter().filter(|(_, card)| card.tokens.is_some())
                .map(|(id, card)| ((session.id.clone(), id.clone()), (card.tokens, card.tokens_per_second)))
        }).collect();
        for active in &self.activity.active {
            let route = self.routes.get(&active.route);
            if route.is_some_and(|r| r.done || r.kind == "routing") {
                continue;
            }
            // 活跃请求只带 route id。没有对应路由时不猜测它属于父会话。
            let Some(route) = route else { continue };
            let child_id = route.subagent_id.trim();
            let child = (!child_id.is_empty()).then_some(child_id);
            let session_id = if active.session.is_empty() {
                route.session.as_str()
            } else {
                &active.session
            };
            // 没有会话身份的请求无法正确归属，实时阶段也不显示。
            if session_id.is_empty() {
                continue;
            }
            let started = DateTime::parse_from_rfc3339(&active.started).ok();
            let time = started.map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "--:--:--".into());
            let attempt = route.tries.as_deref().unwrap_or_default().last();
            // 尝试已结束但外层请求还在交接时显示路由中，不冒充正在输出。
            let waiting = attempt.is_some_and(|a| a.done) || matches!(active.phase.as_str(), "slot" | "rpm");
            let phase = if waiting { "waiting" } else { "running" };
            let detail = match active.phase.as_str() {
                "slot" => "等待账号并发空位",
                "rpm" => "等待请求速率额度",
                "upstream" if !waiting => "请求中 · 等待或接收上游流",
                _ => "路由处理中",
            };
            let candidate = attempt.and_then(|attempt| selected_candidate(route, attempt));
            let account = candidate.map(|c| c.who.as_str()).filter(|s| !s.is_empty())
                .unwrap_or("正在选择账号");
            let model = attempt.map(|a| a.model.as_str()).filter(|s| !s.is_empty())
                .unwrap_or("正在选择模型");
            let effort = attempt.map(|a| a.effort.as_str()).filter(|s| !s.is_empty())
                .unwrap_or("待定");
            let provider = candidate.map(|c| c.provider.as_str()).filter(|s| !s.is_empty()).unwrap_or("");
            let label = self.name(session_id);
            let note = Note { time: time.clone(), kind: "进行中".into(),
                text: format!("{label} · {account} ({model}) · {effort} · {detail} · {:.1}s", active.ms as f64 / 1000.0) };
            let previous = if let Some(child_id) = child {
                child_metrics.get(&(session_id.to_owned(), child_id.to_owned())).copied()
            } else {
                previous_metrics.get(session_id).copied()
            };
            let second = Note { time: time.clone(), kind: "统计".into(),
                text: if child.is_some() {
                    if previous.is_some() {
                        "卡片数值为该子 Agent 上轮已完成请求；本轮结束后更新，当日统计仅含已完成请求".into()
                    } else {
                        "该子 Agent 暂无上轮记录，本次 Token / TPS 等待完成；当日统计仅含已完成请求".into()
                    }
                } else if previous.is_some() {
                    "卡片数值为该会话上轮已完成请求；本轮结束后更新，当日统计仅含已完成请求".into()
                } else {
                    "该会话暂无上轮记录，本次 Token / TPS 等待完成；当日统计仅含已完成请求".into()
                } };
            let card = RouteCard {
                id: format!("live-{}-{}", self.pid, active.id), session_id: session_id.into(),
                session_label: label.clone(), account: account.into(), model: model.into(), provider: provider.into(),
                effort: effort.into(), state: phase.into(), detail: detail.into(), error: None, time,
                current: true, route_id: Some(active.route),
                tokens: previous.and_then(|(tokens, _)| tokens),
                tokens_per_second: previous.and_then(|(_, speed)| speed),
                metrics_are_previous: previous.is_some(),
                subagent_id: child.map(str::to_owned),
                subagent_label: child.map(child_label),
                note: note.clone(),
            };
            if child.is_some() {
                child_sessions.insert(session_id.to_owned());
                if let Some(session) = view.sessions.iter_mut().find(|s| s.id == session_id) {
                    if let Some(existing) = session.subagents.iter_mut().find(|item| item.subagent_id == card.subagent_id) {
                        if card.route_id > existing.route_id { *existing = card; }
                    } else {
                        session.subagents.push(card);
                    }
                } else {
                    view.sessions.push(SessionView { id: session_id.into(), label, timestamp_ms: started.map(|t| t.timestamp_millis()).unwrap_or(0),
                        latest_sequence: 0, agent: display_agent(&active.agent), request: String::new(),
                        routes: Vec::new(), subagents: vec![card], subagent_history: HashMap::new(), quota_routes: Vec::new(),
                        notes: [note, second], daily: DailyStats::default() });
                }
                continue;
            }
            if let Some(session) = view.sessions.iter_mut().find(|s| s.id == session_id) {
                if active_sessions.insert(session_id.to_owned()) {
                    // 父会话自己的进行中请求替换已完成根卡片，只留仍有失败依据的旧账号。
                    session.routes.retain(|c| c.state == "failed");
                    session.agent = display_agent(&active.agent);
                    session.request = active.model.clone();
                    session.notes = [note, second];
                }
                session.routes.insert(0, card);
            } else {
                active_sessions.insert(session_id.to_owned());
                view.sessions.push(SessionView { id: session_id.into(), label, timestamp_ms: started.map(|t| t.timestamp_millis()).unwrap_or(0),
                    latest_sequence: 0, agent: display_agent(&active.agent), request: active.model.clone(),
                    routes: vec![card], subagents: Vec::new(), subagent_history: HashMap::new(), quota_routes: Vec::new(),
                    notes: [note, second], daily: DailyStats::default() });
            }
        }
        for session in &mut view.sessions {
            session.label = self.name(&session.id);
            if let Some(rest) = session.notes[1].text.strip_prefix("未命名会话 · ") {
                session.notes[1].text = format!("{} · {rest}", session.label);
            }
            for card in &mut session.routes {
                card.session_label = session.label.clone();
            }
            for card in &mut session.subagents {
                card.session_label = session.label.clone();
            }
        }
        // 稳定排序保持原完成顺序，活跃会话始终优先可见。
        view.sessions.sort_by_key(|s| !(active_sessions.contains(&s.id) || child_sessions.contains(&s.id)));
        if let Some(session) = view.sessions.first() {
            view.notes = session.notes.clone();
        }
        let shown = active_sessions.union(&child_sessions).count();
        if shown > 0 && view.state != "warning" {
            view.state = "ready".into();
            view.watch_label = format!("{shown} 个会话请求中");
        }
        if let Some(error) = &self.error {
            view.state = "warning".into();
            view.watch_label = "实时连接中断 · 保留完成记录".into();
            view.error = Some(error.clone());
        }
    }

    /// 未找到标题时明确未命名；UUID 只保留在数据身份与悬停提示中。
    fn name(&self, id: &str) -> String {
        self.names.get(id).cloned().unwrap_or_else(|| if id.is_empty() { "未标记会话".into() } else { "未命名会话".into() })
    }
}

/// 尝试 ID 是失败后休息的键，不是独立账号键。当前登录账号的键是 provider，
/// 同组里该账号的每个模型都会复用它；其余账号是 `provider@user`，多密钥是 `provider#key`。
/// 模型能唯一对应时用该行。否则仅当 provider 与账号完全一致时认领；任一不同就不猜。
fn selected_candidate<'a>(route: &'a LiveRoute, attempt: &Attempt) -> Option<&'a Candidate> {
    if attempt.id.is_empty() { return None; }
    let candidates: Vec<_> = route.order.as_deref().unwrap_or_default().iter()
        .filter(|candidate| candidate.id == attempt.id).collect();
    let exact: Vec<_> = candidates.iter().copied()
        .filter(|candidate| !attempt.model.is_empty() && candidate.model == attempt.model).collect();
    let pool: Vec<_> = if exact.is_empty() {
        if !attempt.model.is_empty() && candidates.iter().any(|candidate| !candidate.model.is_empty()) {
            return None;
        }
        candidates
    } else {
        exact
    };
    let mut identity: Option<&Candidate> = None;
    for candidate in pool {
        if let Some(chosen) = identity {
            if chosen.provider != candidate.provider || chosen.who != candidate.who { return None; }
        } else if candidate.provider.is_empty() && candidate.who.is_empty() {
            return None;
        } else {
            identity = Some(candidate);
        }
    }
    identity
}

/// 来源文案与日志口径一致。
fn display_agent(agent: &str) -> String {
    if agent == "claude-desktop" { "Claude Desktop".into() } else { agent.into() }
}

/// 子 Agent 只给通用标签和短 ID，不展示会话正文。
fn child_label(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("子 Agent {short}")
}

/// 实时接口仅允许回环 HTTP，避免读取配置地址时向远端发出请求。
pub fn gateway_url(host: &str) -> Result<String, String> {
    let url = if host.starts_with("http://") { host.trim_end_matches('/').to_string() } else { format!("http://{}", host.trim_end_matches('/')) };
    let authority = url.strip_prefix("http://").unwrap();
    let valid = ["127.0.0.1:", "localhost:", "[::1]:"].iter()
        .any(|prefix| authority.strip_prefix(prefix).is_some_and(|port| port.parse::<u16>().is_ok()));
    if !valid { return Err("实时路由地址必须是本机回环 HTTP 地址和端口".into()); }
    Ok(url)
}

/// 创建绕过代理的回环客户端；超时覆盖最长 25 秒的路由长轮询。
pub fn client(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(ureq::Agent::config_builder()
        .proxy(None).max_redirects(0).timeout_global(Some(timeout)).build())
}

/// 读取外部 JSON；网络、状态码及字段错误向界面报告。
pub fn read_json<T: serde::de::DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T, String> {
    let mut response = agent.get(url).call().map_err(|e| format!("实时路由读取失败：{e}"))?;
    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("实时路由字段无效：{e}"))
}

/// Desktop 显式身份；只反序列化标题及其关联 ID，不读取消息、凭据和正文。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopTitle {
    session_id: String,
    #[serde(default)] cli_session_id: String,
    #[serde(default)] unarchived_cli_session_id: String,
    #[serde(default)] pre_clear_cli_session_id: String,
    #[serde(default)] prior_cli_session_ids: Vec<String>,
    #[serde(default)] title: String,
}

/// 缓存未变的标题文件；外部文件短暂写坏时保留上次有效标题，删除则移除。
#[derive(Default)]
pub struct Titles {
    files: HashMap<PathBuf, (std::time::SystemTime, u64, DesktopTitle)>,
}

impl Titles {
    /// 与 Magpie 相同：当前 ID 优先历史 ID；同优先级冲突不随意认领名称。
    pub fn scan(&mut self, roots: &[PathBuf]) -> HashMap<String, String> {
        let mut found = HashSet::new();
        let mut result = HashMap::<String, (u8, String)>::new();
        for root in roots {
            for kind in ["claude-code-sessions", "local-agent-mode-sessions"] {
                for profile in directories(&root.join(kind)) {
                    for org in directories(&profile) {
                        if let Ok(entries) = fs::read_dir(org) {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if !entry.file_name().to_string_lossy().starts_with("local_") || path.extension().is_none_or(|e| e != "json") { continue; }
                                found.insert(path.clone());
                                let Ok(meta) = entry.metadata() else { continue };
                                if meta.len() > 8 * 1024 * 1024 { continue; }
                                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                                if !self.files.get(&path).is_some_and(|(m, len, _)| *m == modified && *len == meta.len()) {
                                    if let Ok(bytes) = fs::read(&path) {
                                        if let Ok(title) = serde_json::from_slice::<DesktopTitle>(&bytes) {
                                            self.files.insert(path.clone(), (modified, meta.len(), title));
                                        }
                                    }
                                }
                                if let Some((_, _, title)) = self.files.get(&path) {
                                    add_title(&mut result, &title.cli_session_id, &title.title, 2);
                                    add_title(&mut result, &title.unarchived_cli_session_id, &title.title, 2);
                                    if let Some(id) = title.session_id.strip_prefix("local_") {
                                        add_title(&mut result, id, &title.title, if title.cli_session_id.is_empty() { 2 } else { 1 });
                                    }
                                    add_title(&mut result, &title.pre_clear_cli_session_id, &title.title, 1);
                                    for id in &title.prior_cli_session_ids { add_title(&mut result, id, &title.title, 1); }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.files.retain(|path, _| found.contains(path));
        let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").expect("缺少用户目录")).join(".codex")
        }).join("session_index.jsonl");
        // Magpie 也从此只追加索引读取名称，未换行记录留到下次扫描。
        if let Ok(file) = fs::File::open(codex) {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(file);
            let mut row = String::new();
            while reader.read_line(&mut row).is_ok_and(|n| n > 0) {
                if row.ends_with('\n') {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&row) {
                        if let (Some(id), Some(name)) = (value["id"].as_str(), value["thread_name"].as_str()) {
                            result.insert(id.into(), (2, name.into()));
                        }
                    }
                }
                row.clear();
            }
        }
        result.into_iter().filter(|(_, (_, name))| !name.is_empty()).map(|(id, (_, name))| (id, name)).collect()
    }
}

/// 精确身份映射，同级重名争议留空；不依赖 UUID 前缀猜测。
fn add_title(names: &mut HashMap<String, (u8, String)>, id: &str, title: &str, rank: u8) {
    if id.is_empty() { return; }
    let name = title.split_whitespace().collect::<Vec<_>>().join(" ");
    match names.get_mut(id) {
        Some((old_rank, old_name)) if rank > *old_rank => { *old_rank = rank; *old_name = name; }
        Some((old_rank, old_name)) if rank == *old_rank && *old_name != name => old_name.clear(),
        None => { names.insert(id.into(), (rank, name)); }
        _ => {}
    }
}

/// 只列目录，不跟随文件加载未知内容。
fn directories(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
}

/// Desktop 官方版和第三方供应商版的本机数据目录。
pub fn desktop_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            for name in ["Claude", "Claude-3p"] { roots.push(PathBuf::from(&base).join(name)); }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageLedger;

    /// 活跃请求提前显示，不重复累计；完成与失联都停止活跃态。
    #[test]
    fn live_overlay_does_not_count_unfinished_usage() {
        let now = Local::now();
        let mut live = LiveState::default();
        live.names.insert("alpha".into(), "HUD / 实时会话".into());
        live.activity.active.push(Active { id: 1, session: "alpha".into(), agent: "claude-desktop".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 10, ..Active::default() });
        live.routes.insert(10, LiveRoute { id: 10, session: "alpha".into(), ..LiveRoute::default() });
        let ledger = UsageLedger::new(now.date_naive());
        let mut view = ledger.snapshot("127.0.0.1:3425", "ready", None);
        live.apply(&mut view);
        assert_eq!(view.sessions[0].label, "HUD / 实时会话");
        assert_eq!(view.sessions[0].routes[0].state, "running");
        assert_eq!(view.sessions[0].routes[0].tokens, None);
        assert!(!view.sessions[0].routes[0].metrics_are_previous);
        assert_eq!(view.daily.requests, 0);
        live.activity.active.clear();
        let mut finished = ledger.snapshot("h", "ready", None);
        live.apply(&mut finished);
        assert!(finished.sessions.is_empty());
    }

    /// 没有会话身份的活跃请求不显示，不论实时输出量大小。
    #[test]
    fn skips_every_sessionless_active_request() {
        let now = Local::now();
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 1, session: String::new(), agent: "claude-desktop".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 10, ..Active::default() });
        live.routes.insert(10, LiveRoute { id: 10, ..LiveRoute::default() });
        let ledger = UsageLedger::new(now.date_naive());
        let mut view = ledger.snapshot("127.0.0.1:3425", "ready", None);
        live.apply(&mut view);
        assert!(view.sessions.is_empty());
    }

    /// 只引用同一会话最近完成的一轮；并发卡片共用上轮值，不累计到本轮。
    #[test]
    fn active_metrics_reference_previous_round_in_same_session() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        for (id, session, account, output) in [(1, "alpha", "old", 900), (2, "alpha", "latest", 100), (3, "beta", "other", 200)] {
            let row = serde_json::json!({"route_id":id,"t":now.to_rfc3339(),"session":session,
                "providerAccount":account,"provider":"codex","model":"gpt-test","status":200,
                "in":100,"out":output,"cache_read":800,"ms":3000,"ttft_ms":1000});
            ledger.ingest(row.to_string().as_bytes());
        }
        let baseline = ledger.snapshot("h", "ready", None);
        let mut live = LiveState::default();
        for (id, session) in [(10, "alpha"), (11, "alpha"), (12, "beta"), (13, "first-round")] {
            live.routes.insert(id, LiveRoute { id, session: session.into(), ..LiveRoute::default() });
            live.activity.active.push(Active { id, session: session.into(), agent: "test".into(),
                started: now.to_rfc3339(), phase: "upstream".into(), route: id, ..Default::default() });
        }
        let mut view = baseline.clone();
        live.apply(&mut view);
        for session in &view.sessions {
            for card in &session.routes {
                match session.id.as_str() {
                    "alpha" => {
                        assert_eq!(card.tokens, Some(1_000));
                        assert_eq!(card.tokens_per_second, Some(50.0));
                        assert!(card.metrics_are_previous);
                    }
                    "beta" => {
                        assert_eq!(card.tokens, Some(1_100));
                        assert_eq!(card.tokens_per_second, Some(100.0));
                        assert!(card.metrics_are_previous);
                    }
                    _ => { assert_eq!(card.tokens, None); assert!(!card.metrics_are_previous); }
                }
                assert_eq!(card.state, "running");
            }
        }
        assert_eq!(view.sessions.iter().find(|session| session.id == "alpha").unwrap().routes.len(), 2);
        assert_eq!(view.daily, baseline.daily);
        live.activity.active.clear();
        let mut finished = baseline;
        live.apply(&mut finished);
        assert!(finished.sessions.iter().flat_map(|session| &session.routes).all(|card| !card.metrics_are_previous));
    }

    /// 子请求挂在父会话下；缺路由先不显示，已完成根卡片不被子请求清掉。
    #[test]
    fn active_children_mount_without_replacing_completed_root() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let root = serde_json::json!({"route_id":1,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"root@example.com","provider":"codex","model":"gpt-root","status":200,
            "in":100,"out":100,"cache_read":800,"ms":3000,"ttft_ms":1000});
        let child = serde_json::json!({"route_id":2,"t":now.to_rfc3339(),"session":"alpha","subagent":"child-aaaaaaaa",
            "providerAccount":"child@example.com","provider":"codex","model":"gpt-child","status":200,
            "in":100,"out":200,"cache_read":800,"ms":3000,"ttft_ms":1000});
        ledger.ingest(root.to_string().as_bytes());
        ledger.ingest(child.to_string().as_bytes());
        let baseline = ledger.snapshot("h", "ready", None);
        let mut live = LiveState::default();
        live.routes.insert(21, LiveRoute { id: 21, session: "alpha".into(), subagent_id: "child-aaaaaaaa".into(), ..Default::default() });
        live.routes.insert(22, LiveRoute { id: 22, session: "alpha".into(), subagent_id: "child-bbbbbbbb".into(), ..Default::default() });
        live.routes.insert(23, LiveRoute { id: 23, session: "alpha".into(), subagent_id: "child-aaaaaaaa".into(), ..Default::default() });
        for (id, route) in [(101, 21), (102, 22), (103, 23), (104, 99)] {
            live.activity.active.push(Active { id, session: "alpha".into(), agent: "test".into(),
                started: now.to_rfc3339(), phase: "upstream".into(), route, ..Default::default() });
        }
        let mut view = baseline.clone();
        live.apply(&mut view);
        let session = view.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert!(session.routes.iter().all(|card| !matches!(card.state.as_str(), "running" | "waiting")));
        assert!(session.routes.iter().any(|card| card.current && card.subagent_id.is_none()));
        assert_eq!(session.subagents.len(), 2);
        let known = session.subagents.iter().find(|card| card.subagent_id.as_deref() == Some("child-aaaaaaaa")).unwrap();
        assert_eq!(known.route_id, Some(23));
        assert_eq!(known.tokens, Some(1_100));
        assert!(known.metrics_are_previous);
        assert_eq!(known.subagent_label.as_deref(), Some("子 Agent child-aa"));
        let fresh = session.subagents.iter().find(|card| card.subagent_id.as_deref() == Some("child-bbbbbbbb")).unwrap();
        assert_eq!(fresh.tokens, None);
        assert!(!session.subagents.iter().any(|card| card.route_id == Some(99)));
        assert_eq!(view.daily, baseline.daily);
        assert_eq!(session.daily.requests, 2);
    }

    /// 只有子 Agent 时父会话不编造已完成根卡片。
    #[test]
    fn child_only_session_keeps_root_routes_empty() {
        let now = Local::now();
        let mut live = LiveState::default();
        live.routes.insert(7, LiveRoute { id: 7, session: "solo".into(), subagent_id: "child-only".into(), ..Default::default() });
        live.activity.active.push(Active { id: 1, session: "solo".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 7, ..Default::default() });
        let mut view = UsageLedger::new(now.date_naive()).snapshot("h", "ready", None);
        live.apply(&mut view);
        assert!(view.sessions[0].routes.is_empty());
        assert_eq!(view.sessions[0].subagents.len(), 1);
        assert!(view.sessions[0].request.is_empty());
    }

    /// 只有父会话进行中时，已完成根卡片被替换，不和实时行重复。
    #[test]
    fn root_live_replaces_completed_root_without_duplicating() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let root = serde_json::json!({"route_id":1,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"root@example.com","provider":"codex","model":"gpt-root","status":200,
            "in":100,"out":100,"cache_read":800,"ms":3000,"ttft_ms":1000});
        let failed = serde_json::json!({"route_id":2,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"old@example.com","provider":"codex","model":"gpt-root","status":503,
            "in":100,"out":0,"ms":1000,"ttft_ms":100});
        ledger.ingest(failed.to_string().as_bytes());
        ledger.ingest(root.to_string().as_bytes());
        let mut live = LiveState::default();
        live.routes.insert(30, LiveRoute { id: 30, session: "alpha".into(), ..Default::default() });
        live.activity.active.push(Active { id: 8, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 30, ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        let routes = &view.sessions[0].routes;
        assert_eq!(routes.iter().filter(|card| card.state == "running").count(), 1);
        assert!(!routes.iter().any(|card| card.state == "success" || card.state == "recovered"));
        assert!(routes.iter().any(|card| card.state == "failed"));
        let running = routes.iter().find(|card| card.state == "running").unwrap();
        assert_eq!(running.tokens, Some(1_000));
        assert!(running.metrics_are_previous);
        assert!(running.subagent_id.is_none());
        live.routes.insert(40, LiveRoute { id: 40, session: "alpha".into(), subagent_id: "child-with-root".into(), ..Default::default() });
        live.activity.active.push(Active { id: 9, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 40, ..Default::default() });
        live.apply(&mut view);
        assert_eq!(view.sessions[0].subagents.len(), 1);
        assert_eq!(view.watch_label, "1 个会话请求中");
    }

    /// 同一账号、模型可在多个组成员中重复出现；重复行不构成账号歧义。
    #[test]
    fn selected_candidate_accepts_duplicate_model_rows_with_same_identity() {
        let route = LiveRoute { order: Some(vec![
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into() },
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into() },
        ]), ..Default::default() };
        let selected = selected_candidate(&route, &Attempt {
            id: "seat".into(), model: "model-a".into(), ..Default::default()
        });
        assert_eq!(selected.map(|candidate| (candidate.provider.as_str(), candidate.who.as_str())),
            Some(("provider-a", "account-a")));
    }

    /// 尝试未提供模型时，仅从精确 ID 的所有候选一致的身份认领，不用请求模型推断。
    #[test]
    fn selected_candidate_accepts_missing_model_only_with_same_identity() {
        let route = LiveRoute { order: Some(vec![
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into() },
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-b".into(), who: "account-a".into() },
            Candidate { id: "other-seat".into(), provider: "provider-b".into(), model: "model-a".into(), who: "account-b".into() },
        ]), ..Default::default() };
        let selected = selected_candidate(&route, &Attempt { id: "seat".into(), ..Default::default() });
        assert_eq!(selected.map(|candidate| (candidate.provider.as_str(), candidate.who.as_str())),
            Some(("provider-a", "account-a")));
    }

    /// 同 ID、同模型但账号或供应商不同，模型已知与缺失时都必须拒绝认领。
    #[test]
    fn selected_candidate_rejects_conflicting_account_or_provider() {
        for (provider, who) in [("provider-a", "account-b"), ("provider-b", "account-a")] {
            let route = LiveRoute { order: Some(vec![
                Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into() },
                Candidate { id: "seat".into(), provider: provider.into(), model: "model-a".into(), who: who.into() },
            ]), ..Default::default() };
            for model in ["model-a", ""] {
                assert!(selected_candidate(&route, &Attempt {
                    id: "seat".into(), model: model.into(), ..Default::default()
                }).is_none(), "conflicting identity must remain unknown, with model {model:?}");
            }
        }
    }

    /// 重启替换路由缓存；同级标题争议未知，当前身份优先历史身份。
    #[test]
    fn handles_restart_and_exact_title_identity() {
        let mut live = LiveState::default();
        live.update(TraceUpdate { pid: 1, seq: 10, ids: vec![1], routes: vec![LiveRoute { id: 1, ..Default::default() }] });
        live.update(TraceUpdate { pid: 2, seq: 1, ..Default::default() });
        assert!(live.routes.is_empty());
        let mut names = HashMap::new();
        add_title(&mut names, "id", "old", 1);
        add_title(&mut names, "id", "current", 2);
        assert_eq!(names["id"].1, "current");
        add_title(&mut names, "id", "conflict", 2);
        assert_eq!(names["id"].1, "");
        assert!(gateway_url("127.0.0.1:3425").is_ok());
        assert!(gateway_url("example.com:3425").is_err());
        let route = LiveRoute { provider: "grok".into(), order: Some(vec![
            Candidate { id: "grok".into(), provider: "grok".into(), model: "grok-test".into(), who: "grok@example.com".into() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6.1".into(), who: "gpt@example.com".into() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "other@example.com".into() },
        ]), ..Default::default() };
        let selected = selected_candidate(&route, &Attempt { id: "codex".into(), model: "gpt-6.1".into(), ..Default::default() });
        assert_eq!(selected.map(|c| (c.provider.as_str(), c.who.as_str())), Some(("codex", "gpt@example.com")));
        assert!(selected_candidate(&route, &Attempt { id: "codex".into(), ..Default::default() }).is_none());
        let legacy = LiveRoute { order: Some(vec![Candidate { id: "grok".into(), who: "grok@example.com".into(), ..Default::default() }]), ..Default::default() };
        assert!(selected_candidate(&legacy, &Attempt { id: "grok".into(), model: "grok-test".into(), ..Default::default() }).is_some());
        // 当前登录账号的休息键是 provider，组内每个模型各有一行。
        let shared = LiveRoute { order: Some(vec![
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "one@example.com".into() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6.1".into(), who: "one@example.com".into() },
            Candidate { id: "codex@two@example.com".into(), provider: "codex".into(), model: "gpt-6".into(), who: "two@example.com".into() },
        ]), ..Default::default() };
        let signed_in = selected_candidate(&shared, &Attempt { id: "codex".into(), model: "gpt-6.1".into(), ..Default::default() });
        assert_eq!(signed_in.map(|c| (c.provider.as_str(), c.who.as_str())), Some(("codex", "one@example.com")));
        // 模型不在计划里，或同一键对应不同账号时，保持选择中，不用请求模型补身份。
        assert!(selected_candidate(&shared, &Attempt { id: "codex".into(), model: "gpt-other".into(), ..Default::default() }).is_none());
        let conflict = LiveRoute { order: Some(vec![
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "one@example.com".into() },
            Candidate { id: "codex".into(), provider: "other".into(), model: "gpt-6".into(), who: "one@example.com".into() },
        ]), ..Default::default() };
        assert!(selected_candidate(&conflict, &Attempt { id: "codex".into(), model: "gpt-6".into(), ..Default::default() }).is_none());
    }
}
