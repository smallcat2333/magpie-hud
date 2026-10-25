//! 会话请求履历的按需只读查询：有界扫描 usage.jsonl 尾部并合并实时 trace 摘要。
//! 不读取请求/响应正文、密钥或规则表达式，也不触发费用与额度工作。

use crate::live::{HistoryScope, LiveHistoryRoute, TurnRecord};
use crate::usage::{parse_history_line, ParsedHistoryLine, RawHistoryRecord};
use chrono::{Duration, NaiveDate};
#[cfg(test)]
use chrono::Local;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 默认返回条数；面板展开时最多 200。
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
/// 含今日在内的本地自然日窗口。
pub const DAY_WINDOW: i64 = 7;
/// 单次查询最多读取的日志末尾字节数。
pub const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;
/// 与实时游标一致的单行上限；超长行跳过并计数。
const MAX_LINE_BYTES: usize = 64 * 1024;
const CHUNK_SIZE: usize = 64 * 1024;
/// 字节窗口起点的残行最多丢弃 1 MiB；无换行的异常文件直接放弃该窗口。
const MAX_DISCARD_BYTES: u64 = 1024 * 1024;
/// 日志 t 与 trace Time 都是请求开始时间；实测同一请求二者毫秒差为 0。
/// 保留 1 秒容差仅防序列化舍入，同时拦住网关重启后 route_id 复用造成的假匹配。
const MATCH_TOLERANCE_MS: i64 = 1_000;

/// 一次真实请求的履历行；完成行来自日志，运行行来自 trace，均不含正文。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteHistoryRow {
    pub id: String,
    /// 原始日志会话 ID；根履历可能跨显式别名家族的多行。
    pub session: String,
    pub subagent: Option<String>,
    pub route_id: Option<i64>,
    /// 请求开始时间（本地毫秒时间戳）。
    pub timestamp: i64,
    /// 发送模型（日志 model 字段）；响应报告模型单独见 served。
    pub model: String,
    pub served: Option<String>,
    pub effort: String,
    pub provider: String,
    pub account: String,
    /// 输入 + 输出 + 缓存读取。in/out 任一未记录时为 None，不把未知当 0 累加；
    /// cache_read 上游以 omitempty 序列化，缺失即真实 0。
    pub tokens: Option<u64>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    /// 仅成功且有有效生成时长（总耗时 − 首字耗时 > 0）时给出。
    pub tokens_per_second: Option<f64>,
    pub status: Option<u16>,
    /// success | failed | cancelled | unknown | running | waiting | awaiting_usage
    pub outcome: String,
    pub tries: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_groups: Vec<String>,
    pub rule_target: Option<String>,
    pub rule_effort: Option<String>,
    /// 本轮命中的意图摘要；仅实时 trace 保留期内可用。
    pub rule_intent: Option<String>,
    /// 意图分类置信度（0-1）。
    pub rule_sure: Option<f64>,
    /// 分类器被告知的上一轮意图；本轮意图为空时作为沿用线索。
    pub rule_after: Option<String>,
    /// 上游 fallback 标记；previous 表示意图沿用上一轮而非本轮实测。
    pub rule_fallback: Option<String>,
    /// 该请求所在用户输入（轮）的序号；trace 未保留或非规则组时为 None。
    pub turn: Option<i64>,
    /// 该请求是本轮开始后的后续请求（沿用轮初判定），用于归组。
    #[serde(skip)]
    pub turn_held: bool,
    /// 生成时长（毫秒，总耗时 − 首字耗时）；组行 TPS 由组内求和得出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<f64>,
}

/// 一次用户输入（轮）的履历组：连续且轮次序号相同的请求合并为一行。
/// 无轮次记录的请求各自单独成组（requests 只有一行）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteHistoryGroup {
    /// 组内首个请求的 id，展开状态按它记忆。
    pub id: String,
    /// 该轮序号；无记录时为 None。
    pub turn: Option<i64>,
    /// 组内请求数；1 表示不可折叠。
    pub count: usize,
    /// 组内最早的请求开始时间。
    pub timestamp: i64,
    /// 意图取自该轮开口请求（turn 行），缺省时回退组内首个非空意图。
    pub rule_intent: Option<String>,
    pub rule_sure: Option<f64>,
    /// 意图来源：measured 本轮实测；previous 分类失败沿用上一轮；after 本轮无意图，展示上一轮线索。
    pub intent_source: String,
    /// 开口请求的规则目标与组链；组内后续请求沿用，不逐条罗列。
    pub rule_target: Option<String>,
    pub route_groups: Vec<String>,
    /// 组内出现过的模型与强度（去重，保持首次出现顺序）。
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    /// 组内 Token 合计；任一请求计数缺失时为 None，不把未知当 0。
    pub tokens: Option<u64>,
    /// 组内输出合计 ÷ 生成时长合计（tok/s）；没有可计请求时为 None。
    pub tokens_per_second: Option<f64>,
    /// 组状态按最重者：running > waiting > failed > cancelled > awaiting_usage > unknown > success。
    pub outcome: String,
    /// 组内请求，按开始时间正序。
    pub requests: Vec<RouteHistoryRow>,
}

/// 履历查询结果；覆盖范围显式说明，不把“窗口内无记录”说成“没有历史”。
/// 条数限制按组计：rows 是已选组内请求的扁平视图，兼容按请求读取的调用方。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteHistoryData {
    pub session: String,
    pub subagent: Option<String>,
    pub start_date: String,
    pub end_date: String,
    pub bytes_scanned: u64,
    pub byte_limited: bool,
    pub row_limited: bool,
    /// 组数超过上限时为 true；与 row_limited 同义，按组口径命名。
    pub group_limited: bool,
    pub groups: Vec<RouteHistoryGroup>,
    pub rows: Vec<RouteHistoryRow>,
    pub warnings: Vec<String>,
}

/// 与账本 Record::outcome 相同的状态口径；499 是调用方取消。
fn outcome_of(status: Option<u16>) -> &'static str {
    match status {
        Some(status) if (200..300).contains(&status) => "success",
        Some(499) => "cancelled",
        Some(_) => "failed",
        None => "unknown",
    }
}

/// Token 总数只在输入与输出都已记录时给出；缺失的 cache_read 按上游 omitempty 语义计 0。
fn tokens_of(record: &RawHistoryRecord) -> Option<u64> {
    let (input, output) = (record.input?, record.output?);
    Some(input.saturating_add(output).saturating_add(record.cache_read.unwrap_or(0)))
}

/// TPS 与卡片口径一致：成功且 ms−ttft_ms 为正的有限时长，输出按记录值。
fn tps_of(record: &RawHistoryRecord) -> Option<f64> {
    if outcome_of(record.status) != "success" {
        return None;
    }
    let generation = record
        .duration_ms
        .zip(record.ttft_ms)
        .map(|(ms, ttft)| ms - ttft)
        .filter(|ms| *ms > 0.0)?;
    Some(record.output? as f64 * 1000.0 / generation)
}

/// 组行 TPS 需要组内求和，保留生成时长而不是逐行速度。
fn generation_of(record: &RawHistoryRecord) -> Option<f64> {
    if outcome_of(record.status) != "success" {
        return None;
    }
    record
        .duration_ms
        .zip(record.ttft_ms)
        .map(|(ms, ttft)| ms - ttft)
        .filter(|ms| *ms > 0.0)
}

fn completed_row(record: RawHistoryRecord) -> RouteHistoryRow {
    let tokens = tokens_of(&record);
    let tokens_per_second = tps_of(&record);
    let generation_ms = generation_of(&record);
    RouteHistoryRow {
        id: record.identity.clone(),
        session: record.session.clone(),
        subagent: (!record.subagent.is_empty()).then(|| record.subagent.clone()),
        route_id: record.route_id,
        timestamp: record.timestamp_ms,
        model: record.model.clone(),
        served: record.served.clone(),
        effort: record.effort.clone(),
        provider: record.provider.clone(),
        account: record.account.clone(),
        tokens,
        input: record.input,
        output: record.output,
        cache_read: record.cache_read,
        tokens_per_second,
        status: record.status,
        outcome: outcome_of(record.status).into(),
        tries: record.tries,
        route_groups: Vec::new(),
        rule_target: None,
        rule_effort: None,
        rule_intent: None,
        rule_sure: None,
        rule_after: None,
        rule_fallback: None,
        turn: None,
        turn_held: false,
        generation_ms,
    }
}

struct ScanOutcome {
    /// 窗口内全部匹配行，按开始时间倒序；未截断。
    records: Vec<RawHistoryRecord>,
    bytes_scanned: u64,
    byte_limited: bool,
    malformed: u64,
    warnings: Vec<String>,
}

/// 只读日志末尾窗口内的完整行；不修改实时游标，也不构建整日账本。
fn scan_completed(
    path: &Path,
    family: &HashSet<String>,
    child: Option<&str>,
    start_day: NaiveDate,
    end_day: NaiveDate,
    max_bytes: u64,
) -> Result<ScanOutcome, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        // 日志缺失不挡住面板：调用方仍可展示实时行，并在覆盖说明里告知原因。
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScanOutcome {
                records: Vec::new(),
                bytes_scanned: 0,
                byte_limited: false,
                malformed: 0,
                warnings: vec!["用量日志文件未找到，仅显示当前实时请求".into()],
            });
        }
        Err(error) => return Err(format!("无法读取日志：{error}")),
    };
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let window_start = length.saturating_sub(max_bytes);
    let mut warnings = Vec::new();
    if window_start > 0 {
        let mut aligned = false;
        // 窗口起点前一字节是换行说明恰好对齐行首，不丢首行。
        file.seek(SeekFrom::Start(window_start - 1)).map_err(|e| e.to_string())?;
        let mut probe = [0u8; 1];
        if file.read(&mut probe).map_err(|e| e.to_string())? == 1 && probe[0] == b'\n' {
            file.seek(SeekFrom::Start(window_start)).map_err(|e| e.to_string())?;
            aligned = true;
        }
        if !aligned {
            file.seek(SeekFrom::Start(window_start)).map_err(|e| e.to_string())?;
            // 起点落在行中间，丢弃到下一个换行再开始解析；读取不超过捕获的 EOF。
            let mut buffer = vec![0u8; CHUNK_SIZE];
            let mut discarded: u64 = 0;
            while !aligned && discarded <= MAX_DISCARD_BYTES {
                let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                discarded += count as u64;
                if let Some(index) = buffer[..count].iter().position(|byte| *byte == b'\n') {
                    let after = window_start + discarded - (count as u64 - index as u64 - 1);
                    file.seek(SeekFrom::Start(after)).map_err(|e| e.to_string())?;
                    aligned = true;
                }
            }
            if !aligned {
                warnings.push("日志末尾窗口内没有完整行，未读取到记录".into());
                return Ok(ScanOutcome {
                    records: Vec::new(),
                    bytes_scanned: discarded,
                    byte_limited: true,
                    malformed: 0,
                    warnings,
                });
            }
        }
    } else {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    }
    // 捕获打开时刻的 EOF；读取期间追加的内容留给下一次查询。
    let eof = length;
    let mut position = file.stream_position().map_err(|e| e.to_string())?;
    let mut pending: Vec<u8> = Vec::new();
    let mut skipping_long = false;
    let mut malformed: u64 = 0;
    let mut sequence: u64 = 0;
    let mut seen: HashSet<String> = HashSet::new();
    let mut records: Vec<(i64, u64, RawHistoryRecord)> = Vec::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    while position < eof {
        let amount = ((eof - position) as usize).min(CHUNK_SIZE);
        let count = file.read(&mut buffer[..amount]).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        position += count as u64;
        for segment in buffer[..count].split_inclusive(|byte| *byte == b'\n') {
            let complete = segment.last() == Some(&b'\n');
            if !skipping_long {
                if pending.len() + segment.len() > MAX_LINE_BYTES {
                    pending.clear();
                    skipping_long = true;
                    malformed += 1;
                } else {
                    pending.extend_from_slice(segment);
                }
            }
            if complete {
                if !skipping_long {
                    sequence += 1;
                    match parse_history_line(&pending) {
                        ParsedHistoryLine::Record(record) => {
                            let scope_matches = family.contains(&record.session)
                                && match child {
                                    Some(wanted) => record.subagent == wanted,
                                    None => record.subagent.is_empty(),
                                };
                            // handed_on 是转交下一尝试的计费行，不作为独立最终请求。
                            if scope_matches
                                && !record.handed_on
                                && record.day >= start_day
                                && record.day <= end_day
                                && seen.insert(record.identity.clone())
                            {
                                records.push((record.timestamp_ms, sequence, record));
                            }
                        }
                        ParsedHistoryLine::Malformed => malformed += 1,
                        ParsedHistoryLine::Ignored => {}
                    }
                }
                pending.clear();
                skipping_long = false;
            }
        }
    }
    // 末尾未写完的半行留给下一次读取，不当作坏行。
    records.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    Ok(ScanOutcome {
        records: records.into_iter().map(|(_, _, record)| record).collect(),
        bytes_scanned: eof - window_start,
        byte_limited: window_start > 0,
        malformed,
        warnings,
    })
}

/// 终态证据复核：trace 终态尝试与日志的模型/账号/供应商/状态任一冲突即视为不同请求，
/// 不用该 trace 补组链或响应模型；只有双方都有值时才比较，单侧未知不判冲突。
fn trace_agrees(live: &LiveHistoryRoute, row: &RouteHistoryRow) -> bool {
    let model_agrees = live.model.is_empty() || row.model.is_empty() || live.model == row.model;
    let account_agrees =
        live.account.is_empty() || row.account.is_empty() || live.account == row.account;
    let provider_agrees =
        live.provider.is_empty() || row.provider.is_empty() || live.provider == row.provider;
    let status_agrees = live
        .status
        .zip(row.status)
        .is_none_or(|(live_status, row_status)| live_status == row_status as i64);
    model_agrees && account_agrees && provider_agrees && status_agrees
}

/// 履历排序：进行中/等待用量的请求始终排在完成行之前——一个长时间运行的请求
/// 开始时间可能早于新完成的请求，但仍是当前正在发生的事；同类内按开始时间倒序。
/// 组行沿用同一口径（见 group_rows 末尾排序），不再单独对请求列表排序。
fn is_live_row(row: &RouteHistoryRow) -> bool {
    matches!(
        row.outcome.as_str(),
        "running" | "waiting" | "awaiting_usage"
    )
}

/// 日志行与实时行合并：route_id＋原始会话＋子身份＋开始时间同时吻合即同一请求，
/// 日志终态压掉实时行；组链/规则/响应模型只在终态证据不冲突时补充。
/// 返回未排序结果，由调用方统一排序并截取条数。
fn merge_live(rows: Vec<RouteHistoryRow>, live: &[LiveHistoryRoute]) -> Vec<RouteHistoryRow> {
    // 按实时行下标记录吸收结果：网关重启复用 route_id 时，同 id 的另一条
    // 实时请求仍是独立请求，不能被旧日志行的匹配连带吞掉。
    let mut matched: HashSet<usize> = HashSet::new();
    let mut enriched: Vec<RouteHistoryRow> = Vec::with_capacity(rows.len());
    for mut row in rows {
        if let Some(route_id) = row.route_id {
            let hit = live.iter().position(|live| {
                live.route_id == route_id
                    && live.session == row.session
                    && live.subagent == row.subagent.clone().unwrap_or_default()
                    && live
                        .started_ms
                        .is_some_and(|start| (start - row.timestamp).abs() <= MATCH_TOLERANCE_MS)
            });
            if let Some(index) = hit {
                matched.insert(index);
                let live = &live[index];
                // 证据冲突说明终态尝试不可靠：仍由日志压住实时行，但不借它补链。
                if trace_agrees(live, &row) {
                    if row.route_groups.is_empty() && !live.route_groups.is_empty() {
                        row.route_groups = live.route_groups.clone();
                    }
                    if row.rule_target.is_none() {
                        row.rule_target = live.rule_target.clone();
                    }
                    if row.rule_effort.is_none() {
                        row.rule_effort = live.rule_effort.clone();
                    }
                    if row.rule_intent.is_none() {
                        row.rule_intent = live.rule_intent.clone();
                        row.rule_sure = live.rule_sure;
                    }
                    if row.served.is_none() && !live.served.is_empty() {
                        row.served = Some(live.served.clone());
                    }
                }
            }
        }
        enriched.push(row);
    }
    // 无开始时间或已过等待窗口的实时行不成独立行：前者无法对齐，
    // 后者只保留为日志行的补全证据，避免陈旧 trace 永久挂起。
    let mut rows: Vec<_> = live
        .iter()
        .enumerate()
        .filter(|(index, live)| {
            live.eligible_overlay && live.started_ms.is_some() && !matched.contains(index)
        })
        .map(|(_, live)| RouteHistoryRow {
            id: format!("live-{}", live.route_id),
            session: live.session.clone(),
            subagent: (!live.subagent.is_empty()).then(|| live.subagent.clone()),
            route_id: Some(live.route_id),
            timestamp: live.started_ms.unwrap_or_default(),
            model: live.model.clone(),
            served: (!live.served.is_empty()).then(|| live.served.clone()),
            effort: live.effort.clone(),
            provider: live.provider.clone(),
            account: live.account.clone(),
            tokens: None,
            input: None,
            output: None,
            cache_read: None,
            tokens_per_second: None,
            status: None,
            outcome: live.phase.to_string(),
            tries: 1,
            route_groups: live.route_groups.clone(),
            rule_target: live.rule_target.clone(),
            rule_effort: live.rule_effort.clone(),
            rule_intent: live.rule_intent.clone(),
            rule_sure: live.rule_sure,
            rule_after: None,
            rule_fallback: None,
            turn: None,
            turn_held: false,
            generation_ms: None,
        })
        .collect();
    rows.append(&mut enriched);
    rows
}

/// 轮次索引补到行上。route_id 网关重启后会复用，必须连同开始时间吻合才算同一请求；
/// 对不上的行保持 turn=None，分组时各自单独成组，不并进任何输入。
fn annotate_turns(rows: &mut [RouteHistoryRow], turns: &HashMap<i64, TurnRecord>) {
    for row in rows {
        let Some(route_id) = row.route_id else { continue };
        let Some(record) = turns.get(&route_id) else { continue };
        if record.session != row.session
            || record.subagent != row.subagent.clone().unwrap_or_default()
            || (record.time_ms - row.timestamp).abs() > MATCH_TOLERANCE_MS
        {
            continue;
        }
        row.turn = Some(record.turn);
        row.turn_held = record.held;
        if row.rule_intent.is_none() && !record.intent.is_empty() {
            row.rule_intent = Some(record.intent.clone());
            row.rule_sure = record.sure;
        }
        if row.rule_after.is_none() && !record.after.is_empty() {
            row.rule_after = Some(record.after.clone());
        }
        if row.rule_fallback.is_none() && !record.fallback.is_empty() {
            row.rule_fallback = Some(record.fallback.clone());
        }
    }
}

/// 组状态取最重者：进行中的事优先于已结束的失败，失败优先于取消，
/// 未知与待用量不掩盖已确认的成功。
fn outcome_rank(outcome: &str) -> u8 {
    match outcome {
        "running" => 6,
        "waiting" => 5,
        "failed" => 4,
        "cancelled" => 3,
        "awaiting_usage" => 2,
        "unknown" => 1,
        _ => 0,
    }
}

/// 按开始时间正序归组。轮次序号只在同一会话线内可比，且真实流量里同一会话会交错出现
/// 多条输入（不同会话线共用原生会话 ID），因此按「会话线＋轮次」记住最近一次开口组：
/// - held=false 的开口请求总是新开一组，并成为该轮次的最近开口组；
/// - held=true 的续行挂回该轮次最近的开口组，不因中间夹了别的轮次的行而断开；
/// - 找不到对应开口组（开口落在 trace 窗口外）时，续行自成一组成组，后续同轮续行再并入。
/// 没有轮次记录的请求各自成组，不猜测归属。
fn group_rows(rows: Vec<RouteHistoryRow>) -> Vec<RouteHistoryGroup> {
    let mut ordered = rows;
    ordered.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
    let mut groups: Vec<RouteHistoryGroup> = Vec::new();
    // (会话线, 轮次) → 该轮次最近的组下标；续行按它挂回本轮。
    let mut latest: HashMap<(String, i64), usize> = HashMap::new();
    for row in ordered {
        let held = row.turn_held;
        let join = match row.turn {
            // 开口请求或无法判定轮次的请求：新的一组。
            None => None,
            Some(_) if !held => None,
            Some(turn) => latest.get(&(row.session.clone(), turn)).copied(),
        };
        match join {
            Some(index) => {
                let group = &mut groups[index];
                group.count += 1;
                group.requests.push(row);
            }
            None => {
                let index = groups.len();
                if let Some(turn) = row.turn {
                    latest.insert((row.session.clone(), turn), index);
                }
                groups.push(RouteHistoryGroup {
                    id: row.id.clone(),
                    turn: row.turn,
                    count: 1,
                    timestamp: row.timestamp,
                    rule_intent: None,
                    rule_sure: None,
                    intent_source: "none".into(),
                    rule_target: None,
                    route_groups: Vec::new(),
                    models: Vec::new(),
                    efforts: Vec::new(),
                    tokens: None,
                    tokens_per_second: None,
                    outcome: String::new(),
                    requests: vec![row],
                });
            }
        }
    }
    for group in &mut groups {
        summarize_group(group);
    }
    groups.sort_by(|a, b| {
        let live = |group: &RouteHistoryGroup| {
            group
                .requests
                .iter()
                .any(|row| is_live_row(row))
        };
        live(b)
            .cmp(&live(a))
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.id.cmp(&b.id))
    });
    groups
}

/// 组摘要：意图取该轮开口请求（turn 行），没有则回退组内首个非空意图；
/// 都没有时用开口请求的 after 作为沿用线索。判定取开口请求的组链。
fn summarize_group(group: &mut RouteHistoryGroup) {
    let opening = group
        .requests
        .iter()
        .find(|row| row.rule_intent.is_some() || row.rule_after.is_some())
        .or(group.requests.first());
    if let Some(opening) = opening {
        if let Some(intent) = opening.rule_intent.clone() {
            group.rule_intent = Some(intent);
            group.rule_sure = opening.rule_sure;
            group.intent_source = if opening.rule_fallback.as_deref() == Some("previous") {
                "previous".into()
            } else {
                "measured".into()
            };
        } else if let Some(after) = opening.rule_after.clone() {
            group.rule_intent = Some(after);
            group.intent_source = "after".into();
        }
        group.rule_target = opening.rule_target.clone();
        group.route_groups = opening.route_groups.clone();
    }
    let mut outcome = "success".to_owned();
    let mut tokens_known = true;
    let mut tokens: u64 = 0;
    let mut output: u64 = 0;
    let mut generation: f64 = 0.0;
    let mut generation_known = false;
    for row in &group.requests {
        let model = row.served.as_deref().filter(|s| !s.is_empty()).unwrap_or(&row.model);
        if !model.is_empty() && !group.models.iter().any(|known| known == model) {
            group.models.push(model.to_owned());
        }
        if !row.effort.is_empty() && !group.efforts.iter().any(|known| known == &row.effort) {
            group.efforts.push(row.effort.clone());
        }
        if outcome_rank(&row.outcome) > outcome_rank(&outcome) {
            outcome = row.outcome.clone();
        }
        match row.tokens {
            Some(value) => tokens = tokens.saturating_add(value),
            None => tokens_known = false,
        }
        if let (Some(out), Some(ms)) = (row.output, row.generation_ms) {
            output = output.saturating_add(out);
            generation += ms;
            generation_known = true;
        }
    }
    group.outcome = outcome;
    group.tokens = tokens_known.then_some(tokens);
    group.tokens_per_second = generation_known
        .then_some(output as f64 * 1000.0 / generation)
        .filter(|speed| speed.is_finite());
}

/// 按需履历查询入口；路径由配置解析，调用方只传会话身份与条数。
pub fn query(
    path: &Path,
    session: &str,
    subagent: Option<&str>,
    limit: usize,
    scope: HistoryScope,
    today: NaiveDate,
) -> Result<RouteHistoryData, String> {
    let end_day = today;
    let start_day = today - Duration::days(DAY_WINDOW - 1);
    // 根履历按显式别名家族读日志；子履历严格按原始父会话 ID，不跨别名搬移子请求。
    let scan_family: HashSet<String> = if subagent.is_some() {
        let mut exact = HashSet::new();
        exact.insert(session.to_owned());
        exact
    } else {
        scope.family.clone()
    };
    let scan = scan_completed(path, &scan_family, subagent, start_day, end_day, MAX_SCAN_BYTES)?;
    let mut warnings = scan.warnings;
    if scan.byte_limited {
        warnings.push("更早记录未读取（本次只读日志末尾 4 MiB）".into());
    }
    if scan.malformed > 0 {
        warnings.push(format!("跳过 {} 条无法解析的超长或坏行", scan.malformed));
    }
    // 先合并实时行再去重复计数与截取：运行请求随最新时间自然置顶，
    // done 但日志未到的 trace 也不会因截取顺序与完成行重复出现。
    let completed: Vec<RouteHistoryRow> = scan.records.into_iter().map(completed_row).collect();
    let mut rows = merge_live(completed, &scope.routes);
    // 轮次索引在截断前补齐：trace 已淘汰的请求仍能并进正确的输入组。
    annotate_turns(&mut rows, &scope.turns);
    let groups = group_rows(rows);
    // 条数限制按组计，截断点落在组边界，不把一组输入拆成两半。
    let row_limited = groups.len() > limit;
    let groups: Vec<RouteHistoryGroup> = groups.into_iter().take(limit).collect();
    let rows: Vec<RouteHistoryRow> = groups
        .iter()
        .flat_map(|group| group.requests.iter().cloned())
        .collect();
    Ok(RouteHistoryData {
        session: session.to_owned(),
        subagent: subagent.map(str::to_owned),
        start_date: start_day.to_string(),
        end_date: end_day.to_string(),
        bytes_scanned: scan.bytes_scanned,
        byte_limited: scan.byte_limited,
        row_limited,
        group_limited: row_limited,
        groups,
        rows,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 独立临时目录，不写真实日志。
    fn temp_log(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!("magpie-hud-history-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("usage.jsonl")
    }

    fn at(days_ago: i64, hour: u32) -> String {
        use chrono::{Datelike, TimeZone};
        let day = Local::now().date_naive() - Duration::days(days_ago);
        Local.with_ymd_and_hms(day.year(), day.month(), day.day(), hour, 0, 0)
            .unwrap()
            .to_rfc3339()
    }

    fn line(route: i64, t: &str, extra: serde_json::Value) -> String {
        let mut value = json!({"route_id":route,"t":t,"session":"s1","providerAccount":"a@x.com","model":"m1","effort":"high"});
        if let serde_json::Value::Object(fields) = extra {
            for (key, field) in fields {
                value[key] = field;
            }
        }
        value.to_string()
    }

    fn family(set: &[&str]) -> HashSet<String> {
        set.iter().map(|id| id.to_string()).collect()
    }

    /// 固定在今天正午的锚点：测试内的时间偏移不会跨到昨天/明天。
    fn noon_today() -> chrono::DateTime<Local> {
        use chrono::TimeZone;
        let noon = Local::now().date_naive().and_hms_opt(12, 0, 0).unwrap();
        Local.from_local_datetime(&noon).unwrap()
    }

    fn live_row(route_id: i64, started_ms: Option<i64>, phase: &'static str) -> LiveHistoryRoute {
        LiveHistoryRoute {
            route_id,
            session: "s1".into(),
            subagent: String::new(),
            started_ms,
            model: "m-live".into(),
            effort: "high".into(),
            account: "a@x.com".into(),
            provider: String::new(),
            served: "served-live".into(),
            route_groups: vec!["自动".into(), "干活".into()],
            rule_target: Some("group/worker".into()),
            rule_effort: Some("xhigh".into()),
            rule_intent: None,
            rule_sure: None,
            status: None,
            phase,
            eligible_overlay: true,
        }
    }

    /// 完成行口径：状态分类、TPS 条件、缺失与真实零、handed_on 与去重、子会话隔离。
    #[test]
    fn completed_rows_outcomes_and_missing_counters() {
        let path = temp_log("outcomes");
        let now = noon_today();
        let ok = line(1, &now.to_rfc3339(), json!({"status":200,"in":10,"out":20,"cache_read":30,"ms":2000,"ttft_ms":500}));
        let failed = line(2, &(now - Duration::seconds(60)).to_rfc3339(), json!({"status":500,"in":1,"out":0,"tries":2}));
        let cancelled = line(3, &(now - Duration::seconds(120)).to_rfc3339(), json!({"status":499}));
        let unknown = line(4, &(now - Duration::seconds(180)).to_rfc3339(), json!({}));
        let zero = line(5, &(now - Duration::seconds(240)).to_rfc3339(), json!({"status":200,"in":0,"out":0,"cache_read":0,"ms":1500,"ttft_ms":500}));
        let no_cache_field = line(6, &(now - Duration::seconds(300)).to_rfc3339(), json!({"status":200,"in":7,"out":3}));
        let missing_out = line(7, &(now - Duration::seconds(360)).to_rfc3339(), json!({"status":200,"in":5}));
        let child = line(8, &(now - Duration::seconds(420)).to_rfc3339(), json!({"status":200,"in":1,"out":1,"subagent":"c1"}));
        let other_session = line(9, &(now - Duration::seconds(480)).to_rfc3339(), json!({"status":200,"session":"s2","in":1,"out":1}));
        let handed_on = line(1, &(now - Duration::seconds(500)).to_rfc3339(), json!({"status":402,"in":2,"out":0,"handed_on":true,"providerAccount":"b@x.com"}));
        let routing = json!({"kind":"routing","t":now.to_rfc3339(),"session":"s1"}).to_string();
        let malformed = "{\"oops".to_string();
        std::fs::write(&path, [ok.clone(), failed, cancelled, unknown, zero, no_cache_field, missing_out, child, other_session, handed_on, routing, malformed, ok].join("\n") + "\n").unwrap();

        let scope_family = family(&["s1"]);
        let today = Local::now().date_naive();
        let scan = scan_completed(&path, &scope_family, None, today - Duration::days(6), today, MAX_SCAN_BYTES).unwrap();
        assert_eq!(scan.records.len(), 7);
        let rows: Vec<RouteHistoryRow> = scan.records.clone().into_iter().map(completed_row).collect();
        fn by_route(rows: &[RouteHistoryRow], id: i64) -> &RouteHistoryRow {
            rows.iter().find(|row| row.route_id == Some(id)).unwrap()
        }

        let success = by_route(&rows, 1);
        assert_eq!(success.outcome, "success");
        assert_eq!(success.tokens, Some(60));
        assert_eq!(success.input, Some(10));
        assert!((success.tokens_per_second.unwrap() - 20.0 * 1000.0 / 1500.0).abs() < 1e-9);
        assert_eq!(success.served, None);
        let failed_row = by_route(&rows, 2);
        assert_eq!(failed_row.outcome, "failed");
        assert_eq!(failed_row.tries, 2);
        assert_eq!(failed_row.tokens_per_second, None);
        assert_eq!(by_route(&rows, 3).outcome, "cancelled");
        let unknown_row = by_route(&rows, 4);
        assert_eq!(unknown_row.outcome, "unknown");
        assert_eq!(unknown_row.tokens, None);
        let zero_row = by_route(&rows, 5);
        assert_eq!(zero_row.tokens, Some(0));
        assert!((zero_row.tokens_per_second.unwrap() - 0.0).abs() < 1e-9);
        // cache_read 省略按上游 omitempty 语义计 0；in/out 任一缺失则总数未知。
        assert_eq!(by_route(&rows, 6).tokens, Some(10));
        assert_eq!(by_route(&rows, 7).tokens, None);
        // 子请求、其它会话、handed_on 与重复行都不进根履历。
        assert!(rows.iter().all(|row| row.route_id != Some(8) && row.route_id != Some(9)));
        assert_eq!(scan.records.iter().filter(|record| record.route_id == Some(1)).count(), 1);

        let child_scan = scan_completed(&path, &scope_family, Some("c1"), today - Duration::days(6), today, MAX_SCAN_BYTES).unwrap();
        assert_eq!(child_scan.records.len(), 1);
        assert_eq!(child_scan.records[0].subagent, "c1");
        std::fs::remove_file(&path).ok();
    }

    /// 条数截断必须显式标记，不悄悄丢弃。
    #[test]
    fn limit_marks_row_limited() {
        let path = temp_log("limit");
        let now = noon_today();
        let content: Vec<String> = (1..=3)
            .map(|route| line(route, &(now - Duration::seconds(route * 10)).to_rfc3339(), json!({"status":200,"in":1,"out":1})))
            .collect();
        std::fs::write(&path, content.join("\n") + "\n").unwrap();
        let scope = crate::live::LiveState::default().history_scope("s1", None);
        let data = query(&path, "s1", None, 2, scope, Local::now().date_naive()).unwrap();
        assert_eq!(data.groups.len(), 2);
        assert!(data.row_limited);
        assert!(data.group_limited);
        // 无轮次记录时每请求自成一组，最新的 route 1 在前，最早的 route 3 被截掉。
        assert_eq!(data.groups[0].requests[0].route_id, Some(1));
        assert_eq!(data.groups[1].requests[0].route_id, Some(2));
        std::fs::remove_file(&path).ok();
    }

    /// 字节窗口从行边界开始：起点残行丢弃，只解析窗口内完整行。
    #[test]
    fn byte_window_discards_partial_first_line() {
        let path = temp_log("window");
        let now = noon_today();
        let first = line(1, &(now - Duration::seconds(120)).to_rfc3339(), json!({"status":200,"in":1,"out":1}));
        let second = line(2, &(now - Duration::seconds(60)).to_rfc3339(), json!({"status":200,"in":2,"out":2}));
        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        let length = std::fs::metadata(&path).unwrap().len();
        // 窗口只盖住第二行的中段：残行丢弃后仍应完整解析第二行。
        let max_bytes = (second.len() as u64) + 8;
        let scope_family = family(&["s1"]);
        let today = Local::now().date_naive();
        let scan = scan_completed(&path, &scope_family, None, today - Duration::days(6), today, max_bytes).unwrap();
        assert!(scan.byte_limited);
        assert_eq!(scan.bytes_scanned, max_bytes.min(length));
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.records[0].route_id, Some(2));
        std::fs::remove_file(&path).ok();
    }

    /// 超长完整行跳过并计数，不影响前后行。
    #[test]
    fn oversized_line_is_skipped() {
        let path = temp_log("oversized");
        let now = noon_today();
        let pad = "a".repeat(MAX_LINE_BYTES + 1024);
        let big = line(1, &now.to_rfc3339(), json!({"status":200,"pad":pad}));
        let after = line(2, &(now - Duration::seconds(30)).to_rfc3339(), json!({"status":200,"in":1,"out":1}));
        std::fs::write(&path, format!("{big}\n{after}\n")).unwrap();
        let scope_family = family(&["s1"]);
        let today = Local::now().date_naive();
        let scan = scan_completed(&path, &scope_family, None, today - Duration::days(6), today, MAX_SCAN_BYTES).unwrap();
        assert_eq!(scan.records.len(), 1);
        assert_eq!(scan.records[0].route_id, Some(2));
        assert_eq!(scan.malformed, 1);
        std::fs::remove_file(&path).ok();
    }

    /// 端到端：陈旧 done trace 补全日志行但不单独成行；子履历严格按原始父会话。
    #[test]
    fn stale_done_trace_enriches_without_overlay_and_child_is_exact() {
        let path = temp_log("stale");
        let now = noon_today();
        // 日志 t 与 trace.time 是同一请求的开始时间，必须一致才能对上。
        let old = (now - Duration::minutes(30)).to_rfc3339();
        let root = line(1, &old, json!({"status":200,"in":1,"out":1}));
        let child = line(2, &(now - Duration::seconds(30)).to_rfc3339(), json!({"status":200,"in":1,"out":1,"subagent":"c1"}));
        let alias_child = line(3, &(now - Duration::seconds(60)).to_rfc3339(), json!({"status":200,"in":1,"out":1,"session":"h1","subagent":"c1"}));
        std::fs::write(&path, format!("{root}\n{child}\n{alias_child}\n")).unwrap();

        let mut live = crate::live::LiveState::default();
        live.aliases.insert("h1".into(), "s1".into());
        let old = (now - Duration::minutes(30)).to_rfc3339();
        let trace_route = crate::live::LiveRoute {
            id: 1, session: "s1".into(), done: true, time: Some(old),
            group: Some(crate::live::RouteGroup { name: "自动".into(), subs: vec![
                crate::live::RouteSubgroup { id: "sub-a".into(), name: "干活".into(), ..Default::default() },
            ], ..Default::default() }),
            order: Some(vec![crate::live::Candidate { id: "seat".into(), provider: "codex".into(),
                model: "m1".into(), who: "a@x.com".into(), via: vec!["sub-a".into()], ..Default::default() }]),
            tries: Some(vec![crate::live::Attempt { id: "seat".into(), model: "m1".into(),
                done: true, ..Default::default() }]),
            rule: Some(crate::live::TraceRule { use_target: "group/worker".into(), pick: "xhigh".into(), ..Default::default() }),
            ..Default::default()
        };
        live.update(crate::live::TraceUpdate {
            seq: 1,
            pid: 7,
            ids: vec![1],
            routes: vec![trace_route],
        });

        let scope = live.history_scope("s1", None);
        let data = query(&path, "s1", None, DEFAULT_LIMIT, scope, now.date_naive()).unwrap();
        assert_eq!(data.rows.len(), 1, "别名会话的子请求不进根履历");
        assert_eq!(data.rows[0].route_groups, vec!["自动".to_string(), "干活".to_string()]);
        assert_eq!(data.rows[0].rule_target.as_deref(), Some("group/worker"));
        assert!(data.rows.iter().all(|row| row.outcome != "awaiting_usage"));

        let child_scope = live.history_scope("s1", Some("c1"));
        let child_data = query(&path, "s1", Some("c1"), DEFAULT_LIMIT, child_scope, now.date_naive()).unwrap();
        assert_eq!(child_data.rows.len(), 1);
        assert_eq!(child_data.rows[0].route_id, Some(2));
        // 别名家族里其它会话的同名子请求不串入。
        let alias_scope = live.history_scope("h1", Some("c1"));
        let alias_data = query(&path, "h1", Some("c1"), DEFAULT_LIMIT, alias_scope, now.date_naive()).unwrap();
        assert_eq!(alias_data.rows.len(), 1);
        assert_eq!(alias_data.rows[0].route_id, Some(3));
        std::fs::remove_file(&path).ok();
    }

    /// 自然日窗口：第 8 天的记录不进结果。
    #[test]
    fn date_window_excludes_older_days() {
        let path = temp_log("dates");
        let old = line(1, &at(8, 12), json!({"status":200,"in":1,"out":1}));
        let recent = line(2, &at(6, 12), json!({"status":200,"in":2,"out":2}));
        std::fs::write(&path, format!("{old}\n{recent}\n")).unwrap();
        let scope = crate::live::LiveState::default().history_scope("s1", None);
        let data = query(&path, "s1", None, DEFAULT_LIMIT, scope, Local::now().date_naive()).unwrap();
        assert_eq!(data.rows.len(), 1);
        assert_eq!(data.rows[0].route_id, Some(2));
        assert_eq!(data.start_date, (Local::now().date_naive() - Duration::days(6)).to_string());
        std::fs::remove_file(&path).ok();
    }

    /// 实时合并：日志优先、时间容差防重启复用、无时间实时行不展示、
    /// 终态证据冲突不补链、完成前数值为空。
    #[test]
    fn live_merge_prefers_ledger_and_drops_matched() {
        let now = Local::now().timestamp_millis();
        let row_for = |id: &str, route: i64, model: &str, ts: i64| {
            completed_row(RawHistoryRecord {
                identity: id.into(),
                route_id: Some(route),
                session: "s1".into(),
                subagent: String::new(),
                timestamp_ms: ts,
                day: Local::now().date_naive(),
                model: model.into(),
                served: None,
                provider: String::new(),
                account: "a@x.com".into(),
                effort: "high".into(),
                status: Some(200),
                tries: 1,
                handed_on: false,
                input: Some(10),
                output: Some(20),
                cache_read: None,
                duration_ms: Some(2000.0),
                ttft_ms: Some(500.0),
            })
        };
        let ledger = row_for("id-7", 7, "m1", now - 1000);
        let conflicting = row_for("id-12", 12, "m2", now - 1000);
        // 同一请求的实时行：被日志行吸收，只补组链/规则/响应模型。
        let mut matched = live_row(7, Some(now - 500), "awaiting_usage");
        matched.model = "m1".into();
        // 终态证据冲突（模型不同）：不视为同一请求，不补链，两行并存。
        let mut disagree = live_row(12, Some(now - 500), "running");
        disagree.model = "m-live".into();
        // 真正运行中的新请求时间最新，排序后置顶，数值为空。
        let running = live_row(8, Some(now), "running");
        // 无开始时间：无法排序对齐，不展示。
        let no_time = live_row(9, None, "running");
        // 网关重启后 route_id 复用：时间不吻合不能当成同一请求，实时行独立保留。
        let stale = live_row(7, Some(now - MATCH_TOLERANCE_MS * 10), "running");
        let merged = merge_live(vec![ledger, conflicting], &[matched, disagree, running, no_time, stale]);
        // 排序口径现在在分组末尾：运行中的组在前，组内请求按开始时间正序。
        let rows: Vec<RouteHistoryRow> = group_rows(merged)
            .into_iter()
            .flat_map(|group| group.requests)
            .collect();
        // 身份吻合的实时行都被日志压住：只剩运行中的新请求、重启复用的 route 7 和两行日志。
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].route_id, Some(8), "运行中的请求排在完成行之前");
        assert_eq!(rows[0].outcome, "running");
        assert_eq!(rows[0].tokens, None);
        let ledger = rows.iter().find(|row| row.id == "id-7").unwrap();
        assert_eq!(ledger.outcome, "success");
        assert_eq!(ledger.route_groups, vec!["自动".to_string(), "干活".to_string()]);
        assert_eq!(ledger.rule_target.as_deref(), Some("group/worker"));
        assert_eq!(ledger.served.as_deref(), Some("served-live"));
        // 模型冲突的日志行仍压住实时行（不重复出现），但不借它的组链与响应模型。
        let conflicting = rows.iter().find(|row| row.id == "id-12").unwrap();
        assert!(conflicting.route_groups.is_empty());
        assert_eq!(conflicting.served, None);
        assert_eq!(rows.iter().filter(|row| row.id == "live-7").count(), 1);
        assert!(rows.iter().all(|row| row.id != "live-12"));
        // 已过等待窗口的 done trace 不成独立行，但仍补全已匹配的日志行。
        let mut old_done = live_row(7, Some(now - 1000), "awaiting_usage");
        old_done.model = "m1".into();
        old_done.eligible_overlay = false;
        let rows = merge_live(vec![row_for("id-7", 7, "m1", now - 1000)], &[old_done]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].route_groups, vec!["自动".to_string(), "干活".to_string()]);
    }

    fn row_at(id: &str, route: i64, ts: i64, outcome: &str, tokens: Option<u64>) -> RouteHistoryRow {
        let mut row = completed_row(RawHistoryRecord {
            identity: id.into(),
            route_id: Some(route),
            session: "s1".into(),
            subagent: String::new(),
            timestamp_ms: ts,
            day: Local::now().date_naive(),
            model: "m1".into(),
            served: None,
            provider: String::new(),
            account: "a@x.com".into(),
            effort: "high".into(),
            status: match outcome {
                "success" => Some(200),
                "failed" => Some(500),
                "cancelled" => Some(499),
                _ => None,
            },
            tries: 1,
            handed_on: false,
            input: tokens,
            output: tokens,
            cache_read: None,
            duration_ms: Some(2000.0),
            ttft_ms: Some(1000.0),
        });
        row.outcome = outcome.into();
        row.tokens = tokens;
        row
    }

    /// held 为 true 表示该请求是同一轮的后续请求（沿用轮初判定）。
    fn turn_of(route: i64, turn: i64, intent: &str, after: &str, time_ms: i64) -> (i64, TurnRecord) {
        held_turn_of(route, turn, false, intent, after, time_ms)
    }

    fn held_turn_of(route: i64, turn: i64, held: bool, intent: &str, after: &str, time_ms: i64) -> (i64, TurnRecord) {
        (route, TurnRecord {
            session: "s1".into(),
            subagent: String::new(),
            turn,
            held,
            intent: intent.into(),
            after: after.into(),
            sure: Some(0.9),
            fallback: String::new(),
            time_ms,
        })
    }

    /// 折叠口径：开口请求新起一组，held 续行挂回本轮的开口组；
    /// 意图取开口请求，Token 合计，组状态取最重者，条数限制按组计且不拆组。
    #[test]
    fn turns_fold_held_requests_into_groups() {
        // 时间正序：t1 开口、t1 续行、无记录的一次、t2 开口、t2 续行（失败）。
        let rows = vec![
            row_at("a", 1, 1_000, "success", Some(10)),
            row_at("b", 2, 2_000, "success", Some(20)),
            row_at("c", 3, 3_000, "success", Some(30)),
            row_at("d", 4, 4_000, "success", Some(40)),
            row_at("e", 5, 5_000, "failed", None),
        ];
        let turns = HashMap::from([
            turn_of(1, 1, "写功能", "", 1_000),
            held_turn_of(2, 1, true, "", "写功能", 2_000),
            turn_of(4, 2, "修 bug", "", 4_000),
            held_turn_of(5, 2, true, "", "", 5_000),
        ]);
        let mut annotated = rows;
        annotate_turns(&mut annotated, &turns);
        let groups = group_rows(annotated);
        // 组按开始时间倒序：t2 组、无记录单行、t1 组。
        assert_eq!(groups.len(), 3);
        let newest = &groups[0];
        assert_eq!(newest.turn, Some(2));
        assert_eq!(newest.count, 2);
        assert_eq!(newest.rule_intent.as_deref(), Some("修 bug"));
        assert_eq!(newest.intent_source, "measured");
        assert_eq!(newest.outcome, "failed", "组状态取最重的失败");
        assert_eq!(newest.tokens, None, "组内任一计数缺失则合计未知");
        let alone = &groups[1];
        assert_eq!(alone.turn, None);
        assert_eq!(alone.count, 1);
        assert_eq!(alone.requests[0].id, "c");
        let oldest = &groups[2];
        assert_eq!(oldest.turn, Some(1));
        assert_eq!(oldest.count, 2);
        assert_eq!(oldest.rule_intent.as_deref(), Some("写功能"));
        assert_eq!(oldest.tokens, Some(30), "输入与输出各 10、20，合计 30");
        assert!(oldest.tokens_per_second.is_some());
        // 组内请求保持开始时间正序。
        assert_eq!(oldest.requests[0].id, "a");
        assert_eq!(oldest.requests[1].id, "b");

        // route_id 复用但时间对不上的索引不生效，行保持无轮次。
        let stale = HashMap::from([turn_of(1, 9, "旧轮", "", 1_000 + MATCH_TOLERANCE_MS * 5)]);
        let mut reused = vec![row_at("a", 1, 1_000, "success", Some(10))];
        annotate_turns(&mut reused, &stale);
        assert_eq!(reused[0].turn, None);

        // 真实流量的交错：同一原生会话上两条会话线各自计数，turn 1 与 turn 2 的
        // 开口与续行交替出现；续行必须挂回各自轮次的开口组，不能被交错行打断。
        let interleaved = vec![
            row_at("oa", 6, 6_000, "success", Some(1)),
            row_at("ob", 7, 7_000, "success", Some(1)),
            row_at("oc", 8, 8_000, "success", Some(1)),
            row_at("od", 9, 9_000, "success", Some(1)),
            row_at("oe", 10, 10_000, "success", Some(1)),
        ];
        let turns = HashMap::from([
            turn_of(6, 1, "写功能", "", 6_000),
            turn_of(7, 2, "修 bug", "", 7_000),
            held_turn_of(8, 2, true, "", "", 8_000),
            held_turn_of(9, 1, true, "", "", 9_000),
            held_turn_of(10, 2, true, "", "", 10_000),
        ]);
        let mut annotated = interleaved;
        annotate_turns(&mut annotated, &turns);
        let groups = group_rows(annotated);
        assert_eq!(groups.len(), 2, "两个轮次各自一组，续行不新开组");
        let turn_one = groups.iter().find(|group| group.turn == Some(1)).unwrap();
        assert_eq!(turn_one.count, 2, "轮次1 的续行跨过轮次2 的请求仍归本轮");
        assert_eq!(turn_one.rule_intent.as_deref(), Some("写功能"), "意图取轮次1 的开口");
        let turn_two = groups.iter().find(|group| group.turn == Some(2)).unwrap();
        assert_eq!(turn_two.count, 3, "轮次2 的续行全部挂回轮次2 的开口");
        assert_eq!(turn_two.rule_intent.as_deref(), Some("修 bug"), "意图取轮次2 的开口");

        // 开口落在 trace 窗口外时，先到的续行自成一组，后到的同轮续行并入该组。
        let orphan = vec![
            row_at("xa", 11, 11_000, "success", Some(1)),
            row_at("xb", 12, 12_000, "success", Some(1)),
        ];
        let turns = HashMap::from([held_turn_of(11, 3, true, "", "", 11_000), held_turn_of(12, 3, true, "", "", 12_000)]);
        let mut annotated = orphan;
        annotate_turns(&mut annotated, &turns);
        let groups = group_rows(annotated);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2, "无开口的续行仍按轮次聚在一起，不逐条散开");

        // 续行先于开口到达（时间戳与 held 标记矛盾，真实流量不该出现）：
        // 续行先自成一组成组，后到的开口按 held=false 新开一组，不反向并进续行组。
        let out_of_order = vec![
            row_at("ya", 13, 13_000, "success", Some(1)),
            row_at("yb", 14, 14_000, "success", Some(1)),
        ];
        let turns = HashMap::from([held_turn_of(13, 4, true, "", "", 13_000), turn_of(14, 4, "", "", 14_000)]);
        let mut annotated = out_of_order;
        annotate_turns(&mut annotated, &turns);
        let groups = group_rows(annotated);
        assert_eq!(groups.len(), 2, "开口总是新开组，不并入先到的续行组");
        assert_eq!(groups.iter().map(|group| group.count).sum::<usize>(), 2);

        // 条数限制按组计，截断落在组边界。
        let rows = vec![
            row_at("a", 1, 1_000, "success", Some(1)),
            row_at("b", 2, 2_000, "success", Some(1)),
            row_at("c", 3, 3_000, "success", Some(1)),
        ];
        let turns = HashMap::from([
            turn_of(1, 1, "", "", 1_000),
            held_turn_of(2, 1, true, "", "", 2_000),
        ]);
        let mut annotated = rows;
        annotate_turns(&mut annotated, &turns);
        let groups = group_rows(annotated);
        assert_eq!(groups.len(), 2, "前两行同轮合并，第三行无记录单独成组");
        // 组按开始时间倒序，截断取最新一组：无记录的单行整组保留，合并组整体落在截断外。
        let limited: Vec<_> = groups.into_iter().take(1).collect();
        assert_eq!(limited[0].count, 1);
        assert_eq!(limited[0].turn, None, "截断落在组边界，不把合并组拆开带入");
    }
}
