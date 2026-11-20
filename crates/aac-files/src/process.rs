//! Process identity used to reclaim legacy private-file claims. Unknown identity and
//! denied liveness probes are treated as occupied.

/// Returns true only when the OS confirms that a valid PID is gone.
#[allow(unsafe_code)]
pub fn is_dead(pid: u32) -> bool {
    if pid == 0 || i32::try_from(pid).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        // Signal 0 only asks the OS about the PID. EPERM and all other errors are unverified.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_INVALID_PARAMETER as i32);
        }
        let result = unsafe { WaitForSingleObject(process, 0) };
        unsafe { CloseHandle(process) };
        result == WAIT_OBJECT_0
    }
    #[cfg(not(any(unix, windows)))]
    false
}

/// Hostname and boot time in epoch minutes, matching the TS dashboard's owner marker.
pub fn host_boot() -> Option<(String, u64)> {
    Some((hostname()?, boot_ms()?.checked_add(30_000)? / 60_000))
}

#[cfg(unix)]
#[allow(unsafe_code)]
/// The local machine hostname reported by the operating system.
pub fn hostname() -> Option<String> {
    let mut bytes = [0u8; 256];
    let result = unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) };
    if result != 0 {
        return None;
    }
    let end = bytes.iter().position(|&byte| byte == 0)?;
    (end > 0).then_some(())?;
    String::from_utf8(bytes[..end].to_vec()).ok()
}

#[cfg(windows)]
#[allow(unsafe_code)]
/// The local machine hostname reported by the operating system.
pub fn hostname() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };
    let mut words = [0u16; 256];
    let mut len = words.len() as u32;
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            words.as_mut_ptr(),
            &mut len,
        )
    };
    (ok != 0).then_some(())?;
    String::from_utf16(words.get(..len as usize)?).ok()
}

#[cfg(not(any(unix, windows)))]
/// The local machine hostname reported by the operating system.
pub fn hostname() -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", windows))]
fn now_ms() -> Option<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis(),
    )
    .ok()
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn boot_ms() -> Option<u64> {
    let mut uptime = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut uptime) };
    (result == 0).then_some(())?;
    let seconds = u64::try_from(uptime.tv_sec).ok()?;
    let millis = u64::try_from(uptime.tv_nsec).ok()? / 1_000_000;
    now_ms()?.checked_sub(seconds.checked_mul(1000)?.checked_add(millis)?)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn boot_ms() -> Option<u64> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of_val(&boot);
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            std::ptr::from_mut(&mut boot).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && len == std::mem::size_of_val(&boot)).then_some(())?;
    let seconds = u64::try_from(boot.tv_sec).ok()?;
    let millis = u64::try_from(boot.tv_usec).ok()? / 1000;
    seconds.checked_mul(1000)?.checked_add(millis)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn boot_ms() -> Option<u64> {
    let uptime = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    now_ms()?.checked_sub(uptime)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn boot_ms() -> Option<u64> {
    None
}
