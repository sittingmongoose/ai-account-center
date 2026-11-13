//! AI Account Center usage-log scanner (DESIGN.md sections 2, 3 and 11 in the share folder
//! `2026-11-13-analytics-discovery`): discovery of every tool's logs from what is configured on this
//! computer, and the six readers, one self-contained binary per target. Wire-compatible with the
//! Python helper it replaces (`scripts/analytics-remote/analytics_usage_remote.py`).

pub mod collector;
pub mod discover;
pub mod fsx;
pub mod protocol;
pub mod read;
pub mod util;

use std::path::PathBuf;

/// The home folder as Python's `os.path.expanduser("~")` finds it.
pub fn home_dir(env: &discover::Env) -> Option<PathBuf> {
    if cfg!(windows) {
        if let Some(profile) = env.get("USERPROFILE").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(profile));
        }
        match (env.get("HOMEDRIVE"), env.get("HOMEPATH")) {
            (Some(drive), Some(path)) => Some(PathBuf::from(format!("{drive}{path}"))),
            _ => None,
        }
    } else {
        env.get("HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
    }
}

/// Scan at low CPU and IO priority: nice 10 (plus utility-tier disk IO on macOS); on Windows,
/// below-normal priority and background mode. Best effort.
pub fn lower_priority() {
    #[cfg(unix)]
    // SAFETY: nice only changes this process's scheduling priority.
    unsafe {
        libc::nice(10);
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
        }
        // SAFETY: IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_UTILITY for this process only.
        unsafe {
            setiopolicy_np(0, 0, 4);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass,
        };
        // SAFETY: the current process's pseudo-handle; priority changes only.
        unsafe {
            let process = GetCurrentProcess();
            SetPriorityClass(process, BELOW_NORMAL_PRIORITY_CLASS);
            SetPriorityClass(process, PROCESS_MODE_BACKGROUND_BEGIN);
        }
    }
}
