//! Phase 4 dashboard server memory budget runner (`scripts/perf/memory-budget.mjs`).
//!
//! Boots `dist/native/ai-account-center` against a deterministic real-sized synthetic
//! fixture in an isolated temporary home, signs in via loopback setup, polls dashboard
//! routes on their real page timers, samples server and descendant RSS/PSS from `/proc`,
//! writes a JSON summary, and exits non-zero if any limit in `config/memory-budget.json`
//! is exceeded.

use super::base_fixtures::{iso_ms, to_fixed_num};
use super::fixture::{FixtureOptions, generate_memory_fixture};
use super::http_client::{HttpClient, decode_content_encoding};
use super::proc_mem::{MemorySample, is_pid_alive, sample_process_tree};
use crate::SilentExit;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

const KIB_PER_MIB: f64 = 1024.0;
const MIN_STEADY_SAMPLES_FLOOR: usize = 3;
const POLL_DISPATCH_GRACE_MS: u64 = 250;
const MIN_SAMPLE_INTERVAL_SEC: f64 = 0.2;
const SAMPLING_NOTE: &str = "Child memory peaks are based on /proc samples. A helper that starts and exits between samples may be missed.";

const FIXTURE_USER: &str = "perfadmin";
const FIXTURE_PASS: &str = "Perf-Correct-Horse-Battery-9";

/// Required budget ceilings in `config/memory-budget.json` for check mode.
pub const REQUIRED_LIMIT_KEYS: [&str; 3] = ["steadyRssMiB", "peakRssMiB", "childrenPeakRssMiB"];

const THRESHOLD_CHECKS: [(&str, &str, &str); 8] = [
    ("steadyRssMiB", "steadyMedianRssKiB", "steadyMedianRssMiB"),
    ("peakRssMiB", "peakRssKiB", "peakRssMiB"),
    ("steadyPssMiB", "steadyMedianPssKiB", "steadyMedianPssMiB"),
    ("peakPssMiB", "peakPssKiB", "peakPssMiB"),
    (
        "childrenPeakRssMiB",
        "childrenPeakRssKiB",
        "childrenPeakRssMiB",
    ),
    (
        "childrenPeakPssMiB",
        "childrenPeakPssKiB",
        "childrenPeakPssMiB",
    ),
    (
        "totalSteadyRssMiB",
        "totalSteadyMedianRssKiB",
        "totalSteadyMedianRssMiB",
    ),
    ("totalPeakRssMiB", "totalPeakRssKiB", "totalPeakRssMiB"),
];

/// Every limit key accepted in `config/memory-budget.json`.
pub const ALLOWED_LIMIT_KEYS: [&str; 8] = [
    "steadyRssMiB",
    "peakRssMiB",
    "steadyPssMiB",
    "peakPssMiB",
    "childrenPeakRssMiB",
    "childrenPeakPssMiB",
    "totalSteadyRssMiB",
    "totalPeakRssMiB",
];

/// One route in the dashboard polling schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct PollRoute {
    /// Short route label.
    pub name: String,
    /// HTTP request path and query string.
    pub path: String,
    /// Polling cadence in seconds.
    pub period_sec: f64,
    /// Initial stagger offset in seconds.
    pub offset_sec: f64,
}

/// Default polling plan matching open dashboard pages and both desktop Home trays.
pub fn default_poll_plan() -> Vec<PollRoute> {
    vec![
        PollRoute {
            name: "status".into(),
            path: "/api/status".into(),
            period_sec: 10.0,
            offset_sec: 0.0,
        },
        PollRoute {
            name: "scoreboard".into(),
            path: "/api/scoreboard".into(),
            period_sec: 30.0,
            offset_sec: 1.0,
        },
        PollRoute {
            name: "analytics7d".into(),
            path: "/api/accounts/analytics?platform=mac&range=7d&provider=all&account=all".into(),
            period_sec: 60.0,
            offset_sec: 2.0,
        },
        PollRoute {
            name: "analytics30d".into(),
            path: "/api/accounts/analytics?platform=mac&range=30d&provider=all&account=all".into(),
            period_sec: 300.0,
            offset_sec: 3.0,
        },
        PollRoute {
            name: "home-mac".into(),
            path: "/api/views/home?platform=mac&refresh=false".into(),
            period_sec: 60.0,
            offset_sec: 4.0,
        },
        PollRoute {
            name: "home-win".into(),
            path: "/api/views/home?platform=windows&refresh=false".into(),
            period_sec: 60.0,
            offset_sec: 5.0,
        },
        PollRoute {
            name: "accounts".into(),
            path: "/api/accounts/dashboard?platform=mac&refresh=false".into(),
            period_sec: 60.0,
            offset_sec: 6.0,
        },
    ]
}

/// One recorded HTTP poll attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRecord {
    /// Elapsed seconds from poll start.
    pub t_sec: f64,
    /// Route name.
    pub route: String,
    /// HTTP status code (`-1` on transport/validation failure with no status).
    pub status: i32,
    /// Raw wire response length in bytes.
    pub bytes: usize,
    /// Response `Content-Encoding` header value.
    pub encoding: String,
    /// Round-trip latency in milliseconds.
    pub ms: u64,
    /// Optional failure message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per-route summary metrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RouteSummary {
    /// Total calls attempted.
    pub calls: usize,
    /// Successful calls (HTTP 200 with no validation error).
    pub ok: usize,
    /// Failed calls.
    pub failed: usize,
    /// First error observed for this route, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_error: Option<String>,
    /// Median wire response size of successful calls.
    pub median_bytes: u64,
    /// First call latency in milliseconds.
    pub first_ms: u64,
    /// Median warm latency in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile warm latency in milliseconds.
    pub p95_ms: f64,
    /// Maximum call latency in milliseconds.
    pub max_ms: u64,
}

/// Memory summary metrics computed from process-tree samples.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummary {
    /// Total collected samples.
    pub sample_count: usize,
    /// Configured steady window start in seconds.
    pub steady_after_sec: f64,
    /// Number of samples at `t_sec >= steady_after_sec`.
    pub steady_sample_count: usize,
    /// Whether startup fallback was used because steady window was empty.
    pub steady_used_fallback: bool,
    /// Timestamp of final sample in seconds.
    pub last_sample_sec: f64,
    /// Whether complete total PSS coverage was available.
    pub pss_available: bool,
    /// Steady-window median server RSS in KiB.
    #[serde(rename = "steadyMedianRssKiB")]
    pub steady_median_rss_kib: f64,
    /// Steady-window median server PSS in KiB.
    #[serde(rename = "steadyMedianPssKiB")]
    pub steady_median_pss_kib: Option<f64>,
    /// Peak server RSS in KiB.
    #[serde(rename = "peakRssKiB")]
    pub peak_rss_kib: f64,
    /// Peak server PSS in KiB.
    #[serde(rename = "peakPssKiB")]
    pub peak_pss_kib: Option<f64>,
    /// Peak children RSS in KiB.
    #[serde(rename = "childrenPeakRssKiB")]
    pub children_peak_rss_kib: f64,
    /// Peak children PSS in KiB.
    #[serde(rename = "childrenPeakPssKiB")]
    pub children_peak_pss_kib: Option<f64>,
    /// Steady-window median total RSS in KiB.
    #[serde(rename = "totalSteadyMedianRssKiB")]
    pub total_steady_median_rss_kib: f64,
    /// Steady-window median total PSS in KiB.
    #[serde(rename = "totalSteadyMedianPssKiB")]
    pub total_steady_median_pss_kib: Option<f64>,
    /// Peak total RSS in KiB.
    #[serde(rename = "totalPeakRssKiB")]
    pub total_peak_rss_kib: f64,
    /// Peak total PSS in KiB.
    #[serde(rename = "totalPeakPssKiB")]
    pub total_peak_pss_kib: Option<f64>,
    /// Final sample server RSS in KiB.
    #[serde(rename = "endRssKiB")]
    pub end_rss_kib: f64,
    /// Final sample server PSS in KiB.
    #[serde(rename = "endPssKiB")]
    pub end_pss_kib: Option<f64>,
    /// Steady-window median server RSS in MiB.
    #[serde(rename = "steadyMedianRssMiB")]
    pub steady_median_rss_mib: f64,
    /// Steady-window median server PSS in MiB.
    #[serde(rename = "steadyMedianPssMiB")]
    pub steady_median_pss_mib: Option<f64>,
    /// Peak server RSS in MiB.
    #[serde(rename = "peakRssMiB")]
    pub peak_rss_mib: f64,
    /// Peak server PSS in MiB.
    #[serde(rename = "peakPssMiB")]
    pub peak_pss_mib: Option<f64>,
    /// Peak children RSS in MiB.
    #[serde(rename = "childrenPeakRssMiB")]
    pub children_peak_rss_mib: f64,
    /// Peak children PSS in MiB.
    #[serde(rename = "childrenPeakPssMiB")]
    pub children_peak_pss_mib: Option<f64>,
    /// Steady-window median total RSS in MiB.
    #[serde(rename = "totalSteadyMedianRssMiB")]
    pub total_steady_median_rss_mib: f64,
    /// Steady-window median total PSS in MiB.
    #[serde(rename = "totalSteadyMedianPssMiB")]
    pub total_steady_median_pss_mib: Option<f64>,
    /// Peak total RSS in MiB.
    #[serde(rename = "totalPeakRssMiB")]
    pub total_peak_rss_mib: f64,
    /// Peak total PSS in MiB.
    #[serde(rename = "totalPeakPssMiB")]
    pub total_peak_pss_mib: Option<f64>,
    /// Final sample server RSS in MiB.
    #[serde(rename = "endRssMiB")]
    pub end_rss_mib: f64,
    /// Final sample server PSS in MiB.
    #[serde(rename = "endPssMiB")]
    pub end_pss_mib: Option<f64>,
    /// Maximum thread count observed.
    pub max_threads: u64,
    /// Maximum child count observed.
    pub max_children: usize,
    /// Note explaining `/proc` sampling limits for short-lived helpers.
    pub sampling_note: String,
}

/// One evaluated budget threshold row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdRow {
    /// Limit metric name.
    pub metric: String,
    /// Measured value in MiB.
    #[serde(rename = "measuredMiB")]
    pub measured_mib: f64,
    /// Configured ceiling in MiB.
    #[serde(rename = "limitMiB")]
    pub limit_mib: f64,
    /// Measured minus limit in MiB.
    #[serde(rename = "deltaMiB")]
    pub delta_mib: f64,
    /// Exact measured value in KiB.
    #[serde(rename = "measuredKiB")]
    pub measured_kib: f64,
    /// Exact ceiling in KiB.
    #[serde(rename = "limitKiB")]
    pub limit_kib: f64,
    /// Exact measured minus limit in KiB.
    #[serde(rename = "deltaKiB")]
    pub delta_kib: f64,
    /// Percentage over limit.
    pub pct_over: f64,
    /// Whether the metric stayed within its ceiling.
    pub ok: bool,
}

/// Threshold evaluation summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdResult {
    /// True when every configured limit passed.
    pub ok: bool,
    /// Every evaluated check row.
    pub results: Vec<ThresholdRow>,
    /// Subset of rows that exceeded their limit.
    pub violations: Vec<ThresholdRow>,
}

/// Runner options for [`run_memory_budget`].
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Measurement window duration in seconds.
    pub duration_sec: f64,
    /// Seconds after poll start when steady-state sampling begins.
    pub steady_after_sec: f64,
    /// `/proc` sampling interval in seconds.
    pub sample_interval_sec: f64,
    /// Synthetic fixture PRNG seed.
    pub seed: u64,
    /// Threshold JSON config path.
    pub config_path: PathBuf,
    /// Optional output summary JSON path.
    pub out_path: Option<PathBuf>,
    /// Optional prebuilt fixture directory to copy.
    pub fixture_dir: Option<PathBuf>,
    /// Retain temporary run root on exit.
    pub keep: bool,
    /// Enforce budget checks (false for `--no-check`/`--measure-only`).
    pub check: bool,
    /// Print JSON summary to stdout.
    pub json: bool,
    /// Optional custom launcher binary (for integration tests).
    pub launcher_path: Option<PathBuf>,
    /// Optional custom launcher argv template (`{port}` and `{ccs_dir}` substituted).
    pub launcher_args: Option<Vec<String>>,
    /// Optional custom polling plan.
    pub poll_plan: Option<Vec<PollRoute>>,
    /// Optional test hook: SIGKILL child after this many milliseconds post-sign-in.
    pub kill_after_ms: Option<u64>,
}

impl RunOptions {
    /// Default options rooted at `repo_root`.
    pub fn default_for(repo_root: &Path) -> Self {
        Self {
            duration_sec: 300.0,
            steady_after_sec: 120.0,
            sample_interval_sec: 2.0,
            seed: FixtureOptions::default().seed,
            config_path: repo_root.join("config").join("memory-budget.json"),
            out_path: None,
            fixture_dir: None,
            keep: false,
            check: true,
            json: false,
            launcher_path: None,
            launcher_args: None,
            poll_plan: None,
            kill_after_ms: None,
        }
    }
}

/// Timing breakdown in milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingMs {
    /// Fixture generation or copy time.
    pub fixture: u64,
    /// Server boot to `/api/health` time.
    pub boot: u64,
    /// Polling duration.
    pub poll: u64,
    /// Total wall-clock duration.
    pub total: u64,
}

mod routes_map_serde {
    use super::RouteSummary;
    use indexmap::IndexMap;
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S>(
        map: &IndexMap<String, RouteSummary>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            s.serialize_entry(k, v)?;
        }
        s.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<IndexMap<String, RouteSummary>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RoutesVisitor;

        impl<'de> Visitor<'de> for RoutesVisitor {
            type Value = IndexMap<String, RouteSummary>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of route summaries")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut out = IndexMap::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((k, v)) = access.next_entry::<String, RouteSummary>()? {
                    out.insert(k, v);
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(RoutesVisitor)
    }
}

/// Full JSON summary emitted by [`run_memory_budget`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSummary {
    /// Summary schema version (1).
    pub schema_version: u32,
    /// Overall pass/fail status.
    pub ok: bool,
    /// ISO-8601 UTC timestamp.
    pub measured_at: String,
    /// Hostname.
    pub host: String,
    /// Fixture seed.
    pub seed: u64,
    /// Poll duration in seconds.
    pub duration_sec: f64,
    /// Sampling interval in seconds.
    pub sample_interval_sec: f64,
    /// Wall-clock timings.
    pub timing_ms: TimingMs,
    /// Fixture item counts.
    pub fixture_counts: Json,
    /// Store stats (if migrated).
    pub store: Option<Json>,
    /// Memory summary.
    pub memory: MemorySummary,
    /// Per-route summaries.
    #[serde(with = "routes_map_serde")]
    pub routes: IndexMap<String, RouteSummary>,
    /// Route poll failures.
    pub route_failures: Vec<String>,
    /// Sample coverage failures.
    pub sample_failures: Vec<String>,
    /// Threshold evaluation result (if check mode).
    pub threshold: Option<ThresholdResult>,
    /// Retained run root when `--keep` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_root: Option<PathBuf>,
}

/// Child exit status details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildExitInfo {
    /// Process exit code, or `None` when terminated by a signal.
    pub exit_code: Option<i32>,
    /// Signal name such as `"SIGKILL"`, or `None`.
    pub signal_code: Option<String>,
    /// Human-readable reason (`signal=SIGKILL` or `code=1`).
    pub reason: String,
}

/// Convert a child [`ExitStatus`] into exit code / signal details matching `getChildExitStatus`.
pub fn exit_status_info(status: ExitStatus) -> ChildExitInfo {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = nix::sys::signal::Signal::try_from(sig)
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(|_| sig.to_string());
            return ChildExitInfo {
                exit_code: None,
                signal_code: Some(name.clone()),
                reason: format!("signal={name}"),
            };
        }
    }
    let code = status.code();
    ChildExitInfo {
        exit_code: code,
        signal_code: None,
        reason: match code {
            Some(c) => format!("code={c}"),
            None => "code=unknown".to_string(),
        },
    }
}

/// Refuse symlinks and unexpected file types in fixture inputs before traversing them.
pub fn assert_no_symlinks(root: &Path) -> Result<(), Box<dyn Error>> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let meta = fs::symlink_metadata(&current)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            return Err(format!("Fixture input contains a symlink: {}", current.display()).into());
        }
        if ft.is_dir() {
            for entry in fs::read_dir(&current)? {
                stack.push(entry?.path());
            }
            continue;
        }
        if ft.is_file() {
            continue;
        }
        return Err(format!(
            "Fixture input contains an unexpected file type: {}",
            current.display()
        )
        .into());
    }
    Ok(())
}

/// Verify `target` lies inside `root` (component-safe, no `..` escape).
pub fn is_path_inside(root: &Path, target: &Path) -> bool {
    target.starts_with(root)
}

/// Canonicalize `root` and `file` and verify `file` stays inside `root`.
pub fn assert_resolved_inside(root: &Path, file: &Path) -> Result<(), Box<dyn Error>> {
    let real_root = fs::canonicalize(root)?;
    let real_file = fs::canonicalize(file)
        .map_err(|_| format!("Fixture root does not exist: {}", file.display()))?;
    if !is_path_inside(&real_root, &real_file) {
        return Err(format!(
            "Fixture root escapes the fixture: {} -> {}",
            file.display(),
            real_file.display()
        )
        .into());
    }
    Ok(())
}

fn copy_dir_no_symlinks(src: &Path, dst: &Path) -> Result<(), Box<dyn Error>> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Err(format!("Fixture input contains a symlink: {}", src.display()).into());
    }
    if ft.is_dir() {
        fs::create_dir_all(dst)?;
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(dst, meta.permissions());
        }
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_dir_no_symlinks(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if ft.is_file() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(dst, meta.permissions());
        }
        return Ok(());
    }
    Err(format!(
        "Fixture input contains an unexpected file type: {}",
        src.display()
    )
    .into())
}

/// Compute exact median of a numeric slice.
pub fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

/// Compute percentile `p` (`0.0..=1.0`) matching `memory-budget.mjs`.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((sorted.len() as f64) * p)
        .floor()
        .clamp(0.0, (sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

fn to_mib(kib: f64) -> f64 {
    to_fixed_num(kib / KIB_PER_MIB, 2)
}

/// Minimum valid steady-window samples required for check mode.
pub fn min_required_steady_samples(
    duration_sec: f64,
    steady_after_sec: f64,
    sample_interval_sec: f64,
) -> usize {
    let steady_window_sec = duration_sec - steady_after_sec;
    if !steady_window_sec.is_finite()
        || steady_window_sec <= 0.0
        || !sample_interval_sec.is_finite()
        || sample_interval_sec <= 0.0
    {
        return 0;
    }
    let expected = (steady_window_sec / sample_interval_sec).floor() as usize + 1;
    let half = ((expected as f64) * 0.5).ceil() as usize;
    MIN_STEADY_SAMPLES_FLOOR.max(half)
}

/// Validate timing options before generating the fixture or starting the server.
pub fn validate_run_options(
    opts: &RunOptions,
    poll_plan: &[PollRoute],
) -> Result<(), Box<dyn Error>> {
    if !opts.duration_sec.is_finite() || opts.duration_sec <= 0.0 {
        return Err(format!(
            "Invalid durationSec ({}): must be a finite number > 0.",
            opts.duration_sec
        )
        .into());
    }
    if !opts.steady_after_sec.is_finite() || opts.steady_after_sec < 0.0 {
        return Err(format!(
            "Invalid steadyAfterSec ({}): must be a finite number >= 0.",
            opts.steady_after_sec
        )
        .into());
    }
    if !opts.sample_interval_sec.is_finite() || opts.sample_interval_sec <= 0.0 {
        return Err(format!(
            "Invalid sampleIntervalSec ({}): must be a finite number > 0.",
            opts.sample_interval_sec
        )
        .into());
    }
    if opts.sample_interval_sec < MIN_SAMPLE_INTERVAL_SEC {
        return Err(format!(
            "Invalid sampleIntervalSec ({}): must be >= {MIN_SAMPLE_INTERVAL_SEC} (minimum supported sample interval).",
            opts.sample_interval_sec
        )
        .into());
    }
    if poll_plan.is_empty() {
        return Err("Poll plan must contain at least one route.".into());
    }

    if opts.check {
        if opts.duration_sec <= opts.steady_after_sec {
            return Err(format!(
                "Check mode requires durationSec ({}s) > steadyAfterSec ({}s) to collect steady-window samples.",
                opts.duration_sec, opts.steady_after_sec
            )
            .into());
        }
        let max_offset = poll_plan
            .iter()
            .map(|r| r.offset_sec)
            .fold(f64::NEG_INFINITY, f64::max);
        if opts.duration_sec <= max_offset {
            return Err(format!(
                "Check mode requires durationSec ({}s) > max route offset ({}s) so every route is polled.",
                opts.duration_sec, max_offset
            )
            .into());
        }
        let steady_window_sec = opts.duration_sec - opts.steady_after_sec;
        let expected_steady = (steady_window_sec / opts.sample_interval_sec).floor() as usize + 1;
        if expected_steady < MIN_STEADY_SAMPLES_FLOOR {
            return Err(format!(
                "Check mode steady window ({steady_window_sec}s at {}s interval) allows only {expected_steady} sample(s); need at least {MIN_STEADY_SAMPLES_FLOOR}.",
                opts.sample_interval_sec
            )
            .into());
        }
    }
    Ok(())
}

/// Validate a threshold config JSON object.
pub fn validate_threshold_config(
    config: &Json,
    require_schema_version: bool,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    let Some(obj) = config.as_object() else {
        return Err(format!("Invalid {source}: expected a JSON object.").into());
    };
    if (require_schema_version || obj.contains_key("schemaVersion"))
        && obj.get("schemaVersion").and_then(Json::as_u64) != Some(1)
    {
        return Err(format!(
            "Invalid {source}: unsupported schemaVersion {} (expected 1).",
            obj.get("schemaVersion").unwrap_or(&Json::Null)
        )
        .into());
    }
    let Some(limits) = obj.get("limits").and_then(Json::as_object) else {
        return Err(format!("Invalid {source}: missing \"limits\" object.").into());
    };

    let unknown: Vec<String> = limits
        .keys()
        .filter(|k| !ALLOWED_LIMIT_KEYS.contains(&k.as_str()))
        .map(|k| format!("\"{k}\""))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "Invalid {source}: unknown limit key(s) {} (allowed: {}).",
            unknown.join(", "),
            ALLOWED_LIMIT_KEYS.join(", ")
        )
        .into());
    }

    for required in REQUIRED_LIMIT_KEYS {
        if !limits.contains_key(required) {
            return Err(format!("Invalid {source}: missing required limit \"{required}\".").into());
        }
    }

    for (key, val) in limits {
        let ok = val.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0);
        if !ok {
            return Err(format!(
                "Invalid {source}: limit \"{key}\" must be a finite non-negative number (got {val})."
            )
            .into());
        }
    }

    Ok(())
}

/// Read and validate `config/memory-budget.json` before generating the fixture.
pub fn load_threshold_config(config_path: &Path) -> Result<Json, Box<dyn Error>> {
    if !config_path.exists() {
        return Err(format!("Threshold config not found: {}", config_path.display()).into());
    }
    let raw = fs::read_to_string(config_path)?;
    let parsed: Json = serde_json::from_str(&raw).map_err(|err| {
        format!(
            "Invalid threshold config JSON ({}): {err}",
            config_path.display()
        )
    })?;
    let source = config_path.to_string_lossy();
    validate_threshold_config(&parsed, true, &source)?;
    Ok(parsed)
}

/// Compute steady median, peaks, and per-metric PSS coverage summary.
pub fn summarize_samples(
    samples: &[MemorySample],
    steady_after_sec: f64,
    allow_steady_fallback: bool,
) -> Result<MemorySummary, Box<dyn Error>> {
    if samples.is_empty() {
        return Err("No memory samples were collected from /proc.".into());
    }
    let steady_window: Vec<&MemorySample> = samples
        .iter()
        .filter(|s| s.t_sec >= steady_after_sec)
        .collect();
    if steady_window.is_empty() && !allow_steady_fallback {
        return Err(format!(
            "No memory samples were collected in the steady window (t >= {steady_after_sec}s; collected {} earlier sample(s)).",
            samples.len()
        )
        .into());
    }
    let steady_source: Vec<&MemorySample> = if steady_window.is_empty() {
        samples.iter().collect()
    } else {
        steady_window.clone()
    };

    let peak_f64 = |vals: Vec<f64>| -> f64 { vals.into_iter().fold(f64::NEG_INFINITY, f64::max) };
    let complete_pss = |items: &[&MemorySample],
                        pick: fn(&MemorySample) -> Option<u64>,
                        reduce: fn(&[f64]) -> f64|
     -> Option<f64> {
        let mut vals = Vec::with_capacity(items.len());
        for s in items {
            let v = pick(s)? as f64;
            if !v.is_finite() {
                return None;
            }
            vals.push(v);
        }
        Some(reduce(&vals))
    };
    let peak_slice =
        |vals: &[f64]| -> f64 { vals.iter().copied().fold(f64::NEG_INFINITY, f64::max) };

    let all_refs: Vec<&MemorySample> = samples.iter().collect();
    let peak_server_rss_kib = peak_f64(samples.iter().map(|s| s.server_rss_kib as f64).collect());
    let peak_server_pss_kib = complete_pss(&all_refs, |s| s.server_pss_kib, peak_slice);
    let children_peak_rss_kib =
        peak_f64(samples.iter().map(|s| s.children_rss_kib as f64).collect());
    let children_peak_pss_kib = complete_pss(&all_refs, |s| s.children_pss_kib, peak_slice);
    let total_peak_rss_kib = peak_f64(samples.iter().map(|s| s.total_rss_kib as f64).collect());
    let total_peak_pss_kib = complete_pss(&all_refs, |s| s.total_pss_kib, peak_slice);

    let steady_rss_vals: Vec<f64> = steady_source
        .iter()
        .map(|s| s.server_rss_kib as f64)
        .collect();
    let steady_median_rss_kib = median(&steady_rss_vals);
    let steady_median_pss_kib = complete_pss(&steady_source, |s| s.server_pss_kib, median);
    let steady_total_rss_vals: Vec<f64> = steady_source
        .iter()
        .map(|s| s.total_rss_kib as f64)
        .collect();
    let steady_median_total_rss_kib = median(&steady_total_rss_vals);
    let steady_median_total_pss_kib = complete_pss(&steady_source, |s| s.total_pss_kib, median);

    let last = &samples[samples.len() - 1];
    let end_rss_kib = last.server_rss_kib as f64;
    let end_pss_kib = last.server_pss_kib.map(|v| v as f64);

    Ok(MemorySummary {
        sample_count: samples.len(),
        steady_after_sec,
        steady_sample_count: steady_window.len(),
        steady_used_fallback: steady_window.is_empty(),
        last_sample_sec: last.t_sec,
        pss_available: total_peak_pss_kib.is_some(),
        steady_median_rss_kib,
        steady_median_pss_kib,
        peak_rss_kib: peak_server_rss_kib,
        peak_pss_kib: peak_server_pss_kib,
        children_peak_rss_kib,
        children_peak_pss_kib,
        total_steady_median_rss_kib: steady_median_total_rss_kib,
        total_steady_median_pss_kib: steady_median_total_pss_kib,
        total_peak_rss_kib,
        total_peak_pss_kib,
        end_rss_kib,
        end_pss_kib,
        steady_median_rss_mib: to_mib(steady_median_rss_kib),
        steady_median_pss_mib: steady_median_pss_kib.map(to_mib),
        peak_rss_mib: to_mib(peak_server_rss_kib),
        peak_pss_mib: peak_server_pss_kib.map(to_mib),
        children_peak_rss_mib: to_mib(children_peak_rss_kib),
        children_peak_pss_mib: children_peak_pss_kib.map(to_mib),
        total_steady_median_rss_mib: to_mib(steady_median_total_rss_kib),
        total_steady_median_pss_mib: steady_median_total_pss_kib.map(to_mib),
        total_peak_rss_mib: to_mib(total_peak_rss_kib),
        total_peak_pss_mib: total_peak_pss_kib.map(to_mib),
        end_rss_mib: to_mib(end_rss_kib),
        end_pss_mib: end_pss_kib.map(to_mib),
        max_threads: samples.iter().map(|s| s.threads).max().unwrap_or(1),
        max_children: samples.iter().map(|s| s.children_count).max().unwrap_or(0),
        sampling_note: SAMPLING_NOTE.to_string(),
    })
}

/// Check whether steady-window sampling met check-mode sufficiency requirements.
pub fn evaluate_sample_failures(
    memory: &MemorySummary,
    duration_sec: f64,
    steady_after_sec: f64,
    sample_interval_sec: f64,
) -> Vec<String> {
    let mut failures = Vec::new();
    if memory.sample_count == 0 {
        failures.push("No memory samples were collected from /proc.".to_string());
        return failures;
    }
    if memory.steady_used_fallback {
        failures.push(format!(
            "Steady window (t >= {steady_after_sec}s) collected 0 samples; startup fallback is not permitted in check mode."
        ));
    }
    let min_steady =
        min_required_steady_samples(duration_sec, steady_after_sec, sample_interval_sec);
    if memory.steady_sample_count < min_steady {
        failures.push(format!(
            "Insufficient steady-window samples (t >= {steady_after_sec}s): collected {}, required >= {min_steady}.",
            memory.steady_sample_count
        ));
    }
    let min_last_sec = steady_after_sec.max(duration_sec - (sample_interval_sec * 3.0).max(5.0));
    if !memory.last_sample_sec.is_finite() || memory.last_sample_sec < min_last_sec {
        failures.push(format!(
            "Memory sampling ended too early: last sample at t={}s, required >= {:.1}s.",
            aac_num::ecma::to_string(memory.last_sample_sec),
            min_last_sec
        ));
    }
    failures
}

/// Evaluate measured metrics against `config/memory-budget.json` limits using unrounded KiB values.
pub fn evaluate_thresholds(
    memory: &MemorySummary,
    config: &Json,
) -> Result<ThresholdResult, Box<dyn Error>> {
    validate_threshold_config(config, false, "threshold config")?;
    if memory.steady_used_fallback || memory.steady_sample_count == 0 {
        return Err(
            "Cannot evaluate thresholds without steady-window samples (steadyUsedFallback=true or steadySampleCount=0)."
                .into(),
        );
    }

    let limits = config
        .get("limits")
        .and_then(Json::as_object)
        .ok_or("missing limits object")?;
    let mem_json = serde_json::to_value(memory)?;
    let mut results = Vec::new();
    let mut violations = Vec::new();

    for (limit_key, kib_key, mib_key) in THRESHOLD_CHECKS {
        let Some(limit_val) = limits.get(limit_key) else {
            continue;
        };
        let limit = limit_val
            .as_f64()
            .ok_or_else(|| format!("non-numeric limit for {limit_key}"))?;
        let limit_kib = limit * KIB_PER_MIB;

        let (measured_kib, exact_measured_mib) = if let Some(kib) = mem_json
            .get(kib_key)
            .and_then(Json::as_f64)
            .filter(|n| n.is_finite())
        {
            (kib, kib / KIB_PER_MIB)
        } else if let Some(mib) = mem_json
            .get(mib_key)
            .and_then(Json::as_f64)
            .filter(|n| n.is_finite())
        {
            (mib * KIB_PER_MIB, mib)
        } else if limit_key.contains("Pss") {
            return Err(format!(
                "Memory summary lacks valid PSS coverage for limit \"{limit_key}\"."
            )
            .into());
        } else {
            return Err(format!(
                    "Memory summary is missing a finite value for \"{limit_key}\" ({kib_key}/{mib_key})."
                )
                .into());
        };

        let delta_kib = measured_kib - limit_kib;
        let exact_delta_mib = delta_kib / KIB_PER_MIB;
        let ok = delta_kib <= 0.0;

        let mut measured_mib = to_fixed_num(exact_measured_mib, 2);
        let mut delta_mib = to_fixed_num(exact_delta_mib, 2);
        if !ok && (measured_mib <= limit || delta_mib <= 0.0) {
            measured_mib = to_fixed_num(exact_measured_mib, 4);
            delta_mib = to_fixed_num(exact_delta_mib, 4);
            if measured_mib <= limit || delta_mib <= 0.0 {
                measured_mib = exact_measured_mib;
                delta_mib = exact_delta_mib;
            }
        }
        let raw_pct = if limit_kib > 0.0 {
            (delta_kib / limit_kib) * 100.0
        } else if delta_kib > 0.0 {
            100.0
        } else {
            0.0
        };
        let pct_over = if !ok && to_fixed_num(raw_pct, 1).abs() == 0.0 {
            let p4 = to_fixed_num(raw_pct, 4);
            if p4 != 0.0 { p4 } else { raw_pct }
        } else {
            to_fixed_num(raw_pct, 1)
        };

        let row = ThresholdRow {
            metric: limit_key.to_string(),
            measured_mib,
            limit_mib: limit,
            delta_mib,
            measured_kib,
            limit_kib,
            delta_kib,
            pct_over,
            ok,
        };
        if !ok {
            violations.push(row.clone());
        }
        results.push(row);
    }

    if results.len() < REQUIRED_LIMIT_KEYS.len() {
        return Err(format!(
            "Threshold evaluation executed only {} check(s); expected at least {}.",
            results.len(),
            REQUIRED_LIMIT_KEYS.len()
        )
        .into());
    }

    Ok(ThresholdResult {
        ok: violations.is_empty(),
        results,
        violations,
    })
}

/// Expected number of scheduled calls for a route during `[0, duration_sec)`.
pub fn expected_route_calls(route: &PollRoute, duration_sec: f64) -> usize {
    if !duration_sec.is_finite()
        || duration_sec <= 0.0
        || duration_sec <= route.offset_sec
        || !route.period_sec.is_finite()
        || route.period_sec <= 0.0
    {
        return 0;
    }
    ((duration_sec - route.offset_sec - 1e-9) / route.period_sec).floor() as usize + 1
}

/// Summarize per-route poll records in `poll_plan` order.
pub fn summarize_routes(
    records: &[RouteRecord],
    poll_plan: &[PollRoute],
) -> IndexMap<String, RouteSummary> {
    let mut by_route: BTreeMap<&str, Vec<&RouteRecord>> = BTreeMap::new();
    for rec in records {
        by_route.entry(rec.route.as_str()).or_default().push(rec);
    }
    let mut summary = IndexMap::new();
    for route in poll_plan {
        let list = by_route
            .get(route.name.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let ok: Vec<&RouteRecord> = list
            .iter()
            .copied()
            .filter(|r| r.status == 200 && r.error.is_none())
            .collect();
        let first_error = list.iter().find_map(|r| r.error.clone());
        let ms_list: Vec<f64> = list.iter().map(|r| r.ms as f64).collect();
        let warm_ms = if ms_list.len() > 1 {
            &ms_list[1..]
        } else {
            &ms_list[..]
        };
        let ok_bytes: Vec<f64> = ok.iter().map(|r| r.bytes as f64).collect();
        let median_bytes = aac_num::ecma::math_round(median(&ok_bytes)).max(0.0) as u64;
        summary.insert(
            route.name.clone(),
            RouteSummary {
                calls: list.len(),
                ok: ok.len(),
                failed: list.len().saturating_sub(ok.len()),
                first_error,
                median_bytes,
                first_ms: list.first().map(|r| r.ms).unwrap_or(0),
                p50_ms: to_fixed_num(median(warm_ms), 1),
                p95_ms: to_fixed_num(percentile(warm_ms, 0.95), 1),
                max_ms: list.iter().map(|r| r.ms).max().unwrap_or(0),
            },
        );
    }
    summary
}

/// Verify every route polled successfully across the full window.
pub fn evaluate_route_failures(
    routes: &IndexMap<String, RouteSummary>,
    duration_sec: f64,
    poll_plan: &[PollRoute],
    check: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    for route in poll_plan {
        let scheduled = expected_route_calls(route, duration_sec);
        let min_required = if check { scheduled.max(1) } else { scheduled };
        let Some(row) = routes.get(&route.name) else {
            if min_required > 0 {
                failures.push(format!(
                    "{}: 0 polls completed (expected >= {min_required})",
                    route.name
                ));
            }
            continue;
        };
        if row.calls == 0 {
            if min_required > 0 {
                failures.push(format!(
                    "{}: 0 polls completed (expected >= {min_required})",
                    route.name
                ));
            }
            continue;
        }
        let failed_count = row.failed.max(row.calls.saturating_sub(row.ok));
        if failed_count > 0 || row.ok < row.calls {
            let suffix = row
                .first_error
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            failures.push(format!(
                "{}: {failed_count}/{} polls failed ({}/{} successful){suffix}",
                route.name, row.calls, row.ok, row.calls
            ));
            continue;
        }
        if min_required > 0 && row.calls < min_required {
            failures.push(format!(
                "{}: only {}/{scheduled} polls completed across {}s window (expected >= {min_required})",
                route.name, row.calls, duration_sec
            ));
        }
    }
    failures
}

/// Build the isolated server environment with every home-like variable pointed at the temp dir.
pub fn build_isolated_env(
    home: &Path,
    ccs_dir: &Path,
    reports_dir: &Path,
    registry_dir: &Path,
    stubs_dir: &Path,
    xdg_runtime_dir: &Path,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    let s = |p: PathBuf| p.to_string_lossy().into_owned();
    env.insert(
        "PATH".into(),
        format!("{}:/usr/bin:/bin", stubs_dir.display()),
    );
    env.insert("LANG".into(), "C.UTF-8".into());
    env.insert("TZ".into(), "UTC".into());
    env.insert("USER".into(), "perfuser".into());
    env.insert("LOGNAME".into(), "perfuser".into());
    env.insert("HOME".into(), s(home.to_path_buf()));
    env.insert("CCS_HOME".into(), s(home.to_path_buf()));
    env.insert("CCS_DIR".into(), s(ccs_dir.to_path_buf()));
    env.insert("USERPROFILE".into(), s(home.to_path_buf()));
    env.insert("APPDATA".into(), s(home.join("AppData").join("Roaming")));
    env.insert("LOCALAPPDATA".into(), s(home.join("AppData").join("Local")));
    env.insert("XDG_CONFIG_HOME".into(), s(home.join(".config")));
    env.insert("XDG_DATA_HOME".into(), s(home.join(".local").join("share")));
    env.insert(
        "XDG_STATE_HOME".into(),
        s(home.join(".local").join("state")),
    );
    env.insert("XDG_CACHE_HOME".into(), s(home.join(".cache")));
    env.insert("XDG_RUNTIME_DIR".into(), s(xdg_runtime_dir.to_path_buf()));
    env.insert("CODEX_HOME".into(), s(home.join(".codex")));
    env.insert("CLAUDE_CONFIG_DIR".into(), s(home.join(".claude")));
    env.insert("AAC_AGENT_REPORTS_DIR".into(), s(reports_dir.to_path_buf()));
    env.insert(
        "AAC_PM_MAIL_REGISTRY_DIR".into(),
        s(registry_dir.to_path_buf()),
    );
    env.insert("AAC_STATUS_DOORBELL".into(), "off".into());
    env.insert("AAC_AGENT_TRANSPORT".into(), "off".into());
    env.insert("AAC_AGENT_VERBS".into(), "off".into());
    env.insert("AAC_STORE_SHARE_BACKUP_DIR".into(), "off".into());
    env.insert("AAC_CURSOR_BILLING_ENABLED".into(), "0".into());
    env.insert("AAC_CURSOR_API_BASE".into(), "http://127.0.0.1:1".into());
    env.insert("SSH_AUTH_SOCK".into(), String::new());
    env
}

fn write_stubs(stubs_dir: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(stubs_dir)?;
    for name in ["ssh", "simlock", "browser-check", "pm-mail"] {
        let file = stubs_dir.join(name);
        fs::write(&file, "#!/bin/sh\nexit 1\n")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Verify required built artifacts exist (`dist/native/ai-account-center` + `dist/ui/app/index.html`).
///
/// Drops the legacy `dist/native/aac-store.*.node` requirement per `AUDIT-scripts.md` §9
/// because `aac-xtask` links `aac-store` directly in Rust.
pub fn check_built_artifacts(repo_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let bin_name = if cfg!(windows) {
        "ai-account-center.exe"
    } else {
        "ai-account-center"
    };
    let launcher = repo_root.join("dist").join("native").join(bin_name);
    let ui_entry = repo_root
        .join("dist")
        .join("ui")
        .join("app")
        .join("index.html");
    let mut missing = Vec::new();
    if !launcher.exists() {
        missing.push(format!("dist/native/{bin_name}"));
    }
    if !ui_entry.exists() {
        missing.push("dist/ui/app/index.html".to_string());
    }
    if !missing.is_empty() {
        return Err(format!(
            "Missing built artifacts ({}). Run \"cargo run -p aac-xtask -- build\" (or \"bun run build\") on a Rust build host first.",
            missing.join(", ")
        )
        .into());
    }
    Ok(launcher)
}

fn get_free_port() -> Result<u16, Box<dyn Error>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn sleep_interruptible(duration: Duration, abort: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while !abort.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

fn wait_for_health(port: u16, abort: &AtomicBool, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut client = HttpClient::new(port, false);
    while !abort.load(Ordering::Acquire) && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Ok(resp) = client.request(
            "GET",
            "/api/health",
            &[],
            None,
            remaining.min(Duration::from_secs(2)),
            abort,
        ) && resp.status == 200
        {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        sleep_interruptible(remaining.min(Duration::from_millis(200)), abort);
    }
    if abort.load(Ordering::Acquire) {
        return Err("Dashboard health check aborted.".into());
    }
    Err(format!(
        "Dashboard server did not answer /api/health within {} ms.",
        timeout.as_millis()
    )
    .into())
}

fn sign_in_loopback(port: u16, abort: &AtomicBool) -> Result<String, Box<dyn Error>> {
    let origin = format!("http://127.0.0.1:{port}");
    let host = format!("127.0.0.1:{port}");
    let mut client = HttpClient::new(port, false);
    let setup_check = client.request(
        "GET",
        "/api/auth/setup",
        &[
            ("Origin", &origin),
            ("Host", &host),
            ("Accept", "application/json"),
        ],
        None,
        Duration::from_secs(60),
        abort,
    )?;
    if setup_check.status != 200 {
        return Err(format!("GET /api/auth/setup answered {}", setup_check.status).into());
    }
    let parsed: Json = serde_json::from_slice(&setup_check.body)?;
    let configured = parsed
        .get("configured")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let target = if configured {
        "/api/auth/login"
    } else {
        "/api/auth/setup"
    };
    let payload = if configured {
        serde_json::to_vec(&serde_json::json!({
            "username": FIXTURE_USER,
            "password": FIXTURE_PASS,
            "rememberMe": true,
        }))?
    } else {
        serde_json::to_vec(&serde_json::json!({
            "username": FIXTURE_USER,
            "password": FIXTURE_PASS,
        }))?
    };
    let reply = client.request(
        "POST",
        target,
        &[
            ("Origin", &origin),
            ("Host", &host),
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
        ],
        Some(&payload),
        Duration::from_secs(60),
        abort,
    )?;
    let cookie: String = reply
        .set_cookies
        .iter()
        .filter_map(|v| v.split(';').next().map(str::trim))
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if reply.status < 200 || reply.status >= 300 || cookie.is_empty() {
        let preview = String::from_utf8_lossy(&reply.body);
        let snippet: String = preview.chars().take(200).collect();
        return Err(format!("POST {target} failed (status={}): {snippet}", reply.status).into());
    }
    Ok(cookie)
}

fn verify_analytics_reply(url: &Url, body: &[u8], encoding: &str) -> Result<(), Box<dyn Error>> {
    let decoded = decode_content_encoding(encoding, body)?;
    let payload: Json = serde_json::from_slice(&decoded)?;
    if payload.get("stale").and_then(Json::as_bool) == Some(true)
        || payload
            .get("history")
            .and_then(|h| h.get("status"))
            .and_then(Json::as_str)
            != Some("ok")
    {
        return Err("Analytics quota history is unavailable.".into());
    }
    let act_status = payload
        .get("activity")
        .and_then(|a| a.get("status"))
        .and_then(Json::as_str);
    if !matches!(act_status, Some("ok" | "cached")) {
        return Err("Analytics activity is unavailable.".into());
    }
    let expected_range = url
        .query_pairs()
        .find(|(k, _)| k == "range")
        .map(|(_, v)| v.into_owned());
    let actual_range = payload
        .get("range")
        .and_then(|r| r.get("preset"))
        .and_then(Json::as_str)
        .map(ToString::to_string);
    let sample_count = payload
        .get("history")
        .and_then(|h| h.get("sampleCount"))
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let by_day_non_empty = payload
        .get("activity")
        .and_then(|a| a.get("byDay"))
        .and_then(Json::as_array)
        .is_some_and(|arr| !arr.is_empty());

    if actual_range != expected_range
        || !sample_count.is_finite()
        || sample_count <= 0.0
        || !by_day_non_empty
    {
        return Err(
            "Analytics reply is missing quota or activity data for the requested range.".into(),
        );
    }
    Ok(())
}

fn run_poll_loop(
    route: &PollRoute,
    port: u16,
    cookie: &str,
    poll_start: Instant,
    duration_sec: f64,
    abort: &AtomicBool,
) -> Vec<RouteRecord> {
    let mut records = Vec::new();
    let mut client = HttpClient::new(port, true);
    let parsed_url = Url::parse(&format!("http://127.0.0.1{}", route.path)).ok();
    let scheduled = expected_route_calls(route, duration_sec);
    let dispatch_deadline = poll_start
        + Duration::from_secs_f64(duration_sec)
        + Duration::from_millis(POLL_DISPATCH_GRACE_MS);
    let origin = format!("http://127.0.0.1:{port}");
    let host = format!("127.0.0.1:{port}");

    for slot in 0..scheduled {
        let due_offset = route.offset_sec + (slot as f64) * route.period_sec;
        let due = poll_start + Duration::from_secs_f64(due_offset.max(0.0));
        let now = Instant::now();
        if due > now {
            sleep_interruptible(due.saturating_duration_since(now), abort);
        }
        let call_start = Instant::now();
        if abort.load(Ordering::Acquire) || call_start >= dispatch_deadline {
            break;
        }
        let mut status: i32 = 0;
        let mut bytes: usize = 0;
        let mut encoding = String::new();
        let mut error: Option<String> = None;

        let res = client.request(
            "GET",
            &route.path,
            &[
                ("Accept", "application/json"),
                ("Accept-Encoding", "gzip, deflate, br"),
                ("Origin", &origin),
                ("Host", &host),
                ("Cookie", cookie),
            ],
            None,
            Duration::from_secs(60),
            abort,
        );
        match res {
            Ok(resp) => {
                status = i32::from(resp.status);
                bytes = resp.body.len();
                encoding = resp
                    .headers
                    .get("content-encoding")
                    .cloned()
                    .unwrap_or_default();
                if status == 200
                    && let Some(ref url) = parsed_url
                    && url.path() == "/api/accounts/analytics"
                    && let Err(err) = verify_analytics_reply(url, &resp.body, &encoding)
                {
                    error = Some(err.to_string());
                }
            }
            Err(err) => {
                if abort.load(Ordering::Acquire) {
                    break;
                }
                if status == 0 {
                    status = -1;
                }
                error = Some(err.to_string());
            }
        }
        let ms = call_start.elapsed().as_millis() as u64;
        records.push(RouteRecord {
            t_sec: to_fixed_num(call_start.duration_since(poll_start).as_secs_f64(), 2),
            route: route.name.clone(),
            status,
            bytes,
            encoding,
            ms,
            error,
        });
    }
    records
}

fn stop_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn current_hostname() -> String {
    #[cfg(unix)]
    {
        if let Ok(name) = nix::unistd::gethostname() {
            return name.to_string_lossy().into_owned();
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string())
}

struct ChildGuard {
    child: Option<Arc<Mutex<Child>>>,
    stopping: Arc<AtomicBool>,
    sampler_stop: Arc<AtomicBool>,
    run_root: PathBuf,
    keep: bool,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.sampler_stop.store(true, Ordering::Release);
        self.stopping.store(true, Ordering::Release);
        if let Some(child) = self.child.take()
            && let Ok(mut guard) = child.lock()
        {
            stop_child(&mut guard);
        }
        if !self.keep {
            let _ = fs::remove_dir_all(&self.run_root);
        }
    }
}

/// Run the full memory budget benchmark.
pub fn run_memory_budget(
    repo_root: &Path,
    opts: &RunOptions,
) -> Result<BudgetSummary, Box<dyn Error>> {
    let abort = Arc::new(AtomicBool::new(false));
    run_memory_budget_with_abort(repo_root, opts, abort)
}

/// Run the full memory budget benchmark with an external abort flag (used for CLI SIGINT/SIGTERM).
pub fn run_memory_budget_with_abort(
    repo_root: &Path,
    opts: &RunOptions,
    external_abort: Arc<AtomicBool>,
) -> Result<BudgetSummary, Box<dyn Error>> {
    if cfg!(not(target_os = "linux")) {
        return Err(format!(
            "memory-budget requires Linux /proc sampling (current platform: {}).",
            std::env::consts::OS
        )
        .into());
    }
    let poll_plan = opts.poll_plan.clone().unwrap_or_else(default_poll_plan);
    validate_run_options(opts, &poll_plan)?;

    let threshold_config = if opts.check {
        Some(load_threshold_config(&opts.config_path)?)
    } else {
        None
    };

    let wall_start = Instant::now();
    let launcher_bin = match &opts.launcher_path {
        Some(path) => path.clone(),
        None => check_built_artifacts(repo_root)?,
    };

    let temp_dir = tempfile::Builder::new()
        .prefix("aac-perf-memory-")
        .tempdir()?;
    let run_root = temp_dir.keep();
    let stopping = Arc::new(AtomicBool::new(false));
    let sampler_stop = Arc::new(AtomicBool::new(false));
    let mut guard = ChildGuard {
        child: None,
        stopping: Arc::clone(&stopping),
        sampler_stop: Arc::clone(&sampler_stop),
        run_root: run_root.clone(),
        keep: opts.keep,
    };

    // 1. Prepare fixture
    let fixture_start = Instant::now();
    struct LoadedFixture {
        seed: u64,
        counts: Json,
        store: Option<Json>,
        home: PathBuf,
        ccs_dir: PathBuf,
        reports_dir: PathBuf,
        registry_dir: PathBuf,
    }
    let fixture_root = run_root.join("fixture");
    let fixture = if let Some(src_dir) = &opts.fixture_dir {
        assert_no_symlinks(src_dir)?;
        copy_dir_no_symlinks(src_dir, &fixture_root)?;
        assert_no_symlinks(&fixture_root)?;
        let manifest: Json = serde_json::from_str(&fs::read_to_string(
            fixture_root.join("fixture-manifest.json"),
        )?)?;
        let ccs_dir = fixture_root.join("home").join(".ccs");
        let store_path = ccs_dir.join("store").join("aac.redb");
        if opts.check && (manifest.get("store").is_none_or(Json::is_null) || !store_path.is_file())
        {
            return Err("Check mode requires a migrated store in --fixture-dir.".into());
        }
        LoadedFixture {
            seed: manifest
                .get("seed")
                .and_then(Json::as_u64)
                .unwrap_or(opts.seed),
            counts: manifest
                .get("counts")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            store: manifest.get("store").cloned().filter(|v| !v.is_null()),
            home: fixture_root.join("home"),
            ccs_dir,
            reports_dir: fixture_root.join("agent-reports"),
            registry_dir: fixture_root.join("agent-reports").join("pm-mail-registry"),
        }
    } else if opts.launcher_path.is_some() {
        let home = fixture_root.join("home");
        let ccs_dir = home.join(".ccs");
        let reports_dir = fixture_root.join("agent-reports");
        let registry_dir = reports_dir.join("pm-mail-registry");
        fs::create_dir_all(&ccs_dir)?;
        fs::create_dir_all(&registry_dir)?;
        LoadedFixture {
            seed: opts.seed,
            counts: serde_json::json!({}),
            store: None,
            home,
            ccs_dir,
            reports_dir,
            registry_dir,
        }
    } else {
        let fopts = FixtureOptions {
            seed: opts.seed,
            ..FixtureOptions::default()
        };
        let manifest = generate_memory_fixture(repo_root, &fixture_root, &fopts)?;
        assert_resolved_inside(&run_root, &manifest.root)?;
        LoadedFixture {
            seed: manifest.seed,
            counts: serde_json::to_value(&manifest.counts)?,
            store: manifest.store,
            home: manifest.home,
            ccs_dir: manifest.ccs_dir,
            reports_dir: manifest.reports_dir,
            registry_dir: manifest.registry_dir,
        }
    };
    let fixture_ms = fixture_start.elapsed().as_millis() as u64;

    for candidate in [
        &fixture.home,
        &fixture.ccs_dir,
        &fixture.reports_dir,
        &fixture.registry_dir,
    ] {
        assert_resolved_inside(&fixture_root, candidate)?;
        if !fs::symlink_metadata(candidate)?.file_type().is_dir() {
            return Err(
                format!("Fixture root must be a directory: {}", candidate.display()).into(),
            );
        }
    }

    let stubs_dir = run_root.join("stubs");
    let xdg_runtime_dir = run_root.join("xdg");
    let logs_dir = run_root.join("logs");
    write_stubs(&stubs_dir)?;
    fs::create_dir_all(&xdg_runtime_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&xdg_runtime_dir, fs::Permissions::from_mode(0o700))?;
    }
    fs::create_dir_all(&logs_dir)?;

    let port = get_free_port()?;
    let env = build_isolated_env(
        &fixture.home,
        &fixture.ccs_dir,
        &fixture.reports_dir,
        &fixture.registry_dir,
        &stubs_dir,
        &xdg_runtime_dir,
    );

    let stdout_file = File::create(logs_dir.join("server-stdout.log"))?;
    let stderr_file = File::create(logs_dir.join("server-stderr.log"))?;
    let boot_start = Instant::now();
    let launch_args: Vec<String> = match &opts.launcher_args {
        Some(template) => template
            .iter()
            .map(|a| {
                a.replace("{port}", &port.to_string())
                    .replace("{ccs_dir}", &fixture.ccs_dir.to_string_lossy())
            })
            .collect(),
        None => vec![
            "dashboard".into(),
            "--no-open".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--config-dir".into(),
            fixture.ccs_dir.to_string_lossy().into_owned(),
        ],
    };

    let mut cmd = Command::new(&launcher_bin);
    cmd.args(&launch_args)
        .current_dir(repo_root)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    let child = cmd
        .spawn()
        .map_err(|err| format!("Dashboard server failed during launch: {err}"))?;
    let child_pid = child.id() as i32;
    let child_arc = Arc::new(Mutex::new(child));
    guard.child = Some(Arc::clone(&child_arc));

    let unexpected_exit: Arc<Mutex<Option<ChildExitInfo>>> = Arc::new(Mutex::new(None));
    let abort = Arc::new(AtomicBool::new(false));

    // Watcher thread: detects unexpected exit or external abort immediately.
    let watcher_child = Arc::clone(&child_arc);
    let watcher_exit = Arc::clone(&unexpected_exit);
    let watcher_abort = Arc::clone(&abort);
    let watcher_ext = Arc::clone(&external_abort);
    let watcher_stopping = Arc::clone(&stopping);
    let watcher_handle = thread::spawn(move || {
        while !watcher_stopping.load(Ordering::Acquire) {
            if watcher_ext.load(Ordering::Acquire) {
                watcher_abort.store(true, Ordering::Release);
                break;
            }
            let exited = watcher_child
                .lock()
                .ok()
                .and_then(|mut c| c.try_wait().ok().flatten());
            if let Some(status) = exited {
                if !watcher_stopping.load(Ordering::Acquire) {
                    if let Ok(mut slot) = watcher_exit.lock()
                        && slot.is_none()
                    {
                        *slot = Some(exit_status_info(status));
                    }
                    watcher_abort.store(true, Ordering::Release);
                }
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });

    let assert_child_running = |phase: &str| -> Result<(), Box<dyn Error>> {
        if let Ok(mut c) = child_arc.lock()
            && let Ok(Some(status)) = c.try_wait()
            && let Ok(mut slot) = unexpected_exit.lock()
            && slot.is_none()
        {
            *slot = Some(exit_status_info(status));
        }
        if let Ok(slot) = unexpected_exit.lock()
            && let Some(ref info) = *slot
        {
            return Err(format!(
                "Dashboard server exited unexpectedly {phase} ({}).",
                info.reason
            )
            .into());
        }
        Ok(())
    };

    let samples: Arc<Mutex<Vec<MemorySample>>> = Arc::new(Mutex::new(Vec::new()));
    let poll_start_slot: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    let take_sample =
        move |samples: &Arc<Mutex<Vec<MemorySample>>>,
              poll_start_slot: &Arc<Mutex<Option<Instant>>>,
              unexpected_exit: &Arc<Mutex<Option<ChildExitInfo>>>| {
            if unexpected_exit.lock().ok().is_some_and(|s| s.is_some()) {
                return;
            }
            let elapsed_sec = match poll_start_slot.lock().ok().and_then(|g| *g) {
                None => -1.0,
                Some(start) => start.elapsed().as_secs_f64(),
            };
            if let Some(sample) = sample_process_tree(child_pid, elapsed_sec)
                && let Ok(mut vec) = samples.lock()
            {
                vec.push(sample);
            }
        };

    take_sample(&samples, &poll_start_slot, &unexpected_exit);
    let sample_interval = Duration::from_secs_f64(opts.sample_interval_sec);
    let s_samples = Arc::clone(&samples);
    let s_poll_start = Arc::clone(&poll_start_slot);
    let s_exit = Arc::clone(&unexpected_exit);
    let s_stop = Arc::clone(&sampler_stop);
    let sampler_handle = thread::spawn(move || {
        while !s_stop.load(Ordering::Acquire) {
            sleep_interruptible(sample_interval, &s_stop);
            if s_stop.load(Ordering::Acquire) {
                break;
            }
            take_sample(&s_samples, &s_poll_start, &s_exit);
        }
    });

    let run_result = (|| -> Result<BudgetSummary, Box<dyn Error>> {
        if let Err(err) = wait_for_health(port, &abort, Duration::from_secs(60)) {
            assert_child_running("during the health check")?;
            return Err(err);
        }
        assert_child_running("during the health check")?;
        let boot_ms = boot_start.elapsed().as_millis() as u64;

        let cookie = match sign_in_loopback(port, &abort) {
            Ok(c) => c,
            Err(err) => {
                assert_child_running("during loopback sign-in")?;
                return Err(err);
            }
        };
        assert_child_running("during loopback sign-in")?;

        // Optional test hook: kill child after `kill_after_ms`
        let killer_handle = opts.kill_after_ms.map(|delay_ms| {
            let kill_abort = Arc::clone(&abort);
            thread::spawn(move || {
                sleep_interruptible(Duration::from_millis(delay_ms), &kill_abort);
                #[cfg(unix)]
                {
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(child_pid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
            })
        });

        let poll_start = Instant::now();
        if let Ok(mut slot) = poll_start_slot.lock() {
            *slot = Some(poll_start);
        }
        take_sample(&samples, &poll_start_slot, &unexpected_exit);

        let mut poll_handles = Vec::with_capacity(poll_plan.len());
        for route in &poll_plan {
            let route_clone = route.clone();
            let cookie_clone = cookie.clone();
            let abort_clone = Arc::clone(&abort);
            let duration_sec = opts.duration_sec;
            poll_handles.push(thread::spawn(move || {
                run_poll_loop(
                    &route_clone,
                    port,
                    &cookie_clone,
                    poll_start,
                    duration_sec,
                    &abort_clone,
                )
            }));
        }

        let target_end = poll_start + Duration::from_secs_f64(opts.duration_sec);
        let remaining = target_end.saturating_duration_since(Instant::now());
        sleep_interruptible(remaining, &abort);

        let mut route_records = Vec::new();
        for handle in poll_handles {
            if let Ok(recs) = handle.join() {
                route_records.extend(recs);
            }
        }
        if let Some(kh) = killer_handle {
            let _ = kh.join();
        }

        take_sample(&samples, &poll_start_slot, &unexpected_exit);
        sampler_stop.store(true, Ordering::Release);

        // Check liveness if watcher hasn't caught an exit yet.
        let has_exit = unexpected_exit.lock().ok().is_some_and(|s| s.is_some());
        if !has_exit && !is_pid_alive(child_pid) {
            let wait_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < wait_deadline {
                if let Ok(mut c) = child_arc.lock()
                    && let Ok(Some(status)) = c.try_wait()
                {
                    if let Ok(mut slot) = unexpected_exit.lock()
                        && slot.is_none()
                    {
                        *slot = Some(exit_status_info(status));
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            if unexpected_exit.lock().ok().is_none_or(|s| s.is_none()) {
                return Err(format!(
                    "Dashboard server died unexpectedly during run (pid={child_pid})."
                )
                .into());
            }
        }
        assert_child_running("during run")?;
        if external_abort.load(Ordering::Acquire) {
            return Err("Memory budget run interrupted by signal.".into());
        }

        let collected_samples = samples.lock().map(|v| v.clone()).unwrap_or_default();
        let memory = summarize_samples(&collected_samples, opts.steady_after_sec, !opts.check)?;
        let routes = summarize_routes(&route_records, &poll_plan);
        let route_failures =
            evaluate_route_failures(&routes, opts.duration_sec, &poll_plan, opts.check);
        let sample_failures = if opts.check {
            evaluate_sample_failures(
                &memory,
                opts.duration_sec,
                opts.steady_after_sec,
                opts.sample_interval_sec,
            )
        } else {
            Vec::new()
        };
        let threshold = match &threshold_config {
            Some(cfg) => Some(evaluate_thresholds(&memory, cfg)?),
            None => None,
        };
        let ok = route_failures.is_empty()
            && sample_failures.is_empty()
            && threshold.as_ref().is_none_or(|t| t.ok);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let summary = BudgetSummary {
            schema_version: 1,
            ok,
            measured_at: iso_ms(now_ms),
            host: current_hostname(),
            seed: fixture.seed,
            duration_sec: opts.duration_sec,
            sample_interval_sec: opts.sample_interval_sec,
            timing_ms: TimingMs {
                fixture: fixture_ms,
                boot: boot_ms,
                poll: poll_start.elapsed().as_millis() as u64,
                total: wall_start.elapsed().as_millis() as u64,
            },
            fixture_counts: fixture.counts,
            store: fixture.store,
            memory,
            routes,
            route_failures,
            sample_failures,
            threshold,
            run_root: if opts.keep {
                Some(run_root.clone())
            } else {
                None
            },
        };

        if let Some(out_path) = &opts.out_path {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut text = serde_json::to_string_pretty(&summary)?;
            text.push('\n');
            fs::write(out_path, text)?;
        }

        Ok(summary)
    })();

    sampler_stop.store(true, Ordering::Release);
    stopping.store(true, Ordering::Release);
    let _ = sampler_handle.join();
    let _ = watcher_handle.join();
    drop(guard);
    run_result
}

/// Format a human-readable console summary matching `formatHumanSummary` in `memory-budget.mjs`.
pub fn format_human_summary(summary: &BudgetSummary) -> String {
    let m = &summary.memory;
    let num = |v: f64| aac_num::ecma::to_string(v);
    let pss = |value: Option<f64>| -> String {
        match value {
            None => "unavailable".to_string(),
            Some(v) => format!("{} MiB", num(v)),
        }
    };
    let mut lines = vec![
        format!(
            "[i] AAC memory budget run ({}s, seed={}, samples={})",
            num(summary.duration_sec),
            summary.seed,
            m.sample_count
        ),
        format!(
            "    timing: fixture={:.1}s boot={:.2}s total={:.1}s",
            (summary.timing_ms.fixture as f64) / 1000.0,
            (summary.timing_ms.boot as f64) / 1000.0,
            (summary.timing_ms.total as f64) / 1000.0
        ),
        format!(
            "    server steady (t>={}s): RSS={} MiB, PSS={}",
            num(m.steady_after_sec),
            num(m.steady_median_rss_mib),
            pss(m.steady_median_pss_mib)
        ),
        format!(
            "    server peak:               RSS={} MiB, PSS={}",
            num(m.peak_rss_mib),
            pss(m.peak_pss_mib)
        ),
        format!(
            "    children peak:             RSS={} MiB, PSS={} (max children={})",
            num(m.children_peak_rss_mib),
            pss(m.children_peak_pss_mib),
            m.max_children
        ),
        format!(
            "    total peak:                RSS={} MiB, PSS={}",
            num(m.total_peak_rss_mib),
            pss(m.total_peak_pss_mib)
        ),
        format!("    {}", m.sampling_note),
    ];

    for (name, row) in &summary.routes {
        lines.push(format!(
            "    route {:<13} {:>2}/{:<2} ok  bytes={:>8}  first={:>5}ms  p50={:>5}ms  max={:>5}ms",
            name,
            row.ok,
            row.calls,
            row.median_bytes,
            row.first_ms,
            num(row.p50_ms),
            row.max_ms
        ));
    }
    if !summary.route_failures.is_empty() {
        lines.push(format!(
            "[X] Route failures: {}",
            summary.route_failures.join("; ")
        ));
    }
    if !summary.sample_failures.is_empty() {
        lines.push(format!(
            "[X] Sample failures: {}",
            summary.sample_failures.join("; ")
        ));
    }
    if let Some(threshold) = &summary.threshold {
        if threshold.ok {
            lines.push("[OK] Memory within all configured limits:".to_string());
            for r in &threshold.results {
                let sign = if r.delta_mib >= 0.0 { "+" } else { "" };
                lines.push(format!(
                    "     {}: {} MiB <= {} MiB ({sign}{} MiB)",
                    r.metric,
                    num(r.measured_mib),
                    num(r.limit_mib),
                    num(r.delta_mib)
                ));
            }
        } else {
            lines.push("[X] Memory budget EXCEEDED:".to_string());
            for v in &threshold.violations {
                let kib_detail = if v.delta_kib > 0.0 && v.delta_kib < KIB_PER_MIB {
                    format!(" / +{} KiB", num(v.delta_kib))
                } else {
                    String::new()
                };
                lines.push(format!(
                    "     EXCEEDED {}: {} MiB > limit {} MiB (+{} MiB{kib_detail}, +{}%)",
                    v.metric,
                    num(v.measured_mib),
                    num(v.limit_mib),
                    num(v.delta_mib),
                    num(v.pct_over)
                ));
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Parse CLI flags for `aac-xtask perf [memory-budget]`.
pub fn parse_cli(repo_root: &Path, args: &[String]) -> Result<(RunOptions, bool), Box<dyn Error>> {
    let mut opts = RunOptions::default_for(repo_root);
    let mut help = false;
    let cwd = std::env::current_dir()?;
    let resolve = |raw: &str| -> PathBuf {
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { cwd.join(p) }
    };

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let next = |idx: &mut usize, flag: &str| -> Result<&str, Box<dyn Error>> {
            *idx += 1;
            args.get(*idx)
                .map(String::as_str)
                .ok_or_else(|| format!("Missing value for {flag}").into())
        };
        match arg {
            "--duration" => opts.duration_sec = next(&mut i, arg)?.parse()?,
            "--steady-after" => opts.steady_after_sec = next(&mut i, arg)?.parse()?,
            "--sample-interval" => opts.sample_interval_sec = next(&mut i, arg)?.parse()?,
            "--seed" => opts.seed = next(&mut i, arg)?.parse()?,
            "--config" => opts.config_path = resolve(next(&mut i, arg)?),
            "--out" => opts.out_path = Some(resolve(next(&mut i, arg)?)),
            "--fixture-dir" => opts.fixture_dir = Some(resolve(next(&mut i, arg)?)),
            "--keep" => opts.keep = true,
            "--no-check" | "--measure-only" => opts.check = false,
            "--json" => opts.json = true,
            "-h" | "--help" => help = true,
            other => return Err(format!("Unknown argument: {other}").into()),
        }
        i += 1;
    }
    Ok((opts, help))
}

/// CLI entrypoint for `aac-xtask perf [memory-budget]`.
pub fn run_cli(repo_root: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    let (opts, help) = parse_cli(repo_root, args)?;
    if help {
        println!(
            "Usage: aac-xtask perf [memory-budget] [--duration 300] [--steady-after 120] [--sample-interval 2] [--seed 20261125] [--config config/memory-budget.json] [--out <file.json>] [--fixture-dir <dir>] [--keep] [--no-check] [--json]"
        );
        return Ok(());
    }

    let abort = Arc::new(AtomicBool::new(false));
    let signal_exit = Arc::new(AtomicI32::new(0));
    let sig_abort = Arc::clone(&abort);
    let sig_code = Arc::clone(&signal_exit);
    let _sig_thread = thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigint = signal(SignalKind::interrupt()).ok();
                let mut sigterm = signal(SignalKind::terminate()).ok();
                tokio::select! {
                    _ = async {
                        if let Some(ref mut s) = sigint {
                            s.recv().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        sig_code.store(130, Ordering::Release);
                        sig_abort.store(true, Ordering::Release);
                    }
                    _ = async {
                        if let Some(ref mut s) = sigterm {
                            s.recv().await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {
                        sig_code.store(143, Ordering::Release);
                        sig_abort.store(true, Ordering::Release);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
                sig_code.store(130, Ordering::Release);
                sig_abort.store(true, Ordering::Release);
            }
        });
    });

    let result = run_memory_budget_with_abort(repo_root, &opts, abort);
    let caught = signal_exit.load(Ordering::Acquire);
    if caught != 0 {
        return Err(Box::new(SilentExit(caught)));
    }
    let summary = result?;
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print!("{}", format_human_summary(&summary));
    }
    if summary.ok {
        Ok(())
    } else {
        Err(Box::new(SilentExit(1)))
    }
}
