//! G16c: the machine readings the Status feed carries (CPU, memory, the disk the home folder
//! is on).
//!
//! A port of `status-collectors.ts`: the one read-only probe each remote host runs, the local
//! reading's shape (`readLocalMachine`), the parser and the busy percent between two Linux
//! counts. A part that failed is named and keeps the earlier reading in the service; a probe
//! without its disk or memory is a partial reading, never a zero.

use super::browsers::short_text;
use serde::Serialize;
use serde_json::Value as Json;

/// The probe's platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Linux,
    Mac,
    Windows,
}

impl HostOs {
    /// The platform word the other readers use (`linux`, `mac`, `windows`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Mac => "mac",
            Self::Windows => "windows",
        }
    }

    /// The registry's `os` word.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        match word {
            "linux" => Some(Self::Linux),
            "mac" => Some(Self::Mac),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// The disk the home folder is on, as one reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub mount: String,
    pub free_bytes: f64,
    pub total_bytes: f64,
}

/// One machine reading. Unknown readings are null; a failed part names itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub cores: Option<i64>,
    pub load1: Option<f64>,
    /// CPU busy percent; null until two readings exist on a host that reports CPU times.
    pub busy_pct: Option<i64>,
    pub mem_available_bytes: Option<f64>,
    pub mem_total_bytes: Option<f64>,
    /// The disk the home folder is on: agent work lives there, so a split layout's system
    /// disk says nothing about it.
    pub disk: Option<Disk>,
    /// Why that disk could not be read this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_error: Option<String>,
    /// Why the CPU was not read this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_error: Option<String>,
    /// Why memory was not reported this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_error: Option<String>,
    /// `/proc/stat` totals, for the busy percent between two readings.
    pub cpu_times: Option<CpuTimes>,
    /// Set by the service: when `busyPct` was read (null when it never was). The probe's own
    /// parse leaves these three out (`to_collected_json` adds them, as the collector does).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_read_at: Option<i64>,
    /// Set by the service: when `disk` was read (null when it never was).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_read_at: Option<i64>,
    /// Set by the service: when the memory numbers were read (null when they never were).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_read_at: Option<i64>,
}

impl Sample {
    /// The reading as `collectMachines` puts it in `data`: the probe's members, then `busyPct`
    /// (already a member) and the three read times, always present (null when never read).
    #[must_use]
    pub fn to_collected_json(&self) -> Json {
        let mut value = serde_json::to_value(self).unwrap_or(Json::Null);
        if let Some(object) = value.as_object_mut() {
            object.insert("cpuReadAt".into(), self.cpu_read_at.map_or(Json::Null, Json::from));
            object.insert("diskReadAt".into(), self.disk_read_at.map_or(Json::Null, Json::from));
            object.insert(
                "memoryReadAt".into(),
                self.memory_read_at.map_or(Json::Null, Json::from),
            );
        }
        value
    }
}

/// `/proc/stat`'s first line totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CpuTimes {
    pub busy: i64,
    pub total: i64,
}

const DISK_ERROR: &str = "The machine did not report its system disk.";
const MEMORY_ERROR: &str = "The machine did not report its memory.";
const CPU_ERROR: &str = "The machine did not report its CPU.";

const LINUX_PROBE: &str = r#"export LC_ALL=C; printf 'cores=%s\n' "$(getconf _NPROCESSORS_ONLN)"; printf 'load=%s\n' "$(cut -d' ' -f1 /proc/loadavg)"; awk '/^MemTotal:/{print "memTotalKb="$2} /^MemAvailable:/{print "memAvailKb="$2}' /proc/meminfo; head -n1 /proc/stat | sed 's/^cpu */cpu=/'; df -Pk "$HOME" | awk 'NR==2{print "diskTotalKb="$2; print "diskFreeKb="$4; print "mount="$6}'"#;

/// The Mac memory half of the probe, run alone for a local reading (`readLocalMachine`).
pub const MAC_MEMORY_PROBE: &str = r#"t=$(sysctl -n hw.memsize 2>/dev/null) || t=; case "$t" in ''|*[!0-9]*) t=;; esac; printf 'memTotalB=%s\n' "$t"; m=$(sysctl -n kern.memorystatus_level 2>/dev/null) || m=; case "$m" in ''|*[!0-9]*) m=;; *) [ "$m" -le 100 ] 2>/dev/null || m=;; esac; if [ -z "$m" ]; then p=$(memory_pressure 2>/dev/null) && m=$(printf '%s\n' "$p" | awk '{sub(/\r+$/, "")} /^System-wide memory free percentage:[[:space:]]+[0-9]+%[[:space:]]*$/ {v=$5+0; if (v<=100) {if (!have) {have=1; val=v} else if (val!=v) bad=1}} END {if (have && !bad) print val}'); fi; printf 'memAvailPct=%s\n' "$m""#;

const MAC_PROBE: &str = r#"export LC_ALL=C; printf 'cores=%s\n' "$(sysctl -n hw.ncpu)"; printf 'load=%s\n' "$(sysctl -n vm.loadavg | awk '{print $2}')"; t=$(sysctl -n hw.memsize 2>/dev/null) || t=; case "$t" in ''|*[!0-9]*) t=;; esac; printf 'memTotalB=%s\n' "$t"; m=$(sysctl -n kern.memorystatus_level 2>/dev/null) || m=; case "$m" in ''|*[!0-9]*) m=;; *) [ "$m" -le 100 ] 2>/dev/null || m=;; esac; if [ -z "$m" ]; then p=$(memory_pressure 2>/dev/null) && m=$(printf '%s\n' "$p" | awk '{sub(/\r+$/, "")} /^System-wide memory free percentage:[[:space:]]+[0-9]+%[[:space:]]*$/ {v=$5+0; if (v<=100) {if (!have) {have=1; val=v} else if (val!=v) bad=1}} END {if (have && !bad) print val}'); fi; printf 'memAvailPct=%s\n' "$m"; c=$(ps -A -o %cpu=) && p=$(printf '%s\n' "$c" | awk '$1 ~ /^[0-9]+([.][0-9]+)?$/ {s+=$1; n++} END {if (n) print s}') && [ -n "$p" ] && printf 'cpuPct=%s\n' "$p"; d=/System/Volumes/Data; [ -d "$d" ] || d=/; df -Pk "$d" | awk 'NR==2{print "diskTotalKb="$2; print "diskFreeKb="$4; print "mount="$6}'"#;

const WINDOWS_PROBE_SCRIPT: &str = "$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'; $os = Get-CimInstance Win32_OperatingSystem; $cpu = @(Get-CimInstance Win32_Processor); $drive = $env:SystemDrive; $disk = Get-CimInstance Win32_LogicalDisk -Filter (\"DeviceID='\" + $drive + \"'\"); foreach ($p in $cpu) { \"coreCount=\" + $p.NumberOfLogicalProcessors }; foreach ($p in $cpu) { \"cpuLoad=\" + $p.LoadPercentage }; \"memTotalKb=\" + $os.TotalVisibleMemorySize; \"memAvailKb=\" + $os.FreePhysicalMemory; \"diskTotalB=\" + $disk.Size; \"diskFreeB=\" + $disk.FreeSpace; \"mount=\" + $drive";

/// `machineProbeCommand`: the one read-only command a host runs per reading.
#[must_use]
pub fn probe_command(host: HostOs) -> String {
    match host {
        HostOs::Linux => LINUX_PROBE.to_owned(),
        HostOs::Mac => MAC_PROBE.to_owned(),
        HostOs::Windows => format!(
            "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
            base64_utf16le(WINDOWS_PROBE_SCRIPT)
        ),
    }
}

fn base64_utf16le(text: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `linuxCpuTimes`: the busy and total counters of `/proc/stat`'s `cpu` line.
#[must_use]
pub fn linux_cpu_times(line: Option<&str>) -> Option<CpuTimes> {
    let line = line?;
    let values: Vec<f64> = line
        .split_whitespace()
        .map(|part| part.parse::<f64>().unwrap_or(f64::NAN))
        .collect();
    if values.len() < 4 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let total = values.iter().take(8).sum::<f64>();
    let idle = values[3] + values.get(4).copied().unwrap_or(0.0);
    #[allow(clippy::cast_possible_truncation)]
    Some(CpuTimes {
        busy: (total - idle) as i64,
        total: total as i64,
    })
}

fn probe_cores(stdout: &str, cores: Option<f64>) -> Option<i64> {
    let counts: Vec<&str> = stdout
        .split(['\r', '\n'])
        .filter(|line| line.starts_with("coreCount="))
        .map(|line| line["coreCount=".len()..].trim())
        .collect();
    if !counts.is_empty() {
        let mut sum = 0i64;
        for value in counts {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let Ok(count) = value.parse::<i64>() else {
                return None;
            };
            if count <= 0 {
                return None;
            }
            sum += count;
        }
        return Some(sum);
    }
    #[allow(clippy::cast_possible_truncation)]
    cores.filter(|n| *n > 0.0).map(|n| n as i64)
}

fn remote_busy_pct(host: HostOs, stdout: &str, cpu_pct: Option<f64>, cores: Option<i64>) -> Option<i64> {
    if host == HostOs::Windows {
        let loads: Vec<&str> = stdout
            .split(['\r', '\n'])
            .filter(|line| line.starts_with("cpuLoad="))
            .map(|line| line["cpuLoad=".len()..].trim())
            .collect();
        if !loads.is_empty() {
            let mut sum = 0.0;
            for value in &loads {
                if value.is_empty() {
                    return None;
                }
                let Ok(n) = value.parse::<f64>() else {
                    return None;
                };
                if !n.is_finite() || n < 0.0 {
                    return None;
                }
                sum += n;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let average = sum / loads.len() as f64;
            return Some(math_round_clamp(average, 100.0));
        }
    }
    let cpu_pct = cpu_pct?;
    if host == HostOs::Mac {
        #[allow(clippy::cast_precision_loss)]
        let cores = cores? as f64;
        return Some(math_round_clamp(cpu_pct / cores, 100.0));
    }
    Some(math_round_clamp(cpu_pct, 100.0))
}

fn math_round_clamp(value: f64, max: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let rounded = aac_num::ecma::math_round(value.min(max)) as i64;
    rounded
}

/// `parseMachineProbe`: a probe's `key=value` lines to a sample; null when the essentials
/// are missing.
#[must_use]
pub fn parse_probe(host: HostOs, stdout: &str) -> Option<Sample> {
    let mut values = std::collections::HashMap::new();
    for line in stdout.split(['\r', '\n']) {
        if let Some(at) = line.find('=')
            && at > 0
        {
            values.insert(line[..at].trim().to_owned(), line[at + 1..].trim().to_owned());
        }
    }
    let n = |key: &str| -> Option<f64> {
        let raw = values.get(key)?;
        if raw.is_empty() {
            return None;
        }
        let value = raw.parse::<f64>().ok()?;
        (value.is_finite() && value >= 0.0).then_some(value)
    };
    let cores = probe_cores(stdout, n("cores"));
    let kib = |key: &str| n(key).map(|value| value * 1024.0);
    let mut mem_total = kib("memTotalKb").or_else(|| n("memTotalB")).filter(|n| *n != 0.0);
    let mut mem_avail = kib("memAvailKb");
    if host == HostOs::Mac {
        let unsigned = |key: &str| -> Option<f64> {
            let raw = values.get(key).map(String::as_str).unwrap_or("");
            if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let value = raw.parse::<u64>().ok()?;
            let value = value as f64;
            (value > 0.0 && value <= 9_007_199_254_740_991.0).then_some(value)
        };
        mem_total = unsigned("memTotalB");
        let pct = unsigned("memAvailPct");
        mem_avail = match (mem_total, pct) {
            (Some(total), Some(pct)) if pct <= 100.0 => {
                if pct == 100.0 {
                    Some(total)
                } else {
                    Some(total * pct / 100.0)
                }
            }
            _ => None,
        };
    }
    let disk_total = kib("diskTotalKb").or_else(|| n("diskTotalB"));
    let disk_free = kib("diskFreeKb").or_else(|| n("diskFreeB"));
    if cores.is_none() && mem_total.is_none() && disk_total.is_none() {
        return None;
    }
    let cpu_pct = n("cpuPct");
    let cpu_times = if host == HostOs::Linux {
        linux_cpu_times(values.get("cpu").map(String::as_str))
    } else {
        None
    };
    let busy_pct = remote_busy_pct(host, stdout, cpu_pct, cores);
    let cpu_read = if host == HostOs::Linux {
        cpu_times.is_some()
    } else {
        busy_pct.is_some()
    };
    let disk_read = disk_total.is_some() && disk_free.is_some() && disk_total.unwrap_or(0.0) > 0.0;
    let disk = if disk_read {
        Some(Disk {
            mount: short_text(values.get("mount").map(|v| Json::String(v.clone())).as_ref(), 80)
                .unwrap_or_else(|| "/".to_owned()),
            free_bytes: disk_free.unwrap_or(0.0),
            total_bytes: disk_total.unwrap_or(0.0),
        })
    } else {
        None
    };
    Some(Sample {
        cores,
        load1: n("load"),
        busy_pct,
        mem_available_bytes: mem_avail,
        mem_total_bytes: mem_total,
        disk,
        disk_error: (!disk_read).then(|| DISK_ERROR.to_owned()),
        cpu_error: (!cpu_read).then(|| CPU_ERROR.to_owned()),
        memory_error: (mem_total.is_none() || mem_avail.is_none()).then(|| MEMORY_ERROR.to_owned()),
        cpu_times,
        cpu_read_at: None,
        disk_read_at: None,
        memory_read_at: None,
    })
}

/// `busyBetween`: the busy percent between two `/proc/stat` readings.
#[must_use]
pub fn busy_between(before: Option<CpuTimes>, after: Option<CpuTimes>) -> Option<i64> {
    let (before, after) = (before?, after?);
    let total = after.total - before.total;
    if total <= 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = (after.busy - before.busy) as f64 / total as f64;
    let percent = aac_num::ecma::math_round(ratio * 100.0);
    #[allow(clippy::cast_possible_truncation)]
    Some((percent.clamp(0.0, 100.0)) as i64)
}

/// Which host a local read runs on, as `platform` reads: `linux`, `mac`, `windows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Mac,
    Windows,
}

impl Platform {
    /// The platform word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Mac => "mac",
            Self::Windows => "windows",
        }
    }

    /// The registry's host kind for this platform.
    #[must_use]
    pub fn host_os(self) -> HostOs {
        match self {
            Self::Linux => HostOs::Linux,
            Self::Mac => HostOs::Mac,
            Self::Windows => HostOs::Windows,
        }
    }
}

/// The reads `readLocalMachine` takes from the host; tests inject them.
pub struct LocalReads<'a> {
    pub platform: Platform,
    /// `os.cpus().length`.
    pub cpus: usize,
    /// `os.totalmem()` / `os.freemem()` (null on the Mac: its probe answers).
    pub total_mem: Option<f64>,
    pub free_mem: Option<f64>,
    /// `os.loadavg()[0]`; unused on Windows.
    pub load1: Option<f64>,
    /// The file's text, or null as the read fails.
    pub read_file: &'a dyn Fn(&str) -> Option<String>,
    /// `os.homedir()`, the folder whose filesystem the reading reports.
    pub home: &'a dyn Fn() -> Option<String>,
    /// The `(free bytes, total bytes)` of the filesystem holding the path given, or null when
    /// it cannot be read.
    pub statfs: &'a dyn Fn(&str) -> Option<(f64, f64)>,
    /// The Mac memory probe's sample, when the caller ran it (its totals and the memory error).
    pub mac_memory: Option<Sample>,
}

/// `readLocalMachine`: this computer, read from the injected host facts. macOS reports
/// available memory through the probe; Linux's busy percent comes only from `/proc/stat`
/// counters, and without them it is a named failure.
#[must_use]
pub fn local_sample(reads: &LocalReads<'_>) -> Sample {
    let mac = reads.platform == Platform::Mac;
    let mut mem_total = if mac { None } else { reads.total_mem };
    let mut mem_avail = if mac { None } else { reads.free_mem };
    let mut memory_error = None;
    let mut cpu_times = None;
    let mut cpu_error = None;
    if mac {
        let sample = reads.mac_memory.as_ref();
        mem_total = sample.and_then(|s| s.mem_total_bytes);
        mem_avail = sample.and_then(|s| s.mem_available_bytes);
        if mem_total.is_none() || mem_avail.is_none() {
            memory_error = Some(MEMORY_ERROR.to_owned());
        }
    } else if let Some(meminfo) = (reads.read_file)("/proc/meminfo")
        && let Some(available) = mem_available_from(&meminfo)
    {
        mem_avail = Some(available);
    }
    if reads.platform == Platform::Linux {
        cpu_times = (reads.read_file)("/proc/stat")
            .and_then(|stat| cpu_line(&stat))
            .and_then(|line| linux_cpu_times(Some(&line)));
        if cpu_times.is_none() {
            cpu_error = Some("The CPU counters could not be read.".to_owned());
        }
    }
    // The disk the home folder is on: agent work lives there, and on a split layout (the
    // Odroid's eMMC system disk with /home on NVMe) the system disk's free space says nothing
    // about it.
    let home = (reads.home)();
    let disk_read = home.as_deref().and_then(reads.statfs);
    let (disk, disk_error) = match (disk_read, home) {
        (Some((free, total)), Some(home)) => (
            Some(Disk {
                mount: home,
                free_bytes: free,
                total_bytes: total,
            }),
            None,
        ),
        _ => (None, Some("The system disk could not be read.".to_owned())),
    };
    Sample {
        cores: (reads.cpus > 0).then(|| i64::try_from(reads.cpus).unwrap_or(i64::MAX)),
        load1: if reads.platform == Platform::Windows {
            None
        } else {
            reads.load1
        },
        busy_pct: None,
        mem_available_bytes: mem_avail,
        mem_total_bytes: mem_total,
        disk,
        disk_error,
        cpu_error,
        memory_error,
        cpu_times,
        cpu_read_at: None,
        disk_read_at: None,
        memory_read_at: None,
    }
}

/// `/^MemAvailable:\s+(\d+)\s+kB/m` on `/proc/meminfo`, in bytes.
#[must_use]
pub fn mem_available_from(meminfo: &str) -> Option<f64> {
    for line in meminfo.lines() {
        let Some(rest) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        let rest = rest.trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let after = &rest[digits.len()..];
        if !after.trim_start().starts_with("kB") {
            return None;
        }
        let kb = digits.parse::<f64>().ok()?;
        return Some(kb * 1024.0);
    }
    None
}

/// `/^cpu\s+(.*)$/m` on `/proc/stat`: the counters after the `cpu` label.
fn cpu_line(stat: &str) -> Option<String> {
    for line in stat.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        if rest.starts_with(char::is_whitespace) {
            return Some(rest.trim_start().to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_host_probe_and_works_out_the_busy_percent() {
        let linux = parse_probe(
            HostOs::Linux,
            "cores=6\nload=7.48\nmemTotalKb=20210000\nmemAvailKb=13245000\ncpu=100 0 50 800 50 0 0 0\ndiskTotalKb=812506724\ndiskFreeKb=528836920\nmount=/\n",
        )
        .unwrap();
        assert_eq!(linux.cores, Some(6));
        assert_eq!(linux.mem_available_bytes, Some(13_245_000.0 * 1024.0));
        assert_eq!(linux.cpu_times, Some(CpuTimes { busy: 150, total: 1000 }));
        assert_eq!(
            busy_between(
                Some(CpuTimes { busy: 150, total: 1000 }),
                Some(CpuTimes { busy: 250, total: 1200 })
            ),
            Some(50)
        );
        let mac = parse_probe(
            HostOs::Mac,
            "cores=8\nload=2.84\nmemTotalB=17179869184\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=482766932\ndiskFreeKb=18378524\nmount=/System/Volumes/Data\n",
        )
        .unwrap();
        assert_eq!(mac.mem_available_bytes, Some(11_510_512_353.28));
        assert_eq!(mac.busy_pct, Some(4));
        assert_eq!(mac.disk.as_ref().unwrap().mount, "/System/Volumes/Data");
        let windows = parse_probe(
            HostOs::Windows,
            "cores=32\r\ncpuPct=40\r\nmemTotalKb=33463884\r\nmemAvailKb=10291080\r\ndiskTotalB=1999342923776\r\ndiskFreeB=218592583680\r\nmount=C:\r\n",
        )
        .unwrap();
        assert_eq!(windows.load1, None);
        assert_eq!(windows.busy_pct, Some(40));
        assert_eq!(
            windows.disk,
            Some(Disk {
                mount: "C:".into(),
                free_bytes: 218_592_583_680.0,
                total_bytes: 1_999_342_923_776.0
            })
        );
        assert!(parse_probe(HostOs::Linux, "garbage").is_none());
        let no_disk = parse_probe(HostOs::Linux, "cores=6\nmemTotalKb=20210000\nmemAvailKb=13245000\n").unwrap();
        assert_eq!(no_disk.disk, None);
        assert_eq!(no_disk.disk_error.as_deref(), Some(DISK_ERROR));
        assert_eq!(no_disk.memory_error, None);
        let no_memory = parse_probe(
            HostOs::Windows,
            "cores=32\r\ndiskTotalB=100\r\ndiskFreeB=50\r\nmount=C:\r\n",
        )
        .unwrap();
        assert_eq!(no_memory.mem_available_bytes, None);
        assert_eq!(no_memory.memory_error.as_deref(), Some(MEMORY_ERROR));
        assert_eq!(no_memory.disk_error, None);
        assert!(
            probe_command(HostOs::Windows).starts_with("powershell.exe -NoProfile -NonInteractive -EncodedCommand ")
        );
        assert!(probe_command(HostOs::Linux).contains(r#"df -Pk "$HOME""#));
        assert!(!probe_command(HostOs::Linux).contains("df -Pk /"));
    }

    #[test]
    fn a_host_probe_whose_cpu_command_fails_reports_no_cpu_never_zero() {
        let no_cpu = parse_probe(
            HostOs::Linux,
            "cores=6\nmemTotalKb=2\nmemAvailKb=1\ndiskTotalKb=10\ndiskFreeKb=5\n",
        )
        .unwrap();
        assert_eq!(no_cpu.busy_pct, None);
        assert_eq!(no_cpu.cpu_error.as_deref(), Some(CPU_ERROR));
        let mac_no_cores = parse_probe(
            HostOs::Mac,
            "load=1\nmemTotalB=17179869184\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=10\ndiskFreeKb=5\n",
        )
        .unwrap();
        assert_eq!(mac_no_cores.busy_pct, None);
        assert_eq!(mac_no_cores.cpu_error.as_deref(), Some(CPU_ERROR));
    }

    #[test]
    fn mac_totals_reject_malformed_and_keep_the_rest() {
        assert!(parse_probe(HostOs::Mac, "memTotalB=1e308\nmemAvailPct=100\n").is_none());
        let malformed = parse_probe(HostOs::Mac, "cores=8\nmemTotalB=1e308\nmemAvailPct=100\n").unwrap();
        assert_eq!(malformed.cores, Some(8));
        assert_eq!(malformed.mem_total_bytes, None);
        assert_eq!(malformed.memory_error.as_deref(), Some(MEMORY_ERROR));
        let oversized = parse_probe(HostOs::Mac, "cores=8\nmemTotalB=18446744073709551615\nmemAvailPct=67\n").unwrap();
        assert_eq!(oversized.mem_available_bytes, None);
        let limit = parse_probe(HostOs::Mac, "memTotalB=9007199254740985\nmemAvailPct=100\n").unwrap();
        assert_eq!(limit.mem_available_bytes, Some(9_007_199_254_740_985.0));
        assert_eq!(limit.memory_error, None);
        let whole = parse_probe(HostOs::Mac, "memTotalB=17179869184\nmemAvailPct=100\n").unwrap();
        assert_eq!(whole.mem_available_bytes, Some(17_179_869_184.0));
        assert!(parse_probe(HostOs::Mac, "memTotalB=\nmemAvailPct=67\n").is_none());
        let partial = parse_probe(
            HostOs::Mac,
            "cores=8\nmemTotalB=\nmemAvailPct=67\ncpuPct=32\ndiskTotalKb=10\ndiskFreeKb=5\n",
        )
        .unwrap();
        assert_eq!(partial.mem_available_bytes, None);
        assert_eq!(partial.busy_pct, Some(4));
        assert_eq!(partial.disk.as_ref().unwrap().total_bytes, 10.0 * 1024.0);
    }

    #[test]
    fn local_reads_name_a_failed_disk_or_cpu_and_keep_the_rest() {
        let no_disk = local_sample(&LocalReads {
            platform: Platform::Linux,
            cpus: 8,
            total_mem: Some(2.0 * 1024.0 * 1024.0 * 1024.0),
            free_mem: Some(1024.0 * 1024.0 * 1024.0),
            load1: Some(1.2),
            read_file: &|file| match file {
                "/proc/meminfo" => Some("MemTotal: 2097152 kB\nMemAvailable: 1048576 kB\n".into()),
                "/proc/stat" => Some("cpu  10 0 5 80 5 0 0 0 0 0\n".into()),
                _ => None,
            },
            home: &|| Some("/home/agent".into()),
            statfs: &|_| None,
            mac_memory: None,
        });
        assert_eq!(no_disk.disk, None);
        assert_eq!(
            no_disk.disk_error.as_deref(),
            Some("The system disk could not be read.")
        );
        assert_eq!(no_disk.mem_available_bytes, Some(1024.0 * 1024.0 * 1024.0));
        assert_eq!(no_disk.cores, Some(8));
        assert_eq!(no_disk.load1, Some(1.2));
        assert_eq!(no_disk.cpu_times, Some(CpuTimes { busy: 15, total: 100 }));
        let no_cpu = local_sample(&LocalReads {
            platform: Platform::Linux,
            cpus: 8,
            total_mem: None,
            free_mem: None,
            load1: None,
            read_file: &|_| None,
            home: &|| Some("/home/agent".into()),
            statfs: &|_| Some((50.0, 100.0)),
            mac_memory: None,
        });
        assert_eq!(no_cpu.cpu_times, None);
        assert_eq!(no_cpu.cpu_error.as_deref(), Some("The CPU counters could not be read."));
        assert_eq!(no_cpu.disk.as_ref().unwrap().total_bytes, 100.0);
        let windows = local_sample(&LocalReads {
            platform: Platform::Windows,
            cpus: 32,
            total_mem: Some(1.0),
            free_mem: Some(0.5),
            load1: Some(9.0),
            read_file: &|_| None,
            home: &|| Some("/home/agent".into()),
            statfs: &|_| Some((1.0, 2.0)),
            mac_memory: None,
        });
        assert_eq!(windows.load1, None);
        assert_eq!(windows.busy_pct, None);
        assert_eq!(windows.cores, Some(32));
    }

    #[test]
    fn the_local_reading_is_the_disk_the_home_folder_is_on() {
        // A split layout: the system disk says 2 bytes while the home folder's filesystem has
        // 100. The reading must be the home folder's, and say so.
        let asked = std::cell::RefCell::new(Vec::new());
        let statfs = |path: &str| {
            asked.borrow_mut().push(path.to_owned());
            if path == "/home/agent" {
                Some((10.0, 100.0))
            } else {
                Some((1.0, 2.0))
            }
        };
        let sample = local_sample(&LocalReads {
            platform: Platform::Linux,
            cpus: 8,
            total_mem: None,
            free_mem: None,
            load1: None,
            read_file: &|_| None,
            home: &|| Some("/home/agent".into()),
            statfs: &statfs,
            mac_memory: None,
        });
        assert_eq!(asked.borrow().as_slice(), ["/home/agent"]);
        assert_eq!(
            sample.disk,
            Some(Disk {
                mount: "/home/agent".into(),
                free_bytes: 10.0,
                total_bytes: 100.0
            })
        );
    }

    #[test]
    fn mem_available_reads_proc_meminfo_only_in_kilobytes() {
        assert_eq!(
            mem_available_from("MemTotal: 1 kB\nMemAvailable: 1048576 kB\n"),
            Some(1_073_741_824.0)
        );
        assert_eq!(mem_available_from("MemTotal: 1 kB\n"), None);
        assert_eq!(mem_available_from("MemAvailable: 12 MB\n"), None);
    }
}
