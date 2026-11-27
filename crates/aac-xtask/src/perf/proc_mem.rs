//! Linux `/proc` memory and process-tree samplers (`scripts/perf/memory-budget.mjs`).

use super::base_fixtures::to_fixed_num;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;

/// Parsed counters from `/proc/<pid>/smaps_rollup` or `/proc/<pid>/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMemory {
    /// Resident set size in KiB.
    pub rss_kib: u64,
    /// Proportional set size in KiB (`None` when unavailable).
    pub pss_kib: Option<u64>,
    /// Anonymous PSS in KiB (`None` when unavailable).
    pub pss_anon_kib: Option<u64>,
    /// File-backed PSS in KiB (`None` when unavailable).
    pub pss_file_kib: Option<u64>,
    /// Swap usage in KiB.
    pub swap_kib: u64,
    /// True only when PSS came from `smaps_rollup`.
    pub pss_available: bool,
}

/// Parsed parent PID and thread count from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// Parent process ID.
    pub ppid: i32,
    /// Thread count.
    pub threads: u64,
}

/// One point-in-time process-tree memory sample.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemorySample {
    /// Elapsed seconds relative to poll start (`-1.0` during startup).
    pub t_sec: f64,
    /// Server RSS in KiB.
    #[serde(rename = "serverRssKiB")]
    pub server_rss_kib: u64,
    /// Server PSS in KiB (`None` when unavailable).
    #[serde(rename = "serverPssKiB")]
    pub server_pss_kib: Option<u64>,
    /// Server anonymous PSS in KiB (`None` when unavailable).
    #[serde(rename = "serverAnonKiB")]
    pub server_anon_kib: Option<u64>,
    /// Server file-backed PSS in KiB (`None` when unavailable).
    #[serde(rename = "serverFileKiB")]
    pub server_file_kib: Option<u64>,
    /// Server swap in KiB.
    #[serde(rename = "serverSwapKiB")]
    pub server_swap_kib: u64,
    /// Whether both server and children had valid PSS.
    pub pss_available: bool,
    /// Whether the server had valid PSS.
    pub server_pss_available: bool,
    /// Server thread count.
    pub threads: u64,
    /// Descendant process count.
    pub children_count: usize,
    /// Sum of descendant RSS in KiB.
    #[serde(rename = "childrenRssKiB")]
    pub children_rss_kib: u64,
    /// Sum of descendant PSS in KiB (`None` if any child lacked PSS).
    #[serde(rename = "childrenPssKiB")]
    pub children_pss_kib: Option<u64>,
    /// Server + children RSS in KiB.
    #[serde(rename = "totalRssKiB")]
    pub total_rss_kib: u64,
    /// Server + children PSS in KiB (`None` if total PSS unavailable).
    #[serde(rename = "totalPssKiB")]
    pub total_pss_kib: Option<u64>,
}

fn parse_leading_u64(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

/// Parse `/proc/<pid>/smaps_rollup` text into KiB counters.
pub fn parse_smaps_rollup(text: &str) -> ProcMemory {
    let mut rss_kib = 0_u64;
    let mut pss_kib = 0_u64;
    let mut pss_anon_kib = 0_u64;
    let mut pss_file_kib = 0_u64;
    let mut swap_kib = 0_u64;

    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(val) = parse_leading_u64(rest) else {
            continue;
        };
        match key.trim() {
            "Rss" => rss_kib = val,
            "Pss" => pss_kib = val,
            "Pss_Anon" => pss_anon_kib = val,
            "Pss_File" => pss_file_kib = val,
            "Swap" => swap_kib = val,
            _ => {}
        }
    }

    ProcMemory {
        rss_kib,
        pss_kib: Some(pss_kib),
        pss_anon_kib: Some(pss_anon_kib),
        pss_file_kib: Some(pss_file_kib),
        swap_kib,
        pss_available: true,
    }
}

/// Fallback parser for `/proc/<pid>/status` when `smaps_rollup` is unreadable.
///
/// PSS apportions shared mappings; `VmRSS` is not PSS. Marks PSS unavailable rather
/// than reporting RSS as PSS (which would falsely fail a valid PSS budget).
pub fn parse_proc_status_rss(text: &str) -> ProcMemory {
    let mut rss_kib = 0_u64;
    let mut swap_kib = 0_u64;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kib = parse_leading_u64(rest).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("VmSwap:") {
            swap_kib = parse_leading_u64(rest).unwrap_or(0);
        }
    }
    ProcMemory {
        rss_kib,
        pss_kib: None,
        pss_anon_kib: None,
        pss_file_kib: None,
        swap_kib,
        pss_available: false,
    }
}

/// Extract the process state character (e.g. `S`, `R`, `Z`, `X`) from `/proc/<pid>/stat`.
pub fn read_proc_stat_state(text: &str) -> Option<&str> {
    let close = text.rfind(')')?;
    let rest = text.get(close + 2..)?.trim();
    rest.split_whitespace().next()
}

/// Parse `/proc/<pid>/stat` text for parent PID and thread count.
pub fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let close = text.rfind(')')?;
    let rest = text.get(close + 2..)?.trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() < 18 {
        return None;
    }
    let ppid = fields[1].parse::<i32>().ok()?;
    let threads = fields[17].parse::<u64>().unwrap_or(1);
    Some(ProcStat { ppid, threads })
}

/// Walk a `pid -> ppid` map from `root_pid` and return every descendant PID.
pub fn collect_descendants(root_pid: i32, parent_by_pid: &BTreeMap<i32, i32>) -> Vec<i32> {
    let mut kids_by_parent: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for (&pid, &ppid) in parent_by_pid {
        kids_by_parent.entry(ppid).or_default().push(pid);
    }
    let mut found = Vec::new();
    let mut stack = vec![root_pid];
    let mut visited = HashSet::new();
    visited.insert(root_pid);
    while let Some(current) = stack.pop() {
        if let Some(children) = kids_by_parent.get(&current) {
            for &child in children {
                if visited.insert(child) {
                    found.push(child);
                    stack.push(child);
                }
            }
        }
    }
    found
}

/// Check whether a process is alive and not a zombie/dead process on Linux.
pub fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
            return false;
        }
    }
    let Ok(stat_text) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    !matches!(read_proc_stat_state(&stat_text), Some("Z" | "X"))
}

fn read_proc_memory(pid: i32) -> Option<ProcMemory> {
    if let Ok(text) = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) {
        return Some(parse_smaps_rollup(&text));
    }
    if let Ok(text) = fs::read_to_string(format!("/proc/{pid}/status")) {
        return Some(parse_proc_status_rss(&text));
    }
    None
}

fn list_proc_parents() -> BTreeMap<i32, i32> {
    let mut parent_by_pid = BTreeMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return parent_by_pid;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = name_str.parse::<i32>() else {
            continue;
        };
        if let Ok(stat_text) = fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some(parsed) = parse_proc_stat(&stat_text)
        {
            parent_by_pid.insert(pid, parsed.ppid);
        }
    }
    parent_by_pid
}

/// Sample the server process and all descendant processes from `/proc`.
pub fn sample_process_tree(root_pid: i32, elapsed_sec: f64) -> Option<MemorySample> {
    let root_stat_text = fs::read_to_string(format!("/proc/{root_pid}/stat")).ok()?;
    if matches!(read_proc_stat_state(&root_stat_text), Some("Z" | "X")) {
        return None;
    }
    let root_stat = parse_proc_stat(&root_stat_text);
    let server = read_proc_memory(root_pid)?;
    let parent_by_pid = list_proc_parents();
    let kids = collect_descendants(root_pid, &parent_by_pid);
    let mut children_rss_kib = 0_u64;
    let mut children_pss_kib = 0_u64;
    let mut children_pss_available = true;
    for kid in &kids {
        let Some(mem) = read_proc_memory(*kid) else {
            continue;
        };
        children_rss_kib += mem.rss_kib;
        if !mem.pss_available || mem.pss_kib.is_none() {
            children_pss_available = false;
        } else if let Some(pss) = mem.pss_kib {
            children_pss_kib += pss;
        }
    }
    let server_pss_available = server.pss_available && server.pss_kib.is_some();
    let total_pss_available = server_pss_available && children_pss_available;
    Some(MemorySample {
        t_sec: to_fixed_num(elapsed_sec, 2),
        server_rss_kib: server.rss_kib,
        server_pss_kib: if server_pss_available {
            server.pss_kib
        } else {
            None
        },
        server_anon_kib: if server_pss_available {
            server.pss_anon_kib
        } else {
            None
        },
        server_file_kib: if server_pss_available {
            server.pss_file_kib
        } else {
            None
        },
        server_swap_kib: server.swap_kib,
        pss_available: total_pss_available,
        server_pss_available,
        threads: root_stat.map(|s| s.threads).unwrap_or(1),
        children_count: kids.len(),
        children_rss_kib,
        children_pss_kib: if children_pss_available {
            Some(children_pss_kib)
        } else {
            None
        },
        total_rss_kib: server.rss_kib + children_rss_kib,
        total_pss_kib: if total_pss_available {
            server.pss_kib.map(|p| p + children_pss_kib)
        } else {
            None
        },
    })
}
