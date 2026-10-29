//! 只读 Magpie 活跃请求/路由与本地会话标题；进行中数据只覆盖展示，不计入已完成账本。
//! 接口契约来自 yetone/magpie 的 gateway/{active,trace}.go 与 sessions/desktop_titles.go。

use crate::usage::{DailyStats, Note, RouteCard, RouteSnapshot, SessionView};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FIRST_RESPONSE_WAIT_MS: u64 = 15_000;

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
    pub agent: String,
    /// 只取工作目录标记，识别 Desktop 临时辅助 fork；不读取该目录内容。
    #[serde(default)]
    pub folder: String,
    /// 子 Agent 身份。与 usage.jsonl 的 subagent 相同；根请求没有该字段。
    #[serde(default, rename = "subagentId")]
    pub subagent_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub provider: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default, rename = "firstText")]
    pub first_text: Option<u64>,
    #[serde(default)]
    pub kind: String,
    /// 入口组；子组及其归属只用于把选中尝试映射回实际组链。
    #[serde(default)]
    pub group: Option<RouteGroup>,
    #[serde(default)]
    pub order: Option<Vec<Candidate>>,
    #[serde(default)]
    pub tries: Option<Vec<Attempt>>,
    /// 请求开始时间（Route.Time）；履历用它把 trace 与日志行按时间对齐。
    #[serde(default)]
    pub time: Option<String>,
    /// 路由终态 HTTP 状态；done 但无状态不能推断成功。
    #[serde(default)]
    pub status: Option<i64>,
    /// 响应报告模型（Route.Served）；与请求发送模型分开，不互相冒充。
    #[serde(default)]
    pub served: String,
    /// 组规则判定的白名单摘要；when 表达式等其余字段不接收。
    #[serde(default)]
    pub rule: Option<TraceRule>,
    /// 嵌套组判定，只保留组身份与规则白名单。
    #[serde(default)]
    pub nested: Vec<TraceNestedRule>,
}

#[derive(Clone, Default, Deserialize)]
pub struct TraceNestedRule {
    /// 上游 NestedRule.group：子组的 id 或 ref，用来精确关联，不按组名匹配。
    #[serde(default)]
    pub group: String,
    /// 上游给的组名只作反序列化兼容；展示名取精确匹配到的子组，不用它匹配。
    #[serde(default)]
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub rule: Option<TraceRule>,
}

/// 规则判定摘要：规则目标（use）、判定强度（pick）与本轮命中的意图（classified.intent）。
/// turn 是该请求所在用户输入（轮）的序号；
/// held=true 表示本请求是该轮开始后追加的后续请求，沿用轮初判定、自身不触发分类；
/// grown=true 表示同一轮内因上下文超出模型容量而重新判定，仍属同一输入；
/// waits=true 也是同轮续行，只是网关未持有该轮初判定（或规则已变更）；
/// compact 表示请求体被识别为 Agent 自身的会话压缩，不属于任何用户输入。
/// 上游仅对配置了规则的组给出 rule，普通单账号路由没有 turn 可言。
#[derive(Clone, Default, Deserialize)]
pub struct TraceRule {
    /// 命中的规则序号，从 1 起；显式 0 表示没有任何规则命中，缺字段则为未知。
    #[serde(default)]
    pub n: Option<i64>,
    #[serde(default, rename = "use")]
    pub use_target: String,
    #[serde(default, rename = "pick")]
    pub pick: String,
    #[serde(default)]
    pub turn: Option<i64>,
    /// 规则优先目标当前无人可接，组按没有该规则来路由。
    #[serde(default)]
    pub unready: bool,
    /// 优先目标无人可接时，实际先走的替代成员。
    #[serde(default)]
    pub instead: String,
    #[serde(default)]
    pub held: bool,
    #[serde(default)]
    pub waits: bool,
    #[serde(default)]
    pub grown: bool,
    #[serde(default)]
    pub compact: bool,
    #[serde(default)]
    pub classified: TraceClassified,
}

/// 意图分类结果；只取本轮命中的意图与置信度，不导出候选清单。
/// after 是分类器被告知的上一轮意图：本轮意图为空时可作沿用线索。
/// fallback=previous 表示分类失败沿用了上一轮意图，展示时应标注非本轮实测。
#[derive(Clone, Default, Deserialize)]
pub struct TraceClassified {
    /// 实际分类器的模型标识，不预设具体供应商或模型。
    #[serde(default)]
    pub by: String,
    /// 判定函数总耗时（毫秒）；缺失不当作零。
    #[serde(default)]
    pub ms: Option<i64>,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub after: String,
    #[serde(default)]
    pub sure: Option<f64>,
    #[serde(default)]
    pub fallback: String,
}

/// 组链信息只保留组身份、已知选择策略与直接子组，不读取成员账号或规则。
#[derive(Clone, Default, Deserialize)]
pub struct RouteGroup {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub name: String,
    /// 已知选择策略的结构字符串：order / rotate / usage，或空。
    #[serde(default)]
    pub routing: String,
    #[serde(default)]
    pub subs: Vec<RouteSubgroup>,
}

/// 子组通过 `in` 指回父组；`via` 出现在候选行，值是子组的 id 或 ref。
#[derive(Clone, Default, Deserialize)]
pub struct RouteSubgroup {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub name: String,
    /// 已知选择策略的结构字符串：order / rotate / usage，或空。
    #[serde(default)]
    pub routing: String,
    /// 该子组所在的父组 id；用来核验父子关系，不按组名推断。
    #[serde(default, rename = "in")]
    pub parent: String,
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
    /// 该候选实际经过的子组 id（通常是直接子组，不含父组）。
    #[serde(default)]
    pub via: Vec<String>,
    /// 组成员固定强度；与 Try.fixed 对应，不是拟合后的实际 effort。
    #[serde(default)]
    pub fixed: String,
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
    pub fixed: String,
    /// 上游语义：Effort 是本轮判定的强度选择（代替 Agent 原始强度），
    /// 不表示该尝试最终胜出；胜出证据以终态尝试与日志交叉核对为准。
    #[serde(default)]
    pub picked: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub status: Option<i64>,
    #[serde(default, rename = "firstText")]
    pub first_text: Option<u64>,
    /// 响应报告模型；与发送模型分开。
    #[serde(default)]
    pub served: String,
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

/// 履历折叠所需的轮次元数据；按 route_id 累积，trace 淘汰后仍可把日志行归到输入组。
/// time_ms 保留请求开始时间：网关重启会复用 route_id，只有 id 不够判定同一请求。
#[derive(Clone, Debug)]
pub struct TurnRecord {
    pub session: String,
    /// 子 Agent ID；根请求为空。轮次序号按各自会话线独立计数。
    pub subagent: String,
    pub turn: i64,
    /// 该轮开始后追加的后续请求，沿用轮初判定。
    pub held: bool,
    pub intent: String,
    pub after: String,
    pub sure: Option<f64>,
    /// 上游 fallback 标记；previous 表示意图是沿用上一轮而非本轮实测。
    pub fallback: String,
    /// 请求开始时间（本地毫秒），与日志 t 对齐用。
    pub time_ms: i64,
}

/// 判定区只发送路由白名单；不包含请求正文、规则表达式、候选账号或原始错误。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRuleView {
    pub group: String,
    pub classifier: Option<String>,
    pub duration_ms: Option<i64>,
    pub cached: bool,
    pub intent: Option<String>,
    pub intent_source: &'static str,
    pub confidence: Option<f64>,
    pub target: Option<String>,
    pub effort: Option<String>,
    /// 命中的规则序号：正数是命中，显式 0 是未命中，缺字段为未知。
    pub n: Option<i64>,
    /// 该请求所在用户输入的轮次序号；普通单账号路由没有轮次。
    pub turn: Option<i64>,
    /// 规则优先目标当前无人可接。
    pub unready: bool,
    /// 优先目标无人可接时记录的替代目标。
    pub instead: Option<String>,
    pub state: &'static str,
}

/// 已确认的组身份；routing 只保留已知策略结构字符串，不读取配置。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGroupView {
    pub id: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub name: String,
    pub routing: Option<String>,
}

/// 判定上下文的一个阶段：按上游记录顺序排列，不是该请求的实际执行路径。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStageView {
    /// 稳定阶段键：根组为 root，嵌套组带序号与精确身份。
    pub key: String,
    pub group: DecisionGroupView,
    pub rule: Option<DecisionRuleView>,
    /// 该阶段只是入口顺序上的判定上下文，不表示请求按此顺序经过这些组。
    pub context: &'static str,
    /// 组身份已用 id/ref 精确关联，且父子关系有证据；否则为假，不能当成路径的一环。
    pub confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionAttemptView {
    /// 该请求内的尝试序号，从 1 起，包含模型未记录的尝试。
    pub ordinal: i64,
    pub model: String,
    pub effort: Option<String>,
    pub state: &'static str,
    /// 该次尝试自己确认的组链，不借用其它尝试的路径。
    pub group_chain: Vec<DecisionGroupView>,
    /// 公共前缀在未知层级截断，后续路径不能唯一确认。
    pub path_incomplete: bool,
}

/// 计划尝试顺序的一项；只保留序号、模型与固定强度，不发送账号或供应商。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCandidateItem {
    pub ordinal: i64,
    pub model: String,
    pub fixed: Option<String>,
}

/// 计划尝试顺序，不是已经发生的尝试；超出上限的项只计数量。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCandidatePlan {
    pub items: Vec<DecisionCandidateItem>,
    pub total: i64,
    pub omitted: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionView {
    /// 网关进程与路由组成的稳定身份；不使用用户输入内容作为标识。
    pub id: String,
    pub session: String,
    pub label: String,
    pub subagent: Option<String>,
    pub subagent_id: Option<String>,
    pub started_ms: Option<i64>,
    /// 明确用户轮次的身份；包含网关、原始会话、子项与入口组，不靠正文推测。
    pub turn_key: Option<String>,
    pub user_turn: Option<i64>,
    pub continuation: bool,
    pub active: bool,
    pub state: &'static str,
    /// 活跃请求的等待原因；非等待或已结束时为 null。
    pub wait_reason: Option<&'static str>,
    /// 活跃请求的网关阶段白名单；已结束时不发送。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<&'static str>,
    pub rules: Vec<DecisionRuleView>,
    pub route_groups: Vec<String>,
    /// 根组与嵌套组的有序阶段，按上游原序。
    pub stages: Vec<DecisionStageView>,
    pub attempts: Vec<DecisionAttemptView>,
    /// 计划尝试顺序；没有候选时不发送。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_plan: Option<DecisionCandidatePlan>,
}

/// 累积的轮次索引上限；超出后按 route_id 递增顺序淘汰最旧条目。
const TURN_HISTORY_CAP: usize = 8192;

/// 实时状态与已完成用量独立；失联后清空活跃态并显示提示，避免动画永远运行。
#[derive(Default)]
pub struct LiveState {
    pub seq: i64,
    pid: u32,
    routes: BTreeMap<i64, LiveRoute>,
    /// trace 淘汰后仍记住已确认的辅助会话，避免完成卡再次出现。
    auxiliary_sessions: HashSet<String>,
    /// 按输入折叠履历所需的轮次元数据；网关重启清空（route_id 空间随之重置）。
    turn_history: BTreeMap<i64, TurnRecord>,
    pub activity: Activity,
    pub names: HashMap<String, String>,
    /// 同一 Desktop 会话的历史 CLI ID 映射到当前规范 ID；只来自标题元数据里的显式关联。
    /// 显示名相同但无此关联的独立会话不会被合并。
    pub aliases: HashMap<String, String>,
    /// 子 Agent 可读短标签，键是精确的 (父会话, 子 ID)。
    pub child_labels: HashMap<(String, String), String>,
    pub error: Option<String>,
}

impl LiveState {
    /// 合并增量，服务重启或序号回退时清理旧数据。
    pub fn update(&mut self, update: TraceUpdate) {
        if self.pid != update.pid || update.seq < self.seq {
            self.routes.clear();
            self.turn_history.clear();
        }
        self.pid = update.pid;
        self.seq = update.seq;
        let ids: HashSet<_> = update.ids.into_iter().collect();
        self.routes.retain(|id, _| ids.contains(id));
        for route in update.routes {
            if desktop_auxiliary(&route) && !route.session.is_empty() {
                self.auxiliary_sessions.insert(route.session.clone());
            }
            self.remember_turn(&route);
            self.routes.insert(route.id, route);
        }
        while self.turn_history.len() > TURN_HISTORY_CAP {
            self.turn_history.pop_first();
        }
    }

    /// 记录用户输入（轮）内的请求序号；标题生成/压缩等辅助调用自成一个合成轮，
    /// 与真实输入无关，不参与折叠，否则会把标题行并进用户输入组。
    fn remember_turn(&mut self, route: &LiveRoute) {
        if !route.kind.is_empty() || desktop_auxiliary(route) {
            return;
        }
        let Some(rule) = route.rule.as_ref() else { return };
        if rule.compact {
            return;
        }
        let Some(turn) = rule.turn else { return };
        let time_ms = route.time.as_deref()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.timestamp_millis())
            .unwrap_or_default();
        self.turn_history.insert(route.id, TurnRecord {
            session: route.session.clone(),
            subagent: route.subagent_id.trim().to_owned(),
            turn,
            held: rule.held || rule.grown || rule.waits,
            intent: rule.classified.intent.trim().to_owned(),
            after: rule.classified.after.trim().to_owned(),
            sure: rule.classified.sure,
            fallback: rule.classified.fallback.trim().to_owned(),
            time_ms,
        });
    }

    /// 用活跃请求覆盖展示，不改账本中的次数、费用与当日 Token。
    pub fn apply(&self, view: &mut RouteSnapshot) {
        // Desktop 起标题等辅助任务会用临时 fork ID 发请求，但没有父级关联。
        // 不把它画成独立会话；账本与来源统计仍保留真实消耗。
        let auxiliary_sessions: HashSet<_> = self.routes.values()
            .filter(|route| desktop_auxiliary(route)).map(|route| route.session.as_str()).collect();
        view.sessions.retain(|session| !auxiliary_sessions.contains(session.id.as_str())
            && !self.auxiliary_sessions.contains(&session.id));
        // 账本卡没有完整链，按日志 route_id 复用同一条已结束 trace；只补空链，
        // 不改日志算出的 time/updatedAt。活跃卡稍后由自身选中尝试单独计算。
        for session in &mut view.sessions {
            for card in session.routes.iter_mut().chain(session.completed_subagents.iter_mut())
                .filter(|card| card.route_groups.is_empty()) {
                if let Some(chain) = card.route_id.and_then(|id| self.routes.get(&id))
                    .map(trace_chain).filter(|chain| !chain.is_empty()) {
                    card.route_groups = chain;
                }
            }
        }
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
            if route.is_some_and(|r| r.kind == "routing" || desktop_auxiliary(r)) {
                continue;
            }
            // concurrency 不含子 Agent 身份；等 trace 到齐再挂卡，不能把未知请求先当根请求。
            if route.is_none() {
                continue;
            }
            let child = route.map(|r| r.subagent_id.trim()).filter(|id| !id.is_empty());
            let session_id = if active.session.is_empty() {
                route.map(|r| r.session.as_str()).unwrap_or("")
            } else {
                &active.session
            };
            // 没有会话身份的请求无法正确归属，实时阶段也不显示。
            if session_id.is_empty() {
                continue;
            }
            if let Some(route) = route {
                // activity 与 trace 的身份不一致且没有显式别名时，不猜测归属。
                if !route.session.is_empty()
                    && (active.session.is_empty() || self.canonical(&active.session) == self.canonical(&route.session))
                    && !route.rule.as_ref().is_some_and(|rule| rule.compact) {
                    view.decisions.push(self.decision_view(route, active, session_id));
                }
            }
            let started = DateTime::parse_from_rfc3339(&active.started).ok();
            let time = started.map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "--:--:--".into());
            let attempt = route.and_then(|r| r.tries.as_deref()).unwrap_or_default().last();
            let settling = route.is_some_and(|r| r.done);
            let retrying = !settling && attempt.is_some_and(|a| a.done);
            let first_response_seen = route.is_some_and(|r| r.first_text.is_some())
                || attempt.is_some_and(|a| a.first_text.is_some());
            // Magpie 不实时发布首字；15 秒只表示首字状态仍未确认，不声称上游没有输出。
            let slow_first_response = !settling && !retrying && active.phase == "upstream"
                && active.ms >= FIRST_RESPONSE_WAIT_MS && !first_response_seen;
            let waiting = route.is_none() || retrying || slow_first_response
                || matches!(active.phase.as_str(), "slot" | "rpm");
            let phase = if waiting { "waiting" } else { "running" };
            let detail = if route.is_none() {
                "请求已开始 · 等待路由信息"
            } else if settling {
                "请求仍活跃 · 等待请求结束"
            } else if retrying {
                "请求中 · 等待路由切换"
            } else {
                match active.phase.as_str() {
                    "slot" => "等待账号并发空位",
                    "rpm" => "等待请求速率额度",
                    "upstream" if slow_first_response => "请求超过 15 秒 · 首字状态未确认",
                    "upstream" if first_response_seen => "请求中 · 已收到上游正文",
                    "upstream" => "请求中 · 等待或接收上游流",
                    _ => "路由处理中",
                }
            };
            let candidate = route.and_then(|r| attempt.and_then(|a| selected_candidate(r, a)));
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
            let previous = if route.is_none() {
                None
            } else if let Some(child_id) = child {
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
                history_session: route.filter(|r| r.session != session_id).map(|r| r.session.clone()),
                session_label: label.clone(), account: account.into(), model: model.into(), provider: provider.into(),
                effort: effort.into(), state: phase.into(), detail: detail.into(), error: None, time,
                current: true, route_id: (active.route > 0).then_some(active.route),
                tokens: previous.and_then(|(tokens, _)| tokens),
                tokens_per_second: previous.and_then(|(_, speed)| speed),
                metrics_are_previous: previous.is_some(),
                subagent_id: child.map(str::to_owned),
                subagent_label: child.map(|id| self.child_label(session_id, id)),
                route_groups: route.map(|r| route_chain(r, attempt, candidate)).unwrap_or_default(),
                updated_at: started.map(|start| updated_at(start, active.ms)),
                note: note.clone(),
            };
            if child.is_some() {
                child_sessions.insert(session_id.to_owned());
                // 子请求并进父会话，不新开一行，所以不会改写父会话已有的网关密钥。
                if let Some(session) = view.sessions.iter_mut().find(|s| s.id == session_id) {
                    if let Some(existing) = session.subagents.iter_mut().find(|item| item.subagent_id == card.subagent_id) {
                        if card.route_id > existing.route_id { *existing = card; }
                    } else {
                        session.subagents.push(card);
                    }
                } else {
                    view.sessions.push(SessionView { id: session_id.into(), label, timestamp_ms: started.map(|t| t.timestamp_millis()).unwrap_or(0),
                        latest_sequence: 0, agent: display_agent(&active.agent), request: String::new(),
                        routes: Vec::new(), subagents: vec![card], completed_subagents: Vec::new(), subagent_history: HashMap::new(), quota_routes: Vec::new(),
                        notes: [note, second], daily: DailyStats::default(), caller_key_id: String::new(), caller_key_name: String::new() });
                }
                continue;
            }
            if let Some(session) = view.sessions.iter_mut().find(|s| s.id == session_id) {
                if active_sessions.insert(session_id.to_owned()) {
                    // 父会话自己的进行中请求直接盖住同会话更早的根卡片，不再并列旧的失败或取消尝试。
                    session.routes.clear();
                    session.agent = display_agent(&active.agent);
                    session.request = active.model.clone();
                    session.notes = [note, second];
                }
                session.routes.insert(0, card);
            } else {
                active_sessions.insert(session_id.to_owned());
                // 进行中请求的 usage 记录尚未写入，网关密钥还不可知。沿用该会话已完成记录的密钥，
                // 不能留空：空密钥在来源列表里会被显示成「本机」，请求一完成又跳回真实密钥。
                let (caller_key_id, caller_key_name) = known_caller(view, session_id);
                view.sessions.push(SessionView { id: session_id.into(), label, timestamp_ms: started.map(|t| t.timestamp_millis()).unwrap_or(0),
                    latest_sequence: 0, agent: display_agent(&active.agent), request: active.model.clone(),
                    routes: vec![card], subagents: Vec::new(), completed_subagents: Vec::new(), subagent_history: HashMap::new(), quota_routes: Vec::new(),
                    notes: [note, second], daily: DailyStats::default(), caller_key_id, caller_key_name });
            }
        }
        for route in self.routes.values().filter(|route| route.done && route.kind.is_empty()
            && !route.session.is_empty() && !desktop_auxiliary(route)
            && !route.rule.as_ref().is_some_and(|rule| rule.compact)) {
            if view.decisions.iter().any(|decision| decision.id == format!("{}:{}:{}:{}", self.pid, route.id, route.session, route.subagent_id)) {
                continue;
            }
            let active = Active { session: route.session.clone(), ..Active::default() };
            let mut decision = self.decision_view(route, &active, &route.session);
            decision.active = false;
            view.decisions.push(decision);
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
                if let Some(id) = card.subagent_id.as_deref() {
                    card.subagent_label = Some(self.child_label(&session.id, id));
                }
            }
            for card in &mut session.completed_subagents {
                card.session_label = session.label.clone();
                if let Some(id) = card.subagent_id.as_deref() {
                    card.subagent_label = Some(self.child_label(&session.id, id));
                }
            }
        }
        // 稳定排序保持原完成顺序，活跃会话始终优先可见。
        view.sessions.sort_by_key(|s| !(active_sessions.contains(&s.id) || child_sessions.contains(&s.id)));
        let aliases_changed = self.suppress_alias_roots(view);
        self.fold_model_usage(view);
        if let Some(session) = view.sessions.first() {
            view.notes = session.notes.clone();
        }
        // 别名行被隐藏后，活跃计数仍按实际可见会话统计，避免“N 个请求中”虚高。
        let visible: HashSet<_> = view.sessions.iter().map(|session| session.id.as_str()).collect();
        let shown = active_sessions.union(&child_sessions).filter(|id| visible.contains(id.as_str())).count();
        if shown > 0 && view.state != "warning" {
            view.state = "ready".into();
            view.watch_label = format!("{shown} 个会话请求中");
        } else if aliases_changed && shown == 0 && view.state != "warning" && view.state != "missing" {
            view.watch_label = if view.sessions.is_empty() { "等待今日记录".into() } else { "当日记录 · 只读监视".into() };
        }
        if let Some(error) = &self.error {
            view.state = "warning".into();
            view.watch_label = "实时连接中断 · 保留完成记录".into();
            view.error = Some(error.clone());
        }
    }

    /// 单个活跃主请求或子请求的实时决策，不混入内部分类器请求。
    fn decision_view(&self, route: &LiveRoute, active: &Active, session: &str) -> DecisionView {
        let stages = decision_stages(route);
        let rules = stages.iter().filter_map(|stage| stage.rule.clone()).collect();
        let tries = route.tries.as_deref().unwrap_or_default();
        let terminal = tries.last();
        let candidate = terminal.and_then(|attempt| selected_candidate(route, attempt));
        let attempts = tries.iter().enumerate().map(|(index, attempt)| {
            let (chain, incomplete) = decision_attempt_chain(route, attempt);
            DecisionAttemptView {
                ordinal: index as i64 + 1,
                model: attempt.model.trim().to_owned(),
                effort: nonempty(&attempt.effort),
                state: if !attempt.done { "running" } else { decision_status(attempt.status) },
                group_chain: chain,
                path_incomplete: incomplete,
            }
        }).collect();
        let started_ms = route.time.as_deref().and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .or_else(|| DateTime::parse_from_rfc3339(&active.started).ok()).map(|value| value.timestamp_millis());
        let (state, phase, wait_reason) = if route.done {
            (decision_status(route.status), None, None)
        } else {
            let phase = decision_phase(active);
            (history_phase(route, Some(active)), Some(phase), decision_wait_reason(route, active))
        };
        DecisionView {
            id: format!("{}:{}:{}:{}", self.pid, route.id, route.session, route.subagent_id),
            session: session.into(),
            label: self.name(session),
            subagent: (!route.subagent_id.trim().is_empty()).then(|| self.child_label(session, &route.subagent_id)),
            subagent_id: nonempty(&route.subagent_id),
            started_ms,
            turn_key: route.rule.as_ref().and_then(|rule| rule.turn.filter(|turn| *turn > 0)).map(|turn| {
                let group = route.group.as_ref().map(|group| if group.id.is_empty() { &group.reference } else { &group.id });
                serde_json::json!([self.pid, route.session, route.subagent_id, group, turn]).to_string()
            }),
            user_turn: route.rule.as_ref().and_then(|rule| rule.turn.filter(|turn| *turn > 0)),
            continuation: route.rule.as_ref().is_some_and(|rule| rule.held || rule.waits || rule.grown),
            active: true,
            state,
            wait_reason,
            phase,
            rules,
            route_groups: route_chain(route, terminal, candidate),
            stages,
            attempts,
            candidate_plan: candidate_plan(route),
        }
    }

    /// 同一 Desktop 会话可能轮换 CLI ID：当前 ID 成功后，历史 ID 的取消/失败根卡
    /// 仍在账本里各自成行。只按标题元数据的显式关联归并，不以显示名相同推断。
    /// 当前规范 ID 的新成功根或进行中根是权威展示，会盖住历史别名里更早的根；
    /// 历史别名自己的进行中根只覆盖其它历史别名，不反压当前 ID 的成功。
    /// 没有其它可见内容的空别名行被移除，额度来源并入存活行，统计口径不变。
    fn suppress_alias_roots(&self, view: &mut RouteSnapshot) -> bool {
        if self.aliases.is_empty() {
            return false;
        }
        let mut groups = HashMap::<String, Vec<usize>>::new();
        for (index, session) in view.sessions.iter().enumerate() {
            groups.entry(self.canonical(&session.id).to_owned()).or_default().push(index);
        }
        let mut hidden: HashMap<usize, HashSet<String>> = HashMap::new();
        for (canonical, members) in groups.iter().filter(|(_, members)| members.len() > 1) {
            let canonical_index = members.iter().copied().find(|index| view.sessions[*index].id == *canonical);
            let priors: Vec<_> = members.iter().copied()
                .filter(|index| Some(*index) != canonical_index).collect();
            // 当前 ID 的成功/活跃根：按完成时刻覆盖历史别名的更早根。
            let canonical_marks: Vec<_> = canonical_index.into_iter()
                .flat_map(|index| view.sessions[index].routes.iter())
                .filter(|card| matches!(card.state.as_str(), "success" | "recovered" | "running" | "waiting"))
                .filter_map(root_mark).collect();
            for &prior in &priors {
                for card in &view.sessions[prior].routes {
                    if canonical_marks.iter().any(|mark| root_mark(card).is_some_and(|time| time < *mark)) {
                        hidden.entry(prior).or_default().insert(card.id.clone());
                    }
                }
            }
            // 历史别名之间的进行中根覆盖更早的根；成功行是另一条结论，不在此被删除。
            for &source in &priors {
                let active: Vec<_> = view.sessions[source].routes.iter()
                    .filter(|card| matches!(card.state.as_str(), "running" | "waiting"))
                    .filter_map(root_mark).collect();
                for &target in &priors {
                    if target == source {
                        continue;
                    }
                    for card in &view.sessions[target].routes {
                        if !matches!(card.state.as_str(), "success" | "recovered" | "running" | "waiting")
                            && active.iter().any(|mark| root_mark(card).is_some_and(|time| time < *mark)) {
                            hidden.entry(target).or_default().insert(card.id.clone());
                        }
                    }
                }
            }
        }
        if hidden.is_empty() {
            return false;
        }
        for (index, ids) in &hidden {
            view.sessions[*index].routes.retain(|card| !ids.contains(&card.id));
        }
        // 先收集历史别名的额度来源，再移除空行；统计与子 Agent 身份不搬移。
        // 即使别名行仍有子 Agent 内容而保留，额度来源也归到当前 ID，避免账号额度随旧行漂移。
        let mut quota_moves = Vec::new();
        let mut removed = HashSet::new();
        for members in groups.values() {
            if members.len() < 2 {
                continue;
            }
            let canonical = self.canonical(&view.sessions[members[0]].id).to_owned();
            let Some(keeper) = members.iter().copied().find(|index| view.sessions[*index].id == canonical) else { continue };
            for index in members.iter().copied().filter(|index| *index != keeper) {
                let session = &view.sessions[index];
                if hidden.contains_key(&index) {
                    quota_moves.push((keeper, index, session.quota_routes.clone()));
                }
                if hidden.contains_key(&index) && session.routes.is_empty()
                    && session.subagents.is_empty() && session.completed_subagents.is_empty() {
                    removed.insert(index);
                }
            }
        }
        // 有子 Agent 内容的别名行不会被移除，额度来源仍并入当前 ID，旧行不再保留副本。
        for (keeper, source, quota_routes) in quota_moves {
            for card in quota_routes {
                let duplicate = view.sessions[keeper].quota_routes.iter().position(|existing| {
                    existing.provider.eq_ignore_ascii_case(&card.provider) && existing.account == card.account
                });
                match duplicate {
                    // 同一账号以日志追加顺序的新旧为准，保留较新的一条。
                    Some(index) if card.updated_at.as_deref() > view.sessions[keeper].quota_routes[index].updated_at.as_deref() => {
                        view.sessions[keeper].quota_routes[index] = card;
                    }
                    Some(_) => {}
                    None => view.sessions[keeper].quota_routes.push(card),
                }
            }
            if !removed.contains(&source) {
                view.sessions[source].quota_routes.clear();
            }
        }
        if !removed.is_empty() {
            view.sessions = view.sessions.drain(..).enumerate()
                .filter_map(|(index, session)| (!removed.contains(&index)).then_some(session)).collect();
        }
        true
    }

    /// 历史 CLI ID 归到标题元数据声明的当前 ID；没有显式关联时保持自身。
    fn canonical<'a>(&'a self, id: &'a str) -> &'a str {
        self.aliases.get(id).map(String::as_str).unwrap_or(id)
    }

    /// 把历史 CLI ID 的模型用量归到当前规范会话，前端选中展示 ID 时不会漏计。
    fn fold_model_usage(&self, view: &mut RouteSnapshot) {
        if self.aliases.is_empty() { return; }
        let mut folded: Vec<crate::usage::ModelUsage> = Vec::new();
        let mut index = HashMap::<(String, String, String, String, String, String, String), usize>::new();
        for row in view.model_usage.drain(..) {
            let session = self.canonical(&row.session).to_owned();
            let key = (session.clone(), row.agent.clone(), row.caller_key_id.clone(), row.caller_key_name.clone(),
                row.model.clone(), row.effort.clone(), row.outcome.clone());
            if let Some(existing) = index.get(&key).copied() {
                folded[existing].requests += row.requests;
                folded[existing].tokens = folded[existing].tokens.saturating_add(row.tokens);
                folded[existing].unknown_token_requests += row.unknown_token_requests;
                folded[existing].output_tokens = folded[existing].output_tokens.saturating_add(row.output_tokens);
                folded[existing].generation_ms += row.generation_ms;
                folded[existing].speed_samples += row.speed_samples;
            } else {
                index.insert(key, folded.len());
                folded.push(crate::usage::ModelUsage { session, ..row });
            }
        }
        view.model_usage = folded;
    }

    /// 未找到标题时明确未命名；UUID 只保留在数据身份与悬停提示中。
    fn name(&self, id: &str) -> String {
        self.names.get(id).cloned().unwrap_or_else(|| if id.is_empty() { "未标记会话".into() } else { "未命名会话".into() })
    }

    /// 有元数据短标签时用它；否则退回「子 Agent」加 ID 前缀。
    fn child_label(&self, session: &str, child: &str) -> String {
        self.child_labels.get(&(session.to_owned(), child.to_owned()))
            .cloned()
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| fallback_child_label(child))
    }
}

/// 履历查询的实时请求摘要；只有 trace 已确认会话/子身份的请求才会出现。
#[derive(Clone, Debug)]
pub struct LiveHistoryRoute {
    pub route_id: i64,
    /// trace 侧的原始会话 ID。
    pub session: String,
    /// 子 Agent ID；根请求为空。
    pub subagent: String,
    /// trace 的请求开始时间；不可解析时为 None，合并时无法与日志对齐。
    pub started_ms: Option<i64>,
    /// 终态尝试的发送模型与强度。
    pub model: String,
    pub effort: String,
    /// 候选归属的账号与供应商；无法唯一认领时为空。
    pub account: String,
    pub provider: String,
    /// 响应报告模型（终态尝试优先，回退路由级）。
    pub served: String,
    /// 已确认的组链；复用 route_chain 的歧义处理，不拼候选全集。
    pub route_groups: Vec<String>,
    pub rule_target: Option<String>,
    pub rule_effort: Option<String>,
    /// 本轮命中的意图摘要与置信度；仅实时 trace 保留期内可用。
    pub rule_intent: Option<String>,
    pub rule_sure: Option<f64>,
    /// trace 记录的终态 HTTP 状态；与日志状态冲突说明不是同一请求。
    pub status: Option<i64>,
    /// running / waiting / awaiting_usage（done 但日志未到）。
    pub phase: &'static str,
    /// 只有近期的 done trace 才在日志未到时单独成行；更早的只用于补全已匹配日志行。
    pub eligible_overlay: bool,
}

/// 履历查询的会话范围：根查询给显式别名家族，子查询精确到原始父会话＋子 ID。
#[derive(Debug)]
pub struct HistoryScope {
    /// 日志按原始会话 ID 匹配的集合（查询 ID 与其显式别名双向闭包）。
    pub family: HashSet<String>,
    pub routes: Vec<LiveHistoryRoute>,
    /// 当前范围内的轮次索引（route_id → 输入序号等）；trace 淘汰后仍可折叠日志行。
    pub turns: HashMap<i64, TurnRecord>,
}

/// done 的 trace 若一直等不到 usage 记录，只保留最近 10 分钟，避免永久挂起。
const HISTORY_SETTLE_MS: i64 = 10 * 60_000;

impl LiveState {
    /// 按需提取履历的实时部分；不克隆整个 routes，也不把未确认身份的请求当根请求。
    pub fn history_scope(&self, session: &str, subagent: Option<&str>) -> HistoryScope {
        // 显式别名家族：历史 CLI ID → 当前规范 ID，双向闭包；同名标题不建立关联。
        let canonical = self.aliases.get(session).cloned();
        let mut family = HashSet::new();
        family.insert(session.to_owned());
        if let Some(canonical) = &canonical {
            family.insert(canonical.clone());
        }
        for (history_id, target) in &self.aliases {
            if target == session || Some(target) == canonical.as_ref() {
                family.insert(history_id.clone());
            }
        }
        let now_ms = Local::now().timestamp_millis();
        let mut routes = Vec::new();
        for route in self.routes.values() {
            if route.kind == "routing" || desktop_auxiliary(route) {
                continue;
            }
            let child = route.subagent_id.trim();
            match subagent {
                // 子履历严格按原始父会话 ID 匹配，不把别名家族里其它会话的子请求搬过来。
                Some(wanted) if route.session != session || child != wanted => continue,
                // 根履历覆盖显式别名家族，并排除子请求。
                None if !family.contains(&route.session) || !child.is_empty() => continue,
                _ => {}
            }
            if route.session.is_empty() {
                continue;
            }
            let started_ms = route.time.as_deref()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|time| time.timestamp_millis());
            // done 的 trace 始终保留，供已匹配的日志行补组链与规则；
            // 只有近期的才在日志未到时单独成行，避免陈旧 trace 永久挂起。
            let terminal = route.tries.as_deref().unwrap_or_default().last();
            let candidate = terminal.and_then(|attempt| selected_candidate(route, attempt));
            let activity = self.activity.active.iter().find(|active| active.route == route.id
                && (active.session.is_empty() || active.session == route.session));
            let eligible_overlay = if route.done {
                started_ms.is_some_and(|start| now_ms.saturating_sub(start) <= HISTORY_SETTLE_MS)
            } else {
                activity.is_some()
            };
            routes.push(LiveHistoryRoute {
                route_id: route.id,
                session: route.session.clone(),
                subagent: child.to_owned(),
                started_ms,
                model: terminal.map(|attempt| attempt.model.as_str()).filter(|m| !m.is_empty()).unwrap_or_default().to_owned(),
                effort: terminal.map(|attempt| attempt.effort.as_str()).filter(|e| !e.is_empty()).unwrap_or_default().to_owned(),
                account: candidate.map(|c| c.who.as_str()).filter(|w| !w.is_empty()).unwrap_or_default().to_owned(),
                provider: candidate.map(|c| c.provider.as_str()).unwrap_or_default().to_owned(),
                served: terminal.map(|a| a.served.as_str()).filter(|s| !s.is_empty()).unwrap_or(route.served.as_str()).to_owned(),
                route_groups: route_chain(route, terminal, candidate),
                rule_target: route.rule.as_ref().map(|rule| rule.use_target.as_str()).filter(|t| !t.is_empty()).map(str::to_owned),
                rule_effort: route.rule.as_ref().map(|rule| rule.pick.as_str()).filter(|p| !p.is_empty()).map(str::to_owned),
                rule_intent: route.rule.as_ref().map(|rule| rule.classified.intent.as_str()).filter(|i| !i.is_empty()).map(str::to_owned),
                rule_sure: route.rule.as_ref().and_then(|rule| rule.classified.sure),
                status: route.status,
                phase: history_phase(route, activity),
                eligible_overlay,
            });
        }
        routes.sort_by(|a, b| b.route_id.cmp(&a.route_id));
        // 轮次索引按同一范围规则过滤：根查询覆盖别名家族的根请求，子查询精确到该子线。
        let turns = self
            .turn_history
            .iter()
            .filter(|(_, record)| match subagent {
                Some(wanted) => {
                    record.session == session && record.subagent == wanted
                }
                None => family.contains(&record.session) && record.subagent.is_empty(),
            })
            .map(|(id, record)| (*id, record.clone()))
            .collect();
        HistoryScope { family, routes, turns }
    }
}

/// 履历实时行的阶段：done=等待用量；其余按 concurrency 相同口径区分等待与运行。
fn history_phase(route: &LiveRoute, activity: Option<&Active>) -> &'static str {
    if route.done {
        return "awaiting_usage";
    }
    let Some(active) = activity else { return "running"; };
    let attempt = route.tries.as_deref().unwrap_or_default().last();
    let retrying = attempt.is_some_and(|attempt| attempt.done);
    let first_seen = route.first_text.is_some()
        || attempt.is_some_and(|attempt| attempt.first_text.is_some());
    let slow_first =
        active.phase == "upstream" && active.ms >= FIRST_RESPONSE_WAIT_MS && !first_seen;
    if retrying || slow_first || matches!(active.phase.as_str(), "slot" | "rpm") {
        "waiting"
    } else {
        "running"
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn decision_status(status: Option<i64>) -> &'static str {
    match status {
        Some(status) if (200..300).contains(&status) => "success",
        Some(499) => "cancelled",
        Some(status) if status >= 400 => "failed",
        _ => "unknown",
    }
}

fn decision_rule(group: &str, rule: &TraceRule) -> DecisionRuleView {
    let classified = &rule.classified;
    let state = if rule.waits { "missing" } else if rule.grown { "revised" }
        else if rule.held { "held" } else if !classified.by.is_empty() || !classified.intent.is_empty() { "classified" }
        else { "unknown" };
    let (intent, intent_source) = if !classified.intent.trim().is_empty() {
        (nonempty(&classified.intent), if classified.fallback == "previous" { "previous" } else { "measured" })
    } else if !classified.after.trim().is_empty() {
        (nonempty(&classified.after), "after")
    } else { (None, "none") };
    DecisionRuleView {
        group: group.into(),
        classifier: nonempty(&classified.by),
        duration_ms: classified.ms.filter(|ms| *ms >= 0 && !rule.held && !rule.waits),
        cached: classified.cached && !rule.held && !rule.waits,
        intent,
        intent_source,
        confidence: classified.sure.filter(|value| value.is_finite() && (0.0..=1.0).contains(value)),
        target: nonempty(&rule.use_target),
        effort: nonempty(&rule.pick),
        n: rule.n,
        turn: rule.turn,
        unready: rule.unready,
        instead: nonempty(&rule.instead),
        state,
    }
}

/// 网关阶段白名单；slot / rpm / upstream 原样保留，其余一律 unknown。
fn decision_phase(active: &Active) -> &'static str {
    match active.phase.as_str() {
        "slot" => "slot",
        "rpm" => "rpm",
        "upstream" => "upstream",
        _ => "unknown",
    }
}

/// 活跃请求的等待原因，与 history_phase 的 15 秒口径一致；非等待时为 null。
fn decision_wait_reason(route: &LiveRoute, active: &Active) -> Option<&'static str> {
    if history_phase(route, Some(active)) != "waiting" {
        return None;
    }
    let attempt = route.tries.as_deref().unwrap_or_default().last();
    if attempt.is_some_and(|attempt| attempt.done) {
        return Some("retry");
    }
    let first_seen = route.first_text.is_some()
        || attempt.is_some_and(|attempt| attempt.first_text.is_some());
    if active.phase == "upstream" && active.ms >= FIRST_RESPONSE_WAIT_MS && !first_seen {
        return Some("first_response");
    }
    match active.phase.as_str() {
        "slot" => Some("slot"),
        "rpm" => Some("rpm"),
        _ => Some("unknown"),
    }
}

/// 已知选择策略只认结构字符串；其它值（含配置内容）一律不外发。
fn known_routing(value: &str) -> Option<String> {
    matches!(value, "order" | "rotate" | "usage").then(|| value.to_owned())
}

/// 根组与嵌套组按上游记录顺序组成判定上下文，不表示实际执行路径。
/// 嵌套组只按 id/ref 精确关联；父子关系必须有证据才算确认，同名不匹配。
fn decision_stages(route: &LiveRoute) -> Vec<DecisionStageView> {
    let Some(group) = route.group.as_ref().filter(|group| !group.name.trim().is_empty()) else {
        return Vec::new();
    };
    let mut stages = vec![DecisionStageView {
        key: "root".into(),
        group: DecisionGroupView {
            id: nonempty(&group.id), reference: nonempty(&group.reference),
            name: group.name.clone(), routing: known_routing(&group.routing),
        },
        rule: route.rule.as_ref().map(|rule| decision_rule(&group.name, rule)),
        context: "recorded",
        confirmed: true,
    }];
    // 只跟踪从根组开始、父子关系连续有证据的那一条；断掉之后不再确认。
    let mut parent = group.id.trim().to_owned();
    let mut lineage = !parent.is_empty();
    for (index, nested) in route.nested.iter().enumerate() {
        let Some(subgroup) = exact_subgroup(group, &nested.group) else { continue };
        let linked = lineage && !subgroup.parent.is_empty() && subgroup.parent == parent && !subgroup.id.trim().is_empty();
        if !linked {
            lineage = false;
        }
        let identity = subgroup.id.trim();
        let reference = subgroup.reference.trim();
        stages.push(DecisionStageView {
            key: format!("nested:{}:{}:{}", index, identity, reference),
            group: DecisionGroupView {
                id: nonempty(identity), reference: nonempty(reference),
                name: subgroup.name.clone(), routing: known_routing(&subgroup.routing),
            },
            rule: nested.rule.as_ref().map(|rule| decision_rule(&subgroup.name, rule)),
            context: "recorded",
            confirmed: linked,
        });
        if linked {
            parent = identity.to_owned();
        }
    }
    stages
}

/// 子组身份只认 id 或 ref；身份缺失、重复或组名不唯一时不关联。
fn exact_subgroup<'a>(group: &'a RouteGroup, identity: &str) -> Option<&'a RouteSubgroup> {
    let identity = identity.trim();
    if identity.is_empty() {
        return None;
    }
    let mut matched = group.subs.iter()
        .filter(|subgroup| subgroup.id == identity || subgroup.reference == identity);
    let subgroup = matched.next()?;
    matched.next().is_none().then_some(subgroup)
}

/// 一次尝试自己确认的组链：入口组加上该尝试候选 via 的公共前缀。
/// 认不出候选、父子证据缺失、层级未知或多条路径只剩公共前缀时，都标记不能唯一确认。
fn decision_attempt_chain(route: &LiveRoute, attempt: &Attempt) -> (Vec<DecisionGroupView>, bool) {
    let Some(group) = route.group.as_ref().filter(|group| !group.name.trim().is_empty()) else {
        return (Vec::new(), false);
    };
    let root = DecisionGroupView {
        id: nonempty(&group.id), reference: nonempty(&group.reference),
        name: group.name.clone(), routing: known_routing(&group.routing),
    };
    // 有尝试但认不出唯一候选：路径无法确认，只保留入口组。
    let Some(candidate) = selected_candidate(route, attempt) else {
        return (vec![root], true);
    };
    let rows: Vec<_> = route.order.as_deref().unwrap_or_default().iter()
        .filter(|row| row.id == attempt.id && row.model == candidate.model && row.fixed == attempt.fixed
            && row.provider == candidate.provider && row.who == candidate.who)
        .collect();
    let mut paths = Vec::new();
    let mut incomplete = false;
    for row in &rows {
        let mut path = Vec::new();
        let mut parent = group.id.trim();
        for via in &row.via {
            let Some(subgroup) = exact_subgroup(group, via) else {
                incomplete = true;
                break;
            };
            if subgroup.name.trim().is_empty() {
                incomplete = true;
                break;
            }
            // 任一侧没有身份，或 in 与已确认的父组不符，都不能核验父子关系。
            if parent.is_empty() || subgroup.parent.is_empty() || subgroup.parent != parent || subgroup.id.trim().is_empty() {
                incomplete = true;
                break;
            }
            path.push(DecisionGroupView {
                id: nonempty(&subgroup.id), reference: nonempty(&subgroup.reference),
                name: subgroup.name.clone(), routing: known_routing(&subgroup.routing),
            });
            parent = subgroup.id.trim();
        }
        paths.push(path);
    }
    if paths.is_empty() {
        return (vec![root], true);
    }
    let mut common = paths.remove(0);
    for path in &paths {
        let length = common.iter().zip(path).take_while(|(left, right)| {
            left.id == right.id && left.reference == right.reference
        }).count();
        // 其它路径比公共前缀更长或分叉：前缀之后不能唯一确认。
        if length < path.len() || length < common.len() {
            incomplete = true;
        }
        common.truncate(length);
    }
    let mut chain = vec![root];
    chain.extend(common);
    (chain, incomplete)
}

/// 计划尝试顺序最多展开 12 项；只保留序号、模型与固定强度。
fn candidate_plan(route: &LiveRoute) -> Option<DecisionCandidatePlan> {
    let order = route.order.as_deref().filter(|order| !order.is_empty())?;
    let total = order.len() as i64;
    let shown = order.len().min(12);
    Some(DecisionCandidatePlan {
        items: order.iter().take(shown).enumerate().map(|(index, candidate)| DecisionCandidateItem {
            ordinal: index as i64 + 1,
            model: candidate.model.trim().to_owned(),
            fixed: nonempty(&candidate.fixed),
        }).collect(),
        total,
        omitted: total - shown as i64,
    })
}

/// 精确识别 Desktop 的临时辅助工作目录，普通 fork/未命名短请求不受影响。
fn desktop_auxiliary(route: &LiveRoute) -> bool {
    if route.agent != "claude-desktop" || !route.subagent_id.is_empty() { return false; }
    let folder = route.folder.replace('\\', "/");
    let parts: Vec<_> = folder.trim_end_matches('/').split('/').collect();
    parts.len() >= 2 && parts[parts.len() - 2] == "ccd-session-secrets"
        && parts.last().is_some_and(|name| name.strip_prefix("fork-").is_some_and(|id| !id.is_empty()))
}

/// 尝试 ID 是失败后休息的键，不是独立账号键。当前登录账号的键是 provider，
/// 同组里该账号的每个模型都会复用它；其余账号是 `provider@user`，多密钥是 `provider#key`。
/// 模型和成员原始固定强度共同定位；否则仅当 provider 与账号完全一致时认领。
/// 不用拟合后的 effort 反推 fixed，任一身份不同就不猜。
fn selected_candidate<'a>(route: &'a LiveRoute, attempt: &Attempt) -> Option<&'a Candidate> {
    if attempt.id.is_empty() { return None; }
    let candidates: Vec<_> = route.order.as_deref().unwrap_or_default().iter()
        .filter(|candidate| candidate.id == attempt.id && candidate.fixed == attempt.fixed).collect();
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

/// via 按路径顺序列出子组 id/ref，映射成组名后前置入口组。
/// 同一账号与模型可能同时出现在多个路径上，只展示这些候选路径的共同前缀；
/// 无法确认子组时仍保留 trace 明确给出的入口组，不把候选全集拼成实际路径。
fn route_chain(route: &LiveRoute, attempt: Option<&Attempt>, candidate: Option<&Candidate>) -> Vec<String> {
    let Some(group) = route.group.as_ref() else { return Vec::new(); };
    if group.name.trim().is_empty() { return Vec::new(); }
    let mut chain = vec![group.name.clone()];
    let Some(attempt) = attempt.filter(|attempt| !attempt.id.is_empty()) else { return chain; };
    let Some(candidate) = candidate else { return chain; };
    let mut paths = route.order.as_deref().unwrap_or_default().iter()
        .filter(|row| row.id == attempt.id && row.model == candidate.model && row.fixed == attempt.fixed
            && row.provider == candidate.provider && row.who == candidate.who)
        .map(|row| {
            let mut path = Vec::new();
            for via in &row.via {
                // 未知层级之后的路径不再确认，但已确认的父级仍有效。
                let Some(subgroup) = group.subs.iter().find(|sub| sub.id == *via || sub.reference == *via) else { break; };
                if subgroup.name.trim().is_empty() { break; }
                path.push(subgroup.name.clone());
            }
            path
        });
    if let Some(mut common) = paths.next() {
        for path in paths {
            let length = common.iter().zip(&path).take_while(|(left, right)| left == right).count();
            common.truncate(length);
        }
        chain.extend(common);
    }
    chain
}

/// 已结束的 trace 优先用被选中尝试；多次尝试无法确认时只展示入口组。
fn trace_chain(route: &LiveRoute) -> Vec<String> {
    let tries = route.tries.as_deref().unwrap_or_default();
    let attempt = tries.iter().rev().find(|attempt| attempt.picked && attempt.done)
        .or_else(|| tries.iter().rev().find(|attempt| attempt.picked))
        .or_else(|| (tries.len() == 1).then(|| &tries[0]));
    let candidate = attempt.and_then(|attempt| selected_candidate(route, attempt));
    route_chain(route, attempt, candidate)
}

/// 活跃卡的更新时间跟随 Magpie 报告的已用时长；start 不可解析时由调用方留空。
fn updated_at(start: DateTime<chrono::FixedOffset>, elapsed_ms: u64) -> String {
    (start + chrono::Duration::milliseconds(elapsed_ms.min(i64::MAX as u64) as i64))
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// 根卡的请求时序标记：完成卡是真实完成时刻，活跃卡是 start + 已用时长。
/// 同一 Desktop 会话跨 CLI ID 只比较这个时刻，旧快照或无更新时间时不隐藏。
fn root_mark(card: &RouteCard) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(card.updated_at.as_deref()?).ok()
}

/// 进行中请求还没有 usage 记录，网关密钥未知。只沿用这个会话自己已完成记录里的密钥；
/// 同名来源可能是另一台机器的另一个密钥，不能拿来填。没有记录就保持空字符串。
fn known_caller(view: &RouteSnapshot, session_id: &str) -> (String, String) {
    view.sessions.iter()
        .find(|session| session.id == session_id)
        .filter(|session| !session.caller_key_id.is_empty() || !session.caller_key_name.is_empty())
        .map(|session| (session.caller_key_id.clone(), session.caller_key_name.clone()))
        .unwrap_or_default()
}

/// 来源文案与日志口径一致。
fn display_agent(agent: &str) -> String {
    if agent == "claude-desktop" { "Claude Desktop".into() } else { agent.into() }
}

/// 没有元数据时只给通用标签和短 ID，不展示会话正文。
pub(crate) fn fallback_child_label(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("子 Agent {short}")
}

/// 子 Agent 元数据只取短任务名，不读取 prompt、正文或密钥。
#[derive(Clone, Default, Deserialize)]
struct ChildMeta {
    #[serde(default)]
    description: String,
    #[serde(default)]
    name: String,
}

/// 缓存单个子 Agent 元数据文件。短暂写坏时保留上次有效标签，删除则移除。
#[derive(Default)]
pub struct ChildTitles {
    files: HashMap<PathBuf, (std::time::SystemTime, u64, Option<String>)>,
    /// 父会话目录。有未知会话时才重建，并按标题刷新节拍重试。
    parents: HashMap<String, PathBuf>,
    retry_missing: bool,
}

impl ChildTitles {
    /// 只查当天快照里出现的父会话和子 ID。
    pub fn scan(&mut self, wanted: &HashSet<(String, String)>) -> HashMap<(String, String), String> {
        let root = std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".claude").join("projects"));
        self.scan_root(root.as_deref(), wanted)
    }

    /// 在指定 projects 根下按精确父会话和子 ID 读取短标签。
    pub fn scan_root(&mut self, root: Option<&Path>, wanted: &HashSet<(String, String)>) -> HashMap<(String, String), String> {
        let mut found = HashSet::new();
        let mut result = HashMap::new();
        let Some(root) = root.filter(|path| path.is_dir()) else {
            self.files.clear();
            self.parents.clear();
            self.retry_missing = false;
            return result;
        };
        let mut by_parent: HashMap<&str, Vec<&str>> = HashMap::new();
        for (parent, child) in wanted {
            if safe_session_id(parent) && safe_child_id(child) {
                by_parent.entry(parent).or_default().push(child);
            }
        }
        let unknown = by_parent.keys().any(|parent| !self.parents.contains_key(*parent));
        if unknown && !self.retry_missing {
            self.parents = index_parent_dirs(root);
            self.retry_missing = by_parent.keys().any(|parent| !self.parents.contains_key(*parent));
        } else if unknown {
            self.retry_missing = false;
        }
        for (parent, children) in by_parent {
            let Some(session_dir) = self.parents.get(parent).cloned() else { continue };
            if !session_dir.is_dir() {
                self.parents.remove(parent);
                self.retry_missing = false;
                continue;
            }
            for child in children {
                let path = session_dir.join("subagents").join(format!("agent-{child}.meta.json"));
                found.insert(path.clone());
                let Ok(meta) = fs::metadata(&path) else { continue };
                if !meta.is_file() || meta.len() > 64 * 1024 { continue; }
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if !self.files.get(&path).is_some_and(|(time, len, _)| *time == modified && *len == meta.len()) {
                    if let Ok(bytes) = fs::read(&path) {
                        if let Some(label) = serde_json::from_slice::<ChildMeta>(&bytes).ok().and_then(child_meta_label) {
                            self.files.insert(path.clone(), (modified, meta.len(), Some(label)));
                        } else if let Some(previous) = self.files.get(&path).and_then(|(_, _, label)| label.clone()) {
                            self.files.insert(path.clone(), (modified, meta.len(), Some(previous)));
                        }
                    }
                }
                if let Some(label) = self.files.get(&path).and_then(|(_, _, label)| label.clone()) {
                    result.insert((parent.to_owned(), child.to_owned()), label);
                }
            }
        }
        self.files.retain(|path, _| found.contains(path));
        result
    }
}

/// 建立父会话到目录的索引。同一 UUID 出现在不同目录时不猜测，整项留空。
fn index_parent_dirs(root: &Path) -> HashMap<String, PathBuf> {
    let mut parents: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut remember = |name: String, path: PathBuf| {
        let path = path.canonicalize().unwrap_or(path);
        parents.entry(name).and_modify(|existing| {
            if existing.as_ref().is_some_and(|old| old != &path) { *existing = None; }
        }).or_insert(Some(path));
    };
    let Ok(entries) = fs::read_dir(root) else { return HashMap::new() };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
        if let Some(name) = entry.file_name().to_str().filter(|name| safe_session_id(name)) {
            remember(name.to_owned(), entry.path());
        }
        let Ok(sessions) = fs::read_dir(entry.path()) else { continue };
        for session in sessions.flatten() {
            if !session.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
            if let Some(name) = session.file_name().to_str().filter(|name| safe_session_id(name)) {
                remember(name.to_owned(), session.path());
            }
        }
    }
    parents.into_iter().filter_map(|(name, path)| path.map(|path| (name, path))).collect()
}

/// 优先用短任务描述；只有元数据真有 name 字段时才用它。
fn child_meta_label(meta: ChildMeta) -> Option<String> {
    let description = meta.description.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = meta.name.split_whitespace().collect::<Vec<_>>().join(" ");
    (!description.is_empty()).then_some(description).or((!name.is_empty()).then_some(name))
}

/// 会话目录名只接受 UUID 形态，拒绝路径分隔。
fn safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

/// 文件名只接受子 ID 本身，拒绝路径分隔和父目录跳转。
fn safe_child_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// 从当日会话快照收集需要查标签的精确父子对。
pub fn wanted_children(view: &RouteSnapshot) -> HashSet<(String, String)> {
    let mut wanted = HashSet::new();
    for session in &view.sessions {
        for card in session.subagents.iter().chain(&session.completed_subagents).chain(session.subagent_history.values()) {
            if let Some(child) = card.subagent_id.as_deref().filter(|id| !id.is_empty()) {
                wanted.insert((session.id.clone(), child.to_owned()));
            }
        }
    }
    wanted
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
    /// 显式 CLI ID 关联；与显示名无关，只描述同一个 Desktop 会话的身份轮换。
    aliases: HashMap<String, String>,
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
        self.aliases = alias_links(self.files.values().map(|(_, _, title)| title));
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

    /// 最近一次扫描得到的显式 CLI 身份关联；只做展示归一，不改变账本或统计。
    pub fn aliases(&self) -> HashMap<String, String> {
        self.aliases.clone()
    }
}

/// 同一 Desktop 会话的显式 ID 关联：当前 CLI ID、未归档 ID、清空前的 ID 和
/// 历史 CLI ID 都归到 `cliSessionId`；没有当前 ID 时依次用未归档 ID 和
/// `local_` 会话 ID。一个 ID 被多个 Desktop 会话声明时视为冲突，不猜测归属；
/// 显示名不参与判断。
fn alias_links<'a>(titles: impl Iterator<Item = &'a DesktopTitle>) -> HashMap<String, String> {
    let mut claims = HashMap::<&str, HashSet<&str>>::new();
    let mut canonical_ids = HashSet::<&str>::new();
    let mut rows = Vec::new();
    for title in titles {
        let canonical = [&title.cli_session_id, &title.unarchived_cli_session_id].into_iter()
            .map(String::as_str).find(|id| !id.is_empty())
            .or_else(|| title.session_id.strip_prefix("local_"))
            .unwrap_or("");
        if canonical.is_empty() || !safe_session_id(canonical) {
            continue;
        }
        canonical_ids.insert(canonical);
        rows.push((canonical, title));
    }
    for (canonical, title) in rows {
        for alias in [&title.unarchived_cli_session_id, &title.pre_clear_cli_session_id]
            .into_iter().chain(title.prior_cli_session_ids.iter())
        {
            // 一个 ID 同时是另一个 Desktop 会话的当前 ID 时，说明身份已被重新分配给
            // 独立会话，不能再当历史别名归并。
            if alias.is_empty() || alias == canonical || !safe_session_id(alias) || canonical_ids.contains(alias.as_str()) {
                continue;
            }
            claims.entry(alias.as_str()).or_default().insert(canonical);
        }
    }
    claims.into_iter().filter_map(|(alias, canonicals)| {
        let mut canonicals = canonicals.into_iter();
        let canonical = canonicals.next()?;
        canonicals.next().is_none().then(|| (alias.to_owned(), canonical.to_owned()))
    }).collect()
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

    /// 完成卡从同 route_id 的 trace 补组链，但保留日志算出的完成时间；
    /// 没有同 id 的 trace 时保持空链并继续显示完成时间。
    #[test]
    fn completed_card_reuses_trace_chain_without_renewing_time() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let row = serde_json::json!({"route_id":7,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"one@example.com","provider":"codex","model":"gpt-test",
            "status":200,"in":10,"out":10,"ms":2000,"req":"group/smart-split"});
        ledger.ingest(row.to_string().as_bytes());
        let mut live = LiveState::default();
        live.routes.insert(7, LiveRoute { id: 7, session: "alpha".into(), done: true,
            group: Some(RouteGroup { name: "自动".into(), subs: vec![
                RouteSubgroup { id: "sub-a".into(), name: "干活".into(), ..Default::default() },
            ], ..Default::default() }),
            order: Some(vec![Candidate { id: "seat".into(), provider: "codex".into(),
                model: "gpt-test".into(), who: "one@example.com".into(), via: vec!["sub-a".into()], ..Default::default() }]),
            tries: Some(vec![Attempt { id: "seat".into(), model: "gpt-test".into(), picked: true, done: true, ..Default::default() }]),
            ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        let baseline_time = view.sessions[0].routes[0].updated_at.clone();
        live.apply(&mut view);
        let card = view.sessions[0].routes.iter()
            .find(|card| card.route_id == Some(7) && !card.id.starts_with("live-")).unwrap();
        assert_eq!(card.route_groups, vec!["自动", "干活"]);
        assert_eq!(card.updated_at, baseline_time, "复用组链不得改写完成时间");

        // 同会话但 route_id 不匹配的 trace 不能被借用。
        live.routes.clear();
        live.routes.insert(70, LiveRoute { id: 70, group: Some(RouteGroup { name: "其他".into(), ..Default::default() }), ..Default::default() });
        let mut unmatched = ledger.snapshot("h", "ready", None);
        live.apply(&mut unmatched);
        let card = unmatched.sessions[0].routes.iter()
            .find(|card| card.route_id == Some(7) && !card.id.starts_with("live-")).unwrap();
        assert!(card.route_groups.is_empty());
        assert_eq!(card.updated_at, baseline_time);
    }

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
        assert_eq!(view.sessions[0].routes[0].detail, "请求中 · 等待或接收上游流");
        assert_eq!(view.sessions[0].routes[0].tokens, None);
        assert!(!view.sessions[0].routes[0].metrics_are_previous);
        assert_eq!(view.daily.requests, 0);
        live.activity.active.clear();
        let mut finished = ledger.snapshot("h", "ready", None);
        live.apply(&mut finished);
        assert!(finished.sessions.is_empty());
    }

    /// 进行中请求的 usage 记录还没落地，沿用该会话已完成记录的网关密钥，
    /// 不留空密钥把来源显示成「本机」。会话没有记录时再按来源找，两边都没有就保持空。
    #[test]
    fn live_request_keeps_known_caller_until_usage_arrives() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let row = serde_json::json!({"route_id":3,"t":now.to_rfc3339(),"session":"alpha",
            "agent":"claude","callerKeyId":"key-1","callerKeyName":"杨润杰",
            "providerAccount":"one@example.com","provider":"codex","model":"gpt-test",
            "status":200,"in":10,"out":10,"ms":2000,"req":"group/smart-split"});
        ledger.ingest(row.to_string().as_bytes());
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 2, session: "alpha".into(), agent: "claude".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 11, ..Active::default() });
        live.routes.insert(11, LiveRoute { id: 11, session: "alpha".into(), ..LiveRoute::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].caller_key_id, "key-1");
        assert_eq!(view.sessions[0].caller_key_name, "杨润杰");

        // 全新会话没有已完成记录：保持空字符串，不拿别的会话的密钥来填。
        live.activity.active.push(Active { id: 3, session: "beta".into(), agent: "claude".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 12, ..Active::default() });
        live.routes.insert(12, LiveRoute { id: 12, session: "beta".into(), ..LiveRoute::default() });
        live.apply(&mut view);
        // 另一个会话即使来源同名，也可能是另一台机器的另一个密钥，不能借用 alpha 的。
        let beta = view.sessions.iter().find(|session| session.id == "beta").unwrap();
        assert!(beta.caller_key_id.is_empty() && beta.caller_key_name.is_empty());
    }

    /// 未到阈值的上游为蓝色运行，仍活跃的已结束 trace 不撤卡。
    #[test]
    fn active_request_stays_visible_through_waiting_and_trace_completion() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let row = serde_json::json!({"route_id":1,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"old@example.com","provider":"grok","model":"grok-old","status":200,
            "in":100,"out":100,"ms":3000,"ttft_ms":1000});
        ledger.ingest(row.to_string().as_bytes());
        let baseline = ledger.snapshot("h", "ready", None);
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 2, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), ms: 120_000, ..Default::default() });
        let mut pending = baseline.clone();
        live.apply(&mut pending);
        assert_eq!(pending.sessions[0].routes, baseline.sessions[0].routes);
        assert_eq!(pending.sessions[0].routes[0].state, "success");
        assert_eq!(pending.daily, baseline.daily);

        live.activity.active[0].route = 10;
        live.activity.active[0].phase = "upstream".into();
        live.activity.active[0].ms = FIRST_RESPONSE_WAIT_MS - 1;
        live.routes.insert(10, LiveRoute { id: 10, session: "alpha".into(),
            order: Some(vec![Candidate { id: "seat".into(), provider: "codex".into(),
                model: "gpt-test".into(), who: "current@example.com".into(), ..Default::default() }]),
            tries: Some(vec![Attempt { id: "seat".into(), model: "gpt-test".into(), effort: "xhigh".into(), ..Default::default() }]),
            ..Default::default() });
        let mut upstream = baseline.clone();
        live.apply(&mut upstream);
        assert_eq!(upstream.sessions[0].routes[0].state, "running");
        assert_eq!(upstream.sessions[0].routes[0].detail, "请求中 · 等待或接收上游流");
        assert_eq!(upstream.sessions[0].routes[0].account, "current@example.com");

        for phase in ["slot", "rpm"] {
            live.activity.active[0].phase = phase.into();
            let mut queued = baseline.clone();
            live.apply(&mut queued);
            assert_eq!(queued.sessions[0].routes[0].state, "waiting");
            assert_eq!(queued.daily, baseline.daily);
        }
        live.activity.active[0].phase = "upstream".into();
        live.routes.get_mut(&10).unwrap().tries.as_mut().unwrap()[0].done = true;
        let mut retrying = baseline.clone();
        live.apply(&mut retrying);
        assert_eq!(retrying.sessions[0].routes[0].state, "waiting");
        assert_eq!(retrying.sessions[0].routes[0].detail, "请求中 · 等待路由切换");
        live.routes.get_mut(&10).unwrap().tries.as_mut().unwrap()[0].done = false;
        live.routes.get_mut(&10).unwrap().first_text = Some(90_000);
        let mut streaming = baseline.clone();
        live.apply(&mut streaming);
        assert_eq!(streaming.sessions[0].routes[0].state, "running");
        live.routes.get_mut(&10).unwrap().done = true;
        let mut settling = baseline.clone();
        live.apply(&mut settling);
        assert_eq!(settling.sessions[0].routes[0].state, "running");
        assert_eq!(settling.sessions[0].routes[0].detail, "请求仍活跃 · 等待请求结束");
        assert_eq!(settling.daily, baseline.daily);

        live.activity.active.clear();
        let mut finished = baseline.clone();
        live.apply(&mut finished);
        assert_eq!(finished.sessions[0].routes[0].state, "success");
        assert_eq!(finished.daily, baseline.daily);
    }

    /// 根与子请求共用 15 秒边界，重复刷新不重置，首字依据或结束后恢复。
    #[test]
    fn first_response_warning_uses_active_elapsed_without_mutating_usage() {
        let now = Local::now();
        let baseline = UsageLedger::new(now.date_naive()).snapshot("h", "ready", None);
        for child in ["", "child-one"] {
            let mut live = LiveState::default();
            live.routes.insert(10, LiveRoute { id: 10, session: "alpha".into(),
                subagent_id: child.into(), ..Default::default() });
            live.activity.active.push(Active { id: 1, session: "alpha".into(), route: 10,
                agent: "test".into(), started: now.to_rfc3339(), phase: "upstream".into(), ..Default::default() });
            for (elapsed, expected) in [(0, "running"), (14_999, "running"), (15_000, "waiting"),
                (15_000, "waiting"), (30_000, "waiting")] {
                live.activity.active[0].ms = elapsed;
                let mut view = baseline.clone();
                live.apply(&mut view);
                let session = &view.sessions[0];
                let card = if child.is_empty() { &session.routes[0] } else { &session.subagents[0] };
                assert_eq!(card.state, expected, "{child}: elapsed={elapsed}");
                if expected == "waiting" {
                    assert_eq!(card.detail, "请求超过 15 秒 · 首字状态未确认");
                    assert!(card.note.text.contains("首字状态未确认"));
                }
                assert_eq!(view.daily, baseline.daily);
            }
            // 等待期间切会话、刷新或路由尝试更新不影响 active 的已运行时间。
            live.routes.get_mut(&10).unwrap().tries = Some(vec![Attempt::default()]);
            let mut repeated = baseline.clone();
            live.apply(&mut repeated);
            let card = if child.is_empty() { &repeated.sessions[0].routes[0] } else { &repeated.sessions[0].subagents[0] };
            assert_eq!(card.state, "waiting");
            for from_attempt in [true, false] {
                let route = live.routes.get_mut(&10).unwrap();
                route.first_text = (!from_attempt).then_some(0);
                route.tries.as_mut().unwrap()[0].first_text = from_attempt.then_some(0);
                let mut streaming = baseline.clone();
                live.apply(&mut streaming);
                let card = if child.is_empty() { &streaming.sessions[0].routes[0] } else { &streaming.sessions[0].subagents[0] };
                assert_eq!(card.state, "running");
                assert_eq!(card.detail, "请求中 · 已收到上游正文");
            }
            live.routes.get_mut(&10).unwrap().done = true;
            let mut settling = baseline.clone();
            live.apply(&mut settling);
            let card = if child.is_empty() { &settling.sessions[0].routes[0] } else { &settling.sessions[0].subagents[0] };
            assert_eq!(card.state, "running");
            live.activity.active.clear();
            let mut gone = baseline.clone();
            live.apply(&mut gone);
            assert!(gone.sessions.is_empty());
            // 新请求沿用会话身份，但使用它自己的 active.ms。
            live.activity.active.push(Active { id: 2, session: "alpha".into(), route: 11, agent: "test".into(),
                started: now.to_rfc3339(), ms: 500, phase: "upstream".into(), ..Default::default() });
            live.routes.insert(11, LiveRoute { id: 11, session: "alpha".into(), subagent_id: child.into(), ..Default::default() });
            let mut fresh = baseline.clone();
            live.apply(&mut fresh);
            let card = if child.is_empty() { &fresh.sessions[0].routes[0] } else { &fresh.sessions[0].subagents[0] };
            assert_eq!(card.state, "running");
            assert_eq!(fresh.daily, baseline.daily);
        }
    }

    /// 未知身份不挂伪根请求；trace 到齐后首次直接作为子 Agent 出现。
    #[test]
    fn pending_identity_moves_to_child_when_trace_arrives() {
        let now = Local::now();
        let baseline = UsageLedger::new(now.date_naive()).snapshot("h", "ready", None);
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 1, route: 10, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), ..Default::default() });
        let mut pending = baseline.clone();
        live.apply(&mut pending);
        assert!(pending.sessions.is_empty());
        live.routes.insert(10, LiveRoute { id: 10, session: "alpha".into(), subagent_id: "child-one".into(), ..Default::default() });
        let mut known = baseline.clone();
        live.apply(&mut known);
        assert!(known.sessions[0].routes.is_empty());
        assert_eq!(known.sessions[0].subagents[0].state, "running");
        assert_eq!(known.sessions[0].subagents[0].subagent_id.as_deref(), Some("child-one"));
        live.routes.get_mut(&10).unwrap().subagent_id.clear();
        let mut root = baseline.clone();
        live.apply(&mut root);
        assert_eq!(root.sessions[0].routes[0].state, "running");
        assert!(root.sessions[0].subagents.is_empty());
        live.routes.get_mut(&10).unwrap().kind = "routing".into();
        let mut internal = baseline;
        live.apply(&mut internal);
        assert!(internal.sessions.is_empty());
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
            live.routes.insert(id, LiveRoute { id, session: session.into(), first_text: Some(1), ..LiveRoute::default() });
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

    /// 子请求挂在父会话下；未知身份不覆盖已完成根卡或借用任何指标。
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
        let unresolved = std::mem::take(&mut live.activity.active);
        live.activity.active.push(Active { id: 104, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 99, ..Default::default() });
        let mut pending = baseline.clone();
        live.apply(&mut pending);
        assert_eq!(pending.sessions[0].routes, baseline.sessions[0].routes);
        assert!(pending.sessions[0].subagents.is_empty());
        live.activity.active = unresolved;
        let mut view = baseline.clone();
        live.apply(&mut view);
        let session = view.sessions.iter().find(|session| session.id == "alpha").unwrap();
        assert_eq!(session.routes.len(), 1);
        assert_eq!(session.routes, baseline.sessions.iter().find(|session| session.id == "alpha").unwrap().routes);
        assert_eq!(session.routes[0].state, "success");
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
        live.routes.insert(30, LiveRoute { id: 30, session: "alpha".into(), first_text: Some(1), ..Default::default() });
        live.activity.active.push(Active { id: 8, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 30, ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        let routes = &view.sessions[0].routes;
        assert_eq!(routes.iter().filter(|card| card.state == "running").count(), 1);
        assert!(!routes.iter().any(|card| card.state == "success" || card.state == "recovered"));
        assert!(!routes.iter().any(|card| card.state == "failed"));
        let running = routes.iter().find(|card| card.state == "running").unwrap();
        assert_eq!(running.tokens, Some(1_000));
        assert!(running.metrics_are_previous);
        assert!(running.subagent_id.is_none());
        live.routes.insert(40, LiveRoute { id: 40, session: "alpha".into(), subagent_id: "child-with-root".into(), first_text: Some(1), ..Default::default() });
        live.activity.active.push(Active { id: 9, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 40, ..Default::default() });
        live.apply(&mut view);
        assert_eq!(view.sessions[0].subagents.len(), 1);
        assert_eq!(view.watch_label, "1 个会话请求中");
        let later = serde_json::json!({"route_id":3,"t":now.to_rfc3339(),"session":"alpha",
            "providerAccount":"later@example.com","provider":"codex","model":"gpt-root","status":503,
            "in":10,"out":0,"ms":500,"ttft_ms":100});
        ledger.ingest(later.to_string().as_bytes());
        let mut after = ledger.snapshot("h", "ready", None);
        live.apply(&mut after);
        // 进行中的根请求盖住同会话全部旧尝试，包括刚落盘的失败行，避免运行中与失败卡并列。
        assert_eq!(after.sessions[0].routes.len(), 1);
        assert_eq!(after.sessions[0].routes[0].state, "running");
        // 请求结束后账本的最新失败卡重新出现，失败证据不丢。
        live.activity.active.clear();
        let mut settled = ledger.snapshot("h", "ready", None);
        live.apply(&mut settled);
        assert!(settled.sessions[0].routes.iter().any(|card| card.state == "failed" && card.account == "later@example.com"));
    }

    /// 标签只认精确父会话和子 ID；坏文件保留上次有效描述，进行中和完成行都使用它。
    #[test]
    fn child_labels_match_exact_parent_and_cover_active_and_completed() {
        let root = std::env::temp_dir().join(format!("magpie-hud-child-meta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let parent = "11111111-1111-1111-1111-111111111111";
        let other = "22222222-2222-2222-2222-222222222222";
        let project = root.join("encoded-cwd");
        let session = project.join(parent).join("subagents");
        let other_dir = project.join(other).join("subagents");
        let outside = root.join("other-project").join(other).join("subagents");
        fs::create_dir_all(&session).unwrap();
        fs::create_dir_all(&other_dir).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(session.join("agent-child-one.meta.json"), br#"{"description":"Read quota","prompt":"do not keep"}"#).unwrap();
        fs::write(other_dir.join("agent-child-one.meta.json"), br#"{"description":"Wrong parent"}"#).unwrap();
        fs::write(outside.join("agent-child-one.meta.json"), br#"{"description":"Outside"}"#).unwrap();
        let mut titles = ChildTitles::default();
        let mut wanted = HashSet::new();
        wanted.insert((parent.into(), "child-one".into()));
        wanted.insert((other.into(), "child-one".into()));
        wanted.insert(("33333333-3333-3333-3333-333333333333".into(), "child-one".into()));
        wanted.insert((parent.into(), "../child-one".into()));
        let labels = titles.scan_root(Some(&project), &wanted);
        assert_eq!(labels.get(&(parent.into(), "child-one".into())).map(String::as_str), Some("Read quota"));
        assert_eq!(labels.get(&(other.into(), "child-one".into())).map(String::as_str), Some("Wrong parent"));
        assert_eq!(labels.len(), 2);
        let broken = session.join("agent-child-one.meta.json");
        fs::write(&broken, b"...................................").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        let file = fs::File::options().write(true).open(&broken).unwrap();
        file.set_modified(later).unwrap();
        let retained = titles.scan_root(Some(&project), &wanted);
        assert_eq!(retained.get(&(parent.into(), "child-one".into())).map(String::as_str), Some("Read quota"));
        let _ = fs::remove_dir_all(&root);

        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let row = serde_json::json!({"route_id":1,"t":now.to_rfc3339(),"session":parent,"subagent":"child-one",
            "providerAccount":"child@example.com","provider":"codex","model":"gpt-child","status":200,
            "in":10,"out":10,"cache_read":10,"ms":2000,"ttft_ms":500});
        ledger.ingest(row.to_string().as_bytes());
        let mut live = LiveState::default();
        live.child_labels.insert((parent.into(), "child-one".into()), "Read quota".into());
        live.routes.insert(8, LiveRoute { id: 8, session: parent.into(), subagent_id: "child-two".into(), ..Default::default() });
        live.activity.active.push(Active { id: 3, session: parent.into(), agent: "test".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 8, ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        let session = view.sessions.iter().find(|session| session.id == parent).unwrap();
        assert_eq!(session.completed_subagents[0].subagent_label.as_deref(), Some("Read quota"));
        assert!(!session.completed_subagents[0].current);
        let active = session.subagents.iter().find(|card| card.subagent_id.as_deref() == Some("child-two")).unwrap();
        assert_eq!(active.subagent_label.as_deref(), Some("子 Agent child-tw"));
        assert!(session.routes.iter().all(|card| card.subagent_id.is_none()));
    }

    /// 同一父 UUID 出现在两个项目目录时不认领任一标签。
    #[test]
    fn duplicate_parent_directories_do_not_claim_a_label() {
        let root = std::env::temp_dir().join(format!("magpie-hud-child-conflict-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let parent = "44444444-4444-4444-4444-444444444444";
        for project in ["project-a", "project-b"] {
            let dir = root.join(project).join(parent).join("subagents");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("agent-child-one.meta.json"), format!(r#"{{"description":"{project}"}}"#)).unwrap();
        }
        let mut wanted = HashSet::new();
        wanted.insert((parent.into(), "child-one".into()));
        let labels = ChildTitles::default().scan_root(Some(&root), &wanted);
        assert!(labels.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// 同一账号、模型可在多个组成员中重复出现；重复行不构成账号歧义。
    #[test]
    fn selected_candidate_accepts_duplicate_model_rows_with_same_identity() {
        let route = LiveRoute { order: Some(vec![
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into(), ..Default::default() },
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into(), ..Default::default() },
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
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into(), ..Default::default() },
            Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-b".into(), who: "account-a".into(), ..Default::default() },
            Candidate { id: "other-seat".into(), provider: "provider-b".into(), model: "model-a".into(), who: "account-b".into(), ..Default::default() },
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
                Candidate { id: "seat".into(), provider: "provider-a".into(), model: "model-a".into(), who: "account-a".into(), ..Default::default() },
                Candidate { id: "seat".into(), provider: provider.into(), model: "model-a".into(), who: who.into(), ..Default::default() },
            ]), ..Default::default() };
            for model in ["model-a", ""] {
                assert!(selected_candidate(&route, &Attempt {
                    id: "seat".into(), model: model.into(), ..Default::default()
                }).is_none(), "conflicting identity must remain unknown, with model {model:?}");
            }
        }
    }

    /// 同一 Desktop 会话轮换 CLI ID 后，当前 ID 的新成功根隐藏历史 ID 的更早取消；
    /// 只按显式关联归并，保留统计和额度路由。
    #[test]
    fn alias_roots_hide_older_cancelled_rows_from_prior_cli_ids() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let current = "092398da-37c9-43d9-bf87-3ae8e5d8ec16";
        let prior = ["57594e5e-f162-433e-870a-dc6dd3b9c37c", "2cceaf1c-1d50-43e2-ab93-c7ad4c3098d8"];
        let mut record = |route_id, session: &str, account: &str, status, seconds: i64| {
            let time = now - chrono::Duration::seconds(seconds);
            let row = serde_json::json!({"route_id":route_id,"t":time.to_rfc3339(),"session":session,
                "providerAccount":account,"provider":"codex","model":"gpt-test",
                "status":status,"in":100,"out":100,"ms":1000});
            ledger.ingest(row.to_string().as_bytes());
        };
        record(1, prior[0], "first@example.com", 499, 90);
        record(2, prior[1], "second@example.com", 499, 60);
        record(3, current, "current@example.com", 200, 10);
        let baseline = ledger.snapshot("h", "ready", None);
        assert!(baseline.sessions.iter().any(|session| session.id == prior[0]));
        assert!(baseline.sessions.iter().any(|session| session.id == prior[1]));

        let mut live = LiveState::default();
        live.names.insert(current.into(), "VACS / ID 提取".into());
        for id in prior { live.names.insert(id.into(), "VACS / ID 提取".into()); }
        for id in prior { live.aliases.insert(id.into(), current.into()); }
        let mut view = baseline.clone();
        live.apply(&mut view);
        assert!(view.sessions.iter().all(|session| session.id != prior[0]));
        assert!(view.sessions.iter().all(|session| session.id != prior[1]));
        let first = view.sessions.iter().find(|session| session.id == current).unwrap();
        let shown = first.clone();
        assert_eq!(shown.routes.len(), 1);
        assert_eq!(shown.routes[0].state, "success");
        // 展示归一不得改变当日统计或额度来源。
        assert_eq!(view.daily, baseline.daily);
        let quota_accounts = |sessions: &[SessionView]| {
            let mut accounts: Vec<_> = sessions.iter().flat_map(|session| session.quota_routes.iter())
                .map(|card| (card.provider.clone(), card.account.clone())).collect();
            accounts.sort();
            accounts
        };
        assert_eq!(quota_accounts(&view.sessions), quota_accounts(&baseline.sessions));
        assert_eq!(first.quota_routes.len(), 3, "隐藏别名的额度来源并入当前行");
        let total: u64 = baseline.sessions.iter().map(|session| session.daily.requests).sum();
        assert_eq!(total, 3);
        // 展示行消失不会改写每个会话自己的统计或全局统计。
        assert_eq!(first.daily, baseline.sessions.iter().find(|session| session.id == current).unwrap().daily);
    }

    /// 显示名相同但没有显式 CLI 关联的独立 Desktop 会话必须保留；活跃根卡也不被旧成功隐藏。
    #[test]
    fn alias_suppression_keeps_independent_sessions_and_active_roots() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let current = "11111111-1111-4111-8111-111111111111";
        let unrelated = "22222222-2222-4222-8222-222222222222";
        let prior = "33333333-3333-4333-8333-333333333333";
        for (route_id, session, status, seconds) in [
            (1, unrelated, 499, 80), (2, prior, 499, 60), (3, current, 200, 10),
        ] {
            let time = now - chrono::Duration::seconds(seconds);
            let row = serde_json::json!({"route_id":route_id,"t":time.to_rfc3339(),"session":session,
                "providerAccount":"one@example.com","provider":"codex","model":"gpt-test",
                "status":status,"in":10,"out":10,"ms":1000});
            ledger.ingest(row.to_string().as_bytes());
        }
        let baseline = ledger.snapshot("h", "ready", None);
        let mut live = LiveState::default();
        live.aliases.insert(prior.into(), current.into());
        for id in [current, unrelated, prior] { live.names.insert(id.into(), "同名 / 会话".into()); }
        let mut view = baseline.clone();
        live.apply(&mut view);
        // 无显式关联的同名会话保留自己的取消卡。
        assert!(view.sessions.iter().any(|session| session.id == unrelated
            && session.routes.iter().any(|card| card.state == "cancelled")));
        assert!(view.sessions.iter().all(|session| session.id != prior));

        // 历史 ID 上仍有进行中的根请求时，进行中的行保留；当前 ID 的成功不会被反压。
        live.routes.insert(80, LiveRoute { id: 80, session: prior.into(), first_text: Some(1), ..Default::default() });
        live.activity.active.push(Active { id: 8, session: prior.into(), agent: "claude-desktop".into(),
            started: now.to_rfc3339(), phase: "upstream".into(), route: 80, ..Default::default() });
        let mut active = baseline.clone();
        live.apply(&mut active);
        let running = active.sessions.iter().find(|session| session.id == prior).unwrap();
        assert_eq!(running.routes.len(), 1);
        assert_eq!(running.routes[0].state, "running");
        assert!(active.sessions.iter().any(|session| session.id == current));
        assert_eq!(active.daily, baseline.daily);
    }

    /// 子 Agent 身份仍挂在各自的父会话下；别名只隐藏没有其它可见内容的旧式根行。
    #[test]
    fn alias_suppression_preserves_child_identity_and_content() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        let current = "44444444-4444-4444-8444-444444444444";
        let prior = "55555555-5555-4555-8555-555555555555";
        for (route_id, session, subagent, account, status, seconds) in [
            (1, current, "", "current@example.com", 200, 30),
            (2, prior, "child-prior", "child@example.com", 200, 40),
            (3, prior, "", "prior@example.com", 499, 50),
        ] {
            let time = now - chrono::Duration::seconds(seconds);
            let row = serde_json::json!({"route_id":route_id,"t":time.to_rfc3339(),"session":session,
                "subagent":subagent,"providerAccount":account,"provider":"codex",
                "model":"gpt-test","status":status,"in":10,"out":10,"ms":1000});
            ledger.ingest(row.to_string().as_bytes());
        }
        let baseline = ledger.snapshot("h", "ready", None);
        let mut live = LiveState::default();
        live.aliases.insert(prior.into(), current.into());
        let mut view = baseline.clone();
        live.apply(&mut view);
        // 旧别名仍有子 Agent 完成记录，保留整行与其子身份；比规范行成功更早的取消根被盖住。
        let old = view.sessions.iter().find(|session| session.id == prior).unwrap();
        assert_eq!(old.completed_subagents.len(), 1);
        assert_eq!(old.completed_subagents[0].subagent_id.as_deref(), Some("child-prior"));
        assert!(old.routes.is_empty());
        let shown = view.sessions.iter().find(|session| session.id == current).unwrap();
        assert!(shown.completed_subagents.is_empty());
        assert_eq!(view.daily, baseline.daily);
        // 别名行因子 Agent 内容保留时，额度来源仍并入当前 ID，不随旧行漂移。
        let accounts = |session: &SessionView| {
            let mut accounts: Vec<_> = session.quota_routes.iter().map(|card| card.account.clone()).collect();
            accounts.sort();
            accounts
        };
        assert_eq!(accounts(shown), vec!["child@example.com", "current@example.com", "prior@example.com"]);
        assert!(accounts(old).is_empty(), "保留的子 Agent 行不再持有已并入当前 ID 的额度来源");
    }

    #[test]
    fn model_usage_folds_only_explicit_session_aliases() {
        let mut ledger = UsageLedger::new(Local::now().date_naive());
        let now = Local::now().to_rfc3339();
        for (id, session) in [(1, "current"), (2, "prior"), (3, "other")] {
            let row = serde_json::json!({"route_id":id,"t":now,"session":session,"agent":"claude-desktop",
                "model":"m","effort":"high","status":200,"in":10,"out":20,"ms":3000,"ttft_ms":1000});
            ledger.ingest(row.to_string().as_bytes());
        }
        let mut view = ledger.snapshot("", "ready", None);
        let mut live = LiveState::default();
        live.aliases.insert("prior".into(), "current".into());
        live.apply(&mut view);
        assert_eq!(view.model_usage.iter().filter(|row| row.session == "current").map(|row| row.requests).sum::<u64>(), 2);
        assert!(view.model_usage.iter().any(|row| row.session == "other"));
        assert!(!view.model_usage.iter().any(|row| row.session == "prior"));
        let canonical = view.model_usage.iter().find(|row| row.session == "current").unwrap();
        assert_eq!((canonical.tokens, canonical.output_tokens, canonical.generation_ms, canonical.speed_samples), (60, 40, 4000.0, 2));
    }

    /// 标题元数据里的显式 CLI 关联形成别名映射；同名、冲突声明与无当前 ID 时都不猜。
    #[test]
    fn title_aliases_follow_explicit_metadata_only() {
        let title = |session: &str, cli: &str, prior: &[&str]| DesktopTitle {
            session_id: session.into(), cli_session_id: cli.into(),
            prior_cli_session_ids: prior.iter().map(|id| (*id).to_string()).collect(),
            title: "VACS / ID 提取".into(), ..Default::default()
        };
        let current = "092398da-37c9-43d9-bf87-3ae8e5d8ec16";
        let prior = "57594e5e-f162-433e-870a-dc6dd3b9c37c";
        let same_title = "66666666-6666-4666-8666-666666666666";
        let aliases = alias_links([
            title("local_05b47592-aab9-4657-86df-9c6cdbb8b1a1", current, &[prior]),
            title(&format!("local_{same_title}"), same_title, &[]),
        ].iter());
        assert_eq!(aliases.get(prior).map(String::as_str), Some(current));
        assert_eq!(aliases.len(), 1, "同名独立会话不应产生关联");
        // 同一历史 ID 被两个 Desktop 会话声明：冲突，不归并。
        let conflict = alias_links([
            title("local_77777777-7777-4777-8777-777777777777", current, &[prior]),
            title("local_88888888-8888-4888-8888-888888888888", same_title, &[prior]),
        ].iter());
        assert!(conflict.is_empty());
        // 历史 ID 已经是另一个独立 Desktop 会话的当前 ID：身份已重新分配，不归并。
        let reassigned = alias_links([
            title("local_77777777-7777-4777-8777-777777777777", current, &[prior]),
            title("local_88888888-8888-4888-8888-888888888888", prior, &[]),
        ].iter());
        assert!(reassigned.is_empty());
        // 没有当前 CLI ID 时退回 local_ 会话 ID。
        let fallback = alias_links([title("local_99999999-9999-4999-8999-999999999999", "", &[prior])].iter());
        assert_eq!(fallback.get(prior).map(String::as_str), Some("99999999-9999-4999-8999-999999999999"));
        // 当前 ID 为空但存在未归档 ID 时，用它作为规范身份，未归档 ID 不再自映射。
        let unarchived = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let fallback = alias_links([DesktopTitle {
            session_id: "local_99999999-9999-4999-8999-999999999999".into(),
            unarchived_cli_session_id: unarchived.into(),
            prior_cli_session_ids: vec![prior.into()], title: "VACS / ID 提取".into(), ..Default::default()
        }].iter());
        assert_eq!(fallback.get(prior).map(String::as_str), Some(unarchived));
        assert!(!fallback.contains_key(unarchived));
    }

    /// 同一标题文件返回的别名来自显式 ID 字段，不受标题显示名影响。
    #[test]
    fn titles_scan_exposes_alias_links_without_merging_titles() {
        let root = std::env::temp_dir().join(format!("magpie-hud-title-alias-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let org = root.join("claude-code-sessions").join("profile").join("org");
        fs::create_dir_all(&org).unwrap();
        let current = "092398da-37c9-43d9-bf87-3ae8e5d8ec16";
        let prior = "57594e5e-f162-433e-870a-dc6dd3b9c37c";
        fs::write(org.join("local_05b47592-aab9-4657-86df-9c6cdbb8b1a1.json"),
            serde_json::json!({"sessionId":"local_05b47592-aab9-4657-86df-9c6cdbb8b1a1",
                "cliSessionId":current,"priorCliSessionIds":[prior],"title":"VACS / ID 提取",
                "messages":[{"body":"do not read"}]}).to_string()).unwrap();
        let mut titles = Titles::default();
        let names = titles.scan(std::slice::from_ref(&root));
        assert_eq!(names.get(current).map(String::as_str), Some("VACS / ID 提取"));
        assert_eq!(names.get(prior).map(String::as_str), Some("VACS / ID 提取"));
        assert_eq!(titles.aliases().get(prior).map(String::as_str), Some(current));
        let _ = fs::remove_dir_all(&root);
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
            Candidate { id: "grok".into(), provider: "grok".into(), model: "grok-test".into(), who: "grok@example.com".into(), ..Default::default() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6.1".into(), who: "gpt@example.com".into(), ..Default::default() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "other@example.com".into(), ..Default::default() },
        ]), ..Default::default() };
        let selected = selected_candidate(&route, &Attempt { id: "codex".into(), model: "gpt-6.1".into(), ..Default::default() });
        assert_eq!(selected.map(|c| (c.provider.as_str(), c.who.as_str())), Some(("codex", "gpt@example.com")));
        assert!(selected_candidate(&route, &Attempt { id: "codex".into(), ..Default::default() }).is_none());
        let legacy = LiveRoute { order: Some(vec![Candidate { id: "grok".into(), who: "grok@example.com".into(), ..Default::default() }]), ..Default::default() };
        assert!(selected_candidate(&legacy, &Attempt { id: "grok".into(), model: "grok-test".into(), ..Default::default() }).is_some());
        // 当前登录账号的休息键是 provider，组内每个模型各有一行。
        let shared = LiveRoute { order: Some(vec![
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "one@example.com".into(), ..Default::default() },
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6.1".into(), who: "one@example.com".into(), ..Default::default() },
            Candidate { id: "codex@two@example.com".into(), provider: "codex".into(), model: "gpt-6".into(), who: "two@example.com".into(), ..Default::default() },
        ]), ..Default::default() };
        let signed_in = selected_candidate(&shared, &Attempt { id: "codex".into(), model: "gpt-6.1".into(), ..Default::default() });
        assert_eq!(signed_in.map(|c| (c.provider.as_str(), c.who.as_str())), Some(("codex", "one@example.com")));
        // 模型不在计划里，或同一键对应不同账号时，保持选择中，不用请求模型补身份。
        assert!(selected_candidate(&shared, &Attempt { id: "codex".into(), model: "gpt-other".into(), ..Default::default() }).is_none());
        let conflict = LiveRoute { order: Some(vec![
            Candidate { id: "codex".into(), provider: "codex".into(), model: "gpt-6".into(), who: "one@example.com".into(), ..Default::default() },
            Candidate { id: "codex".into(), provider: "other".into(), model: "gpt-6".into(), who: "one@example.com".into(), ..Default::default() },
        ]), ..Default::default() };
        assert!(selected_candidate(&conflict, &Attempt { id: "codex".into(), model: "gpt-6".into(), ..Default::default() }).is_none());
    }

    /// 组链只跟随选中尝试的 via，并经子组 id/ref 映射组名；不含最终模型。
    #[test]
    fn route_chain_follows_selected_attempt_only() {
        let route = LiveRoute {
            group: Some(RouteGroup { name: "自动".into(),
                subs: vec![
                    RouteSubgroup { id: "sub-a".into(), reference: "ref-a".into(), name: "干活".into(), ..Default::default() },
                    RouteSubgroup { id: "sub-b".into(), reference: "ref-b".into(), name: "deepseek".into(), ..Default::default() },
                ], ..Default::default() }),
            order: Some(vec![
                Candidate { id: "seat".into(), provider: "p".into(), model: "m".into(), who: "a@example.com".into(), via: vec!["ref-a".into()], ..Default::default() },
                Candidate { id: "other".into(), provider: "p".into(), model: "m".into(), who: "b@example.com".into(), via: vec!["sub-b".into()], ..Default::default() },
            ]),
            ..Default::default()
        };
        let attempt = Attempt { id: "seat".into(), model: "m".into(), ..Default::default() };
        let candidate = selected_candidate(&route, &attempt).unwrap();
        assert_eq!(route_chain(&route, Some(&attempt), Some(candidate)), vec!["自动", "干活"]);

        // 选中的候选缺少 via：只报入口组，绝不借用其它候选的子组。
        let legacy = LiveRoute {
            group: route.group.clone(),
            order: Some(vec![Candidate { id: "seat".into(), provider: "p".into(), model: "m".into(), who: "a@example.com".into(), ..Default::default() }]),
            ..Default::default()
        };
        let candidate = selected_candidate(&legacy, &attempt).unwrap();
        assert_eq!(route_chain(&legacy, Some(&attempt), Some(candidate)), vec!["自动"]);
    }

    /// 同一尝试的候选子组不一致或未知时，仍保留已确认的入口组。
    #[test]
    fn route_chain_rejects_conflicting_or_unknown_via() {
        let group = Some(RouteGroup { name: "自动".into(),
            subs: vec![RouteSubgroup { id: "sub-a".into(), name: "干活".into(), ..Default::default() }], ..Default::default() });
        let attempt = Attempt { id: "seat".into(), model: "m".into(), ..Default::default() };
        let candidate = Candidate { id: "seat".into(), provider: "p".into(), model: "m".into(), who: "a@example.com".into(), ..Default::default() };
        let conflicting = LiveRoute { group: group.clone(), order: Some(vec![
            Candidate { via: vec!["sub-a".into()], ..candidate.clone() },
            Candidate { via: vec!["unknown".into()], ..candidate.clone() },
        ]), ..Default::default() };
        assert_eq!(route_chain(&conflicting, Some(&attempt), Some(&candidate)), vec!["自动"]);
        let unknown = LiveRoute { group, order: Some(vec![
            Candidate { via: vec!["unknown".into()], ..candidate.clone() },
        ]), ..Default::default() };
        assert_eq!(route_chain(&unknown, Some(&attempt), Some(&candidate)), vec!["自动"]);
        // 旧 trace 没有 group：不猜链。
        assert!(route_chain(&LiveRoute::default(), Some(&attempt), Some(&candidate)).is_empty());
    }

    #[test]
    fn desktop_temporary_fork_is_not_a_root_session_but_usage_is_kept() {
        let now = Local::now();
        let mut ledger = UsageLedger::new(now.date_naive());
        for (id, session) in [(1, "normal"), (2, "helper")] {
            ledger.ingest(serde_json::json!({"route_id":id,"t":now.to_rfc3339(),"session":session,
                "agent":"claude-desktop","provider":"fixture","model":"m","status":200,
                "in":2478,"out":27,"ms":2276}).to_string().as_bytes());
        }
        let helper = LiveRoute { id: 2, session: "helper".into(), agent: "claude-desktop".into(),
            folder: "C:\\Temp\\ccd-session-secrets\\fork-fixture".into(), ..Default::default() };
        assert!(desktop_auxiliary(&helper));
        assert!(!desktop_auxiliary(&LiveRoute { folder: "project/fork-fixture".into(), ..helper.clone() }));
        assert!(!desktop_auxiliary(&LiveRoute { agent: "codex".into(), ..helper.clone() }));
        assert!(!desktop_auxiliary(&LiveRoute { subagent_id: "explicit-child".into(), ..helper.clone() }));
        let mut live = LiveState::default();
        live.update(TraceUpdate { pid: 1, seq: 1, ids: vec![2], routes: vec![helper] });
        live.activity.active.push(Active { id: 1, route: 2, session: "helper".into(),
            agent: "claude-desktop".into(), started: now.to_rfc3339(), ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        let daily = view.daily.clone();
        live.apply(&mut view);
        assert_eq!(view.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["normal"]);
        assert_eq!(view.daily, daily);
        // trace 滚动淘汰、网关重启之后，不重新出现辅助完成卡。
        live.update(TraceUpdate { pid: 2, seq: 0, ..Default::default() });
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.sessions[0].id, "normal");
        assert_eq!(view.daily.requests, 2);
    }

    /// 多层 via 保留顺序；同一模型也有直达候选时不能把深层路径当作已确定。
    #[test]
    fn nested_route_chain_preserves_order_and_common_prefix() {
        let candidate = Candidate { id: "seat".into(), provider: "p".into(), model: "m".into(),
            who: "fixture@example.com".into(), via: vec!["worker".into(), "group/gpt".into()], ..Default::default() };
        let attempt = Attempt { id: "seat".into(), model: "m".into(), picked: true, done: true, ..Default::default() };
        let mut route = LiveRoute {
            group: Some(RouteGroup { name: "自动".into(), subs: vec![
                RouteSubgroup { id: "worker".into(), name: "干活".into(), ..Default::default() },
                RouteSubgroup { id: "gpt".into(), reference: "group/gpt".into(), name: "GPT重置狂欢".into(), ..Default::default() },
            ], ..Default::default() }),
            order: Some(vec![candidate.clone()]), tries: Some(vec![attempt.clone()]), ..Default::default()
        };
        assert_eq!(route_chain(&route, Some(&attempt), Some(&candidate)), vec!["自动", "干活", "GPT重置狂欢"]);
        assert_eq!(trace_chain(&route), vec!["自动", "干活", "GPT重置狂欢"]);
        route.order.as_mut().unwrap().push(Candidate { via: vec!["worker".into()], ..candidate.clone() });
        assert_eq!(trace_chain(&route), vec!["自动", "干活"]);
        // 截图中的实际形态：相同账号/模型还存在无 via 的直达候选。
        route.order.as_mut().unwrap().push(Candidate { via: Vec::new(), ..candidate });
        assert_eq!(trace_chain(&route), vec!["自动"]);
        route.tries = None;
        assert_eq!(trace_chain(&route), vec!["自动"]);
    }

    /// 同账号同模型跨组出现时按原始固定强度区分，不用拟合后的 High 猜 Max。
    #[test]
    fn route_chain_distinguishes_fixed_members_of_the_same_model() {
        let mut route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id":1,
            "group":{"name":"自动","subs":[{"id":"worker","name":"干活"},{"id":"nano","name":"高速"}]},
            "order":[
                {"id":"zcode","provider":"zcode","model":"GLM-5.3-Flash","who":"fixture","via":["worker"],"fixed":"max"},
                {"id":"zcode","provider":"zcode","model":"GLM-5.3-Flash","who":"fixture","via":["nano"]}
            ],
            "tries":[{"id":"zcode","model":"GLM-5.3-Flash","effort":"high","picked":true,"done":true}]
        })).unwrap();
        assert_eq!(trace_chain(&route), vec!["自动", "高速"]);
        route.tries.as_mut().unwrap()[0].fixed = "max".into();
        assert_eq!(trace_chain(&route), vec!["自动", "干活"]);
        route.tries.as_mut().unwrap()[0].fixed = "xhigh".into();
        assert_eq!(trace_chain(&route), vec!["自动"], "没有匹配成员时不借用另一条路径");
        route.tries.as_mut().unwrap()[0].fixed.clear();
        let duplicate = Candidate { via: Vec::new(), ..route.order.as_ref().unwrap()[1].clone() };
        route.order.as_mut().unwrap().push(duplicate);
        assert_eq!(trace_chain(&route), vec!["自动"], "固定强度相同而路径仍歧义时保留共同前缀");
    }

    /// 活跃卡 updatedAt 取 start + 已用毫秒，按 UTC 毫秒精度输出。
    #[test]
    fn active_card_updated_at_tracks_elapsed_time() {
        let now = Local::now();
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 1, session: "alpha".into(), agent: "test".into(),
            started: now.to_rfc3339(), ms: 3_200, phase: "upstream".into(), route: 10, ..Active::default() });
        live.routes.insert(10, LiveRoute { id: 10, session: "alpha".into(), ..LiveRoute::default() });
        let ledger = UsageLedger::new(now.date_naive());
        let mut view = ledger.snapshot("h", "ready", None);
        live.apply(&mut view);
        let card = &view.sessions[0].routes[0];
        assert_eq!(card.updated_at.as_deref(), Some(updated_at(now.into(), 3_200).as_str()));
        assert!(card.updated_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(card.time.len(), 8);
        // 无子组信息时组链为空，不用模型名填充。
        assert!(card.route_groups.is_empty());
    }

    /// 规则白名单只取 use/pick；when 表达式与其余字段不进入结构，时间/状态/响应模型可用。
    #[test]
    fn history_whitelist_keeps_rule_summary_and_terminal_fields() {
        let now = Local::now();
        let payload = serde_json::json!({
            "id": 31, "seq": 1, "time": now.to_rfc3339(), "session": "alpha", "agent": "test",
            "model": "gpt-test", "provider": "codex", "status": 200, "served": "served-x", "done": true,
            "rule": {"n": 2, "use": "group/worker", "when": ["tokens > 1000"], "held": true, "pick": "xhigh", "future": [1]},
            "tries": [{"id": "seat", "model": "gpt-test", "effort": "high", "start": now.to_rfc3339(),
                        "done": true, "status": 200, "served": "served-try", "firstText": 400}]
        });
        let route: LiveRoute = serde_json::from_value(payload).unwrap();
        let rule = route.rule.as_ref().unwrap();
        assert_eq!(rule.use_target, "group/worker");
        assert_eq!(rule.pick, "xhigh");
        assert_eq!(route.status, Some(200));
        assert_eq!(route.served, "served-x");
        let attempt = route.tries.as_deref().unwrap().last().unwrap();
        assert_eq!(attempt.served, "served-try");
        // 终态尝试的响应模型优先于路由级。
        let mut live = LiveState::default();
        live.routes.insert(31, route);
        let scope = live.history_scope("alpha", None);
        assert_eq!(scope.routes.len(), 1);
        let summary = &scope.routes[0];
        assert_eq!(summary.served, "served-try");
        assert_eq!(summary.phase, "awaiting_usage");
        assert_eq!(summary.rule_target.as_deref(), Some("group/worker"));
        assert_eq!(summary.rule_effort.as_deref(), Some("xhigh"));
    }

    /// 履历范围：显式别名家族双向闭包、子身份精确匹配、分类器/辅助/过期 done 剔除、阶段口径。
    #[test]
    fn history_scope_filters_family_child_and_phase() {
        let now = Local::now();
        let mut live = LiveState::default();
        live.aliases.insert("h1".into(), "cur".into());
        live.aliases.insert("h2".into(), "cur".into());
        live.aliases.insert("other".into(), "nope".into());
        let rfc = |ms_ago: i64| (now - chrono::Duration::milliseconds(ms_ago)).to_rfc3339();
        let base = |id: i64, session: &str| LiveRoute {
            id, session: session.into(), time: Some(rfc(1_000)),
            tries: Some(vec![Attempt { id: "seat".into(), model: "gpt-test".into(),
                ..Attempt::default() }]),
            ..LiveRoute::default()
        };
        // 当前规范 ID 的运行请求：上游阶段 → running。
        live.routes.insert(1, base(1, "cur"));
        live.activity.active.push(Active { id: 11, session: "cur".into(), agent: "test".into(),
            started: rfc(1_000), ms: 2_000, phase: "upstream".into(), route: 1, ..Active::default() });
        // 历史别名的子请求：只有精确 (家族, 子ID) 查询可见。
        let mut child = base(2, "h1");
        child.subagent_id = "c1".into();
        live.routes.insert(2, child);
        // 分类器调用不进履历。
        let mut routing = base(3, "cur");
        routing.kind = "routing".into();
        live.routes.insert(3, routing);
        // Desktop 临时辅助 fork 不进履历。
        let mut auxiliary = base(4, "aux-s");
        auxiliary.agent = "claude-desktop".into();
        auxiliary.folder = "C:/x/ccd-session-secrets/fork-1".into();
        live.routes.insert(4, auxiliary);
        // 等待并发空位 → waiting。
        live.routes.insert(5, base(5, "h2"));
        live.activity.active.push(Active { id: 15, session: "h2".into(), agent: "test".into(),
            started: rfc(1_000), ms: 100, phase: "slot".into(), route: 5, ..Active::default() });
        // done 但日志迟迟未到：超过 10 分钟不再挂起。
        let mut stale = base(6, "cur");
        stale.done = true;
        stale.time = Some(rfc(11 * 60_000));
        live.routes.insert(6, stale);
        // done 且仍在窗口内 → awaiting_usage。
        let mut settling = base(7, "cur");
        settling.done = true;
        settling.time = Some(rfc(5 * 60_000));
        live.routes.insert(7, settling);
        // 无 trace 的活跃请求不能先当根请求；与其它会话的请求不串。
        live.activity.active.push(Active { id: 18, session: "stranger".into(), agent: "test".into(),
            started: rfc(1_000), ms: 100, phase: "upstream".into(), route: 99, ..Active::default() });

        let scope = live.history_scope("h1", None);
        assert_eq!(scope.family, ["h1", "cur", "h2"].iter().map(|id| id.to_string()).collect::<std::collections::HashSet<_>>());
        let root_ids: Vec<i64> = scope.routes.iter().map(|route| route.route_id).collect();
        // done 的 trace 始终保留用于补全日志行；6 号只是不再单独成行。
        assert_eq!(root_ids, vec![7, 6, 5, 1], "根履历只含家族内的根请求，剔除子/分类器/辅助");
        assert_eq!(scope.routes.iter().find(|r| r.route_id == 7).unwrap().phase, "awaiting_usage");
        assert!(scope.routes.iter().find(|r| r.route_id == 7).unwrap().eligible_overlay);
        assert!(!scope.routes.iter().find(|r| r.route_id == 6).unwrap().eligible_overlay);
        assert_eq!(scope.routes.iter().find(|r| r.route_id == 1).unwrap().phase, "running");

        // 子请求只归属原始父会话 h1，规范 ID 查不到它。
        let child_scope = live.history_scope("h1", Some("c1"));
        assert_eq!(child_scope.routes.len(), 1);
        assert_eq!(child_scope.routes[0].subagent, "c1");
        assert_eq!(child_scope.routes[0].route_id, 2);
        assert!(!child_scope.routes[0].eligible_overlay, "无活跃证据的 trace 不生成运行行");
        assert!(live.history_scope("cur", Some("c1")).routes.is_empty());

        let waiting = live.history_scope("h2", None);
        assert_eq!(waiting.routes.iter().find(|route| route.route_id == 5).unwrap().phase, "waiting");
    }

    /// 轮次累积：只记带 turn 的真实输入请求，范围过滤与网关重启清理同 routes 口径。
    #[test]
    fn turn_history_accumulates_filters_and_clears() {
        let now = Local::now();
        let mut live = LiveState::default();
        let route = |id: i64, session: &str, sub: &str, kind: &str, turn: Option<i64>| LiveRoute {
            id, session: session.into(), subagent_id: sub.into(), kind: kind.into(),
            time: Some(now.to_rfc3339()),
            rule: Some(TraceRule { turn, ..TraceRule::default() }),
            ..LiveRoute::default()
        };
        live.update(TraceUpdate {
            seq: 1,
            pid: 7,
            ids: vec![1, 2, 3, 4, 5],
            routes: vec![
                route(1, "s1", "", "", Some(3)),
                // 标题生成自成一个合成轮，不属于用户输入，不参与折叠。
                route(2, "s1", "", "title_generation", Some(1)),
                // 子 Agent 线独立计数，只在子查询里可见。
                route(3, "s1", "c1", "", Some(1)),
                // 其它会话的轮次不进 s1 的履历。
                route(4, "s2", "", "", Some(1)),
                // 无 turn 的请求（非规则组）不记。
                route(5, "s1", "", "", None),
            ],
        });
        let root = live.history_scope("s1", None);
        assert_eq!(root.turns.len(), 1);
        assert_eq!(root.turns[&1].turn, 3);
        assert_eq!(root.turns[&1].time_ms, now.timestamp_millis());
        let child = live.history_scope("s1", Some("c1"));
        assert_eq!(child.turns.len(), 1);
        assert_eq!(child.turns[&3].turn, 1);
        assert!(live.history_scope("s2", None).turns.iter().all(|(id, _)| *id != 1));

        // 网关重启（pid 变化）清空累积，route_id 空间随之重置。
        live.update(TraceUpdate { seq: 1, pid: 8, ids: vec![], routes: vec![] });
        assert!(live.history_scope("s1", None).turns.is_empty());
    }

    /// 网关没有轮初判定的 waits 也是同轮续行，不能因缺少 held 就开新组。
    #[test]
    fn turn_history_recognizes_waits_and_grown_continuations() {
        let mut live = LiveState::default();
        let rules = [
            serde_json::json!({"turn": 1}),
            serde_json::json!({"turn": 1, "held": true}),
            serde_json::json!({"turn": 1, "waits": true}),
            serde_json::json!({"turn": 1, "grown": true}),
            serde_json::json!({"turn": 1, "compact": true, "waits": true}),
        ];
        let routes: Vec<LiveRoute> = rules.into_iter().enumerate().map(|(index, rule)| {
            serde_json::from_value(serde_json::json!({
                "id": index + 1, "session": "s1",
                "time": "2026-10-25T12:00:00+08:00", "rule": rule,
            })).unwrap()
        }).collect();
        live.update(TraceUpdate { pid: 7, seq: 1, ids: vec![1, 2, 3, 4, 5], routes });
        let scope = live.history_scope("s1", None);
        assert_eq!(scope.turns.len(), 4, "压缩调用仍不参与折叠");
        assert!(!scope.turns[&1].held, "无续行标记的请求是开口");
        for id in [2, 3, 4] {
            assert!(scope.turns[&id].held, "held、waits、grown 均是同轮续行");
        }
    }

    /// 分类模型、嵌套判定、实际尝试和完成结果均来自同一路由的白名单字段。
    #[test]
    fn decision_view_uses_trace_evidence_and_ignores_unrelated_sessions() {
        let mut live = LiveState::default();
        let route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 101, "session": "s1", "time": "2026-10-27T12:00:00+08:00",
            "group": {"name": "自动", "subs": [{"id": "worker", "name": "干活"}]},
            "order": [{"id": "seat", "provider": "test", "who": "fixture", "model": "m2", "via": ["worker"]}],
            "rule": {"turn": 2, "use": "group/worker", "pick": "high",
                "classified": {"by": "other/classifier", "intent": "编码", "sure": 0.8,
                    "intents": ["PRIVATE"], "error": "PRIVATE"}, "when": ["PRIVATE"]},
            "nested": [{"group": "worker", "name": "干活", "rule": {"turn": 2, "waits": true}}],
            "tries": [{"id": "old", "model": "m1", "done": true, "status": 503},
                {"id": "seat", "model": "m2", "effort": "high", "done": false}]
        })).unwrap();
        live.update(TraceUpdate { pid: 7, seq: 1, ids: vec![101], routes: vec![route.clone()] });
        live.activity.active.push(Active { id: 20, session: "s1".into(), agent: "test".into(),
            started: "2026-10-27T12:00:00+08:00".into(), route: 101, ..Active::default() });
        let mut snapshot = crate::usage::UsageLedger::new(Local::now().date_naive()).snapshot("local", "ready", None);
        live.apply(&mut snapshot);
        assert_eq!(snapshot.decisions.len(), 1);
        let decision = &snapshot.decisions[0];
        assert!(decision.active);
        assert_eq!(decision.rules[0].classifier.as_deref(), Some("other/classifier"));
        assert_eq!(decision.rules[0].intent.as_deref(), Some("编码"));
        assert_eq!(decision.rules[1].state, "missing");
        assert_eq!(decision.route_groups, vec!["自动", "干活"]);
        assert_eq!(decision.attempts[0].state, "failed");
        assert_eq!(decision.attempts[1].state, "running");
        assert!(!serde_json::to_string(decision).unwrap().contains("PRIVATE"));
        live.activity.active[0].session = "other".into();
        let mut conflicting = crate::usage::UsageLedger::new(Local::now().date_naive()).snapshot("local", "ready", None);
        live.apply(&mut conflicting);
        assert!(conflicting.decisions.is_empty(), "身份冲突不归到错误会话");
        let mut completed = route;
        completed.done = true;
        completed.status = Some(200);
        live.update(TraceUpdate { pid: 7, seq: 2, ids: vec![101], routes: vec![completed] });
        live.activity.active.clear();
        let mut snapshot = crate::usage::UsageLedger::new(Local::now().date_naive()).snapshot("local", "ready", None);
        live.apply(&mut snapshot);
        assert!(!snapshot.decisions[0].active);
        assert_eq!(snapshot.decisions[0].state, "success");
    }

    /// 第 7 轮命中第 5 条规则且字段齐全；嵌套组按上游原序，缺身份的同名组不关联。
    #[test]
    fn decision_turn_identity_isolates_scope_and_tracks_continuations() {
        let mut live = LiveState::default();
        live.pid = 7;
        let mut route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 1, "session": "s1", "group": {"id": "auto", "name": "自动"},
            "rule": {"turn": 3, "classified": {"by": "typesafe/jev-latest", "ms": 224}}
        })).unwrap();
        let opening = live.decision_view(&route, &Active::default(), "s1");
        assert_eq!(opening.user_turn, Some(3));
        assert!(!opening.continuation);
        route.id = 2;
        route.rule.as_mut().unwrap().held = true;
        let followup = live.decision_view(&route, &Active::default(), "s1");
        assert_eq!(opening.turn_key, followup.turn_key);
        assert!(followup.continuation);
        route.subagent_id = "child".into();
        assert_ne!(opening.turn_key, live.decision_view(&route, &Active::default(), "s1").turn_key);
        route.subagent_id.clear();
        route.rule.as_mut().unwrap().turn = Some(4);
        assert_ne!(opening.turn_key, live.decision_view(&route, &Active::default(), "s1").turn_key);
        route.rule.as_mut().unwrap().turn = None;
        assert!(live.decision_view(&route, &Active::default(), "s1").turn_key.is_none());
        route.rule.as_mut().unwrap().turn = Some(3);
        live.pid = 8;
        assert_ne!(opening.turn_key, live.decision_view(&route, &Active::default(), "s1").turn_key);
    }

    #[test]
    fn decision_timing_keeps_missing_cache_and_reuse_distinct() {
        for (record, duration, cached) in [
            (serde_json::json!({"classified": {"by": "typesafe/jev-latest", "ms": 224}}), Some(224), false),
            (serde_json::json!({"classified": {"by": "typesafe/jev-latest", "ms": 1220}}), Some(1220), false),
            (serde_json::json!({"classified": {"ms": 0, "cached": true}}), Some(0), true),
            (serde_json::json!({"classified": {"ms": -1}}), None, false),
            (serde_json::json!({"classified": {"by": "typesafe/jev-latest"}}), None, false),
            (serde_json::json!({"held": true, "classified": {"ms": 224, "cached": true}}), None, false),
            (serde_json::json!({"waits": true, "classified": {"ms": 224}}), None, false),
            (serde_json::json!({"grown": true, "classified": {"ms": 2100}}), Some(2100), false),
        ] {
            let rule: TraceRule = serde_json::from_value(record).unwrap();
            let view = decision_rule("自动", &rule);
            assert_eq!((view.duration_ms, view.cached), (duration, cached));
            let serialized = serde_json::to_value(view).unwrap();
            assert_eq!(serialized["durationMs"], serde_json::json!(duration));
        }
    }

    #[test]
    fn decision_stages_keep_rule_detail_and_source_order() {
        let route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 7,
            "group": {"id": "root", "ref": "group/auto", "name": "自动", "routing": "order",
                "subs": [
                    {"id": "worker", "ref": "group/worker", "name": "干活", "routing": "rotate", "in": "root"},
                    {"id": "later", "ref": "group/later", "name": "收尾", "routing": "usage", "in": "root"},
                    {"id": "nameless", "name": "干活", "in": "root"}
                ]},
            "rule": {"n": 5, "turn": 7, "use": "group/worker", "pick": "high", "unready": true,
                "instead": "group/later", "classified": {"by": "fixture/classifier", "intent": "编码"}},
            "nested": [
                {"group": "worker", "name": "同名干扰", "rule": {"n": 1, "turn": 7, "held": true}},
                {"group": "later", "name": "同名干扰", "rule": {"n": 0, "turn": 7, "waits": true}},
                {"name": "干活", "rule": {"n": 9, "turn": 7}}
            ]
        })).unwrap();
        let stages = decision_stages(&route);
        assert_eq!(stages.iter().map(|stage| stage.key.as_str()).collect::<Vec<_>>(),
            vec!["root", "nested:0:worker:group/worker", "nested:1:later:group/later"]);
        let root = &stages[0];
        assert_eq!((root.group.id.as_deref(), root.group.reference.as_deref(), root.group.routing.as_deref()),
            (Some("root"), Some("group/auto"), Some("order")));
        let rule = root.rule.as_ref().unwrap();
        assert_eq!((rule.n, rule.turn, rule.unready, rule.instead.as_deref()),
            (Some(5), Some(7), true, Some("group/later")));
        assert_eq!(rule.state, "classified");
        assert_eq!(stages[1].group.name, "干活");
        assert_eq!(stages[1].group.routing.as_deref(), Some("rotate"));
        assert!(stages[1].confirmed, "in 指向根组，父子有证据");
        assert_eq!(stages[1].rule.as_ref().unwrap().state, "held");
        // later 的 in 也指向根组，不接在 worker 之后：保留记录顺序，但不确认成路径。
        assert!(!stages[2].confirmed);
        assert_eq!(stages[2].group.name, "收尾");
        assert_eq!(stages[2].rule.as_ref().unwrap().n, Some(0));
        assert_eq!(stages[2].rule.as_ref().unwrap().state, "missing");
        assert!(stages.iter().all(|stage| stage.context == "recorded"));

        // 缺 n 是未知，显式 0 才是未命中。
        let missing: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 71, "group": {"name": "自动"}, "rule": {"turn": 7}
        })).unwrap();
        assert_eq!(decision_stages(&missing)[0].rule.as_ref().unwrap().n, None);
    }

    /// 两次尝试各自取自己的候选路径；模型未记录的尝试也保留。
    #[test]
    fn decision_attempts_keep_distinct_chains_without_dropping_blank_models() {
        let route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 8,
            "group": {"id": "root", "name": "自动", "routing": "order", "subs": [
                {"id": "worker", "name": "干活", "routing": "rotate", "in": "root"},
                {"id": "fast", "name": "高速", "routing": "usage", "in": "root"}
            ]},
            "order": [
                {"id": "first", "provider": "fixture", "who": "one", "model": "m1", "via": ["worker"]},
                {"id": "second", "provider": "fixture", "who": "two", "model": "m2", "via": ["fast"]}
            ],
            "tries": [
                {"id": "first", "model": "m1", "effort": "high", "done": true, "status": 503},
                {"id": "second", "model": "", "done": false}
            ]
        })).unwrap();
        let live = LiveState::default();
        let decision = live.decision_view(&route, &Active::default(), "s1");
        assert_eq!(decision.attempts.len(), 2);
        assert_eq!(decision.attempts[0].ordinal, 1);
        assert_eq!(decision.attempts[0].group_chain.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(),
            vec!["自动", "干活"]);
        assert!(!decision.attempts[0].path_incomplete);
        assert_eq!(decision.attempts[1].ordinal, 2);
        assert!(decision.attempts[1].model.is_empty(), "模型未记录的尝试不跳过");
        assert_eq!(decision.attempts[1].group_chain.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(),
            vec!["自动", "高速"]);
        assert_eq!(decision.attempts[1].state, "running");
    }

    /// 同名子组只按身份关联；父子关系错误或未知层级截断，已确认的父级仍保留。
    #[test]
    fn decision_chain_rejects_ambiguous_names_and_broken_parents() {
        let route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 9,
            "group": {"id": "root", "name": "自动", "subs": [
                {"id": "worker-a", "name": "干活", "in": "root"},
                {"id": "worker-b", "name": "干活", "in": "root"},
                {"id": "child", "name": "深层", "in": "worker-a"},
                {"id": "stray", "name": "错挂", "in": "other"}
            ]},
            "order": [{"id": "seat", "provider": "fixture", "who": "one", "model": "m",
                "via": ["worker-a", "child", "stray", "missing"]}],
            "tries": [{"id": "seat", "model": "m", "done": false}]
        })).unwrap();
        let (chain, incomplete) = decision_attempt_chain(&route, route.tries.as_ref().unwrap().last().unwrap());
        assert_eq!(chain.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(),
            vec!["自动", "干活", "深层"]);
        assert!(incomplete, "父子错误与未知层级之后的路径不确认");
        let ambiguous: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 10,
            "group": {"id": "root", "name": "自动", "subs": [
                {"id": "worker-a", "ref": "shared", "name": "甲", "in": "root"},
                {"id": "worker-b", "ref": "shared", "name": "乙", "in": "root"}
            ]},
            "order": [{"id": "seat", "provider": "fixture", "who": "one", "model": "m", "via": ["shared"]}],
            "tries": [{"id": "seat", "model": "m"}]
        })).unwrap();
        let (chain, incomplete) = decision_attempt_chain(&ambiguous, ambiguous.tries.as_ref().unwrap().last().unwrap());
        assert_eq!(chain.len(), 1, "同一 ref 指向两个子组时不按同名或同 ref 猜测");
        assert!(incomplete);

        // 认不出唯一候选：只留入口组，并标明路径不能确认。
        let unknown: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 12, "group": {"id": "root", "name": "自动"},
            "order": [
                {"id": "seat", "provider": "a", "who": "one", "model": "m"},
                {"id": "seat", "provider": "b", "who": "two", "model": "m"}
            ],
            "tries": [{"id": "seat", "model": "m"}]
        })).unwrap();
        let (chain, incomplete) = decision_attempt_chain(&unknown, unknown.tries.as_ref().unwrap().last().unwrap());
        assert_eq!(chain.len(), 1);
        assert!(incomplete, "候选身份冲突时不能把入口组当成完整路径");

        // 多条路径只共享前缀：前缀之后不能唯一确认。
        let forked: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 13, "group": {"id": "root", "name": "自动", "subs": [
                {"id": "worker", "name": "干活", "in": "root"},
                {"id": "left", "name": "甲", "in": "worker"},
                {"id": "right", "name": "乙", "in": "worker"}
            ]},
            "order": [
                {"id": "seat", "provider": "fixture", "who": "one", "model": "m", "via": ["worker", "left"]},
                {"id": "seat", "provider": "fixture", "who": "one", "model": "m", "via": ["worker", "right"]}
            ],
            "tries": [{"id": "seat", "model": "m"}]
        })).unwrap();
        let (chain, incomplete) = decision_attempt_chain(&forked, forked.tries.as_ref().unwrap().last().unwrap());
        assert_eq!(chain.iter().map(|group| group.name.as_str()).collect::<Vec<_>>(), vec!["自动", "干活"]);
        assert!(incomplete, "公共前缀被截短后必须标路径不完整");

        // 子组没有 in：父组身份再明确也不能核验，截断并标不完整。
        let unlinked: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 14, "group": {"id": "root", "name": "自动", "subs": [
                {"id": "worker", "name": "干活"}
            ]},
            "order": [{"id": "seat", "provider": "fixture", "who": "one", "model": "m", "via": ["worker"]}],
            "tries": [{"id": "seat", "model": "m"}]
        })).unwrap();
        let (chain, incomplete) = decision_attempt_chain(&unlinked, unlinked.tries.as_ref().unwrap().last().unwrap());
        assert_eq!(chain.len(), 1);
        assert!(incomplete, "缺 in 不能当成已确认的父子关系");
    }

    /// 候选计划最多 12 项并标记省略数；账号、供应商与私有字段不外发。
    #[test]
    fn candidate_plan_truncates_and_keeps_account_identity_private() {
        let order: Vec<_> = (0..15).map(|index| serde_json::json!({
            "id": format!("account-{index}"), "provider": format!("provider-{index}"),
            "who": format!("user-{index}@example.com"), "model": format!("model-{index}"),
            "fixed": "max", "routing": "rotate", "name": "PRIVATE-ACCOUNT"
        })).collect();
        let route: LiveRoute = serde_json::from_value(serde_json::json!({
            "id": 11, "session": "s1", "group": {"name": "自动", "routing": "custom-secret"},
            "rule": {"n": 1, "turn": 1, "when": ["PRIVATE"], "classified": {"error": "PRIVATE"}},
            "order": order,
            "tries": [{"id": "account-0", "model": "model-0", "fixed": "max", "done": false}]
        })).unwrap();
        let mut live = LiveState::default();
        live.activity.active.push(Active { id: 1, session: "s1".into(), agent: "test".into(),
            started: "2026-10-27T12:00:00+08:00".into(), phase: "mystery".into(), route: 11, ..Active::default() });
        live.routes.insert(11, route);
        let mut snapshot = crate::usage::UsageLedger::new(Local::now().date_naive()).snapshot("local", "ready", None);
        live.apply(&mut snapshot);
        let decision = &snapshot.decisions[0];
        let plan = decision.candidate_plan.as_ref().unwrap();
        assert_eq!((plan.items.len(), plan.total, plan.omitted), (12, 15, 3));
        assert_eq!(plan.items[0].model, "model-0");
        assert_eq!(plan.items[11].ordinal, 12);
        assert_eq!(decision.phase, Some("unknown"));
        assert_eq!(decision.stages[0].group.routing, None, "未知策略字符串不外发");
        let json = serde_json::to_string(decision).unwrap();
        for secret in ["account-0", "provider-0", "user-0@example.com", "PRIVATE", "custom-secret"] {
            assert!(!json.contains(secret), "{secret} 不得进入判定区");
        }
    }

    /// 轮次索引有界：超出上限后最旧的条目被淘汰，不会无限增长。
    #[test]
    fn turn_history_evicts_oldest_beyond_cap() {
        let now = Local::now();
        let mut live = LiveState::default();
        for batch in 0..=(TURN_HISTORY_CAP / 500) {
            let routes: Vec<LiveRoute> = (0..500)
                .map(|index| LiveRoute {
                    id: (batch * 500 + index) as i64 + 1,
                    session: "s1".into(),
                    time: Some(now.to_rfc3339()),
                    rule: Some(TraceRule { turn: Some(1), ..TraceRule::default() }),
                    ..LiveRoute::default()
                })
                .collect();
            let ids: Vec<i64> = routes.iter().map(|route| route.id).collect();
            live.update(TraceUpdate { seq: 1, pid: 7, ids, routes });
        }
        assert_eq!(live.turn_history.len(), TURN_HISTORY_CAP);
        assert!(!live.turn_history.contains_key(&1), "最旧条目已淘汰");
        assert!(live.turn_history.contains_key(&(live.turn_history.len() as i64)));
    }
}
