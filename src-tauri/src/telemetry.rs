//! 只读 Magpie 额度与定价用量；账号精确关联，额度、余额、美元费用各保留自身单位。

use crate::usage::RouteSnapshot;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub name: String,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub resets_at: Option<String>,
    #[serde(default)] pub unlimited: bool,
    pub display: Option<String>,
    pub amount: Option<f64>,
    pub limit: Option<f64>,
    pub unit: Option<String>,
    pub family: Option<String>,
    pub pool: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    pub provider: String,
    pub name: String,
    #[serde(default)] pub user: String,
    #[serde(default)] pub kind: String,
    #[serde(default)] pub plan: String,
    #[serde(default)] pub windows: Vec<QuotaWindow>,
    pub balance: Option<String>,
    pub error: Option<String>,
    pub read_at: Option<String>,
    pub as_of: Option<String>,
}

#[derive(Default, Deserialize)]
pub struct Quotas {
    #[serde(default)] pub data: Vec<Quota>,
}

/// 用量页包含内部路由调用；保留其费用口径，不从日志拼凑或推算模型单价。
#[derive(Default, Deserialize)]
pub struct Usage {
    pub since: String,
    pub calls: u64,
    pub cost: f64,
    #[serde(default)] pub unpriced: u64,
    #[serde(default)] pub sessions: Vec<SessionCost>,
}

#[derive(Default, Deserialize)]
pub struct SessionCost {
    pub id: String,
    pub calls: u64,
    pub cost: f64,
    #[serde(default)] pub unpriced: u64,
}

/// 读取和使用时间都由网关返回；失联时保留最后读数，并明确标记旧数据。
#[derive(Default)]
pub struct Telemetry {
    pub quotas: Vec<Quota>,
    pub quota_error: Option<String>,
    pub usage: Option<Usage>,
    pub cost_error: Option<String>,
}

impl Telemetry {
    /// 网关成功返回也可能有单个账号的供应商错误；留住该账号上次有效窗口。
    pub fn update_quotas(&mut self, mut next: Vec<Quota>) {
        for quota in &mut next {
            if quota.error.is_none() { continue; }
            let mut previous = self.quotas.iter().filter(|old| old.provider == quota.provider
                && old.user == quota.user && old.kind == quota.kind && old.name == quota.name);
            if let Some(old) = previous.next() {
                if previous.next().is_none() {
                    let error = quota.error.clone();
                    *quota = old.clone();
                    quota.error = error;
                }
            }
        }
        self.quotas = next;
        self.quota_error = None;
    }

    pub fn apply(&self, view: &mut RouteSnapshot) {
        view.quotas = self.quotas.clone();
        view.quota_error = self.quota_error.clone();
        view.cost_error = self.cost_error.clone();
        let Some(usage) = &self.usage else { return };
        // 跨日时不能把昨天缓存的金额盖到今天；费用由下一次独立轮询补齐。
        let day = chrono::DateTime::parse_from_rfc3339(&usage.since).ok()
            .map(|time| time.with_timezone(&chrono::Local).date_naive().to_string());
        if day.as_deref() != Some(&view.date) {
            view.cost_error = Some("等待 Magpie 当日费用刷新".into());
            return;
        }
        fill_cost(&mut view.daily, usage.calls, usage.cost, usage.unpriced);
        for session in &mut view.sessions {
            let mut matches = usage.sessions.iter().filter(|cost| cost.id == session.id);
            if let Some(cost) = matches.next() {
                if matches.next().is_none() {
                    fill_cost(&mut session.daily, cost.calls, cost.cost, cost.unpriced);
                }
            }
        }
    }
}

fn fill_cost(stats: &mut crate::usage::DailyStats, calls: u64, cost: f64, unpriced: u64) {
    if cost.is_finite() && cost >= 0.0 && unpriced <= calls {
        // 所有请求未定价时 0 只是缺价格的结果，不能显示为真实零费用。
        stats.cost = (calls == 0 || calls > unpriced).then_some(cost);
        stats.priced_requests = calls - unpriced;
        stats.unpriced_requests = unpriced;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageLedger;

    #[test]
    fn cost_keeps_gateway_scope_and_cannot_leak_across_days() {
        let now = chrono::Local::now();
        let mut telemetry = Telemetry { usage: Some(Usage {
            since: now.to_rfc3339(), calls: 5, cost: 1.25, unpriced: 2, sessions: vec![],
        }), ..Default::default() };
        let mut view = UsageLedger::new(now.date_naive()).snapshot("h", "ready", None);
        telemetry.apply(&mut view);
        assert_eq!(view.daily.cost, Some(1.25));
        assert_eq!(view.daily.priced_requests, 3);
        assert_eq!(view.daily.requests, 0); // 费用覆盖不会改写完成账本
        view.date = (now.date_naive() + chrono::Duration::days(1)).to_string();
        view.daily.cost = None;
        telemetry.apply(&mut view);
        assert_eq!(view.daily.cost, None);
        assert!(view.cost_error.is_some());
        telemetry.usage.as_mut().unwrap().unpriced = 5;
        view.date = now.date_naive().to_string();
        telemetry.apply(&mut view);
        assert_eq!(view.daily.cost, None);
    }

    #[test]
    fn per_account_error_retains_only_exact_previous_reading() {
        let quota = Quota { provider: "codex".into(), name: "codex".into(), user: "KeyA".into(),
            windows: vec![QuotaWindow { name: "5 hours".into(), remaining: Some(62.0), ..Default::default() }], ..Default::default() };
        let mut telemetry = Telemetry { quotas: vec![quota.clone()], ..Default::default() };
        let failed = Quota { windows: vec![], error: Some("vendor unavailable".into()), ..quota.clone() };
        telemetry.update_quotas(vec![failed.clone()]);
        assert_eq!(telemetry.quotas[0].windows[0].remaining, Some(62.0));
        assert!(telemetry.quotas[0].error.is_some());
        telemetry.update_quotas(vec![Quota { user: "keya".into(), ..failed }]);
        assert!(telemetry.quotas[0].windows.is_empty());
    }

    #[test]
    fn quota_missing_percent_is_unknown_instead_of_zero() {
        let quotas: Quotas = serde_json::from_str(r#"{"object":"list","data":[{"provider":"codex","name":"codex","user":"test@example.com","kind":"subscription","windows":[{"name":"5 hours"}]}]}"#).unwrap();
        assert_eq!(quotas.data[0].windows[0].remaining, None);
    }
}
