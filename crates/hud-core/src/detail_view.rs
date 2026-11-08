//! 详情数据集缓存与有界视图。
//! 原始请求和费用组留在后端；`insert` 只交摘要，`view` 只交统计、图和一页表格。
use crate::details::{CostGroup, Cycle, DetailData, DetailFilters};
use crate::usage::UsageDetailRow;
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_DATASETS: usize = 32;
const MAX_ROWS: usize = 600_000;
const PAGE_SIZE: usize = 30;
const GROUP_CHART_LIMIT: usize = 16;
const MIN_CHART_SLOTS: usize = 8;
const MAX_CHART_SLOTS: usize = 160;

const VIEWS: &[&str] = &["trend", "groups", "cycles"];
const GRAINS: &[&str] = &["hour", "day"];
const MODES: &[&str] = &["groups", "chart", "requests"];
const METRICS: &[&str] = &["tokens", "speed", "cost", "cache", "calls", "average"];
const SORTS: &[&str] = &[
    "label",
    "time",
    "value",
    "calls",
    "input",
    "output",
    "cacheRead",
];
const GROUPS: &[&str] = &[
    "model",
    "provider",
    "accountKey",
    "effort",
    "agent",
    "sourceKey",
    "date",
];

fn default_view() -> String {
    "trend".into()
}
fn default_grain() -> String {
    "hour".into()
}
fn default_group() -> String {
    "model".into()
}
fn default_metric() -> String {
    "tokens".into()
}
fn default_mode() -> String {
    "requests".into()
}
fn default_sort() -> String {
    "time".into()
}
fn default_true() -> bool {
    true
}
fn default_slots() -> usize {
    MIN_CHART_SLOTS
}

/// 重置周期过滤。时间戳为毫秒，区间是半开 `[start, end)`。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CycleFilter {
    pub provider: String,
    pub account: String,
    pub start: i64,
    pub end: i64,
}

/// 表格下钻。分组下钻比对 `field`/`key`；时间下钻按字典序比较 `[from, to]`（含端点）。
/// 省略的字符串视为空，不构成限制。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Drill {
    pub field: String,
    pub key: String,
    pub from: String,
    pub to: String,
    pub grain: String,
}

/// 一次有界视图请求。空字符串按默认值处理：趋势、小时、模型、Token、逐请求、时间降序。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewRequest {
    #[serde(default)]
    pub dataset_id: u64,
    #[serde(default)]
    pub filters: DetailFilters,
    #[serde(default)]
    pub cycle: Option<CycleFilter>,
    #[serde(default = "default_view")]
    pub view: String,
    #[serde(default = "default_grain")]
    pub grain: String,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_true")]
    pub descending: bool,
    #[serde(default)]
    pub page: usize,
    #[serde(default = "default_slots")]
    pub chart_slots: usize,
    #[serde(default)]
    pub drill: Option<Drill>,
    /// 前端依照当前显示标签的中文排序生成等价组，后端据此分页而不改变语言排序规则。
    #[serde(default)]
    pub label_order: Vec<Vec<String>>,
    /// 为真时确认数据集仍有效后直接返回空视图，不扫描请求。
    #[serde(default)]
    pub empty: bool,
}

impl Default for ViewRequest {
    fn default() -> Self {
        Self {
            dataset_id: 0,
            filters: DetailFilters::default(),
            cycle: None,
            view: default_view(),
            grain: default_grain(),
            group: default_group(),
            metric: default_metric(),
            mode: default_mode(),
            sort: default_sort(),
            descending: default_true(),
            page: 0,
            chart_slots: default_slots(),
            drill: None,
            label_order: Vec::new(),
            empty: false,
        }
    }
}

/// 与 `ui/details-data.js` 的 `aggregate` 同形。空选择的 `cost` 为 0；有请求但费用组未完整命中时为 `null`。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub calls: u64,
    pub tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub generation: f64,
    pub speed_output: u64,
    pub samples: u64,
    pub priced: u64,
    pub cost: Option<f64>,
    pub success: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub unknown: u64,
    pub speed: Option<f64>,
    pub cache: Option<f64>,
    pub average: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bin {
    pub key: String,
    pub label: String,
    pub stats: Stats,
    pub drill: Drill,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableEntry {
    pub key: String,
    pub label: String,
    pub stats: Stats,
    pub row: Option<UsageDetailRow>,
    pub drill: Option<Drill>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailView {
    pub stats: Stats,
    pub chart: Vec<Bin>,
    pub rows: Vec<TableEntry>,
    pub page: usize,
    pub page_size: usize,
    pub total_entries: usize,
    pub table_requests: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFacet {
    pub value: String,
    pub agent: String,
    pub caller_key_name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFacet {
    pub value: String,
    pub provider: String,
    pub account: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub sources: Vec<SourceFacet>,
    pub models: Vec<String>,
    pub providers: Vec<String>,
    pub accounts: Vec<AccountFacet>,
    pub efforts: Vec<String>,
}

/// 数据集摘要。不含请求行，也不含费用组。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailOverview {
    pub dataset_id: u64,
    pub start: String,
    pub end: String,
    pub total: u64,
    pub facets: Facets,
    pub cycles: Vec<Cycle>,
    pub warnings: Vec<String>,
}

#[derive(Clone)]
struct CostRef {
    members: Vec<u32>,
    cost: f64,
    priced: u64,
    /// 组里每个 id 都能在数据集中找到。缺任何一个则永远不能命中。
    matchable: bool,
}

struct Dataset {
    start: String,
    end: String,
    rows: Vec<UsageDetailRow>,
    account_keys: Vec<String>,
    groups: Vec<CostRef>,
    /// 每个请求当前指向的费用组（后写覆盖先写，与前端 Map 一致）。
    row_group: Vec<Option<u32>>,
    facets: Facets,
    cycles: Vec<Cycle>,
    warnings: Vec<String>,
}

struct Scratch {
    stamp: Vec<u32>,
    gen: u32,
    seen: HashSet<u32>,
}

impl Scratch {
    fn new(rows: usize) -> Self {
        Self {
            stamp: vec![0; rows],
            gen: 0,
            seen: HashSet::new(),
        }
    }

    fn begin(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.stamp.fill(0);
            self.gen = 1;
        }
        self.seen.clear();
    }
}

#[derive(Clone, Default)]
struct Acc {
    calls: u64,
    tokens: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    generation: f64,
    speed_output: u64,
    samples: u64,
    priced: u64,
    cost: f64,
    cost_known: bool,
    success: u64,
    failed: u64,
    cancelled: u64,
    unknown: u64,
}

impl Acc {
    fn add_row(&mut self, row: &UsageDetailRow) {
        self.calls = self.calls.saturating_add(1);
        self.tokens = self.tokens.saturating_add(row.tokens);
        self.input = self.input.saturating_add(row.input);
        self.output = self.output.saturating_add(row.output);
        self.cache_read = self.cache_read.saturating_add(row.cache_read);
        match row.outcome.as_str() {
            "success" => self.success = self.success.saturating_add(1),
            "failed" => self.failed = self.failed.saturating_add(1),
            "cancelled" => self.cancelled = self.cancelled.saturating_add(1),
            _ => self.unknown = self.unknown.saturating_add(1),
        }
        if let Some(ms) = row.generation_ms {
            if ms.is_finite() && ms > 0.0 {
                self.generation += ms;
                self.speed_output = self.speed_output.saturating_add(row.output);
                self.samples = self.samples.saturating_add(1);
            }
        }
    }

    fn add_cost(&mut self, group: &CostRef) {
        if !group.cost.is_finite() {
            return;
        }
        self.cost += group.cost;
        self.priced = self.priced.saturating_add(group.priced);
        self.cost_known = true;
    }

    fn finish(&self) -> Stats {
        let cost = if self.calls == 0 {
            Some(0.0)
        } else if self.cost_known {
            Some(self.cost)
        } else {
            None
        };
        let speed =
            (self.generation > 0.0).then(|| self.speed_output as f64 * 1000.0 / self.generation);
        let denom = self.input.saturating_add(self.cache_read);
        let cache = (denom > 0).then(|| 100.0 * self.cache_read as f64 / denom as f64);
        let average = (self.priced > 0).then(|| self.cost / self.priced as f64);
        Stats {
            calls: self.calls,
            tokens: self.tokens,
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            generation: self.generation,
            speed_output: self.speed_output,
            samples: self.samples,
            priced: self.priced,
            cost,
            success: self.success,
            failed: self.failed,
            cancelled: self.cancelled,
            unknown: self.unknown,
            speed,
            cache,
            average,
        }
    }
}

struct StoreInner {
    next_id: u64,
    epoch: u64,
    /// 队首是最久未使用。
    lru: VecDeque<u64>,
    sets: HashMap<u64, Arc<Dataset>>,
    rows: usize,
}

/// 进程内详情缓存。数据集放在自己的锁里，视图计算在释放锁之后进行，不接触 Runtime。
#[derive(Default)]
pub struct DetailStore {
    inner: Mutex<StoreInner>,
}

impl Default for StoreInner {
    fn default() -> Self {
        Self {
            next_id: Self::seed(),
            epoch: 0,
            lru: VecDeque::new(),
            sets: HashMap::new(),
            rows: 0,
        }
    }
}

impl StoreInner {
    /// 进程级随机起点：重启后数据集编号不复用 1，旧详情页持有的过期 datasetId 不会
    /// 命中新进程里恰好同号但内容不同的数据集。限制在 2^52 内，保证 JSON 精确传输
    /// 且足够多次递增不回绕到 0。
    fn seed() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        const MAX_SEED: u64 = 1u64 << 52;
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| (d.as_secs() << 20) ^ d.subsec_nanos() as u64)
            .unwrap_or(0);
        let pid = u64::from(std::process::id());
        ((time ^ pid.wrapping_mul(0x9E3779B97F4A7C15)) % (MAX_SEED - 1)) + 1
    }
}

impl DetailStore {
    pub fn epoch(&self) -> Result<u64, String> {
        self.inner
            .lock()
            .map(|guard| guard.epoch)
            .map_err(|_| "详情缓存锁损坏".into())
    }

    /// 关闭窗口后清空缓存；迟到的查询不能重新填回旧数据，大对象在后台释放。
    pub fn clear(&self) -> Result<(), String> {
        let released = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "详情缓存锁损坏".to_string())?;
            guard.epoch = guard.epoch.wrapping_add(1);
            guard.rows = 0;
            guard.lru.clear();
            std::mem::take(&mut guard.sets)
        };
        if !released.is_empty() {
            std::thread::spawn(move || drop(released));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn insert(&self, data: DetailData) -> Result<DetailOverview, String> {
        self.insert_at_epoch(data, self.epoch()?)
    }

    pub fn insert_at_epoch(&self, data: DetailData, epoch: u64) -> Result<DetailOverview, String> {
        if data.rows.len() > MAX_ROWS {
            return Err(format!("数据集超过 {MAX_ROWS} 行上限，请缩小日期范围"));
        }
        let dataset = Arc::new(Dataset::build(data));
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "详情缓存锁损坏".to_string())?;
        if guard.epoch != epoch {
            return Err("详情窗口已关闭，请重新查询".into());
        }
        let id = guard.next_id;
        if id == 0 || guard.sets.contains_key(&id) {
            return Err("数据集编号耗尽".into());
        }
        guard.next_id = guard.next_id.saturating_add(1);
        guard.rows = guard.rows.saturating_add(dataset.rows.len());
        guard.sets.insert(id, Arc::clone(&dataset));
        guard.lru.push_back(id);
        evict(&mut guard, id);
        drop(guard);
        Ok(dataset.overview(id))
    }

    pub fn view(&self, mut request: ViewRequest) -> Result<DetailView, String> {
        normalize(&mut request)?;
        let dataset = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| "详情缓存锁损坏".to_string())?;
            let dataset = guard
                .sets
                .get(&request.dataset_id)
                .cloned()
                .ok_or_else(|| "数据集不存在或已过期，请重新查询".to_string())?;
            touch(&mut guard.lru, request.dataset_id);
            dataset
        };
        if request.empty {
            return Ok(empty_view());
        }
        Ok(render(&dataset, &request))
    }
}

impl Dataset {
    fn build(data: DetailData) -> Self {
        let (groups, row_group) = index_costs(&data.rows, &data.costs);
        let account_keys = data
            .rows
            .iter()
            .map(|row| account_key(&row.provider, &row.account))
            .collect();
        let facets = build_facets(&data.rows);
        Self {
            start: data.start,
            end: data.end,
            rows: data.rows,
            account_keys,
            groups,
            row_group,
            facets,
            cycles: data.cycles,
            warnings: data.warnings,
        }
    }

    fn overview(&self, dataset_id: u64) -> DetailOverview {
        DetailOverview {
            dataset_id,
            start: self.start.clone(),
            end: self.end.clone(),
            total: self.rows.len() as u64,
            facets: self.facets.clone(),
            cycles: self.cycles.clone(),
            warnings: self.warnings.clone(),
        }
    }

    fn group_key(&self, idx: u32, field: &str) -> &str {
        let row = &self.rows[idx as usize];
        match field {
            "model" => row.model.as_str(),
            "provider" => row.provider.as_str(),
            "accountKey" => self.account_keys[idx as usize].as_str(),
            "effort" => row.effort.as_str(),
            "agent" => row.agent.as_str(),
            "sourceKey" => row.source_key.as_str(),
            "date" => row.date.as_str(),
            _ => "",
        }
    }

    fn select(&self, filters: &DetailFilters, cycle: Option<&CycleFilter>) -> Vec<u32> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(i, row)| {
                filters_match(filters, row, &self.account_keys[*i])
                    && cycle.is_none_or(|cycle| cycle_match(cycle, row))
            })
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// 一次扫过选中行。费用组只在「当前索引仍指向它，且它的全部成员都在选中集」时计入一次。
    fn aggregate<F>(
        &self,
        indices: &[u32],
        mut slot_of: F,
        bin_count: usize,
        scratch: &mut Scratch,
    ) -> (Stats, Vec<Stats>)
    where
        F: FnMut(u32) -> Option<usize>,
    {
        scratch.begin();
        let mut total = Acc::default();
        let mut bins = vec![Acc::default(); bin_count];
        for &idx in indices {
            let Some(row) = self.rows.get(idx as usize) else {
                continue;
            };
            scratch.stamp[idx as usize] = scratch.gen;
            total.add_row(row);
            if let Some(bin) = slot_of(idx).and_then(|slot| bins.get_mut(slot)) {
                bin.add_row(row);
            }
            if let Some(group) = self.row_group.get(idx as usize).copied().flatten() {
                scratch.seen.insert(group);
            }
        }
        for group_id in scratch.seen.iter().copied() {
            let Some(group) = self.groups.get(group_id as usize) else {
                continue;
            };
            if !group.matchable
                || !group
                    .members
                    .iter()
                    .all(|&member| scratch.stamp.get(member as usize) == Some(&scratch.gen))
            {
                continue;
            }
            total.add_cost(group);
            let mut place = None;
            let mut same = true;
            for &member in &group.members {
                match slot_of(member) {
                    Some(slot) => match place {
                        None => place = Some(slot),
                        Some(current) if current == slot => {}
                        Some(_) => {
                            same = false;
                            break;
                        }
                    },
                    None => {
                        same = false;
                        break;
                    }
                }
            }
            if same {
                if let Some(bin) = place.and_then(|slot| bins.get_mut(slot)) {
                    bin.add_cost(group);
                }
            }
        }
        (total.finish(), bins.iter().map(Acc::finish).collect())
    }

    fn stats_one(&self, idx: u32) -> Stats {
        let mut acc = Acc::default();
        let Some(row) = self.rows.get(idx as usize) else {
            return acc.finish();
        };
        acc.add_row(row);
        if let Some(group_id) = self.row_group.get(idx as usize).copied().flatten() {
            if let Some(group) = self.groups.get(group_id as usize) {
                if group.matchable && group.members.len() == 1 && group.members[0] == idx {
                    acc.add_cost(group);
                }
            }
        }
        acc.finish()
    }
}

fn index_costs(rows: &[UsageDetailRow], costs: &[CostGroup]) -> (Vec<CostRef>, Vec<Option<u32>>) {
    let mut id_to_idx = HashMap::<&str, u32>::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        id_to_idx.insert(row.id.as_str(), i as u32);
    }
    let mut row_group = vec![None; rows.len()];
    let mut groups = Vec::with_capacity(costs.len());
    for cost in costs {
        if !cost.cost.is_finite() {
            continue;
        }
        let mut members = Vec::with_capacity(cost.row_ids.len());
        let mut matchable = !cost.row_ids.is_empty();
        for id in &cost.row_ids {
            match id_to_idx.get(id.as_str()).copied() {
                Some(idx) if !members.contains(&idx) => members.push(idx),
                Some(_) => {}
                None => matchable = false,
            }
        }
        let group_id = groups.len() as u32;
        for &idx in &members {
            row_group[idx as usize] = Some(group_id);
        }
        groups.push(CostRef {
            members,
            cost: cost.cost,
            priced: cost.priced,
            matchable,
        });
    }
    (groups, row_group)
}

fn build_facets(rows: &[UsageDetailRow]) -> Facets {
    let mut sources: BTreeMap<String, (String, String, i64)> = BTreeMap::new();
    let mut models = BTreeSet::new();
    let mut providers = BTreeSet::new();
    let mut accounts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut efforts = BTreeSet::new();
    for row in rows {
        models.insert(row.model.clone());
        providers.insert(row.provider.clone());
        efforts.insert(row.effort.clone());
        accounts
            .entry(account_key(&row.provider, &row.account))
            .or_insert_with(|| (row.provider.clone(), row.account.clone()));
        let entry = sources
            .entry(row.source_key.clone())
            .or_insert_with(|| (row.agent.clone(), row.caller_key_name.clone(), i64::MIN));
        if row.timestamp >= entry.2 {
            entry.0.clone_from(&row.agent);
            entry.1.clone_from(&row.caller_key_name);
            entry.2 = row.timestamp;
        }
    }
    Facets {
        sources: sources
            .into_iter()
            .map(|(value, (agent, caller_key_name, _))| SourceFacet {
                value,
                agent,
                caller_key_name,
            })
            .collect(),
        models: models.into_iter().collect(),
        providers: providers.into_iter().collect(),
        accounts: accounts
            .into_iter()
            .map(|(value, (provider, account))| AccountFacet {
                value,
                provider,
                account,
            })
            .collect(),
        efforts: efforts.into_iter().collect(),
    }
}

fn account_key(provider: &str, account: &str) -> String {
    serde_json::to_string(&(provider, account)).unwrap_or_else(|_| "[]".into())
}

/// 与 `details::DetailFilters::matches` 相同：空列表和空会话都不限制。
fn filters_match(filters: &DetailFilters, row: &UsageDetailRow, account_key: &str) -> bool {
    (filters.source_keys.is_empty() || filters.source_keys.iter().any(|key| key == &row.source_key))
        && (filters.models.is_empty() || filters.models.iter().any(|model| model == &row.model))
        && (filters.providers.is_empty()
            || filters
                .providers
                .iter()
                .any(|provider| provider == &row.provider))
        && (filters.account_keys.is_empty()
            || filters.account_keys.iter().any(|key| key == account_key))
        && (filters.efforts.is_empty()
            || filters.efforts.iter().any(|effort| effort == &row.effort))
        && (filters.session.is_empty() || filters.session == row.session)
}

fn cycle_match(cycle: &CycleFilter, row: &UsageDetailRow) -> bool {
    row.provider == cycle.provider
        && row.account == cycle.account
        && row.timestamp >= cycle.start
        && row.timestamp < cycle.end
}

fn normalize(request: &mut ViewRequest) -> Result<(), String> {
    if request.view.is_empty() {
        request.view = default_view();
    }
    if request.grain.is_empty() {
        request.grain = default_grain();
    }
    if request.group.is_empty() {
        request.group = default_group();
    }
    if request.metric.is_empty() {
        request.metric = default_metric();
    }
    if request.mode.is_empty() {
        request.mode = default_mode();
    }
    if request.sort.is_empty() {
        request.sort = default_sort();
    }
    request.chart_slots = clamp_slots(request.chart_slots);
    known("视图", &request.view, VIEWS, " trend、groups 或 cycles")?;
    known("粒度", &request.grain, GRAINS, " hour 或 day")?;
    known(
        "分组",
        &request.group,
        GROUPS,
        " model、provider、accountKey、effort、agent、sourceKey 或 date",
    )?;
    known(
        "指标",
        &request.metric,
        METRICS,
        " tokens、speed、cost、cache、calls 或 average",
    )?;
    known(
        "表格模式",
        &request.mode,
        MODES,
        " groups、chart 或 requests",
    )?;
    known(
        "排序",
        &request.sort,
        SORTS,
        " label、time、value、calls、input、output 或 cacheRead",
    )?;
    if let Some(drill) = &request.drill {
        if !drill.field.is_empty() {
            known(
                "下钻字段",
                &drill.field,
                GROUPS,
                " model、provider、accountKey、effort、agent、sourceKey 或 date",
            )?;
        }
        if !drill.grain.is_empty() {
            known("下钻粒度", &drill.grain, GRAINS, " hour 或 day")?;
        }
    }
    Ok(())
}

fn known(kind: &str, value: &str, allowed: &[&str], expected: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("无法识别的{kind}「{value}」，应为{expected}"))
    }
}

fn clamp_slots(slots: usize) -> usize {
    slots.clamp(MIN_CHART_SLOTS, MAX_CHART_SLOTS)
}

fn div_ceil(value: usize, divisor: usize) -> usize {
    if divisor == 0 {
        value
    } else {
        value / divisor + usize::from(value % divisor != 0)
    }
}

fn empty_view() -> DetailView {
    DetailView {
        stats: Acc::default().finish(),
        chart: Vec::new(),
        rows: Vec::new(),
        page: 0,
        page_size: PAGE_SIZE,
        total_entries: 0,
        table_requests: 0,
    }
}

fn touch(lru: &mut VecDeque<u64>, id: u64) {
    if let Some(pos) = lru.iter().position(|item| *item == id) {
        lru.remove(pos);
    }
    lru.push_back(id);
}

fn evict(inner: &mut StoreInner, protect: u64) {
    let order: Vec<u64> = inner.lru.iter().copied().collect();
    let gone = victims(
        &order,
        |id| inner.sets.get(&id).map(|ds| ds.rows.len()).unwrap_or(0),
        inner.sets.len(),
        inner.rows,
        protect,
    );
    for id in gone {
        if let Some(ds) = inner.sets.remove(&id) {
            inner.rows = inner.rows.saturating_sub(ds.rows.len());
        }
        if let Some(pos) = inner.lru.iter().position(|item| *item == id) {
            inner.lru.remove(pos);
        }
    }
}

/// 从最久未使用开始淘汰，直到同时满足数据集个数和行数上限。不淘汰 `protect`。
fn victims<F>(
    oldest_first: &[u64],
    mut size_of: F,
    mut sets: usize,
    mut rows: usize,
    protect: u64,
) -> Vec<u64>
where
    F: FnMut(u64) -> usize,
{
    let mut order: Vec<u64> = oldest_first.to_vec();
    let mut gone = Vec::new();
    while sets > MAX_DATASETS || rows > MAX_ROWS {
        let Some(pos) = order.iter().position(|id| *id != protect) else {
            break;
        };
        let id = order.remove(pos);
        sets = sets.saturating_sub(1);
        rows = rows.saturating_sub(size_of(id));
        gone.push(id);
    }
    gone
}

#[derive(Clone)]
struct AxisBin {
    key: String,
    label: String,
    stats: Stats,
}

#[derive(Clone)]
struct GroupBin {
    key: String,
    stats: Stats,
}

fn render(ds: &Dataset, request: &ViewRequest) -> DetailView {
    // 周期列表的统计用 view=groups 附带完整周期对象；只要带了 cycle 就按半开区间过滤。
    // cycles 视图没选周期时，前端会传 empty，这里再兜底成空选择。
    let selection = if request.view == "cycles" && request.cycle.is_none() {
        Vec::new()
    } else {
        ds.select(&request.filters, request.cycle.as_ref())
    };
    let drill = request.drill.as_ref().filter(|drill| drill_active(drill));
    let table_indices: Vec<u32> = match drill {
        Some(drill) => selection
            .iter()
            .copied()
            .filter(|idx| drill_matches(ds, *idx, drill, &request.grain))
            .collect(),
        None => selection.clone(),
    };
    let mut scratch = Scratch::new(ds.rows.len());
    let trend = if request.view == "trend" {
        Some(trend_aggregate(
            ds,
            &selection,
            &request.grain,
            &mut scratch,
        ))
    } else {
        None
    };
    let grouped = if request.view != "trend" {
        Some(group_aggregate(
            ds,
            &selection,
            &request.group,
            &mut scratch,
        ))
    } else {
        None
    };
    let stats = trend
        .as_ref()
        .map(|item| item.0.clone())
        .or_else(|| grouped.as_ref().map(|item| item.0.clone()))
        .unwrap_or_else(|| ds.aggregate(&selection, |_| None, 0, &mut scratch).0);
    let chart = if let Some((stats_ignored, axis)) = &trend {
        let _ = stats_ignored;
        trend_chart(
            ds,
            &selection,
            axis,
            &request.grain,
            request.chart_slots,
            &mut scratch,
        )
    } else {
        let groups = grouped
            .as_ref()
            .map(|item| &item.1)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        top_group_chart(&request.group, &request.metric, groups)
    };

    let rows = match request.mode.as_str() {
        "requests" => request_page(ds, &table_indices, request),
        "groups" => {
            let groups = if drill.is_none() && request.view != "trend" {
                grouped
                    .as_ref()
                    .map(|item| item.1.clone())
                    .unwrap_or_default()
            } else {
                group_aggregate(ds, &table_indices, &request.group, &mut scratch).1
            };
            group_page(&request.group, request, &groups)
        }
        _ => {
            // chart
            if request.view == "trend" {
                let axis = if drill.is_none() {
                    trend
                        .as_ref()
                        .map(|item| item.1.clone())
                        .unwrap_or_default()
                } else {
                    trend_aggregate(ds, &table_indices, &request.grain, &mut scratch).1
                };
                let axis = clip_axis(axis, drill, &request.grain);
                bin_page(
                    axis.into_iter()
                        .map(|bin| (bin.key, bin.label, bin.stats, true))
                        .collect(),
                    request,
                )
            } else {
                let groups = if drill.is_none() {
                    grouped
                        .as_ref()
                        .map(|item| item.1.clone())
                        .unwrap_or_default()
                } else {
                    group_aggregate(ds, &table_indices, &request.group, &mut scratch).1
                };
                group_page(&request.group, request, &groups)
            }
        }
    };
    DetailView {
        stats,
        chart,
        rows: rows.0,
        page: rows.1,
        page_size: PAGE_SIZE,
        total_entries: rows.2,
        table_requests: table_indices.len(),
    }
}

fn drill_active(drill: &Drill) -> bool {
    !drill.field.is_empty() || !drill.from.is_empty() || !drill.to.is_empty()
}

fn drill_matches(ds: &Dataset, idx: u32, drill: &Drill, grain: &str) -> bool {
    if !drill.field.is_empty() && ds.group_key(idx, &drill.field) != drill.key {
        return false;
    }
    if drill.from.is_empty() && drill.to.is_empty() {
        return true;
    }
    let grain = if drill.grain.is_empty() {
        grain
    } else {
        drill.grain.as_str()
    };
    let key = row_time_key(&ds.rows[idx as usize], grain);
    (drill.from.is_empty() || key.as_str() >= drill.from.as_str())
        && (drill.to.is_empty() || key.as_str() <= drill.to.as_str())
}

fn row_time_key(row: &UsageDetailRow, grain: &str) -> String {
    if grain == "hour" {
        format!("{} {:02}:00", row.date, row.hour)
    } else {
        row.date.clone()
    }
}

fn parse_day(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == value)
}

fn build_axis(start: &str, end: &str, grain: &str) -> Vec<String> {
    let (Some(mut day), Some(end)) = (parse_day(start), parse_day(end)) else {
        return Vec::new();
    };
    if day > end {
        return Vec::new();
    }
    let mut keys = Vec::new();
    loop {
        if grain == "hour" {
            for hour in 0..24 {
                keys.push(format!("{day} {hour:02}:00"));
            }
        } else {
            keys.push(day.to_string());
        }
        if day == end {
            break;
        }
        day = day.succ_opt().unwrap_or(end);
        if keys.len() > 400 * 24 {
            break;
        }
    }
    keys
}

fn axis_label(key: &str, grain: &str, single_day: bool) -> String {
    if grain == "hour" && single_day {
        key.get(11..).unwrap_or(key).to_string()
    } else {
        key.get(5..).unwrap_or(key).to_string()
    }
}

fn axis_slot(row: &UsageDetailRow, start: NaiveDate, end: NaiveDate, grain: &str) -> Option<usize> {
    let date = parse_day(&row.date)?;
    if date < start || date > end {
        return None;
    }
    let day = (date - start).num_days() as usize;
    if grain == "day" {
        Some(day)
    } else {
        (row.hour < 24).then_some(day * 24 + row.hour)
    }
}

fn trend_aggregate(
    ds: &Dataset,
    indices: &[u32],
    grain: &str,
    scratch: &mut Scratch,
) -> (Stats, Vec<AxisBin>) {
    let keys = build_axis(&ds.start, &ds.end, grain);
    let single_day = ds.start == ds.end;
    let (Some(start), Some(end)) = (parse_day(&ds.start), parse_day(&ds.end)) else {
        return (ds.aggregate(indices, |_| None, 0, scratch).0, Vec::new());
    };
    if keys.is_empty() {
        return (ds.aggregate(indices, |_| None, 0, scratch).0, Vec::new());
    }
    let (stats, bins) = ds.aggregate(
        indices,
        |idx| axis_slot(&ds.rows[idx as usize], start, end, grain),
        keys.len(),
        scratch,
    );
    let axis = keys
        .into_iter()
        .zip(bins)
        .map(|(key, stats)| {
            let label = axis_label(&key, grain, single_day);
            AxisBin { key, label, stats }
        })
        .collect();
    (stats, axis)
}

fn trend_chart(
    ds: &Dataset,
    indices: &[u32],
    axis: &[AxisBin],
    grain: &str,
    slots: usize,
    scratch: &mut Scratch,
) -> Vec<Bin> {
    if axis.is_empty() {
        return Vec::new();
    }
    if axis.len() <= slots {
        return axis
            .iter()
            .map(|bin| Bin {
                drill: time_drill(&bin.key, &bin.key, grain),
                key: bin.key.clone(),
                label: bin.label.clone(),
                stats: bin.stats.clone(),
            })
            .collect();
    }
    let span = div_ceil(axis.len(), slots);
    let bucket_count = div_ceil(axis.len(), span);
    let (Some(start), Some(end)) = (parse_day(&ds.start), parse_day(&ds.end)) else {
        return Vec::new();
    };
    let mut buckets = vec![Vec::new(); bucket_count];
    for &idx in indices {
        if let Some(slot) = axis_slot(&ds.rows[idx as usize], start, end, grain) {
            let bucket = slot / span;
            if let Some(bucket) = buckets.get_mut(bucket) {
                bucket.push(idx);
            }
        }
    }
    buckets
        .into_iter()
        .enumerate()
        .map(|(bucket, members)| {
            let from = bucket * span;
            let to = ((bucket + 1) * span).min(axis.len()).saturating_sub(1);
            let (stats, _) = ds.aggregate(&members, |_| None, 0, scratch);
            Bin {
                key: axis[from].key.clone(),
                label: format!("{} — {}", axis[from].label, axis[to].label),
                drill: time_drill(&axis[from].key, &axis[to].key, grain),
                stats,
            }
        })
        .collect()
}

fn time_drill(from: &str, to: &str, grain: &str) -> Drill {
    Drill {
        field: String::new(),
        key: String::new(),
        from: from.into(),
        to: to.into(),
        grain: grain.into(),
    }
}

fn group_drill(field: &str, key: &str) -> Drill {
    Drill {
        field: field.into(),
        key: key.into(),
        from: String::new(),
        to: String::new(),
        grain: String::new(),
    }
}

fn group_aggregate(
    ds: &Dataset,
    indices: &[u32],
    field: &str,
    scratch: &mut Scratch,
) -> (Stats, Vec<GroupBin>) {
    let mut map: HashMap<String, usize> = HashMap::new();
    let mut keys = Vec::new();
    let mut slots = vec![None; ds.rows.len()];
    for &idx in indices {
        let key = ds.group_key(idx, field);
        let slot = if let Some(&slot) = map.get(key) {
            slot
        } else {
            let slot = keys.len();
            keys.push(key.to_string());
            map.insert(keys[slot].clone(), slot);
            slot
        };
        if let Some(place) = slots.get_mut(idx as usize) {
            *place = Some(slot);
        }
    }
    let (stats, bins) = ds.aggregate(
        indices,
        |idx| slots.get(idx as usize).copied().flatten(),
        keys.len(),
        scratch,
    );
    let groups = keys
        .into_iter()
        .zip(bins)
        .map(|(key, stats)| GroupBin { key, stats })
        .collect();
    (stats, groups)
}

fn top_group_chart(field: &str, metric: &str, groups: &[GroupBin]) -> Vec<Bin> {
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&left, &right| {
        cmp_metric(&groups[left].stats, &groups[right].stats, metric, true)
    });
    order
        .into_iter()
        .take(GROUP_CHART_LIMIT)
        .map(|idx| {
            let group = &groups[idx];
            Bin {
                key: group.key.clone(),
                label: group.key.clone(),
                stats: group.stats.clone(),
                drill: group_drill(field, &group.key),
            }
        })
        .collect()
}

fn clip_axis(axis: Vec<AxisBin>, drill: Option<&Drill>, grain: &str) -> Vec<AxisBin> {
    let Some(drill) = drill else { return axis };
    if drill.from.is_empty() && drill.to.is_empty() {
        return axis;
    }
    let drill_grain = if drill.grain.is_empty() {
        grain
    } else {
        drill.grain.as_str()
    };
    if drill_grain != grain {
        return axis;
    }
    axis.into_iter()
        .filter(|bin| {
            (drill.from.is_empty() || bin.key.as_str() >= drill.from.as_str())
                && (drill.to.is_empty() || bin.key.as_str() <= drill.to.as_str())
        })
        .collect()
}

fn group_page(
    field: &str,
    request: &ViewRequest,
    groups: &[GroupBin],
) -> (Vec<TableEntry>, usize, usize) {
    let (mut rows, page, total) = bin_page(
        groups
            .iter()
            .map(|group| {
                (
                    group.key.clone(),
                    group.key.clone(),
                    group.stats.clone(),
                    false,
                )
            })
            .collect(),
        request,
    );
    for entry in &mut rows {
        entry.drill = Some(group_drill(field, &entry.key));
    }
    (rows, page, total)
}

fn bin_page(
    mut bins: Vec<(String, String, Stats, bool)>,
    request: &ViewRequest,
) -> (Vec<TableEntry>, usize, usize) {
    let label_rank: HashMap<&str, usize> = request
        .label_order
        .iter()
        .enumerate()
        .flat_map(|(rank, keys)| keys.iter().map(move |key| (key.as_str(), rank)))
        .collect();
    bins.sort_by(|left, right| {
        if request.sort == "label" && !label_rank.is_empty() {
            match (
                label_rank.get(left.0.as_str()),
                label_rank.get(right.0.as_str()),
            ) {
                (Some(left_rank), Some(right_rank)) => {
                    return if request.descending {
                        right_rank.cmp(left_rank)
                    } else {
                        left_rank.cmp(right_rank)
                    }
                }
                (Some(_), None) => return std::cmp::Ordering::Less,
                (None, Some(_)) => return std::cmp::Ordering::Greater,
                _ => {}
            }
        }
        cmp_table(
            &left.1,
            None,
            &left.2,
            &right.1,
            None,
            &right.2,
            &request.sort,
            &request.metric,
            request.descending,
        )
    });
    let (page, start, end) = page_window(bins.len(), request.page);
    let rows = bins[start..end]
        .iter()
        .map(|(key, label, stats, time)| TableEntry {
            drill: Some(if *time {
                time_drill(key, key, &request.grain)
            } else {
                group_drill(&request.group, key)
            }),
            key: key.clone(),
            label: label.clone(),
            stats: stats.clone(),
            row: None,
        })
        .collect();
    (rows, page, bins.len())
}

fn request_page(
    ds: &Dataset,
    indices: &[u32],
    request: &ViewRequest,
) -> (Vec<TableEntry>, usize, usize) {
    let labels: Vec<String> = if request.sort == "label" {
        indices
            .iter()
            .map(|idx| request_label(ds.rows[*idx as usize].timestamp))
            .collect()
    } else {
        Vec::new()
    };
    let mut order: Vec<usize> = (0..indices.len()).collect();
    order.sort_by(|&left, &right| {
        let left_row = &ds.rows[indices[left] as usize];
        let right_row = &ds.rows[indices[right] as usize];
        let left_label = labels.get(left).map(String::as_str).unwrap_or("");
        let right_label = labels.get(right).map(String::as_str).unwrap_or("");
        cmp_request(
            ds,
            indices[left],
            left_row,
            left_label,
            indices[right],
            right_row,
            right_label,
            &request.sort,
            &request.metric,
            request.descending,
        )
    });
    let (page, start, end) = page_window(order.len(), request.page);
    let rows = order[start..end]
        .iter()
        .map(|&pos| {
            let idx = indices[pos];
            let row = ds.rows[idx as usize].clone();
            TableEntry {
                key: row.id.clone(),
                label: request_label(row.timestamp),
                stats: ds.stats_one(idx),
                row: Some(row),
                drill: None,
            }
        })
        .collect();
    (rows, page, indices.len())
}

fn page_window(len: usize, page: usize) -> (usize, usize, usize) {
    if len == 0 {
        return (0, 0, 0);
    }
    let page = page.min((len - 1) / PAGE_SIZE);
    let start = page * PAGE_SIZE;
    (page, start, (start + PAGE_SIZE).min(len))
}

fn request_label(timestamp: i64) -> String {
    DateTime::from_timestamp_millis(timestamp)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

#[derive(Clone, Copy)]
enum Value {
    Num(u64),
    Maybe(Option<f64>),
}

fn cmp_request(
    ds: &Dataset,
    left_idx: u32,
    left: &UsageDetailRow,
    left_label: &str,
    right_idx: u32,
    right: &UsageDetailRow,
    right_label: &str,
    sort: &str,
    metric: &str,
    descending: bool,
) -> std::cmp::Ordering {
    match sort {
        "label" => cmp_str(left_label, right_label, descending),
        "time" => cmp_i64(Some(left.timestamp), Some(right.timestamp), descending),
        "calls" => cmp_u64(1, 1, descending),
        "input" => cmp_u64(left.input, right.input, descending),
        "output" => cmp_u64(left.output, right.output, descending),
        "cacheRead" => cmp_u64(left.cache_read, right.cache_read, descending),
        _ => cmp_value(
            request_metric(ds, left_idx, left, metric),
            request_metric(ds, right_idx, right, metric),
            descending,
        ),
    }
}

fn request_metric(ds: &Dataset, idx: u32, row: &UsageDetailRow, metric: &str) -> Value {
    match metric {
        "calls" => Value::Num(1),
        "tokens" => Value::Num(row.tokens),
        "speed" => Value::Maybe(
            row.generation_ms
                .filter(|ms| ms.is_finite() && *ms > 0.0)
                .map(|ms| row.output as f64 * 1000.0 / ms),
        ),
        "cache" => {
            let denom = row.input.saturating_add(row.cache_read);
            Value::Maybe((denom > 0).then(|| 100.0 * row.cache_read as f64 / denom as f64))
        }
        "cost" => Value::Maybe(singleton_cost(ds, idx).map(|(cost, _)| cost)),
        "average" => Value::Maybe(
            singleton_cost(ds, idx)
                .and_then(|(cost, priced)| (priced > 0).then(|| cost / priced as f64)),
        ),
        _ => Value::Num(row.tokens),
    }
}

fn singleton_cost(ds: &Dataset, idx: u32) -> Option<(f64, u64)> {
    let group_id = ds.row_group.get(idx as usize).copied().flatten()?;
    let group = ds.groups.get(group_id as usize)?;
    (group.matchable
        && group.members.len() == 1
        && group.members[0] == idx
        && group.cost.is_finite())
    .then_some((group.cost, group.priced))
}

fn cmp_table(
    left_label: &str,
    left_time: Option<i64>,
    left: &Stats,
    right_label: &str,
    right_time: Option<i64>,
    right: &Stats,
    sort: &str,
    metric: &str,
    descending: bool,
) -> std::cmp::Ordering {
    match sort {
        "label" => cmp_str(left_label, right_label, descending),
        "time" => cmp_i64(left_time, right_time, descending),
        "calls" => cmp_u64(left.calls, right.calls, descending),
        "input" => cmp_u64(left.input, right.input, descending),
        "output" => cmp_u64(left.output, right.output, descending),
        "cacheRead" => cmp_u64(left.cache_read, right.cache_read, descending),
        _ => cmp_metric(left, right, metric, descending),
    }
}

fn cmp_metric(left: &Stats, right: &Stats, metric: &str, descending: bool) -> std::cmp::Ordering {
    cmp_value(
        metric_value(left, metric),
        metric_value(right, metric),
        descending,
    )
}

fn metric_value(stats: &Stats, metric: &str) -> Value {
    match metric {
        "tokens" => Value::Num(stats.tokens),
        "calls" => Value::Num(stats.calls),
        "cost" => Value::Maybe(stats.cost),
        "speed" => Value::Maybe(stats.speed),
        "cache" => Value::Maybe(stats.cache),
        "average" => Value::Maybe(stats.average),
        _ => Value::Maybe(None),
    }
}

fn cmp_value(left: Value, right: Value, descending: bool) -> std::cmp::Ordering {
    match (left, right) {
        (Value::Num(left), Value::Num(right)) => cmp_u64(left, right, descending),
        (Value::Maybe(left), Value::Maybe(right)) => cmp_f64(left, right, descending),
        _ => std::cmp::Ordering::Equal,
    }
}

/// `None` 在两种方向下都排在最后；相等保持原顺序（稳定排序）。
fn cmp_f64(left: Option<f64>, right: Option<f64>, descending: bool) -> std::cmp::Ordering {
    match (
        left.filter(|value| value.is_finite()),
        right.filter(|value| value.is_finite()),
    ) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => {
            let order = left
                .partial_cmp(&right)
                .unwrap_or(std::cmp::Ordering::Equal);
            if descending {
                order.reverse()
            } else {
                order
            }
        }
    }
}

fn cmp_u64(left: u64, right: u64, descending: bool) -> std::cmp::Ordering {
    if descending {
        right.cmp(&left)
    } else {
        left.cmp(&right)
    }
}

fn cmp_i64(left: Option<i64>, right: Option<i64>, descending: bool) -> std::cmp::Ordering {
    match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => {
            if descending {
                right.cmp(&left)
            } else {
                left.cmp(&right)
            }
        }
    }
}

fn cmp_str(left: &str, right: &str, descending: bool) -> std::cmp::Ordering {
    if descending {
        right.cmp(left)
    } else {
        left.cmp(right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, ts: i64, hour: usize, model: &str) -> UsageDetailRow {
        UsageDetailRow {
            id: id.into(),
            timestamp: ts,
            date: "2026-11-06".into(),
            hour,
            session: "s".into(),
            agent: "Claude Code".into(),
            caller_key_id: "key".into(),
            caller_key_name: "name".into(),
            via: String::new(),
            source_key: "[\"Claude Code\",\"key\"]".into(),
            provider: "vendor".into(),
            account: "acc".into(),
            model: model.into(),
            effort: "high".into(),
            outcome: "success".into(),
            input: 0,
            output: 0,
            cache_read: 0,
            tokens: 0,
            generation_ms: None,
            cost_matchable: true,
        }
    }

    fn cost(ids: &[&str], amount: f64, priced: u64) -> CostGroup {
        CostGroup {
            session: "s".into(),
            row_ids: ids.iter().map(|id| (*id).to_string()).collect(),
            cost: amount,
            priced,
            unpriced: 0,
        }
    }

    fn data_on(
        start: &str,
        end: &str,
        rows: Vec<UsageDetailRow>,
        costs: Vec<CostGroup>,
    ) -> DetailData {
        DetailData {
            start: start.into(),
            end: end.into(),
            rows,
            costs,
            cycles: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn view_of(store: &DetailStore, id: u64, mutate: impl FnOnce(&mut ViewRequest)) -> DetailView {
        let mut request = ViewRequest {
            dataset_id: id,
            ..ViewRequest::default()
        };
        mutate(&mut request);
        store.view(request).unwrap()
    }

    #[test]
    fn weighted_stats_keep_zero_unknown_and_complete_multirow_costs() {
        let mut zero = row("zero", 10, 0, "priced");
        zero.input = 100;
        zero.output = 50;
        zero.cache_read = 100;
        zero.tokens = 250;
        zero.generation_ms = Some(1000.0);
        let mut unknown = row("unknown", 11, 0, "priced");
        unknown.outcome = "failed".into();
        unknown.effort = "low".into();
        let mut left = row("left", 12, 1, "shared");
        left.output = 50;
        left.tokens = 50;
        left.generation_ms = Some(1000.0);
        left.outcome = "cancelled".into();
        let mut right = row("right", 13, 1, "shared");
        right.tokens = 5;
        right.outcome = "other".into();
        right.effort = "max".into();
        let mut data = data_on(
            "2026-11-06",
            "2026-11-06",
            vec![zero, unknown, left, right],
            vec![cost(&["zero"], 0.0, 1), cost(&["left", "right"], 3.0, 2)],
        );
        data.cycles.push(Cycle {
            provider: "vendor".into(),
            account: "acc".into(),
            window: "7 days".into(),
            start: 1,
            end: 2,
            observed_at: 3,
            remaining: Some(40.0),
        });
        data.warnings.push("部分未定价".into());
        let store = DetailStore::default();
        let overview = store.insert(data).unwrap();
        let json = serde_json::to_value(&overview).unwrap();
        assert!(json.get("rows").is_none());
        assert!(json.get("costs").is_none());
        assert!(json["datasetId"].as_u64().unwrap() > 0, "数据集编号非零");
        assert_eq!(json["total"], 4);
        assert_eq!(json["warnings"][0], "部分未定价");
        assert_eq!(json["cycles"][0]["observedAt"], 3);
        assert_eq!(json["facets"]["sources"][0]["callerKeyName"], "name");
        assert_eq!(
            json["facets"]["accounts"][0]["value"],
            serde_json::to_string(&("vendor", "acc")).unwrap()
        );
        assert!(!serde_json::to_string(&overview)
            .unwrap()
            .contains("\"zero\""));

        let all = view_of(&store, overview.dataset_id, |_| {});
        assert_eq!(all.stats.calls, 4);
        assert_eq!(all.stats.tokens, 305);
        assert_eq!(all.stats.input, 100);
        assert_eq!(all.stats.output, 100);
        assert_eq!(all.stats.cache_read, 100);
        assert_eq!(all.stats.samples, 2);
        assert_eq!(all.stats.speed_output, 100);
        assert_eq!(all.stats.speed, Some(50.0));
        assert_eq!(all.stats.cache, Some(50.0));
        assert_eq!(all.stats.cost, Some(3.0));
        assert_eq!(all.stats.priced, 3);
        assert_eq!(all.stats.average, Some(1.0));
        assert_eq!(
            (
                all.stats.success,
                all.stats.failed,
                all.stats.cancelled,
                all.stats.unknown
            ),
            (1, 1, 1, 1)
        );

        let priced = view_of(&store, overview.dataset_id, |req| req.filters.models = vec!["priced".into()]);
        assert_eq!(priced.stats.calls, 2);
        assert_eq!(priced.stats.cost, Some(0.0), "真实零费用不能变成未知");
        assert_eq!(priced.stats.priced, 1);

        let missing = view_of(&store, overview.dataset_id, |req| req.filters.efforts = vec!["low".into()]);
        assert_eq!(missing.stats.calls, 1);
        assert_eq!(
            missing.stats.cost, None,
            "没有命中费用组时保持未知，不能填零"
        );

        let half = view_of(&store, overview.dataset_id, |req| req.filters.models = vec!["shared".into()]);
        // 两个成员都在选中集里，费用只计一次。
        assert_eq!(half.stats.cost, Some(3.0));
        assert_eq!(half.stats.priced, 2);
        let one_side = view_of(&store, overview.dataset_id, |req| {
            req.filters.models = vec!["shared".into()];
            req.filters.efforts = vec!["high".into()];
        });
        assert_eq!(one_side.stats.calls, 1);
        assert_eq!(one_side.stats.cost, None, "多行费用组缺任一成员就不计入");

        let empty = store
            .insert(data_on("2026-11-06", "2026-11-06", Vec::new(), Vec::new()))
            .unwrap();
        let blank = view_of(&store, empty.dataset_id, |_| {});
        assert_eq!(blank.stats.calls, 0);
        assert_eq!(blank.stats.cost, Some(0.0));
        assert_eq!(blank.stats.speed, None);
        assert_eq!(blank.stats.average, None);
    }

    #[test]
    fn cycle_is_half_open_and_intersects_filters() {
        let inside = row("inside", 100, 0, "keep");
        let at_end = row("end", 200, 0, "keep");
        let mut other = row("other", 150, 0, "keep");
        other.account = "other".into();
        other.session = "elsewhere".into();
        let mut dropped = row("dropped", 150, 1, "drop");
        dropped.provider = "other".into();
        let store = DetailStore::default();
        let overview = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![inside, at_end, other, dropped],
                Vec::new(),
            ))
            .unwrap();
        let cycle = CycleFilter {
            provider: "vendor".into(),
            account: "acc".into(),
            start: 100,
            end: 200,
        };
        let narrowed = view_of(&store, overview.dataset_id, |req| {
            req.view = "cycles".into();
            req.filters.models = vec!["keep".into()];
            req.filters.providers = vec!["vendor".into()];
            req.cycle = Some(cycle.clone());
        });
        assert_eq!(narrowed.stats.calls, 1, "结束时刻排除，账号和模型同时生效");
        assert_eq!(narrowed.table_requests, 1);
        assert_eq!(narrowed.rows[0].row.as_ref().unwrap().id, "inside");

        let crossed = view_of(&store, overview.dataset_id, |req| {
            req.view = "cycles".into();
            req.filters.providers = vec!["vendor".into()];
            req.filters.account_keys = vec![account_key("other", "acc")];
            req.cycle = Some(cycle.clone());
        });
        assert_eq!(crossed.stats.calls, 0);
        assert_eq!(crossed.stats.cost, Some(0.0));

        let session = view_of(&store, overview.dataset_id, |req| {
            req.view = "cycles".into();
            req.filters.session = "elsewhere".into();
            req.cycle = Some(CycleFilter {
                account: "other".into(),
                ..cycle.clone()
            });
        });
        assert_eq!(session.stats.calls, 1);
        assert_eq!(session.rows[0].key, "other");

        let on_groups = view_of(&store, overview.dataset_id, |req| {
            req.view = "groups".into();
            req.cycle = Some(cycle);
        });
        assert_eq!(
            on_groups.stats.calls, 1,
            "周期统计请求用 groups 视图，半开区间仍然生效"
        );
        let no_cycle = view_of(&store, overview.dataset_id, |req| {
            req.view = "cycles".into()
        });
        assert_eq!(no_cycle.stats.calls, 0);
        assert_eq!(no_cycle.stats.cost, Some(0.0));
    }

    #[test]
    fn sorts_globally_before_paging_and_drill_does_not_change_totals() {
        let mut rows = Vec::new();
        let mut costs = Vec::new();
        for i in 0..35 {
            let mut item = row(
                &format!("id-{i:02}"),
                1_000 + i,
                (i % 24) as usize,
                if i < 10 { "x" } else { "y" },
            );
            item.tokens = i as u64;
            if i < 30 {
                costs.push(cost(&[item.id.as_str()], i as f64, 1));
            }
            rows.push(item);
        }
        let store = DetailStore::default();
        let id = store
            .insert(data_on("2026-11-06", "2026-11-06", rows, costs))
            .unwrap()
            .dataset_id;
        let desc = view_of(&store, id, |req| {
            req.sort = "value".into();
            req.metric = "cost".into();
            req.page = 99;
        });
        assert_eq!(desc.page, 1, "先排序再把页码夹到最后一页");
        assert_eq!(desc.page_size, 30);
        assert_eq!(desc.total_entries, 35);
        assert_eq!(desc.rows.len(), 5);
        assert!(
            desc.rows.iter().all(|entry| entry.stats.cost.is_none()),
            "未知费用在降序时仍排在最后"
        );
        let first = view_of(&store, id, |req| {
            req.sort = "value".into();
            req.metric = "cost".into();
            req.chart_slots = 160;
        });
        assert_eq!(first.rows[0].stats.cost, Some(29.0));
        assert_eq!(first.rows.len(), 30);
        assert_eq!(first.chart.len(), 24);
        assert_eq!(
            first.chart.iter().map(|bin| bin.stats.calls).sum::<u64>(),
            35
        );
        let asc = view_of(&store, id, |req| {
            req.sort = "value".into();
            req.metric = "cost".into();
            req.descending = false;
            req.page = 5;
        });
        assert_eq!(asc.page, 1);
        assert_eq!(asc.rows[0].stats.cost, None);

        let drilled = view_of(&store, id, |req| {
            req.sort = "value".into();
            req.metric = "tokens".into();
            req.drill = Some(Drill {
                field: "model".into(),
                key: "x".into(),
                ..Drill::default()
            });
        });
        assert_eq!(drilled.stats.calls, 35, "下钻不改变总统计");
        assert_eq!(drilled.stats.tokens, (0..35).sum::<u64>());
        assert_eq!(drilled.table_requests, 10);
        assert_eq!(drilled.rows.len(), 10);
        assert!(drilled
            .rows
            .iter()
            .all(|entry| entry.row.as_ref().unwrap().model == "x"));
        let chart_calls: u64 = drilled.chart.iter().map(|bin| bin.stats.calls).sum();
        assert_eq!(chart_calls, 35, "图也不按下钻缩小");

        let tied = view_of(&store, id, |req| {
            req.sort = "time".into();
            req.filters.models = vec!["x".into()];
        });
        let order: Vec<_> = tied.rows.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(
            order,
            [
                "id-09", "id-08", "id-07", "id-06", "id-05", "id-04", "id-03", "id-02", "id-01",
                "id-00"
            ]
        );
    }

    #[test]
    fn trend_merges_complete_axis_with_weighted_totals() {
        let mut left = row("left", 1, 0, "m");
        left.output = 100;
        left.input = 300;
        left.tokens = 400;
        left.generation_ms = Some(1000.0);
        let mut right = row("right", 2, 1, "m");
        right.output = 100;
        right.cache_read = 100;
        right.tokens = 200;
        right.generation_ms = Some(3000.0);
        let mut zero = row("zero", 3, 5, "m");
        zero.tokens = 1;
        let mut lone = row("lone", 4, 6, "m");
        lone.output = 10;
        lone.tokens = 10;
        lone.outcome = "failed".into();
        let store = DetailStore::default();
        let id = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![left, right, zero, lone],
                vec![cost(&["left", "right"], 4.0, 2), cost(&["zero"], 0.0, 1)],
            ))
            .unwrap()
            .dataset_id;
        let view = view_of(&store, id, |req| {
            req.mode = "chart".into();
            req.chart_slots = 1;
            req.sort = "label".into();
            req.descending = false;
        });
        assert_eq!(
            view.chart.len(),
            8,
            "低于 8 的槽位抬到 8，24 个小时合并成 8 段"
        );
        assert!(view.chart.len() <= 160);
        assert_eq!(view.total_entries, 24, "表格保留未合并的完整小时轴");
        assert_eq!(view.rows.len(), 24);
        assert_eq!(view.rows[0].label, "00:00");
        assert_eq!(
            view.rows[0].stats.cost, None,
            "拆开的多行费用组不能算进单个小时"
        );
        assert_eq!(view.rows[5].stats.cost, Some(0.0));
        assert_eq!(view.rows[5].stats.priced, 1);
        assert_eq!(view.chart[0].label, "00:00 — 02:00");
        assert_eq!(
            view.chart[0].stats.cost,
            Some(4.0),
            "合并后成员都在段内，费用计一次"
        );
        assert_eq!(view.chart[0].stats.priced, 2);
        assert_eq!(
            view.chart[0].stats.speed,
            Some(50.0),
            "速度按输出和生成时间加权，不是对速率取平均"
        );
        assert_eq!(view.chart[0].stats.cache, Some(25.0));
        assert_eq!(view.chart[0].drill.from, "2026-11-06 00:00");
        assert_eq!(view.chart[0].drill.to, "2026-11-06 02:00");
        assert_eq!(view.chart[1].stats.cost, Some(0.0));
        assert_eq!(view.chart[2].stats.cost, None);
        assert_eq!(view.stats.cost, Some(4.0));
        assert_eq!(view.stats.speed, Some(50.0));
        assert_eq!(view.stats.calls, 4);

        let drilled = view_of(&store, id, |req| {
            req.mode = "chart".into();
            req.drill = Some(view.chart[0].drill.clone());
        });
        assert_eq!(drilled.stats.calls, 4);
        assert_eq!(drilled.table_requests, 2);
        assert_eq!(
            drilled.total_entries, 3,
            "时间下钻只留下该段的小时，空小时仍保留"
        );
        assert!(drilled.chart.len() == 8, "图不随下钻改变");

        let wide = view_of(&store, id, |req| req.chart_slots = 1_000);
        assert_eq!(wide.chart.len(), 24);
        assert!(wide.chart.len() <= 160);

        let mut next_day = row("next", 5, 0, "m");
        next_day.date = "2026-11-07".into();
        let span = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-08",
                vec![next_day],
                Vec::new(),
            ))
            .unwrap()
            .dataset_id;
        let days = view_of(&store, span, |req| {
            req.grain = "day".into();
            req.mode = "chart".into();
            req.descending = false;
            req.sort = "label".into();
        });
        assert_eq!(days.chart.len(), 3);
        assert_eq!(days.chart[0].label, "11-06");
        assert_eq!(days.rows[1].key, "2026-11-07");
        assert_eq!(days.rows[1].stats.calls, 1);
        assert_eq!(days.rows[0].stats.calls, 0);
        assert_eq!(days.rows[0].stats.cost, Some(0.0));
    }

    #[test]
    fn group_chart_keeps_top_16_nulls_last_and_table_keeps_all() {
        let mut rows = Vec::new();
        for i in 0..20 {
            let mut item = row(&format!("g{i:02}"), i, 0, &format!("g{i:02}"));
            item.tokens = i as u64;
            item.output = i as u64;
            item.generation_ms = Some(1000.0);
            rows.push(item);
        }
        let mut missing = row("none", 100, 0, "none");
        missing.tokens = 0;
        rows.push(missing);
        let store = DetailStore::default();
        let id = store
            .insert(data_on("2026-11-06", "2026-11-06", rows, Vec::new()))
            .unwrap()
            .dataset_id;
        let view = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.sort = "value".into();
        });
        assert_eq!(view.chart.len(), 16);
        assert_eq!(view.chart[0].key, "g19");
        assert!(view
            .chart
            .iter()
            .all(|bin| bin.key != "none" && bin.key != "g00"));
        assert_eq!(view.total_entries, 21);
        assert_eq!(view.rows.len(), 21);
        assert_eq!(view.rows[0].key, "g19");
        assert!(view.rows.iter().any(|entry| entry.key == "g00"));
        assert_eq!(view.table_requests, 21);
        assert!(view.chart[0].drill.field == "model" && view.chart[0].drill.key == "g19");

        let speed = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.metric = "speed".into();
            req.sort = "value".into();
            req.descending = false;
            req.filters.models = vec!["g10".into(), "g19".into(), "none".into()];
        });
        let order: Vec<_> = speed.rows.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(order, ["g10", "g19", "none"], "升序时未知速度仍在最后");
        let desc: Vec<_> = speed.chart.iter().map(|bin| bin.key.as_str()).collect();
        assert_eq!(desc, ["g19", "g10", "none"]);
    }

    #[test]
    fn evicts_least_recently_used_and_rejects_stale_handles() {
        assert_eq!((MAX_DATASETS, MAX_ROWS), (32, 600_000));
        let store = DetailStore::default();
        let mut ids = Vec::new();
        for i in 0..32 {
            ids.push(
                store
                    .insert(data_on(
                        "2026-11-06",
                        "2026-11-06",
                        vec![row(&format!("r{i}"), i, 0, "m")],
                        Vec::new(),
                    ))
                    .unwrap()
                    .dataset_id,
            );
        }
        assert_eq!(ids[0] + 31, ids[31], "编号在进程内单调递增");
        store
            .view(ViewRequest {
                dataset_id: ids[0],
                empty: true,
                ..ViewRequest::default()
            })
            .unwrap();
        let newest = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![row("fresh", 1, 0, "m")],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(newest.dataset_id, ids[31] + 1, "新数据集接在已有编号之后");
        assert!(store
            .view(ViewRequest {
                dataset_id: ids[1],
                ..ViewRequest::default()
            })
            .unwrap_err()
            .contains("过期"));
        assert_eq!(
            store
                .view(ViewRequest {
                    dataset_id: ids[0],
                    ..ViewRequest::default()
                })
                .unwrap()
                .stats
                .calls,
            1
        );
        assert!(store
            .view(ViewRequest {
                dataset_id: 0,
                ..ViewRequest::default()
            })
            .is_err());
        let empty = store
            .view(ViewRequest {
                dataset_id: newest.dataset_id,
                empty: true,
                ..ViewRequest::default()
            })
            .unwrap();
        assert_eq!(empty.stats.cost, Some(0.0));
        assert!(empty.chart.is_empty() && empty.rows.is_empty());

        assert_eq!(
            victims(
                &[1, 2, 3, 4],
                |id| if id == 4 { 1 } else { 300_001 },
                4,
                900_004,
                4
            ),
            vec![1, 2]
        );
        assert_eq!(
            victims(&[5, 6], |_| 400_000, 2, 800_000, 5),
            vec![6],
            "刚插入的数据集自己不淘汰"
        );
        assert!(victims(&[9], |_| 700_000, 1, 700_000, 9).is_empty());
    }

    #[test]
    fn group_pages_keep_page_numbers_and_display_label_collation() {
        let store = DetailStore::default();
        let rows = (0..75)
            .map(|i| row(&format!("r{i}"), i, 0, &format!("m{i:02}")))
            .collect();
        let id = store
            .insert(data_on("2026-11-06", "2026-11-06", rows, vec![]))
            .unwrap()
            .dataset_id;
        let second = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.sort = "label".into();
            req.descending = false;
            req.page = 1;
        });
        assert_eq!(second.page, 1);
        assert_eq!(second.total_entries, 75);
        assert_eq!(second.rows.len(), 30);
        assert_eq!(second.rows[0].key, "m30");
        let last = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.page = 999;
        });
        assert_eq!(last.page, 2);
        assert_eq!(last.rows.len(), 15);

        let id = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![row("one", 1, 0, "重"), row("two", 2, 0, "阿")],
                vec![],
            ))
            .unwrap()
            .dataset_id;
        let sorted = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.sort = "label".into();
            req.descending = false;
            req.label_order = vec![vec!["阿".into()], vec!["重".into()]];
        });
        assert_eq!(
            sorted.rows[0].key, "阿",
            "按前端中文显示顺序排序，不使用 Unicode 码点顺序"
        );
        let equal = view_of(&store, id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
            req.sort = "label".into();
            req.label_order = vec![vec!["阿".into(), "重".into()]];
        });
        assert_eq!(equal.rows[0].key, "重", "同名/同排序权重保留原先顺序");
    }

    #[test]
    fn window_close_releases_sets_and_rejects_late_queries() {
        let store = DetailStore::default();
        let epoch = store.epoch().unwrap();
        let id = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![row("first", 1, 0, "m")],
                vec![],
            ))
            .unwrap()
            .dataset_id;
        store.clear().unwrap();
        assert_eq!(store.inner.lock().unwrap().rows, 0);
        assert!(store
            .view(ViewRequest {
                dataset_id: id,
                ..ViewRequest::default()
            })
            .is_err());
        assert!(store
            .insert_at_epoch(
                data_on(
                    "2026-11-06",
                    "2026-11-06",
                    vec![row("late", 1, 0, "m")],
                    vec![]
                ),
                epoch
            )
            .is_err());
        let fresh = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![row("fresh", 1, 0, "m")],
                vec![],
            ))
            .unwrap();
        assert!(fresh.dataset_id > id, "重新打开不能复用旧句柄");
        assert_eq!(
            store
                .view(ViewRequest {
                    dataset_id: fresh.dataset_id,
                    ..ViewRequest::default()
                })
                .unwrap()
                .rows[0]
                .key,
            "fresh"
        );
    }

    #[test]
    fn defaults_and_malformed_options_are_explicit() {
        let store = DetailStore::default();
        let id = store
            .insert(data_on(
                "2026-11-06",
                "2026-11-06",
                vec![row("early", 1, 0, "m"), row("late", 2, 1, "m")],
                Vec::new(),
            ))
            .unwrap()
            .dataset_id;
        let request: ViewRequest =
            serde_json::from_str(&format!(r#"{{"datasetId":{id}}}"#)).unwrap();
        let view = store.view(request).unwrap();
        assert_eq!(view.rows[0].key, "late");
        assert_eq!(view.page_size, 30);
        let oldest: ViewRequest =
            serde_json::from_str(&format!(r#"{{"datasetId":{id},"descending":false}}"#)).unwrap();
        assert_eq!(store.view(oldest).unwrap().rows[0].key, "early");

        for (field, value, needle) in [
            ("view", "nope", "视图"),
            ("grain", "week", "粒度"),
            ("group", "session", "分组"),
            ("metric", "money", "指标"),
            ("mode", "pivot", "表格"),
            ("sort", "foo", "排序"),
        ] {
            let mut request = ViewRequest {
                dataset_id: id,
                ..ViewRequest::default()
            };
            match field {
                "view" => request.view = value.into(),
                "grain" => request.grain = value.into(),
                "group" => request.group = value.into(),
                "metric" => request.metric = value.into(),
                "mode" => request.mode = value.into(),
                _ => request.sort = value.into(),
            }
            let err = store.view(request).unwrap_err();
            assert!(err.contains(needle), "{err}");
        }
        let bad_drill = ViewRequest {
            dataset_id: id,
            drill: Some(Drill {
                field: "session".into(),
                ..Drill::default()
            }),
            ..ViewRequest::default()
        };
        assert!(store.view(bad_drill).unwrap_err().contains("下钻"));
    }

    #[test]
    fn fifty_thousand_rows_stay_in_backend_and_benchmark_is_measured() {
        const N: usize = 50_000;
        let mut rows = Vec::with_capacity(N);
        let mut costs = Vec::with_capacity(N);
        for i in 0..N {
            let mut item = row(
                &format!("r{i:05}"),
                i as i64,
                i % 24,
                &format!("m{}", i % 10),
            );
            item.input = 100;
            item.output = 50;
            item.cache_read = 200;
            item.tokens = 350;
            item.generation_ms = Some(1000.0);
            costs.push(cost(&[item.id.as_str()], 0.0, 1));
            rows.push(item);
        }
        let store = DetailStore::default();
        let data = data_on("2026-11-06", "2026-11-06", rows, costs);
        let full_bytes = serde_json::to_vec(&data).unwrap().len();
        let overview = store.insert(data).unwrap();
        assert_eq!(overview.total, N as u64);
        assert_eq!(overview.facets.models.len(), 10);
        let overview_bytes = serde_json::to_vec(&overview).unwrap();
        assert!(!std::str::from_utf8(&overview_bytes)
            .unwrap()
            .contains("r00000"));
        assert!(
            overview_bytes.len() < 256 * 1024,
            "overview {}",
            overview_bytes.len()
        );

        let mut samples = [0.0; 3];
        let mut last = None;
        for sample in &mut samples {
            let started = std::time::Instant::now();
            let view = store
                .view(ViewRequest {
                    dataset_id: overview.dataset_id,
                    ..ViewRequest::default()
                })
                .unwrap();
            *sample = started.elapsed().as_secs_f64() * 1000.0;
            last = Some(view);
        }
        let view = last.unwrap();
        assert_eq!(view.rows.len(), 30);
        assert_eq!(view.total_entries, N);
        assert_eq!(view.table_requests, N);
        assert_eq!(view.page_size, 30);
        assert_eq!(view.stats.calls, N as u64);
        assert_eq!(view.stats.priced, N as u64);
        assert_eq!(view.stats.cost, Some(0.0));
        assert!(view.chart.len() <= 160);
        assert_eq!(view.rows[0].key, "r49999");
        let bytes = serde_json::to_vec(&view).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("r49999"));
        assert!(!text.contains("r00000"), "首页不能带上全部请求");
        assert!(bytes.len() < 256 * 1024, "view {}", bytes.len());
        assert!(overview_bytes.len() + bytes.len() < 256 * 1024);
        assert!(samples.iter().all(|ms| ms.is_finite()));
        samples.sort_by(f64::total_cmp);
        eprintln!("DETAIL_VIEW_PERF rows={N} full_bytes={full_bytes} overview_bytes={} view_bytes={} samples_ms={samples:?} median_ms={:.3}", overview_bytes.len(), bytes.len(), samples[1]);

        let grouped = view_of(&store, overview.dataset_id, |req| {
            req.view = "groups".into();
            req.mode = "groups".into();
        });
        assert_eq!(grouped.chart.len(), 10);
        assert_eq!(grouped.rows.len(), 10);
        assert!(serde_json::to_vec(&grouped).unwrap().len() < 256 * 1024);
    }
}
