//! 无界面采集服务。HTTP 在锁外读取，快照刷新在锁内；不提供界面，也不回调事件。

use crate::{details, history, http, live, query, tail, telemetry, usage};
use chrono::Local;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub usage_path: PathBuf,
    pub magpie_host: String,
    pub cache_dir: PathBuf,
    pub include_unassociated: bool,
}

#[derive(Clone, Debug)]
struct Probe {
    available: bool,
    last_success_at: Option<String>,
    error: Option<String>,
}

impl Default for Probe {
    fn default() -> Self {
        Self {
            available: false,
            last_success_at: None,
            error: None,
        }
    }
}

impl Probe {
    fn succeed(&mut self) {
        self.available = true;
        self.error = None;
        self.last_success_at = Some(chrono::Utc::now().to_rfc3339());
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.available = false;
        self.error = Some(error.into());
    }
}

struct Health {
    trace: Probe,
    concurrency: Probe,
    quotas: Probe,
    usage: Probe,
    logs: Probe,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            trace: Probe::default(),
            concurrency: Probe::default(),
            quotas: Probe::default(),
            usage: Probe::default(),
            logs: Probe::default(),
        }
    }
}

struct Core {
    usage_path: PathBuf,
    magpie_host: String,
    include_unassociated: bool,
    ledger: usage::UsageLedger,
    cursor: tail::LogCursor,
    live: live::LiveState,
    telemetry: telemetry::Telemetry,
    snapshot: usage::RouteSnapshot,
    history: query::HistoryCache,
    health: Health,
    quota_refresh: Option<SyncSender<()>>,
}

impl Core {
    fn new(config: &ServiceConfig) -> Self {
        let mut ledger = usage::UsageLedger::new(Local::now().date_naive());
        ledger.set_include_unassociated(config.include_unassociated);
        let mut telemetry = telemetry::Telemetry::default();
        if config.cache_dir.as_os_str().is_empty() {
            telemetry.disable_persistence();
        } else {
            telemetry.bind_cache(config.cache_dir.join("hourly-cost.json"));
        }
        let snapshot = ledger.snapshot(&config.magpie_host, "waiting", None);
        Self {
            usage_path: config.usage_path.clone(),
            magpie_host: config.magpie_host.clone(),
            include_unassociated: config.include_unassociated,
            ledger,
            cursor: tail::LogCursor::default(),
            live: live::LiveState::default(),
            telemetry,
            snapshot,
            history: query::HistoryCache::default(),
            health: Health::default(),
            quota_refresh: None,
        }
    }

    /// 文件读取失败只记短错误，不把路径放进快照。
    fn refresh(&mut self, watcher_error: Option<String>) {
        let read_error = self.cursor.update(&self.usage_path, &mut self.ledger).err();
        if watcher_error.is_some() {
            self.health.logs.fail("文件监视不可用");
        } else if read_error.is_some() {
            self.health.logs.fail("无法读取用量日志");
        } else if self.cursor.missing {
            self.health.logs.fail("用量日志不存在");
        } else {
            self.health.logs.succeed();
        }
        let snapshot_error = if read_error.is_some() {
            Some("无法读取用量日志".to_string())
        } else if watcher_error.is_some() {
            Some("文件监视不可用".to_string())
        } else {
            None
        };
        let mut snapshot = self.ledger.snapshot(
            &self.magpie_host,
            if self.cursor.missing {
                "missing"
            } else {
                "ready"
            },
            snapshot_error,
        );
        if snapshot.date == self.snapshot.date
            && snapshot.daily.requests > self.snapshot.daily.requests
        {
            if let Some(sender) = &self.quota_refresh {
                let _ = sender.try_send(());
            }
        }
        self.live.apply(&mut snapshot);
        self.telemetry.apply(&mut snapshot);
        snapshot.revision = self.snapshot.revision;
        if snapshot != self.snapshot {
            snapshot.revision = self.snapshot.revision + 1;
            self.snapshot = snapshot;
        }
    }
}

pub struct Service {
    inner: Mutex<Core>,
    stop: Arc<AtomicBool>,
    joins: Mutex<Vec<JoinHandle<()>>>,
}

impl Service {
    pub fn start(config: ServiceConfig) -> Arc<Self> {
        let mut core = Core::new(&config);
        core.refresh(None);
        if live::gateway_url(&config.magpie_host).is_err() {
            let message = "实时路由地址必须是本机回环 HTTP 地址和端口";
            core.health.trace.fail(message);
            core.health.concurrency.fail(message);
            core.health.quotas.fail(message);
            core.health.usage.fail(message);
        }
        let service = Arc::new(Self {
            inner: Mutex::new(core),
            stop: Arc::new(AtomicBool::new(false)),
            joins: Mutex::new(Vec::new()),
        });
        if let Ok(base) = live::gateway_url(&config.magpie_host) {
            service.spawn_collectors(base);
        }
        service.spawn_logs();
        service
    }

    pub fn snapshot(&self) -> usage::RouteSnapshot {
        self.lock().snapshot.clone()
    }

    pub fn health(&self) -> Value {
        let core = self.lock();
        json!({
            "trace": probe_value(&core.health.trace),
            "concurrency": probe_value(&core.health.concurrency),
            "quotas": probe_value(&core.health.quotas),
            "usage": probe_value(&core.health.usage),
            "logs": probe_value(&core.health.logs),
        })
    }

    pub fn query(&self, command: &str, args: Value) -> Result<Value, String> {
        let value = match command {
            "query_date_stats" => {
                let args: DateArgs = parse_args(args)?;
                serde_json::to_value(self.date_stats(&args.date)?)
            }
            "query_month_usage" => {
                let args: MonthArgs = parse_args(args)?;
                let (path, include) = {
                    let core = self.lock();
                    (core.usage_path.clone(), core.include_unassociated)
                };
                serde_json::to_value(query::month_usage(
                    &path,
                    &args.month,
                    Local::now().date_naive(),
                    include,
                )?)
            }
            "query_route_history" => {
                let args: HistoryArgs = parse_args(args)?;
                serde_json::to_value(self.route_history(args)?)
            }
            "query_usage_details" => {
                let args: DetailArgs = parse_args(args)?;
                let (path, host, include) = {
                    let core = self.lock();
                    (
                        core.usage_path.clone(),
                        core.magpie_host.clone(),
                        core.include_unassociated,
                    )
                };
                serde_json::to_value(details::query_mode(
                    &path,
                    &host,
                    &args.start,
                    &args.end,
                    &args.filters.unwrap_or_default(),
                    include,
                    true,
                )?)
            }
            "query_model_costs" => {
                let args: ModelCostArgs = parse_args(args)?;
                serde_json::to_value(self.model_costs(args)?)
            }
            _ => return Err("未知查询".into()),
        };
        value.map_err(|_| "查询结果无法编码".into())
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let joins =
            std::mem::take(&mut *self.joins.lock().unwrap_or_else(|error| error.into_inner()));
        for join in joins {
            let _ = join.join();
        }
    }

    fn date_stats(&self, date: &str) -> Result<query::DateStatsView, String> {
        let today = Local::now().date_naive();
        let day = query::parse_date(date, today)?;
        let (path, host, include, created, modified, len) = {
            let core = self.lock();
            if day == today && core.snapshot.date == today.to_string() {
                return Ok(query::view_from_snapshot(&core.snapshot, today));
            }
            let metadata = std::fs::metadata(&core.usage_path).ok();
            let created = metadata.as_ref().and_then(|meta| meta.created().ok());
            let modified = metadata.as_ref().and_then(|meta| meta.modified().ok());
            let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);
            if day < today {
                if let Some(view) =
                    core.history
                        .fresh(day, &core.magpie_host, created, modified, len, today)
                {
                    return Ok(view);
                }
            }
            (
                core.usage_path.clone(),
                core.magpie_host.clone(),
                core.include_unassociated,
                created,
                modified,
                len,
            )
        };
        let fetched = query::fetch_day(&path, &host, day, include, true)?;
        let mut core = self.lock();
        let view = query::assemble_day(day, today, &core.snapshot.earliest_date, fetched);
        if day < today {
            core.history
                .store(day, host, created, modified, len, view.clone());
        }
        Ok(view)
    }

    fn route_history(&self, args: HistoryArgs) -> Result<history::RouteHistoryData, String> {
        let subagent = args.subagent.filter(|id| !id.trim().is_empty());
        if args.session.trim().is_empty() {
            return Err("缺少会话身份".into());
        }
        let limit = args
            .limit
            .unwrap_or(history::DEFAULT_LIMIT)
            .clamp(1, history::MAX_LIMIT);
        let (path, scope, today) = {
            let core = self.lock();
            (
                core.usage_path.clone(),
                core.live.history_scope(&args.session, subagent.as_deref()),
                Local::now().date_naive(),
            )
        };
        history::query(
            &path,
            &args.session,
            subagent.as_deref(),
            limit,
            scope,
            today,
        )
    }

    fn model_costs(&self, args: ModelCostArgs) -> Result<details::ModelCostData, String> {
        let (host, date, ledger_revision, rows) = {
            let core = self.lock();
            let selected = args
                .session
                .as_deref()
                .filter(|session| !session.is_empty());
            let canonical = selected.map(|session| {
                core.live
                    .aliases
                    .get(session)
                    .map(String::as_str)
                    .unwrap_or(session)
            });
            let source = args
                .source_key
                .as_deref()
                .filter(|source| !source.is_empty());
            (
                core.magpie_host.clone(),
                core.snapshot.date.clone(),
                core.ledger.revision,
                core.ledger
                    .model_cost_rows(canonical, source, &core.live.aliases),
            )
        };
        Ok(details::model_costs_mode(
            rows,
            &host,
            &date,
            ledger_revision,
            true,
        ))
    }

    fn spawn_collectors(self: &Arc<Self>, base: String) {
        let (quota_sender, quota_receiver) = mpsc::sync_channel(1);
        self.lock().quota_refresh = Some(quota_sender);
        self.spawn(self.trace_loop(base.clone()));
        self.spawn(self.quota_loop(base.clone(), quota_receiver));
        self.spawn(self.usage_loop(base.clone()));
        self.spawn(self.concurrency_loop(base));
    }

    fn spawn_logs(self: &Arc<Self>) {
        let path = self.lock().usage_path.clone();
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        self.spawn(move || {
            let (sender, receiver) = mpsc::channel();
            let mut watch_error = None;
            let _watcher = match RecommendedWatcher::new(sender, notify::Config::default()) {
                Ok(mut watcher) => {
                    let parent = path.parent().unwrap_or(std::path::Path::new("."));
                    let ancestor = parent.ancestors().find(|candidate| candidate.is_dir());
                    if let Some(ancestor) = ancestor {
                        if let Err(_error) = watcher.watch(
                            ancestor,
                            if ancestor == parent {
                                RecursiveMode::NonRecursive
                            } else {
                                RecursiveMode::Recursive
                            },
                        ) {
                            watch_error = Some("文件监视不可用".to_string());
                        }
                    }
                    Some(watcher)
                }
                Err(_error) => {
                    watch_error = Some("文件监视不可用".to_string());
                    None
                }
            };
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(event)) => {
                        if !event
                            .paths
                            .iter()
                            .any(|changed| changed == &path || path.starts_with(changed))
                        {
                            continue;
                        }
                        let deadline = Instant::now() + Duration::from_millis(150);
                        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            if receiver
                                .recv_timeout(left.min(Duration::from_millis(100)))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Ok(Err(_error)) => watch_error = Some("文件监视不可用".to_string()),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if !sleep_interruptible(&stop, Duration::from_secs(1)) {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                shared.lock().refresh(watch_error.clone());
            }
            drop(_watcher);
        });
    }

    fn trace_loop(self: &Arc<Self>, base: String) -> impl FnOnce() + Send + 'static {
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        move || {
            let agent = http::client(Duration::from_secs(30));
            while !stop.load(Ordering::Acquire) {
                let seq = shared.lock().live.seq;
                let url = format!(
                    "{base}/v1/magpie/trace?after={seq}{}",
                    if seq > 0 { "&wait=1" } else { "" }
                );
                match http::read_json::<live::TraceUpdate>(&agent, &url, true) {
                    Ok(update) => {
                        let mut core = shared.lock();
                        core.live.update(update);
                        core.health.trace.succeed();
                        core.refresh(None);
                    }
                    Err(error) => {
                        shared.lock().health.trace.fail(error);
                        if !sleep_interruptible(&stop, Duration::from_secs(2)) {
                            break;
                        }
                    }
                }
            }
        }
    }

    fn quota_loop(
        self: &Arc<Self>,
        base: String,
        quota_receiver: Receiver<()>,
    ) -> impl FnOnce() + Send + 'static {
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        move || {
            let agent = http::client(Duration::from_secs(5));
            while !stop.load(Ordering::Acquire) {
                let started = Instant::now();
                let quotas = http::read_json::<telemetry::Quotas>(
                    &agent,
                    &format!("{base}/v1/magpie/quotas?kept=1"),
                    true,
                );
                {
                    let mut core = shared.lock();
                    match quotas {
                        Ok(quotas) => {
                            core.telemetry.update_quotas(quotas.data);
                            core.health.quotas.succeed();
                        }
                        Err(error) => {
                            core.telemetry.quota_error = Some(error.clone());
                            core.health.quotas.fail(error);
                        }
                    }
                    core.refresh(None);
                }
                let wait = Duration::from_secs(60).saturating_sub(started.elapsed());
                match recv_interruptible(&quota_receiver, &stop, wait) {
                    Recv::Signal => {
                        if !sleep_interruptible(&stop, Duration::from_secs(2)) {
                            break;
                        }
                        while quota_receiver.try_recv().is_ok() {}
                    }
                    Recv::Timeout => {}
                    Recv::Stop => break,
                }
            }
        }
    }

    fn usage_loop(self: &Arc<Self>, base: String) -> impl FnOnce() + Send + 'static {
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        let host = self.lock().magpie_host.clone();
        move || {
            let agent = http::client(Duration::from_secs(5));
            while !stop.load(Ordering::Acquire) {
                let started = Instant::now();
                let usage = http::read_json::<telemetry::Usage>(
                    &agent,
                    &format!("{base}/v1/magpie/usage?period=today"),
                    true,
                );
                {
                    let mut core = shared.lock();
                    match usage {
                        Ok(usage) => {
                            core.telemetry.update_usage(usage, &host);
                            core.health.usage.succeed();
                        }
                        Err(error) => {
                            core.telemetry.cost_error = Some(error.clone());
                            core.health.usage.fail(error);
                        }
                    }
                    core.refresh(None);
                }
                if !sleep_interruptible(
                    &stop,
                    Duration::from_secs(5).saturating_sub(started.elapsed()),
                ) {
                    break;
                }
            }
        }
    }

    fn concurrency_loop(self: &Arc<Self>, base: String) -> impl FnOnce() + Send + 'static {
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        move || {
            let agent = http::client(Duration::from_secs(2));
            let mut titles = live::Titles::default();
            let mut custom_titles = live::CustomTitles::default();
            let mut child_titles = live::ChildTitles::default();
            let mut names = std::collections::HashMap::new();
            let mut aliases = std::collections::HashMap::new();
            let mut child_labels = std::collections::HashMap::new();
            let mut last_titles = Instant::now() - Duration::from_secs(3);
            let mut last_children = Instant::now() - Duration::from_secs(8);
            while !stop.load(Ordering::Acquire) {
                if last_titles.elapsed() >= Duration::from_secs(2) {
                    names = custom_titles.scan();
                    names.extend(titles.scan(&live::desktop_roots()));
                    aliases = titles.aliases();
                    last_titles = Instant::now();
                }
                if last_children.elapsed() >= Duration::from_secs(8) {
                    let wanted = live::wanted_children(&shared.lock().snapshot);
                    child_labels = child_titles.scan(&wanted);
                    last_children = Instant::now();
                }
                let result = http::read_json::<live::Activity>(
                    &agent,
                    &format!("{base}/v1/magpie/concurrency"),
                    true,
                );
                {
                    let mut core = shared.lock();
                    core.live.names = names.clone();
                    core.live.aliases = aliases.clone();
                    core.live.child_labels = child_labels.clone();
                    match result {
                        Ok(activity) => {
                            core.live.activity = activity;
                            core.live.error = None;
                            core.health.concurrency.succeed();
                        }
                        Err(error) => {
                            core.live.activity = live::Activity::default();
                            core.live.error = Some(error.clone());
                            core.health.concurrency.fail(error);
                        }
                    }
                    core.refresh(None);
                }
                if !sleep_interruptible(&stop, Duration::from_millis(500)) {
                    break;
                }
            }
        }
    }

    fn spawn<F>(&self, work: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.joins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(std::thread::spawn(work));
    }

    fn clone_handle(self: &Arc<Self>) -> Arc<Self> {
        Arc::clone(self)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Core> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

fn probe_value(probe: &Probe) -> Value {
    json!({
        "available": probe.available,
        "lastSuccessAt": probe.last_success_at,
        "error": probe.error,
    })
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|_| "查询参数无效".to_string())
}

enum Recv {
    Signal,
    Timeout,
    Stop,
}

fn recv_interruptible(receiver: &Receiver<()>, stop: &AtomicBool, wait: Duration) -> Recv {
    let deadline = Instant::now() + wait;
    loop {
        if stop.load(Ordering::Acquire) {
            return Recv::Stop;
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Recv::Timeout;
        };
        match receiver.recv_timeout(left.min(Duration::from_millis(200))) {
            Ok(()) => return Recv::Signal,
            Err(mpsc::RecvTimeoutError::Timeout) if left > Duration::from_millis(200) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Recv::Timeout,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Recv::Stop,
        }
    }
}

fn sleep_interruptible(stop: &AtomicBool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
    !stop.load(Ordering::Acquire)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DateArgs {
    date: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MonthArgs {
    month: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HistoryArgs {
    session: String,
    #[serde(default)]
    subagent: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DetailArgs {
    start: String,
    end: String,
    #[serde(default)]
    filters: Option<details::DetailFilters>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCostArgs {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    source_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use serde_json::json;

    fn sample_line(session: &str, stamp: &str, route: i64) -> String {
        json!({
            "route_id": route,
            "t": stamp,
            "session": session,
            "providerAccount": "one@example.com",
            "agent": "claude",
            "model": "requested",
            "served": "actual",
            "effort": "high",
            "status": 200,
            "in": 10,
            "out": 4,
            "cache_read": 0,
            "ms": 2000,
            "ttft_ms": 1000
        })
        .to_string()
    }

    fn config(path: PathBuf, include_unassociated: bool) -> ServiceConfig {
        ServiceConfig {
            usage_path: path,
            magpie_host: "example.com:9".into(),
            cache_dir: PathBuf::new(),
            include_unassociated,
        }
    }

    #[test]
    fn unassociated_calls_share_scan_totals_without_a_fake_session() {
        let directory =
            std::env::temp_dir().join(format!("hud-core-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("usage.jsonl");
        let stamp = Local::now().to_rfc3339();
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                sample_line("alpha", &stamp, 1),
                sample_line("", &stamp, 2)
            ),
        )
        .unwrap();
        let included = Service::start(config(path.clone(), true));
        let snapshot = included.snapshot();
        assert_eq!(snapshot.daily.requests, 2);
        assert!(snapshot
            .sessions
            .iter()
            .all(|session| !session.id.is_empty()));
        assert_eq!(snapshot.sessions.len(), 1);
        let today = Local::now().date_naive().to_string();
        let stats = included
            .query("query_date_stats", json!({"date": today}))
            .unwrap();
        assert_eq!(stats["daily"]["requests"], 2);
        let card = &snapshot.sessions[0].routes[0];
        assert_eq!(card.model, "actual");
        assert_eq!(card.served.as_deref(), Some("actual"));
        assert!(card.route_groups.is_empty());
        included.stop();

        let excluded = Service::start(config(path, false));
        assert_eq!(excluded.snapshot().daily.requests, 1);
        let health = excluded.health();
        for name in ["trace", "concurrency", "quotas", "usage", "logs"] {
            let probe = &health[name];
            assert!(probe.get("available").is_some(), "{name}");
            assert!(probe.get("lastSuccessAt").is_some(), "{name}");
            assert!(probe.get("error").is_some(), "{name}");
        }
        assert_eq!(health["logs"]["available"], true);
        assert_eq!(health["trace"]["available"], false);
        let error = health["trace"]["error"].as_str().unwrap();
        assert!(error.contains("回环"));
        assert!(!error.contains("example.com"));
        assert!(excluded
            .query("query_date_stats", json!({"date": today, "extra": 1}))
            .is_err());
        assert!(excluded.query("query_unknown", json!({})).is_err());
        excluded.stop();
        excluded.stop();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_log_is_not_a_healthy_collector() {
        let path = std::env::temp_dir().join(format!(
            "hud-core-missing-{}-{}.jsonl",
            std::process::id(),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let service = Service::start(config(path, false));
        let logs = service.health()["logs"].clone();
        assert_eq!(logs["available"], false);
        assert_eq!(logs["error"], "用量日志不存在");
        assert!(logs["lastSuccessAt"].is_null());
        service.stop();
    }
}
