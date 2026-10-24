//! 独立详情查询：本地账本统计配对网关逐请求定价，不占用实时游标。
use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};
use crate::{live, usage};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailContext {
    pub metric: String,
    pub date: String,
    #[serde(default)] pub source: String,
    #[serde(default)] pub session: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailData {
    pub start: String,
    pub end: String,
    pub rows: Vec<usage::UsageDetailRow>,
    pub costs: Vec<CostGroup>,
    pub cycles: Vec<Cycle>,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostGroup {
    pub session: String,
    pub row_ids: Vec<String>,
    pub cost: f64,
    pub priced: u64,
    pub unpriced: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cycle {
    pub provider: String,
    pub account: String,
    pub window: String,
    pub start: i64,
    pub end: i64,
    pub observed_at: i64,
    pub remaining: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    at: String,
    left: Option<f64>,
    start: Option<String>,
    resets_at: Option<String>,
}

pub fn parse_range(start: &str, end: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
        .filter(|date| date.to_string() == s).ok_or_else(|| "日期格式应为 YYYY-MM-DD".to_string());
    let a = parse(start)?;
    let b = parse(end)?;
    if a > b { return Err("开始日期不能晚于结束日期".into()); }
    if b > Local::now().date_naive() { return Err("不能查询未来日期".into()); }
    if (b-a).num_days() > 365 { return Err("一次最多查询 366 天，请缩小日期范围".into()); }
    Ok((a,b))
}

/// 与前端一致的筛选条件；空集合表示不限制。按筛选缩小逐请求费用的匹配范围。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailFilters {
    #[serde(default)] pub source_keys: Vec<String>,
    #[serde(default)] pub models: Vec<String>,
    #[serde(default)] pub providers: Vec<String>,
    #[serde(default)] pub account_keys: Vec<String>,
    #[serde(default)] pub efforts: Vec<String>,
    #[serde(default)] pub session: String,
}

impl DetailFilters {
    fn matches(&self, row: &usage::UsageDetailRow, account_key: &str) -> bool {
        (self.source_keys.is_empty() || self.source_keys.iter().any(|k| *k == row.source_key))
            && (self.models.is_empty() || self.models.iter().any(|m| *m == row.model))
            && (self.providers.is_empty() || self.providers.iter().any(|p| *p == row.provider))
            && (self.account_keys.is_empty() || self.account_keys.iter().any(|k| *k == account_key))
            && (self.efforts.is_empty() || self.efforts.iter().any(|e| *e == row.effort))
            && (self.session.is_empty() || self.session == row.session)
    }
}

pub fn query(path: &Path, host: &str, start: &str, end: &str, filters: &DetailFilters) -> Result<DetailData, String> {
    let (a,b) = parse_range(start,end)?;
    let mut rows = usage::scan_detail_rows(path,a,b)?;
    if rows.len() > 200_000 { return Err("所选范围超过 20 万条请求，请缩小日期范围".into()); }
    // 先按筛选缩小行集，费用匹配随之收窄；网关分页从最新往前，已筛选的近期请求可以提前结束扫描。
    rows.retain(|row| {
        let account_key = serde_json::to_string(&[&row.provider, &row.account]).unwrap_or_default();
        filters.matches(row, &account_key)
    });
    let mut data = DetailData { start:start.into(),end:end.into(),rows,costs:vec![],cycles:vec![],warnings:vec![] };
    let period = format!("{a}..{b}");
    if !data.rows.is_empty() {
        match live::gateway_url(host) {
            Ok(base) => {
                let scope = pricing_scope(&data.rows);
                let client = live::client(Duration::from_secs(5));
                let (costs, warnings) = request_costs(&data.rows, &period, |offset| {
                    live::read_json::<PricePage>(&client, &format!("{base}/v1/magpie/usage/requests?period={period}&via=through&limit=500&offset={offset}{scope}"))
                });
                data.costs = costs;
                data.warnings.extend(warnings);
            }
            Err(_) => data.warnings.push("网关地址无效，逐请求费用未读取".into()),
        }
    }
    match read_cycles(&path.with_file_name("quota-history.json")) {
        Ok((cycles, missing)) => {
            data.cycles = cycles;
            if missing { data.warnings.push("部分额度历史未记录周期边界，未推算为重置周期".into()); }
        }
        Err(e) => data.warnings.push(e),
    }
    Ok(data)
}

/// 仅当全部待匹配行共有同一非空字段时下推过滤；多账号仍读取并核对完整集合。
fn pricing_scope(rows: &[usage::UsageDetailRow]) -> String {
    let Some(first) = rows.first() else { return String::new(); };
    let mut query = String::new();
    for (name, value, common) in [
        ("provider", first.provider.as_str(), rows.iter().all(|r| r.provider == first.provider)),
        ("account", first.account.as_str(), rows.iter().all(|r| r.account == first.account)),
    ] {
        if value.is_empty() || !common { continue; }
        query.push('&'); query.push_str(name); query.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) { query.push(byte as char); }
            else { query.push_str(&format!("%{byte:02X}")); }
        }
    }
    query
}

/// 只反序列化统计/身份字段，不接收正文、凭据或错误原文。
#[derive(Deserialize)]
struct PricePage {
    period: String,
    offset: usize,
    total: usize,
    rows: Vec<PricedRequest>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct PricedRequest {
    t: String,
    route_id: Option<i64>,
    status: Option<u16>,
    #[serde(default)] rid: String,
    #[serde(default)] session: String,
    #[serde(default)] provider: String,
    #[serde(default, rename="providerAccount")] account: String,
    #[serde(default)] kind: String,
    #[serde(default)] source: String,
    #[serde(default, rename="in")] input: u64,
    #[serde(default, rename="out")] output: u64,
    #[serde(default)] cache_read: u64,
    priced: bool,
    cost: Option<f64>,
}

impl PricedRequest {
    fn identity(&self) -> Option<String> {
        let time = stamp(&self.t)?;
        Some(format!("{}|{time}|{}|{}|{}", self.route_id.map(|id|id.to_string()).unwrap_or_default(),
            self.account, self.status.map(|s|s.to_string()).unwrap_or_default(), self.rid))
    }
    fn matches(&self, row: &usage::UsageDetailRow) -> bool {
        self.kind != "routing" && self.source.is_empty() && self.session == row.session
            && self.input == row.input && self.output == row.output && self.cache_read == row.cache_read
    }
}

/// 分页期间可能新增请求：按精确身份去重，不把重叠页重复收费。
/// 无法配对、定价冲突或未返回的请求保持未知；不借用会话/模型合计补齐。
fn request_costs<F>(rows: &[usage::UsageDetailRow], period: &str, mut fetch: F) -> (Vec<CostGroup>, Vec<String>)
where F: FnMut(usize) -> Result<PricePage, String> {
    let wanted: HashMap<_,_> = rows.iter().map(|r|(r.id.as_str(),r)).collect();
    let mut found: HashMap<String, Option<PricedRequest>> = HashMap::new();
    let mut warnings = Vec::new();
    let started = Instant::now();
    let mut offset = 0;
    let mut scan_limit = None;
    loop {
        if started.elapsed() > Duration::from_secs(90) {
            warnings.push("逐请求费用读取超时，已匹配费用保留，其余未知".into()); break;
        }
        let page = match fetch(offset) {
            Ok(page) => page,
            Err(_) => { warnings.push("逐请求费用接口读取失败，已匹配费用保留，其余未知".into()); break; }
        };
        if page.period != period || page.offset != offset || page.rows.len() > 500 {
            warnings.push("逐请求费用分页范围不匹配，该页未采用".into()); break;
        }
        // 有界扫描；多留一页容纳读取过程中前端新增的请求。
        let limit = *scan_limit.get_or_insert(page.total.saturating_add(500).min(250_000));
        let count = page.rows.len();
        for priced in page.rows {
            let Some(id) = priced.identity() else { continue; };
            let Some(row) = wanted.get(id.as_str()) else { continue; };
            if !priced.matches(row) { found.insert(id, None); continue; }
            match found.entry(id) {
                std::collections::hash_map::Entry::Vacant(e) => { e.insert(Some(priced)); }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if e.get().as_ref() != Some(&priced) { e.insert(None); }
                }
            }
        }
        if found.len() == wanted.len() || count == 0 || offset + count >= page.total { break; }
        offset += count;
        if offset >= limit {
            warnings.push("逐请求费用达到本次读取上限，其余请求保持未知".into()); break;
        }
    }
    let mut matched = 0;
    let costs = rows.iter().filter_map(|row| {
        let priced = found.get(&row.id)?.as_ref()?;
        matched += 1;
        let cost = priced.cost.filter(|cost|cost.is_finite() && *cost >= 0.0 && priced.priced)?;
        Some(CostGroup { session: row.session.clone(), row_ids: vec![row.id.clone()], cost, priced:1, unpriced:0 })
    }).collect();
    if matched < rows.len() { warnings.push(format!("逐请求费用匹配 {matched}/{} 次；未匹配或冲突请求费用未知",rows.len())); }
    (costs, warnings)
}

fn stamp(value: &str) -> Option<i64> { DateTime::parse_from_rfc3339(value).ok().map(|v|v.timestamp_millis()) }

fn read_cycles(path: &Path) -> Result<(Vec<Cycle>,bool),String> {
    let meta = std::fs::metadata(path).map_err(|_| "未找到额度历史，暂时无法列出重置周期".to_string())?;
    if meta.len() > 32*1024*1024 { return Err("额度历史过大，未加载周期".into()); }
    let bytes = std::fs::read(path).map_err(|_| "无法读取额度历史".to_string())?;
    parse_cycles(&bytes)
}

fn parse_cycles(bytes: &[u8]) -> Result<(Vec<Cycle>,bool),String> {
    let history: BTreeMap<String,BTreeMap<String,Vec<Observation>>> = serde_json::from_slice(bytes)
        .map_err(|_| "额度历史格式无法识别".to_string())?;
    let mut cycles: BTreeMap<(String,String,String,i64,i64),Cycle> = BTreeMap::new();
    let mut missing = false;
    for (identity,windows) in history {
        let Some((provider,account)) = identity.split_once('|') else { missing=true; continue; };
        for (window, observations) in windows {
            let mut observations = observations;
            observations.sort_by_key(|o| stamp(&o.at));
            let mut windows: Vec<Cycle> = Vec::new();
            for o in observations {
                let times = o.start.as_deref().and_then(stamp).zip(o.resets_at.as_deref().and_then(stamp)).zip(stamp(&o.at));
                let Some(((start,end),at)) = times else { missing=true; continue; };
                if start >= end || at < start || at > end { missing=true; continue; }
                let remaining = o.left.filter(|v|v.is_finite() && *v>=0.0 && *v<=100.0);
                if let Some(previous) = windows.last_mut() {
                    let same = previous.start == start && previous.end == end;
                    let overlap = start < previous.end && end > previous.start;
                    let replenished = previous.remaining.zip(remaining).is_some_and(|(before,after)| after > before + 1.0);
                    let nearby = (start - previous.start).abs() <= 10 * 60 * 1000
                        && (end - previous.end).abs() <= 10 * 60 * 1000;
                    // 相近边界且没有额度回升时视为时间校准；额度回升仍保留为独立重置。
                    if same || overlap && !replenished && nearby {
                        previous.start = start;
                        previous.end = end;
                        previous.observed_at = at;
                        previous.remaining = remaining;
                        continue;
                    }
                    // 迟到的旧周期读数不能再创建一个重叠周期。
                    if start < previous.start { continue; }
                }
                windows.push(Cycle { provider:provider.into(),account:account.into(),window:window.clone(),
                    start,end,observed_at:at,remaining });
            }
            for index in 1..windows.len() {
                if windows[index-1].end > windows[index].start {
                    windows[index-1].end = windows[index].start;
                }
            }
            for cycle in windows {
                cycles.insert((cycle.provider.clone(),cycle.account.clone(),cycle.window.clone(),cycle.start,cycle.end),cycle);
            }
        }
    }
    let mut result: Vec<_> = cycles.into_values().collect();
    result.sort_by(|a,b| b.start.cmp(&a.start));
    Ok((result,missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detail_filters_narrow_rows_before_cost_matching() {
        let row=|model:&str,provider:&str,account:&str,effort:&str| usage::UsageDetailRow{
            id:String::new(),timestamp:0,date:String::new(),hour:0,session:"s".into(),agent:String::new(),
            caller_key_id:String::new(),caller_key_name:String::new(),source_key:r#"["Claude Code","key-a"]"#.into(),
            provider:provider.into(),account:account.into(),model:model.into(),effort:effort.into(),
            outcome:"success".into(),input:0,output:0,cache_read:0,tokens:0,generation_ms:None};
        let account_key=serde_json::to_string(&["vendor","account"]).unwrap();
        let key_for=|p:&str,a:&str| serde_json::to_string(&[p,a]).unwrap();
        let decoded: DetailFilters = serde_json::from_value(serde_json::json!({
            "accountKeys":[account_key.clone()],"sourceKeys":[r#"["Claude Code","key-a"]"#]
        })).unwrap();
        assert!(!decoded.matches(&row("model-a","vendor","other","high"),&key_for("vendor","other")));
        assert_eq!(decoded.source_keys.len(),1);
        let filters=DetailFilters{models:vec!["model-a".into()],account_keys:vec![account_key.clone()],..Default::default()};
        assert!(filters.matches(&row("model-a","vendor","account","high"),&account_key));
        assert!(!filters.matches(&row("model-b","vendor","account","high"),&account_key));
        assert!(!filters.matches(&row("model-a","vendor","other","high"),&key_for("vendor","other")));
        assert!(DetailFilters::default().matches(&row("x","y","z","w"),&account_key));
        let first = row("model-a","vendor","账号+a&b@example.com","high");
        assert_eq!(pricing_scope(&[first.clone()]), "&provider=vendor&account=%E8%B4%A6%E5%8F%B7%2Ba%26b%40example.com");
        assert_eq!(pricing_scope(&[first.clone(), row("b","vendor","other","high")]), "&provider=vendor");
        assert_eq!(pricing_scope(&[first, row("b","other","different","high")]), "");
        assert_eq!(pricing_scope(&[]), "");
    }

    #[test]
    fn validates_range_before_scanning() {
        assert!(parse_range("2026-1-01","2026-01-02").is_err());
        assert!(parse_range("2026-01-03","2026-01-02").is_err());
        assert!(parse_range("2020-01-01","2022-01-01").is_err());
        assert!(parse_range("2025-01-01","2025-01-02").is_ok());
    }
    #[test]
    fn detail_rows_dedupe_and_cost_coverage_remain_exact() {
        let path=std::env::temp_dir().join(format!("hud-detail-test-{}.jsonl",std::process::id()));
        let line = serde_json::json!({"route_id":1,"t":"2025-01-02T10:00:00+08:00","session":"session-a",
            "agent":"claude-code","callerKeyId":"key-a","callerKeyName":"old","model":"model-a",
            "provider":"vendor","providerAccount":"account","effort":"high","status":200,
            "in":100,"out":200,"cache_read":300,"ms":3000,"ttft_ms":1000});
        let mut later=line.clone(); later["route_id"]=2.into();later["t"]="2025-01-03T10:00:00+08:00".into();later["callerKeyName"]="new".into();
        let mut routing=line.clone();routing["kind"]="routing".into();
        std::fs::write(&path,format!("{line}\n{line}\n{routing}\n{later}\n{{\"unfinished\":" )).unwrap();
        let rows=usage::scan_detail_rows(&path,NaiveDate::from_ymd_opt(2025,1,2).unwrap(),NaiveDate::from_ymd_opt(2025,1,2).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(rows.len(),1);
        assert_eq!(rows[0].tokens,600);
        assert_eq!(rows[0].generation_ms,Some(2000.0));
        assert_eq!(rows[0].caller_key_name,"new");
        assert_eq!(rows[0].source_key,r#"["Claude Code","key-a"]"#);
        let priced: PricedRequest=serde_json::from_value(serde_json::json!({"route_id":1,"t":"2025-01-02T02:00:00Z",
            "session":"session-a","provider":"vendor","providerAccount":"account","status":200,
            "in":100,"out":200,"cache_read":300,"cost":0.4,"priced":true})).unwrap();
        let period="2025-01-02..2025-01-02";
        let (costs,warnings)=request_costs(&rows,period,|offset|Ok(PricePage{period:period.into(),offset,total:1,rows:vec![priced.clone()]}));
        assert!(warnings.is_empty());assert_eq!(costs.len(),1);assert_eq!(costs[0].cost,0.4);
        // 同页重复不重复计费；同身份不同金额拒绝采用。
        let (costs,_)=request_costs(&rows,period,|offset|Ok(PricePage{period:period.into(),offset,total:2,rows:vec![priced.clone(),priced.clone()]}));
        assert_eq!(costs.len(),1);
        let mut conflicting=priced.clone();conflicting.cost=Some(1.0);
        let (costs,warnings)=request_costs(&rows,period,|offset|Ok(PricePage{period:period.into(),offset,total:2,rows:vec![priced.clone(),conflicting.clone()]}));
        assert!(costs.is_empty());assert!(!warnings.is_empty());
        let mut unpriced=priced.clone();unpriced.priced=false;
        let (costs,_)=request_costs(&rows,period,|offset|Ok(PricePage{period:period.into(),offset,total:1,rows:vec![unpriced.clone()]}));
        assert!(costs.is_empty());
        let mut zero=priced.clone();zero.cost=Some(0.0);
        let (costs,_)=request_costs(&rows,period,|offset|Ok(PricePage{period:period.into(),offset,total:1,rows:vec![zero.clone()]}));
        assert_eq!(costs[0].cost,0.0);
        let (costs,_)=request_costs(&rows,period,|offset|Ok(PricePage{period:"wrong".into(),offset,total:1,rows:vec![priced.clone()]}));
        assert!(costs.is_empty());
        let mut second=priced.clone();second.route_id=Some(2);
        let mut second_row=rows[0].clone();second_row.id=second.identity().unwrap();
        let both=vec![rows[0].clone(),second_row];
        let mut reads=0;
        let (costs,warnings)=request_costs(&both,period,|offset|{
            reads+=1;
            Ok(PricePage{period:period.into(),offset,total:3,rows:if offset==0 {vec![priced.clone()]} else {vec![priced.clone(),second.clone()]}})
        });
        assert_eq!(reads,2);assert_eq!(costs.len(),2);assert!(warnings.is_empty());
        let (costs,warnings)=request_costs(&both,period,|offset|{
            if offset>0 {return Err("simulated disconnect".into());}
            Ok(PricePage{period:period.into(),offset,total:2,rows:vec![priced.clone()]})
        });
        assert_eq!(costs.len(),1);assert!(!warnings.is_empty());
    }

    #[test]
    fn cycles_merge_boundary_corrections_and_close_early_resets() {
        let bytes = br#"{"vendor|account":{"7 days":[
          {"at":"2025-01-01T00:00:00Z","left":100,"start":"2025-01-01T00:00:00Z","resetsAt":"2025-01-08T00:00:00Z"},
          {"at":"2025-01-01T01:00:00Z","left":100,"start":"2025-01-01T00:04:00Z","resetsAt":"2025-01-08T00:04:00Z"},
          {"at":"2025-01-02T00:00:00Z","left":0,"start":"2025-01-01T00:04:01Z","resetsAt":"2025-01-08T00:04:01Z"},
          {"at":"2025-01-02T00:01:00Z","left":100,"start":"2025-01-02T00:01:00Z","resetsAt":"2025-01-09T00:01:00Z"},
          {"at":"2025-01-02T01:00:00Z","left":90,"start":"2025-01-02T00:01:00Z","resetsAt":"2025-01-09T00:01:00Z"}
        ]}}"#;
        let (cycles,missing)=parse_cycles(bytes).unwrap();
        assert!(!missing);
        assert_eq!(cycles.len(),2);
        assert_eq!(cycles[1].start,stamp("2025-01-01T00:04:01Z").unwrap());
        assert_eq!(cycles[1].end,cycles[0].start);
        assert_eq!(cycles[1].remaining,Some(0.0));
        assert_eq!(cycles[0].remaining,Some(90.0));
    }

    #[test]
    fn cycles_keep_nearby_resets_when_allowance_really_increases() {
        let bytes = br#"{"vendor|account":{"7 days":[
          {"at":"2025-01-01T00:03:00Z","left":0,"start":"2025-01-01T00:00:00Z","resetsAt":"2025-01-08T00:00:00Z"},
          {"at":"2025-01-01T00:04:00Z","left":100,"start":"2025-01-01T00:04:00Z","resetsAt":"2025-01-08T00:04:00Z"}
        ]}}"#;
        let (cycles,missing)=parse_cycles(bytes).unwrap();
        assert!(!missing);
        assert_eq!(cycles.len(),2);
        assert_eq!(cycles[1].end,cycles[0].start);
    }

    #[test]
    fn cycles_use_explicit_boundaries_and_latest_observation() {
        let bytes = br#"{"vendor|account":{"Weekly":[
          {"at":"2025-01-02T00:00:00Z","left":70,"start":"2025-01-01T00:00:00Z","resetsAt":"2025-01-08T00:00:00Z"},
          {"at":"2025-01-03T00:00:00Z","left":40,"start":"2025-01-01T00:00:00Z","resetsAt":"2025-01-08T00:00:00Z"},
          {"at":"2025-01-04T00:00:00Z","left":20}
        ]}}"#;
        let (cycles,missing)=parse_cycles(bytes).unwrap();
        assert!(missing);
        assert_eq!(cycles.len(),1);
        assert_eq!(cycles[0].remaining,Some(40.0));
    }
}
