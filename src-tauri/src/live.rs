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
    #[serde(default)]
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
        for active in &self.activity.active {
            let route = self.routes.get(&active.route);
            if route.is_some_and(|r| r.done || r.kind == "routing") {
                continue;
            }
            let session_id = if active.session.is_empty() {
                route.map(|r| r.session.as_str()).unwrap_or("")
            } else {
                &active.session
            };
            let started = DateTime::parse_from_rfc3339(&active.started).ok();
            let time = started.map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "--:--:--".into());
            let attempt = route.and_then(|r| r.tries.as_deref().unwrap_or_default().last());
            // 尝试已结束但外层请求还在交接时显示路由中，不冒充正在输出。
            let waiting = attempt.is_some_and(|a| a.done) || matches!(active.phase.as_str(), "slot" | "rpm");
            let phase = if waiting { "waiting" } else { "running" };
            let detail = match active.phase.as_str() {
                "slot" => "等待账号并发空位",
                "rpm" => "等待请求速率额度",
                "upstream" if !waiting => "请求中 · 等待或接收上游流",
                _ => "路由处理中",
            };
            let account = attempt.and_then(|a| route?.order.as_deref().unwrap_or_default().iter().find(|c| c.id == a.id))
                .map(|c| c.who.as_str()).filter(|s| !s.is_empty()).unwrap_or("正在选择账号");
            let model = attempt.map(|a| a.model.as_str()).filter(|s| !s.is_empty())
                .unwrap_or("正在选择模型");
            let effort = attempt.map(|a| a.effort.as_str()).filter(|s| !s.is_empty())
                .unwrap_or("强度待确定");
            let provider = route.map(|r| r.provider.as_str()).unwrap_or(&active.provider);
            let label = self.name(session_id);
            let note = Note { time: time.clone(), kind: "进行中".into(),
                text: format!("{label} · {account} ({model}) · {effort} · {detail} · {:.1}s", active.ms as f64 / 1000.0) };
            let second = Note { time: time.clone(), kind: "统计".into(),
                text: "本次 Token / TPS 待上游提供最终用量；当日统计仅含已完成请求".into() };
            let card = RouteCard {
                id: format!("live-{}-{}", self.pid, active.id), session_id: session_id.into(),
                session_label: label.clone(), account: account.into(), model: model.into(), provider: provider.into(),
                effort: effort.into(), state: phase.into(), detail: detail.into(), error: None, time,
                current: true, route_id: Some(active.route), tokens: None, tokens_per_second: None, note: note.clone(),
            };
            if let Some(session) = view.sessions.iter_mut().find(|s| s.id == session_id) {
                if active_sessions.insert(session_id.to_owned()) {
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
                    routes: vec![card], notes: [note, second], daily: DailyStats::default() });
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
        }
        // 稳定排序保持原完成顺序，活跃会话始终优先可见。
        view.sessions.sort_by_key(|s| !active_sessions.contains(&s.id));
        if let Some(session) = view.sessions.first() {
            view.notes = session.notes.clone();
        }
        if !active_sessions.is_empty() && view.state != "warning" {
            view.state = "ready".into();
            view.watch_label = format!("{} 个会话请求中", active_sessions.len());
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

/// 来源文案与日志口径一致。
fn display_agent(agent: &str) -> String {
    if agent == "claude-desktop" { "Claude Desktop".into() } else { agent.into() }
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
        let ledger = UsageLedger::new(now.date_naive());
        let mut view = ledger.snapshot("127.0.0.1:3425", "ready", None);
        live.apply(&mut view);
        assert_eq!(view.sessions[0].label, "HUD / 实时会话");
        assert_eq!(view.sessions[0].routes[0].state, "running");
        assert_eq!(view.sessions[0].routes[0].tokens, None);
        assert_eq!(view.daily.requests, 0);
        live.activity.active.clear();
        let mut finished = ledger.snapshot("h", "ready", None);
        live.apply(&mut finished);
        assert!(finished.sessions.is_empty());
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
    }
}
