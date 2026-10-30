//! The few Windows things the bridge needs beyond `std`: being the only one,
//! taking its children with it, and reading the CLI's GPU marker.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess};

/// One bridge per machine, whoever is logged in.
pub const SINGLETON: &str = "Global\\jdrgb-ha";

/// Take the machine-wide singleton for the life of the process, or say who has
/// it. Being *denied access* counts as held: it means a bridge under another
/// account created it, and that one owns the machine's MQTT identity.
pub fn take_singleton() -> Result<(), String> {
    let wide: Vec<u16> = SINGLETON.encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    let err = unsafe { GetLastError() };
    if h.is_null() {
        return Err(if err == ERROR_ACCESS_DENIED {
            "another jdrgb-ha is already running on this machine, under another account".into()
        } else {
            format!("could not create the {SINGLETON} mutex (error {err})")
        });
    }
    if err == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(h) };
        return Err("another jdrgb-ha is already running on this machine".into());
    }
    // Held for the life of the process; Windows releases it on exit.
    Ok(())
}

/// A job object whose processes all end when its last handle closes.
fn kill_on_close_job() -> Result<HANDLE, String> {
    unsafe {
        // Null security attributes: the handle is not inheritable, so a child
        // holding a copy can't keep the job alive after the bridge is gone.
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(format!("could not create a job object (error {})", GetLastError()));
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            let e = GetLastError();
            CloseHandle(job);
            return Err(format!("could not make the job kill-on-close (error {e})"));
        }
        Ok(job)
    }
}

/// Put the bridge in a kill-on-close job, so every `jdrgb.exe` it starts ends
/// with it.
///
/// Windows doesn't end a process's children when the parent is killed, and
/// `jdrgb::spawn`'s timeout only protects a child while the parent is alive to
/// enforce it. A bridge killed at logoff or by uninstall would otherwise leave a
/// child mid-write with nobody watching. Children are associated with the job
/// automatically.
///
/// The handle is **never closed**: closing the last one ends every process in
/// the job, including the bridge. It lives exactly as long as the process.
pub fn join_kill_on_close_job() -> Result<(), String> {
    let job = kill_on_close_job()?;
    if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
        let e = unsafe { GetLastError() };
        unsafe { CloseHandle(job) };
        return Err(format!("could not join the job object (error {e})"));
    }
    Ok(())
}

/// How long ago anyone last wrote the card, from the CLI's machine-wide marker
/// (`%ProgramData%\jdrgb\gpu-last-write`, milliseconds since the epoch as text).
/// `None` if it is missing, unreadable, or in the future — ignored, never
/// guessed at.
pub fn gpu_marker_age_ms() -> Option<u64> {
    let path = std::path::PathBuf::from(std::env::var_os("ProgramData")?).join("jdrgb").join("gpu-last-write");
    let then: u128 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(now.checked_sub(then)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;

    /// The job's whole point, without risking the test runner: a harmless
    /// sleeping child is put in a kill-on-close job, the job's last handle is
    /// closed, and the child must be gone.
    #[test]
    fn closing_the_job_ends_the_child() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn ping");
        let job = kill_on_close_job().expect("job");
        assert_ne!(unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) }, 0, "assign");
        assert!(child.try_wait().unwrap().is_none(), "the child should still be running");
        unsafe { CloseHandle(job) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the child outlived the job");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
