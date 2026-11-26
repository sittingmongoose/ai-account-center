//! macOS process reads (Python `mac_processes`, `mac_context`): `ps` for the
//! table, libproc for each executable path, `KERN_PROCARGS2` for arguments and environment.
#![allow(unsafe_code)]
use super::{Process, ProcessTable, UpdateFailure};

/// (arguments, environment) of one process; empty when it cannot be read.
pub(super) fn procargs(pid: u32) -> (Vec<String>, Vec<(String, String)>) {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return (Vec::new(), Vec::new());
    };
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query with a null buffer; mib is a valid 3-element array.
    let sized = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 || size == 0 || size > 1024 * 1024 {
        return (Vec::new(), Vec::new());
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: buffer has `size` writable bytes and sysctl writes at most `size`.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return (Vec::new(), Vec::new());
    }
    buffer.truncate(size);
    let int = std::mem::size_of::<libc::c_int>();
    let Some(argc) = buffer
        .get(..int)
        .and_then(|bytes| bytes.try_into().ok())
        .map(libc::c_int::from_ne_bytes)
    else {
        return (Vec::new(), Vec::new());
    };
    if !(1..=4096).contains(&argc) {
        return (Vec::new(), Vec::new());
    }
    let data = &buffer[..];
    let Some(mut start) = data[int..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|at| int + at + 1)
    else {
        return (Vec::new(), Vec::new());
    };
    while start < data.len() && data[start] == 0 {
        start += 1;
    }
    let mut arguments = Vec::new();
    for _ in 0..argc {
        let Some(end) = data[start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|at| start + at)
        else {
            break;
        };
        arguments.push(String::from_utf8_lossy(&data[start..end]).into_owned());
        start = end + 1;
    }
    let values = data
        .get(start..)
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let text = String::from_utf8_lossy(entry);
            text.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect();
    (arguments, values)
}

fn pidpath(pid: libc::c_int) -> Option<String> {
    let mut path = vec![0_u8; 4096];
    // SAFETY: path has 4096 writable bytes, the length passed.
    let written = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), 4096) };
    let written = usize::try_from(written).ok().filter(|count| *count > 0)?;
    path.truncate(written);
    Some(String::from_utf8_lossy(&path).into_owned())
}

/// Python `mac_processes`.
pub(super) async fn processes() -> Result<ProcessTable, UpdateFailure> {
    // SAFETY: getuid has no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("/bin/ps")
            .args(["-U", &uid.to_string(), "-o", "pid=,ppid=,uid=,lstart="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| UpdateFailure::new("timeout"))?
    .map_err(|_| UpdateFailure::failed())?;
    if !output.status.success() || output.stdout.len() > 2 * 1024 * 1024 {
        return Err(UpdateFailure::failed());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut values = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(owner)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(owner)) = (
            pid.parse::<u32>(),
            ppid.parse::<u32>(),
            owner.parse::<u32>(),
        ) else {
            continue;
        };
        let started = parts.collect::<Vec<_>>().join(" ");
        if owner != uid || started.is_empty() {
            continue;
        }
        let Some(exe) = libc::c_int::try_from(pid).ok().and_then(pidpath) else {
            continue;
        };
        let name = std::path::Path::new(&exe)
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let args = if matches!(name.as_str(), "node" | "bun") {
            procargs(pid).0
        } else {
            Vec::new()
        };
        values.push(Process {
            pid,
            ppid,
            uid,
            exe,
            identity: started,
            args,
            ..Process::default()
        });
    }
    Ok(ProcessTable::new(values))
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const libc::c_char) -> *mut libc::c_void;
    fn sel_registerName(name: *const libc::c_char) -> *mut libc::c_void;
    fn objc_msgSend();
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// Python `terminate_desktops`' Mac close: `[NSRunningApplication
/// runningApplicationWithProcessIdentifier:pid] terminate`. False when it refused.
pub(super) fn terminate_application(pid: u32) -> bool {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return false;
    };
    type Lookup = unsafe extern "C" fn(
        *mut libc::c_void,
        *mut libc::c_void,
        libc::c_int,
    ) -> *mut libc::c_void;
    type Terminate = unsafe extern "C" fn(*mut libc::c_void, *mut libc::c_void) -> i8;
    // SAFETY: objc_msgSend is called through the exact signature of each selector
    // (a class method taking a pid_t, and an instance method returning BOOL);
    // names are NUL-terminated literals and every returned object is checked.
    unsafe {
        let send = objc_msgSend as *const ();
        let lookup: Lookup = std::mem::transmute(send);
        let terminate: Terminate = std::mem::transmute(send);
        let class = objc_getClass(c"NSRunningApplication".as_ptr());
        if class.is_null() {
            return false;
        }
        let instance = lookup(
            class,
            sel_registerName(c"runningApplicationWithProcessIdentifier:".as_ptr()),
            pid,
        );
        !instance.is_null() && terminate(instance, sel_registerName(c"terminate".as_ptr())) != 0
    }
}
