//! 历史日期、月份热力与桌面命令共用的只读查询。扫描口径由账本开关决定。

use crate::{http, live, telemetry, usage};
use chrono::{Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DateStatsView {
    pub date: String,
    pub earliest_date: String,
    pub latest_date: String,
    pub daily: usage::DailyStats,
    pub agent_stats: Vec<usage::AgentStats>,
    pub cost_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MonthUsageView {
    pub month: String,
    pub earliest_date: String,
    pub latest_date: String,
    pub days: Vec<MonthDayUsage>,
    pub agent_days: Vec<MonthAgentUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MonthAgentUsage {
    pub agent: String,
    pub caller_key_id: String,
    pub caller_key_name: String,
    pub days: Vec<MonthDayUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MonthDayUsage {
    pub date: String,
    pub tokens: u64,
    pub requests: u64,
}

#[derive(Clone)]
struct CachedDayStats {
    host: String,
    file_created: Option<SystemTime>,
    file_modified: Option<SystemTime>,
    file_len: u64,
    read_at: Instant,
    view: DateStatsView,
}

/// 历史日统计缓存。只在费用读取成功时写入，不占用实时账本。
#[derive(Default)]
pub struct HistoryCache {
    days: HashMap<NaiveDate, CachedDayStats>,
}

impl HistoryCache {
    pub fn fresh(
        &self,
        day: NaiveDate,
        host: &str,
        created: Option<SystemTime>,
        modified: Option<SystemTime>,
        len: u64,
        today: NaiveDate,
    ) -> Option<DateStatsView> {
        let cached = self.days.get(&day)?;
        if cached.host == host
            && cached.file_created == created
            && cached.file_modified == modified
            && cached.file_len == len
            && cached.read_at.elapsed() < Duration::from_secs(60)
            && cached.view.cost_error.is_none()
        {
            let mut view = cached.view.clone();
            view.latest_date = today.to_string();
            Some(view)
        } else {
            None
        }
    }

    pub fn store(
        &mut self,
        day: NaiveDate,
        host: String,
        created: Option<SystemTime>,
        modified: Option<SystemTime>,
        len: u64,
        view: DateStatsView,
    ) {
        if view.cost_error.is_some() {
            return;
        }
        if self.days.len() >= 32 {
            self.days.clear();
        }
        self.days.insert(
            day,
            CachedDayStats {
                host,
                file_created: created,
                file_modified: modified,
                file_len: len,
                read_at: Instant::now(),
                view,
            },
        );
    }
}

pub struct FetchedDay {
    pub daily: usage::DailyStats,
    pub agent_stats: Vec<usage::AgentStats>,
    pub scanned_earliest: Option<NaiveDate>,
    pub cost_error: Option<String>,
}

pub fn view_from_snapshot(snapshot: &usage::RouteSnapshot, today: NaiveDate) -> DateStatsView {
    DateStatsView {
        date: snapshot.date.clone(),
        earliest_date: snapshot.earliest_date.clone(),
        latest_date: today.to_string(),
        daily: snapshot.daily.clone(),
        agent_stats: snapshot.agent_stats.clone(),
        cost_error: snapshot.cost_error.clone(),
    }
}

pub fn assemble_day(
    day: NaiveDate,
    today: NaiveDate,
    snapshot_earliest: &str,
    fetched: FetchedDay,
) -> DateStatsView {
    let mut current_earliest =
        NaiveDate::parse_from_str(snapshot_earliest, "%Y-%m-%d").unwrap_or(today);
    if let Some(earliest) = fetched.scanned_earliest {
        current_earliest = current_earliest.min(earliest);
    }
    DateStatsView {
        date: day.to_string(),
        earliest_date: current_earliest.to_string(),
        latest_date: today.to_string(),
        daily: fetched.daily,
        agent_stats: fetched.agent_stats,
        cost_error: fetched.cost_error,
    }
}

/// 严格校验 `YYYY-MM-DD`。非零填充和未来日期在扫描前拒绝。
pub fn parse_date(input: &str, today: NaiveDate) -> Result<NaiveDate, String> {
    let day = NaiveDate::parse_from_str(input.trim(), "%Y-%m-%d")
        .map_err(|_| "日期格式无效，应为 YYYY-MM-DD".to_string())?;
    if day.format("%Y-%m-%d").to_string() != input.trim() {
        return Err("日期格式无效，应为 YYYY-MM-DD".into());
    }
    if day > today {
        return Err("不能查看未来日期的统计".into());
    }
    Ok(day)
}

/// 流式扫描单日并读取回环网关费用。`classify_http` 只用于无界面服务。
pub fn fetch_day(
    path: &Path,
    host: &str,
    day: NaiveDate,
    include_unassociated: bool,
    classify_http: bool,
) -> Result<FetchedDay, String> {
    let (mut daily, mut agent_stats, scanned_earliest) =
        usage::scan_day_stats_with(path, day, include_unassociated)?;
    let period = format!("{day}..{day}");
    let cost_error = match live::gateway_url(host) {
        Ok(base) => {
            let agent = http::client(Duration::from_secs(5));
            let url = format!("{base}/v1/magpie/usage?period={period}");
            match http::read_json::<telemetry::Usage>(&agent, &url, classify_http) {
                Ok(usage) => telemetry::apply_history_usage(
                    &mut daily,
                    &mut agent_stats,
                    day,
                    &period,
                    &usage,
                )
                .err(),
                Err(error) => Some(error),
            }
        }
        Err(error) => Some(error),
    };
    Ok(FetchedDay {
        daily,
        agent_stats,
        scanned_earliest,
        cost_error,
    })
}

pub fn month_usage(
    path: &Path,
    month: &str,
    today: NaiveDate,
    include_unassociated: bool,
) -> Result<MonthUsageView, String> {
    let (month, start, end) = parse_month(month.trim(), today)?;
    let (days, agent_days, scanned_earliest) =
        usage::scan_month_scoped_usage_with(path, start, end, include_unassociated)?;
    let today = Local::now().date_naive();
    let earliest = scanned_earliest
        .map(|date| date.min(today))
        .unwrap_or(today);
    Ok(MonthUsageView {
        month,
        earliest_date: earliest.to_string(),
        latest_date: today.to_string(),
        agent_days: agent_days
            .into_iter()
            .map(
                |(agent, caller_key_id, caller_key_name, days)| MonthAgentUsage {
                    agent,
                    caller_key_id,
                    caller_key_name,
                    days: days
                        .into_iter()
                        .map(|day| MonthDayUsage {
                            date: day.date.to_string(),
                            tokens: day.tokens,
                            requests: day.requests,
                        })
                        .collect(),
                },
            )
            .collect(),
        days: days
            .into_iter()
            .map(|day| MonthDayUsage {
                date: day.date.to_string(),
                tokens: day.tokens,
                requests: day.requests,
            })
            .collect(),
    })
}

/// 严格校验 `YYYY-MM` 并给出当月闭区间；格式多余零填充和未来月份都在扫描前拒绝。
pub fn parse_month(
    input: &str,
    today: NaiveDate,
) -> Result<(String, NaiveDate, NaiveDate), String> {
    if input.len() != 7 || input.as_bytes().get(4) != Some(&b'-') {
        return Err("月份格式无效，应为 YYYY-MM".into());
    }
    let start = NaiveDate::parse_from_str(&format!("{input}-01"), "%Y-%m-%d")
        .map_err(|_| "月份格式无效，应为 YYYY-MM".to_string())?;
    if start.format("%Y-%m").to_string() != input {
        return Err("月份格式无效，应为 YYYY-MM".into());
    }
    let next = if start.month() == 12 {
        NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(start.year(), start.month() + 1, 1)
    }
    .ok_or("月份超出支持范围")?;
    let end = next.pred_opt().ok_or("月份超出支持范围")?;
    let this_month =
        NaiveDate::from_ymd_opt(today.year(), today.month(), 1).ok_or("无法确定当前月份")?;
    if start > this_month {
        return Err("不能查看未来月份的热力日历".into());
    }
    Ok((input.to_string(), start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_and_future_months_before_scanning() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let (label, start, end) = parse_month("2026-02", today).unwrap();
        assert_eq!(label, "2026-02");
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
        let (_, _, leap_end) = parse_month("2024-02", today).unwrap();
        assert_eq!(leap_end, NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
        assert!(parse_month("2028-02", today)
            .unwrap_err()
            .contains("未来月份"));
        assert!(parse_month("2026-11", today)
            .unwrap_err()
            .contains("未来月份"));
        for invalid in [
            "2026-13", "2026-00", "2026-1", "2026-1-", "2026/01", "2026-01x", "12026-01",
            "2026-1a", "",
        ] {
            assert!(parse_month(invalid, today).is_err(), "{invalid} 不应通过");
        }
        let (_, _, december_end) = parse_month("2025-12", today).unwrap();
        assert_eq!(december_end, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());
    }
}
