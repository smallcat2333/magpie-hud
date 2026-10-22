//! 全局小时费用使用网关原生序列；会话没有序列，只记录之后的真实累计增量。

use crate::telemetry::Usage;
use crate::usage::{DailyStats, RouteSnapshot};
use chrono::{DateTime, Local, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
struct Bucket {
    cost: Option<f64>,
    partial: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Counter {
    hours: [Bucket; 24],
    #[serde(skip)]
    previous: Option<(f64, u64, i64)>,
}

impl Counter {
    fn observe(&mut self, cost: f64, calls: u64, unpriced: u64, now: &DateTime<Local>) {
        if !cost.is_finite() || cost < 0.0 || unpriced > calls || calls > 0 && calls == unpriced {
            self.previous = None;
            return;
        }
        let hour = now.hour() as usize;
        let stamp = now.timestamp_millis();
        match self.previous {
            Some((old_cost, old_calls, old_stamp)) if calls >= old_calls && cost >= old_cost && stamp > old_stamp && stamp - old_stamp <= 30_000 => {
                let old_time = DateTime::from_timestamp_millis(old_stamp).unwrap().with_timezone(&Local);
                let crossed = old_time.hour() != now.hour();
                if crossed {
                    // 轮询跨整点的最后几秒无法精确拆分，以读到增量的小时记录并明确部分观测。
                    self.hours[old_time.hour() as usize].partial = true;
                }
                let bucket = &mut self.hours[hour];
                bucket.partial |= crossed || unpriced > 0 || bucket.cost.is_none() && now.minute() > 0;
                bucket.cost = Some(bucket.cost.unwrap_or(0.0) + (cost - old_cost));
            }
            _ => {
                // 首次/重启/断线/累计回退只建立基线，绝不把离线期间金额记到当前小时。
                if calls == 0 && cost == 0.0 && self.hours.iter().all(|item| item.cost.is_none_or(|value| value == 0.0)) {
                    for item in &mut self.hours[..=hour] {
                        item.cost = Some(0.0);
                        item.partial = false;
                    }
                } else {
                    let bucket = &mut self.hours[hour];
                    bucket.cost.get_or_insert(0.0);
                    bucket.partial = true;
                }
            }
        }
        self.previous = Some((cost, calls, stamp));
    }

    fn apply(&self, stats: &mut DailyStats) {
        for (target, bucket) in stats.hourly.iter_mut().zip(self.hours) {
            target.cost = bucket.cost;
            target.cost_partial = bucket.partial;
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct History {
    host: String,
    date: String,
    sessions: HashMap<String, Counter>,
}

#[derive(Default)]
pub struct HourlyCosts {
    history: History,
    path: Option<PathBuf>,
    pub error: Option<String>,
}

impl HourlyCosts {
    pub fn load(path: PathBuf, host: &str) -> Self {
        let mut error = None;
        let mut history = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() <= 2 * 1024 * 1024 => {
                std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<History>(&bytes).ok()).unwrap_or_else(|| {
                    error = Some("小时金额缓存无效，重新开始观测".into());
                    History::default()
                })
            }
            Ok(_) => { error = Some("小时金额缓存过大，重新开始观测".into()); History::default() }
            Err(_) => History::default(),
        };
        if history.host != host { history = History { host: host.into(), ..Default::default() }; }
        Self { history, path: Some(path), error }
    }

    /// 缓存仅包含当日会话小时金额和会话 ID，不保存账号、请求或正文。
    pub fn observe(&mut self, usage: &Usage, now: DateTime<Local>, host: &str) {
        if self.path.is_none() {
            let path = crate::config::config_path().with_file_name("hourly-cost.json");
            let mut loaded = Self::load(path, host);
            loaded.error = loaded.error.take().or(self.error.take());
            *self = loaded;
        }
        let Some(day) = DateTime::parse_from_rfc3339(&usage.since).ok().map(|time| time.with_timezone(&Local).date_naive()) else { return };
        if day != now.date_naive() { return; }
        let date = day.to_string();
        if self.history.date != date {
            self.history = History { date, host: self.history.host.clone(), ..Default::default() };
        }
        let before = serde_json::to_vec(&self.history).ok();
        let mut unique = HashSet::new();
        let duplicates: HashSet<_> = usage.sessions.iter().filter_map(|session| (!unique.insert(session.id.as_str())).then_some(session.id.as_str())).collect();
        let present: HashSet<_> = usage.sessions.iter().filter(|session| !session.id.is_empty() && !duplicates.contains(session.id.as_str())).map(|session| session.id.as_str()).collect();
        for (id, counter) in &mut self.history.sessions {
            if !present.contains(id.as_str()) { counter.previous = None; }
        }
        for session in &usage.sessions {
            if session.id.is_empty() || duplicates.contains(session.id.as_str()) { continue; }
            self.history.sessions.entry(session.id.clone()).or_default().observe(session.cost, session.calls, session.unpriced, &now);
        }
        let after = serde_json::to_vec(&self.history).ok();
        if before != after {
            if let Some((path, bytes)) = self.path.as_ref().zip(after) {
                self.error = persist(path, &bytes).err().map(|error| format!("小时金额缓存保存失败：{error}"));
            }
        }
    }

    pub fn apply(&self, view: &mut RouteSnapshot, usage: &Usage) {
        if view.date != self.history.date { return; }
        apply_gateway_series(&mut view.daily, usage);
        for session in &mut view.sessions {
            if let Some(counter) = self.history.sessions.get(&session.id) { counter.apply(&mut session.daily); }
        }
    }
}

fn apply_gateway_series(stats: &mut DailyStats, usage: &Usage) {
    let mut assigned = [false; 24];
    for item in &usage.hourly {
        let Ok(time) = DateTime::parse_from_rfc3339(&item.time) else { continue };
        let local = time.with_timezone(&Local);
        // 网关的 time 已带本地偏移；用其自身日期与“今天”比较，避免时区二次换算。
        if time.date_naive() != Local::now().date_naive() || !item.cost.is_finite() || item.cost < 0.0 { continue; }
        let hour = local.hour() as usize;
        if assigned[hour] { continue; }
        let bucket = &mut stats.hourly[hour];
        bucket.cost = Some(item.cost);
        bucket.cost_partial = item.unpriced > 0;
        assigned[hour] = true;
    }
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[test]
    fn observes_only_new_cost_and_keeps_unknown_history() {
        let now = Local.with_ymd_and_hms(2026, 10, 21, 10, 20, 0).unwrap();
        let mut counter = Counter::default();
        counter.observe(100.0, 10, 0, &now);
        assert_eq!(counter.hours[9].cost, None);
        assert_eq!(counter.hours[10].cost, Some(0.0));
        assert!(counter.hours[10].partial);
        counter.observe(101.5, 11, 0, &(now + Duration::seconds(5)));
        assert_eq!(counter.hours[10].cost, Some(1.5));
        counter.observe(110.0, 12, 0, &(now + Duration::hours(1)));
        assert_eq!(counter.hours[11].cost, Some(0.0));
        counter.observe(111.0, 13, 0, &(now + Duration::hours(1) + Duration::seconds(5)));
        assert_eq!(counter.hours[11].cost, Some(1.0));
        counter.observe(2.0, 1, 0, &(now + Duration::hours(1) + Duration::seconds(10)));
        assert_eq!(counter.hours[11].cost, Some(1.0));
    }

    #[test]
    fn exact_session_costs_and_duplicate_ids_do_not_leak() {
        let path = std::env::temp_dir().join(format!("hud-hourly-sessions-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let now = Local.with_ymd_and_hms(2026, 10, 21, 10, 20, 0).unwrap();
        let mut history = HourlyCosts::load(path.clone(), "h");
        let mut usage: Usage = serde_json::from_str(&format!(r#"{{"since":"{}","calls":10,"cost":10,"sessions":[{{"id":"a","calls":4,"cost":4}},{{"id":"b","calls":6,"cost":6}}]}}"#, now.to_rfc3339())).unwrap();
        history.observe(&usage, now, "h");
        usage.calls += 1;
        usage.cost += 2.0;
        usage.sessions[0].calls += 1;
        usage.sessions[0].cost += 2.0;
        history.observe(&usage, now + Duration::seconds(5), "h");
        assert_eq!(history.history.sessions["a"].hours[10].cost, Some(2.0));
        assert_eq!(history.history.sessions["b"].hours[10].cost, Some(0.0));
        usage.sessions.push(crate::telemetry::SessionCost { id: "a".into(), calls: 90, cost: 999.0, unpriced: 0 });
        history.observe(&usage, now + Duration::seconds(10), "h");
        assert_eq!(history.history.sessions["a"].hours[10].cost, Some(2.0));
        assert!(history.history.sessions["a"].previous.is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn persisted_history_rebaselines_and_rolls_over_without_double_charge() {
        let path = std::env::temp_dir().join(format!("hud-hourly-cost-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let now = Local.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap();
        let mut history = HourlyCosts::load(path.clone(), "loopback");
        let mut usage = Usage { since: now.to_rfc3339(), ..Default::default() };
        usage.sessions.push(crate::telemetry::SessionCost { id: "global-check".into(), calls: 0, cost: 0.0, unpriced: 0 });
        history.observe(&usage, now, "loopback");
        usage.calls = 1;
        usage.cost = 2.0;
        usage.sessions[0].calls = 1;
        usage.sessions[0].cost = 2.0;
        history.observe(&usage, now + Duration::seconds(5), "loopback");
        assert_eq!(history.history.sessions["global-check"].hours[10].cost, Some(2.0));
        let mut restored = HourlyCosts::load(path.clone(), "loopback");
        let session = usage.sessions[0].clone();
        restored.observe(&Usage { calls: 2, cost: 3.0, sessions: vec![crate::telemetry::SessionCost { calls: 2, cost: 3.0, ..session.clone() }], ..usage.clone() }, now + Duration::seconds(10), "loopback");
        assert_eq!(restored.history.sessions["global-check"].hours[10].cost, Some(2.0));
        restored.observe(&Usage { calls: 3, cost: 4.0, sessions: vec![crate::telemetry::SessionCost { calls: 3, cost: 4.0, ..session }], ..usage }, now + Duration::seconds(15), "loopback");
        assert_eq!(restored.history.sessions["global-check"].hours[10].cost, Some(3.0));
        let tomorrow = now + Duration::days(1);
        restored.observe(&Usage { since: tomorrow.to_rfc3339(), ..Default::default() }, tomorrow, "loopback");
        assert!(restored.history.sessions.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn gateway_series_fills_global_hours_without_splitting_sessions() {
        let today = Local::now().date_naive();
        let now = today.and_hms_opt(10, 0, 0).unwrap().and_local_timezone(Local).unwrap();
        let usage = Usage { since: now.to_rfc3339(), calls: 2, cost: 3.5, unpriced: 0, hourly: vec![
            crate::telemetry::HourlyCost { time: now.to_rfc3339(), cost: 1.25, unpriced: 0 },
            crate::telemetry::HourlyCost { time: (now - Duration::hours(2)).to_rfc3339(), cost: 2.25, unpriced: 1 },
        ], sessions: vec![] };
        let mut stats = DailyStats::default();
        apply_gateway_series(&mut stats, &usage);
        assert_eq!(stats.hourly[10].cost, Some(1.25));
        assert!(!stats.hourly[10].cost_partial);
        assert_eq!(stats.hourly[8].cost, Some(2.25));
        assert!(stats.hourly[8].cost_partial);
        assert!(stats.hourly[9].cost.is_none());
    }
}
