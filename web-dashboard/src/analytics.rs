use crate::{
    AnalyticsAccountView, AnalyticsMetricView, AnalyticsModelView, AnalyticsPointView,
    AnalyticsProviderView, AnalyticsSummaryView, Dashboard, model,
};
use serde::Deserialize;
use wasm_bindgen::JsValue;

#[derive(Default, Deserialize)]
#[serde(default)]
struct SummaryDto {
    label: String,
    value: String,
    note: String,
}
impl From<SummaryDto> for AnalyticsSummaryView {
    fn from(v: SummaryDto) -> Self {
        Self {
            label: v.label.into(),
            value: v.value.into(),
            note: v.note.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ProviderDto {
    id: String,
    label: String,
    accounts: String,
    availability: String,
    sample: String,
}
impl From<ProviderDto> for AnalyticsProviderView {
    fn from(v: ProviderDto) -> Self {
        Self {
            id: v.id.into(),
            label: v.label.into(),
            accounts: v.accounts.into(),
            availability: v.availability.into(),
            sample: v.sample.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct MetricDto {
    key: String,
    label: String,
    amount: String,
    reset: String,
    expiration: String,
    note: String,
}
impl From<MetricDto> for AnalyticsMetricView {
    fn from(v: MetricDto) -> Self {
        Self {
            key: v.key.into(),
            label: v.label.into(),
            amount: v.amount.into(),
            reset: v.reset.into(),
            expiration: v.expiration.into(),
            note: v.note.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct PointDto {
    x: f32,
    percent: f32,
    label: String,
}
impl From<PointDto> for AnalyticsPointView {
    fn from(v: PointDto) -> Self {
        Self {
            x: v.x,
            percent: v.percent,
            label: v.label.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct AccountDto {
    id: String,
    label: String,
    provider: String,
    status: String,
    platform: String,
    source: String,
    plan: String,
    active: bool,
    samples: String,
}
impl From<AccountDto> for AnalyticsAccountView {
    fn from(v: AccountDto) -> Self {
        Self {
            id: v.id.into(),
            label: v.label.into(),
            provider: v.provider.into(),
            status: v.status.into(),
            platform: v.platform.into(),
            source: v.source.into(),
            plan: v.plan.into(),
            active: v.active,
            samples: v.samples.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ModelDto {
    label: String,
    provider: String,
    input: String,
    output: String,
    cache: String,
    cost: String,
}
impl From<ModelDto> for AnalyticsModelView {
    fn from(v: ModelDto) -> Self {
        Self {
            label: v.label.into(),
            provider: v.provider.into(),
            input: v.input.into(),
            output: v.output.into(),
            cache: v.cache.into(),
            cost: v.cost.into(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AnalyticsDto {
    loading: bool,
    error: String,
    updated: String,
    history_note: String,
    range_value: String,
    provider_value: String,
    account_value: String,
    metric_value: String,
    provider_options: Vec<String>,
    account_options: Vec<String>,
    metric_options: Vec<String>,
    summaries: Vec<SummaryDto>,
    providers: Vec<ProviderDto>,
    accounts: Vec<AccountDto>,
    metrics: Vec<MetricDto>,
    points: Vec<PointDto>,
    selected_account_note: String,
    chart_title: String,
    chart_note: String,
    chart_start: String,
    chart_end: String,
    chart_top: String,
    chart_middle: String,
    chart_bottom: String,
    chart_has_points: bool,
    metric_percent: bool,
    activity_title: String,
    activity_note: String,
    activity_chart_top: String,
    activity_chart_middle: String,
    activity_chart_bottom: String,
    activity_chart_start: String,
    activity_chart_end: String,
    activity_has_data: bool,
    activity_summaries: Vec<SummaryDto>,
    activity_points: Vec<PointDto>,
    activity_models: Vec<ModelDto>,
    activity_providers: Vec<MetricDto>,
}
pub(crate) fn set_analytics(ui: &Dashboard, json: &str) -> Result<(), JsValue> {
    let v: AnalyticsDto = serde_json::from_str(json)
        .map_err(|_| JsValue::from_str("Invalid account analytics view"))?;
    ui.set_analytics_loading(v.loading);
    ui.set_analytics_error(v.error.into());
    ui.set_analytics_updated(v.updated.into());
    ui.set_analytics_history_note(v.history_note.into());
    ui.set_analytics_provider_options(model(
        v.provider_options.into_iter().map(Into::into).collect(),
    ));
    ui.set_analytics_account_options(model(
        v.account_options.into_iter().map(Into::into).collect(),
    ));
    ui.set_analytics_metric_options(model(
        v.metric_options.into_iter().map(Into::into).collect(),
    ));
    // ComboBox resolves the selection against its model on every model change.
    // Supply the choices before selecting their labels so refreshed models do
    // not clear a provider, account or metric selected by the user.
    ui.set_analytics_range_value(v.range_value.into());
    ui.set_analytics_provider_value(v.provider_value.into());
    ui.set_analytics_account_value(v.account_value.into());
    ui.set_analytics_metric_value(v.metric_value.into());
    ui.set_analytics_summary(model(v.summaries.into_iter().map(Into::into).collect()));
    ui.set_analytics_providers(model(v.providers.into_iter().map(Into::into).collect()));
    ui.set_analytics_accounts(model(v.accounts.into_iter().map(Into::into).collect()));
    ui.set_analytics_metrics(model(v.metrics.into_iter().map(Into::into).collect()));
    ui.set_analytics_points(model(v.points.into_iter().map(Into::into).collect()));
    ui.set_analytics_selected_account_note(v.selected_account_note.into());
    ui.set_analytics_chart_title(v.chart_title.into());
    ui.set_analytics_chart_note(v.chart_note.into());
    ui.set_analytics_chart_start(v.chart_start.into());
    ui.set_analytics_chart_end(v.chart_end.into());
    ui.set_analytics_chart_top(v.chart_top.into());
    ui.set_analytics_chart_middle(v.chart_middle.into());
    ui.set_analytics_chart_bottom(v.chart_bottom.into());
    ui.set_analytics_chart_has_points(v.chart_has_points);
    ui.set_analytics_metric_percent(v.metric_percent);
    ui.set_analytics_activity_title(v.activity_title.into());
    ui.set_analytics_activity_note(v.activity_note.into());
    ui.set_analytics_activity_chart_top(v.activity_chart_top.into());
    ui.set_analytics_activity_chart_middle(v.activity_chart_middle.into());
    ui.set_analytics_activity_chart_bottom(v.activity_chart_bottom.into());
    ui.set_analytics_activity_chart_start(v.activity_chart_start.into());
    ui.set_analytics_activity_chart_end(v.activity_chart_end.into());
    ui.set_analytics_activity_has_data(v.activity_has_data);
    ui.set_analytics_activity_summary(model(
        v.activity_summaries.into_iter().map(Into::into).collect(),
    ));
    ui.set_analytics_activity_points(model(
        v.activity_points.into_iter().map(Into::into).collect(),
    ));
    ui.set_analytics_activity_models(model(
        v.activity_models.into_iter().map(Into::into).collect(),
    ));
    ui.set_analytics_activity_providers(model(
        v.activity_providers.into_iter().map(Into::into).collect(),
    ));
    Ok(())
}
