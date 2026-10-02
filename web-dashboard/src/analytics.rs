//! Analytics view model (version 2, public/analytics-data.mjs `analyticsSlintModel`): the header
//! state, the KPI row and the quota-history groups. Persistent models keep row instances alive across
//! refreshes and range changes so later charts can morph instead of re-mounting.
use crate::sync::{Nested, sync_rows};
use crate::{AnalyticsHeadView, Dashboard, KpiView, QuotaGroupView, QuotaRowView};
use serde::Deserialize;
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;
use wasm_bindgen::JsValue;

pub struct AnalyticsModels {
    kpis: Rc<VecModel<KpiView>>,
    groups: Rc<VecModel<QuotaGroupView>>,
    group_rows: Nested<QuotaRowView>,
}

impl Default for AnalyticsModels {
    fn default() -> Self {
        Self {
            kpis: Rc::new(VecModel::default()),
            groups: Rc::new(VecModel::default()),
            group_rows: Nested::default(),
        }
    }
}

pub fn bind(ui: &Dashboard, m: &AnalyticsModels) {
    ui.set_analytics_kpis(ModelRc::from(m.kpis.clone()));
    ui.set_analytics_quota_groups(ModelRc::from(m.groups.clone()));
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct HeadDto {
    updated: String,
    range: String,
    provider: String,
    note: String,
    has_activity: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct KpiDto {
    key: String,
    label: String,
    value: String,
    sub: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct QuotaRowDto {
    id: String,
    key: String,
    provider: String,
    label: String,
    sub: String,
    window_label: String,
    has_value: bool,
    value: f32,
    value_text: String,
    reset: String,
    active: bool,
    spark: String,
    spark_points: i32,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct QuotaGroupDto {
    provider: String,
    label: String,
    rows: Vec<QuotaRowDto>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AnalyticsDto {
    version: u32,
    head: HeadDto,
    kpis: Vec<KpiDto>,
    quota_groups: Vec<QuotaGroupDto>,
}

pub fn set_analytics(ui: &Dashboard, m: &mut AnalyticsModels, json: &str) -> Result<(), JsValue> {
    let v: AnalyticsDto =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid analytics view data"))?;
    if v.version != crate::VIEW_MODEL_VERSION {
        return Err(JsValue::from_str(
            "Unsupported analytics view-model version",
        ));
    }
    let previous = ui.get_analytics_head();
    ui.set_analytics_head(AnalyticsHeadView {
        loading: previous.loading,
        error: previous.error,
        updated: v.head.updated.into(),
        range: v.head.range.clone().into(),
        provider: v.head.provider.into(),
        note: v.head.note.into(),
        has_activity: v.head.has_activity,
    });
    if ["24h", "7d", "30d"].contains(&v.head.range.as_str()) {
        ui.set_analytics_range(v.head.range.into());
    }
    sync_rows(
        &m.kpis,
        v.kpis
            .into_iter()
            .map(|k| KpiView {
                key: k.key.into(),
                label: k.label.into(),
                value: k.value.into(),
                sub: k.sub.into(),
            })
            .collect(),
        |k: &KpiView| k.key.clone(),
    );
    let mut live = Vec::new();
    let mut groups = Vec::new();
    for group in v.quota_groups {
        live.push(group.provider.clone());
        let count = group.rows.len() as i32;
        let rows = m.group_rows.sync(
            &group.provider,
            group
                .rows
                .into_iter()
                .map(|r| QuotaRowView {
                    id: r.id.into(),
                    key: r.key.into(),
                    provider: r.provider.into(),
                    label: r.label.into(),
                    sub: r.sub.into(),
                    window_label: r.window_label.into(),
                    has_value: r.has_value && r.value.is_finite(),
                    value: if r.value.is_finite() { r.value } else { 0. },
                    value_text: r.value_text.into(),
                    reset: r.reset.into(),
                    active: r.active,
                    spark: r.spark.into(),
                    spark_points: r.spark_points,
                })
                .collect(),
            |r: &QuotaRowView| r.key.clone(),
        );
        groups.push(QuotaGroupView {
            provider: group.provider.into(),
            label: group.label.into(),
            count,
            rows,
        });
    }
    sync_rows(&m.groups, groups, |g: &QuotaGroupView| -> SharedString {
        g.provider.clone()
    });
    m.group_rows.retain(&live);
    Ok(())
}
