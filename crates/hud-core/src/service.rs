//! 无界面采集服务。HTTP 在锁外读取，快照刷新在锁内；不提供界面，也不回调事件。

use crate::{details, history, http, live, query, telemetry, usage};
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
    /// 账本与游标单独加锁。日志线程先拿这把锁读文件，再拿 Core 锁替换底稿，
    /// 避免实时刷新被整文件扫描堵住，也避免反序加锁死锁。
    reader: Arc<Mutex<crate::reader::LogReader>>,
    /// 不含活动、额度及通道故障的完成记录底稿。
    base: Arc<usage::RouteSnapshot>,
    live: live::LiveState,
    telemetry: telemetry::Telemetry,
    /// 发布用原子快照；改动时整份替换，读者不复制也能拿到一致视图。
    snapshot: Arc<usage::RouteSnapshot>,
    history: query::HistoryCache,
    health: Health,
    /// 实时/日志通道的独立故障状态；只把当前仍存在的故障写进快照。
    channels: crate::health::Health,
    detail_store: Arc<crate::detail_view::DetailStore>,
    quota_refresh: Option<SyncSender<()>>,
}

impl Core {
    fn new(config: &ServiceConfig) -> Self {
        let mut reader = crate::reader::LogReader::new(Local::now().date_naive());
        reader
            .ledger
            .set_include_unassociated(config.include_unassociated);
        let mut telemetry = telemetry::Telemetry::default();
        let channels = if config.cache_dir.as_os_str().is_empty() {
            telemetry.disable_persistence();
            crate::health::Health::default()
        } else {
            telemetry.bind_cache(config.cache_dir.join("hourly-cost.json"));
            crate::health::Health::with_log(config.cache_dir.join("diagnostics.jsonl"))
        };
        let update = reader.update(&config.usage_path, &config.magpie_host, None);
        let (log_error, missing) = (update.error, update.missing);
        let base = update.base;
        let mut core = Self {
            usage_path: config.usage_path.clone(),
            magpie_host: config.magpie_host.clone(),
            include_unassociated: config.include_unassociated,
            reader: Arc::new(Mutex::new(reader)),
            base: Arc::clone(&base),
            live: live::LiveState::default(),
            telemetry,
            snapshot: Arc::clone(&base),
            history: query::HistoryCache::default(),
            health: Health::default(),
            channels,
            detail_store: Arc::new(crate::detail_view::DetailStore::default()),
            quota_refresh: None,
        };
        core.note_log(log_error, missing);
        core.refresh();
        core
    }

    /// 只叠加最新活动、额度与通道故障。文件读取在 reader 锁内完成，不在这里。
    fn refresh(&mut self) {
        let mut snapshot = (*self.base).clone();
        if snapshot.date == self.snapshot.date
            && snapshot.daily.requests > self.snapshot.daily.requests
        {
            if let Some(sender) = &self.quota_refresh {
                let _ = sender.try_send(());
            }
        }
        self.live.apply(&mut snapshot);
        self.telemetry.apply(&mut snapshot);
        self.channels.apply(&mut snapshot);
        snapshot.revision = self.snapshot.revision;
        if snapshot != *self.snapshot {
            snapshot.revision = self.snapshot.revision + 1;
            self.snapshot = Arc::new(snapshot);
        }
    }

    fn note_log(&mut self, error: Option<String>, missing: bool) {
        if error.is_some() {
            self.health.logs.fail("无法读取用量日志");
            self.channels
                .set(crate::health::Source::UsageLog, Some("无法读取用量日志".into()));
        } else if missing {
            self.health.logs.fail("用量日志不存在");
            self.channels
                .set(crate::health::Source::UsageLog, Some("用量日志不存在".into()));
        } else {
            self.health.logs.succeed();
            self.channels.set(crate::health::Source::UsageLog, None);
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
        service.spawn_titles();
        service
    }

    pub fn snapshot(&self) -> usage::RouteSnapshot {
        (*self.snapshot_arc()).clone()
    }

    /// 共享原子快照：读者序列化 `Arc::as_ref()`，不再深拷贝整份 RouteSnapshot。
    pub fn snapshot_arc(&self) -> Arc<usage::RouteSnapshot> {
        Arc::clone(&self.lock().snapshot)
    }

    /// 仅在 revision 变化时返回新快照。`previous` 为 None 时视为未发布过，总是返回当前。
    /// 服务端发布循环用它在空转时完全避开克隆与序列化。
    pub fn snapshot_if_changed(
        &self,
        previous_revision: Option<u64>,
    ) -> Option<Arc<usage::RouteSnapshot>> {
        let snapshot = self.snapshot_arc();
        match previous_revision {
            Some(previous) if previous == snapshot.revision => None,
            _ => Some(snapshot),
        }
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

    /// 服务支持的可选能力。`detailsPaged` 表示提供 `query_usage_details_overview` +
    /// `query_usage_details_view` 的后端分页协议；`timezone` 给调用方用于展示对齐。
    pub fn capabilities(&self) -> Value {
        json!({
            "features": { "detailsPaged": true },
            "timezone": crate::paths::local_timezone(),
            "serverNow": chrono::Local::now().to_rfc3339(),
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
            // 旧协议：返回完整 DetailData（含全部请求行）。保留给 protocolVersion 1 客户端。
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
            // 分页协议：原始请求与费用组留在后端缓存，只回摘要；前端再发
            // query_usage_details_view 取一页。供 features.detailsPaged=true 的客户端。
            "query_usage_details_overview" => {
                let args: DetailArgs = parse_args(args)?;
                let (path, host, include) = {
                    let core = self.lock();
                    (
                        core.usage_path.clone(),
                        core.magpie_host.clone(),
                        core.include_unassociated,
                    )
                };
                let data = details::query_mode(
                    &path,
                    &host,
                    &args.start,
                    &args.end,
                    &args.filters.unwrap_or_default(),
                    include,
                    true,
                )?;
                let store = self.detail_store();
                let epoch = store.epoch()?;
                serde_json::to_value(store.insert_at_epoch(data, epoch)?)
            }
            "query_usage_details_view" => {
                let args: DetailViewArgs = parse_args(args)?;
                serde_json::to_value(self.detail_store().view(args.query)?)
            }
            "query_model_costs" => {
                let args: ModelCostArgs = parse_args(args)?;
                serde_json::to_value(self.model_costs(args)?)
            }
            "query_hourly_costs" => {
                let args: HourlyCostArgs = parse_args(args)?;
                serde_json::to_value(self.hourly_costs(args)?)
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
        let today = Local::now().date_naive();
        let day = match args.date.as_deref().filter(|date| !date.is_empty()) {
            Some(date) => query::parse_date(date, today)?,
            None => today,
        };
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
                    .cloned()
                    .unwrap_or_else(|| session.to_owned())
            });
            let source = args
                .source_key
                .as_deref()
                .filter(|source| !source.is_empty())
                .map(str::to_owned);
            let use_ledger = day == today && core.snapshot.date == today.to_string();
            let date = if use_ledger {
                core.snapshot.date.clone()
            } else {
                day.to_string()
            };
            let host = core.magpie_host.clone();
            let aliases = core.live.aliases.clone();
            let reader = use_ledger.then(|| Arc::clone(&core.reader));
            let path = core.usage_path.clone();
            let include = core.include_unassociated;
            drop(core);
            let (ledger_revision, rows) = if let Some(reader) = reader {
                let ledger = &reader.lock().unwrap_or_else(|error| error.into_inner()).ledger;
                (
                    ledger.revision,
                    ledger.model_cost_rows(canonical.as_deref(), source.as_deref(), &aliases),
                )
            } else {
                let mut rows = usage::scan_detail_rows_with(&path, day, day, include)?;
                if let Some(session) = canonical.as_deref() {
                    rows.retain(|row| row.session == session);
                }
                if let Some(source) = source.as_deref() {
                    rows.retain(|row| row.source_key == source);
                }
                (0, rows)
            };
            (host, date, ledger_revision, rows)
        };
        Ok(details::model_costs_mode(
            rows,
            &host,
            &date,
            ledger_revision,
            true,
        ))
    }

    /// 小时金额按来源精确匹配逐请求定价。所选日期是当日时优先用账本内行，
    /// 历史日期改为扫描该日日志，统计跟随所选日期而不是强制今天。
    fn hourly_costs(&self, args: HourlyCostArgs) -> Result<details::HourlyCostData, String> {
        let source_key = args.source_key.trim();
        if source_key.is_empty() {
            return Err("缺少来源筛选身份".into());
        }
        let today = Local::now().date_naive();
        let day = query::parse_date(&args.date, today)?;
        let (path, host, reader, aliases, current) = {
            let core = self.lock();
            let current = day == today && core.snapshot.date == today.to_string();
            (
                core.usage_path.clone(),
                core.magpie_host.clone(),
                Arc::clone(&core.reader),
                core.live.aliases.clone(),
                current,
            )
        };
        let rows = if current {
            reader
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .ledger
                .model_cost_rows(None, Some(source_key), &aliases)
        } else {
            usage::scan_detail_rows_with(&path, day, day, self.include_unassociated())?
                .into_iter()
                .filter(|row| row.source_key == source_key)
                .collect()
        };
        Ok(details::hourly_costs_mode(rows, &host, &args.date, true))
    }

    fn include_unassociated(&self) -> bool {
        self.lock().include_unassociated
    }

    /// 详情数据集的进程内缓存句柄；视图分页在锁外完成。
    pub fn detail_store(&self) -> Arc<crate::detail_view::DetailStore> {
        Arc::clone(&self.lock().detail_store)
    }

    /// 关闭本地详情窗口时清空缓存并推进 epoch，迟到的插入会被拒绝。
    /// 只清本进程的 store，远端服务的缓存不受影响。
    pub fn clear_detail_cache(&self) -> Result<(), String> {
        self.detail_store().clear()
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
            let mut watcher: Option<RecommendedWatcher> = None;
            let mut watch_error: Option<String> = None;
            let mut retry_at = Instant::now();
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if watcher.is_none() && Instant::now() >= retry_at {
                    match RecommendedWatcher::new(sender.clone(), notify::Config::default()) {
                        Ok(mut next) => {
                            let parent = path.parent().unwrap_or(std::path::Path::new("."));
                            let ancestor = parent.ancestors().find(|candidate| candidate.is_dir());
                            match ancestor {
                                Some(ancestor) => {
                                    let mode = if ancestor == parent {
                                        RecursiveMode::NonRecursive
                                    } else {
                                        RecursiveMode::Recursive
                                    };
                                    if next.watch(ancestor, mode).is_ok() {
                                        watcher = Some(next);
                                        watch_error = None;
                                    } else {
                                        watch_error = Some("文件监视不可用".into());
                                    }
                                }
                                None => watch_error = Some("文件监视不可用".into()),
                            }
                        }
                        Err(_error) => watch_error = Some("文件监视不可用".into()),
                    }
                    retry_at = Instant::now() + Duration::from_secs(5);
                }
                match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(Ok(event)) => {
                        let relevant = event
                            .paths
                            .iter()
                            .any(|changed| changed == &path || path.starts_with(changed));
                        if relevant {
                            if watcher.is_some() {
                                watch_error = None;
                            }
                            let deadline = Instant::now() + Duration::from_millis(150);
                            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                                if stop.load(Ordering::Acquire) {
                                    break;
                                }
                                match receiver.recv_timeout(left.min(Duration::from_millis(100))) {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(_error)) => {
                                        watch_error = Some("文件监视不可用".into());
                                        watcher = None;
                                        retry_at = Instant::now() + Duration::from_secs(5);
                                        break;
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                    Ok(Err(_error)) => {
                        watch_error = Some("文件监视不可用".into());
                        watcher = None;
                        retry_at = Instant::now() + Duration::from_secs(5);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        watch_error = Some("文件监视不可用".into());
                        watcher = None;
                        if !sleep_interruptible(&stop, Duration::from_secs(1)) {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                {
                    let mut core = shared.lock();
                    core.channels
                        .set(crate::health::Source::Watcher, watch_error.clone());
                }
                shared.refresh_log();
            }
            drop(watcher);
        });
    }

    /// 先在 reader 锁内读日志，释放后再拿 Core 锁替换底稿。两把锁不交叉持有。
    fn refresh_log(&self) {
        let (reader, path, host) = {
            let core = self.lock();
            (
                Arc::clone(&core.reader),
                core.usage_path.clone(),
                core.magpie_host.clone(),
            )
        };
        let update = reader
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .update(&path, &host, None);
        let mut core = self.lock();
        core.base = update.base;
        core.note_log(update.error, update.missing);
        core.refresh();
    }

    fn spawn_titles(self: &Arc<Self>) {
        let shared = self.clone_handle();
        let stop = Arc::clone(&self.stop);
        self.spawn(move || {
            let mut titles = live::Titles::default();
            let mut cli_titles = live::CliTitles::default();
            let mut custom_titles = live::CustomTitles::default();
            let mut child_titles = live::ChildTitles::default();
            let mut names = std::collections::HashMap::new();
            let mut aliases = std::collections::HashMap::new();
            let mut child_labels = std::collections::HashMap::new();
            let mut last_titles = Instant::now() - Duration::from_secs(3);
            let mut last_children = Instant::now() - Duration::from_secs(8);
            while !stop.load(Ordering::Acquire) {
                let title_due = last_titles.elapsed() >= Duration::from_secs(2) || titles.catching_up();
                let child_due = last_children.elapsed() >= Duration::from_secs(8);
                if title_due || child_due {
                    let (wanted, children) = {
                        let core = shared.lock();
                        let wanted = core
                            .snapshot
                            .sessions
                            .iter()
                            .map(|session| session.id.clone())
                            .collect();
                        let children = live::wanted_children(&core.snapshot);
                        (wanted, children)
                    };
                    if title_due {
                        names = cli_titles.scan(&wanted);
                        names.extend(custom_titles.scan());
                        names.extend(titles.scan(&live::desktop_roots()));
                        aliases = titles.aliases();
                        last_titles = Instant::now();
                    }
                    if child_due {
                        child_labels = child_titles.scan(&children);
                        last_children = Instant::now();
                    }
                    let mut core = shared.lock();
                    core.live.names = names.clone();
                    core.live.aliases = aliases.clone();
                    core.live.child_labels = child_labels.clone();
                    core.refresh();
                }
                let pause = if titles.catching_up() {
                    Duration::from_millis(100)
                } else {
                    Duration::from_millis(500)
                };
                if !sleep_interruptible(&stop, pause) {
                    break;
                }
            }
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
                        core.channels.set(crate::health::Source::Trace, None);
                        core.refresh();
                    }
                    Err(error) => {
                        let mut core = shared.lock();
                        core.health.trace.fail(error.clone());
                        // 只把 trace 通道的故障写进快照；并发/额度/用量通道的故障各自保留。
                        core.channels
                            .set(crate::health::Source::Trace, Some(error));
                        core.refresh();
                        drop(core);
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
                    core.refresh();
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
                    core.refresh();
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
            while !stop.load(Ordering::Acquire) {
                let result = http::read_json::<live::Activity>(
                    &agent,
                    &format!("{base}/v1/magpie/concurrency"),
                    true,
                );
                {
                    let mut core = shared.lock();
                    match result {
                        Ok(activity) => {
                            core.live.activity = activity;
                            core.live.error = None;
                            core.health.concurrency.succeed();
                            core.channels.set(crate::health::Source::Concurrency, None);
                        }
                        Err(error) => {
                            core.live.activity = live::Activity::default();
                            core.live.error = Some(error.clone());
                            core.health.concurrency.fail(error.clone());
                            core.channels
                                .set(crate::health::Source::Concurrency, Some(error));
                        }
                    }
                    core.refresh();
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

    /// 测试用：直接写入通道故障并刷新快照，验证恢复互不影响。
    #[cfg(test)]
    fn test_set_channel(&self, source: crate::health::Source, error: Option<&str>) {
        let mut core = self.lock();
        core.channels.set(source, error.map(str::to_owned));
        core.refresh();
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
struct DetailViewArgs {
    query: crate::detail_view::ViewRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HourlyCostArgs {
    date: String,
    source_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelCostArgs {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    source_key: Option<String>,
    /// 历史日期：指定时改为扫描该日日志，统计跟随所选日期而不是强制今天。
    #[serde(default)]
    date: Option<String>,
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

    #[test]
    fn channel_faults_recover_independently_in_the_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "hud-core-channels-{}-{}.jsonl",
            std::process::id(),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "").unwrap();
        let service = Service::start(config(path.clone(), false));
        service.test_set_channel(crate::health::Source::Trace, Some("trace down"));
        service.test_set_channel(crate::health::Source::Concurrency, Some("concurrency down"));
        let failed = service.snapshot_arc();
        assert_eq!(failed.state, "warning");
        let error = failed.error.as_deref().unwrap();
        assert!(error.contains("路由轨迹"), "{error}");
        assert!(error.contains("活跃请求"), "{error}");
        let failed_revision = failed.revision;
        // trace 恢复，并发故障必须保留。
        service.test_set_channel(crate::health::Source::Trace, None);
        let recovered = service.snapshot_arc();
        assert!(recovered.revision > failed_revision, "故障变化应发布新快照");
        let error = recovered.error.as_deref().unwrap();
        assert!(!error.contains("路由轨迹"), "trace 恢复后不应再报：{error}");
        assert!(error.contains("活跃请求"), "并发故障不能被 trace 恢复清掉：{error}");
        service.test_set_channel(crate::health::Source::Concurrency, None);
        let clear = service.snapshot_arc();
        assert!(clear.error.is_none(), "全部恢复后快照不再带故障");
        // 空转时不发布新快照，也不复制。
        assert!(service.snapshot_if_changed(Some(clear.revision)).is_none());
        let again = service.snapshot_if_changed(None).unwrap();
        assert!(Arc::ptr_eq(&clear, &again));
        service.stop();
        std::fs::remove_file(path).unwrap();
    }
}
