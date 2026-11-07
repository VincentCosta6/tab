//! Process trees: everything a process starts ends with it.
//!
//! `kill_on_drop` only kills the direct child. An agent that starts a dev
//! server, or a test runner that forks workers, leaves those behind when a
//! chat is stopped. So each sandboxed process starts as the leader of its
//! own process group on Unix, or inside a job object that kills its members
//! when closed on Windows, and [`Guarded`] ends the whole tree when dropped.

use std::ops::{Deref, DerefMut};

use tokio::process::Child;

use super::proxy;

/// Called before spawning.
pub(super) fn prepare(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// A running child and its tree.
pub struct Guarded {
    child: Child,
    #[cfg(unix)]
    pgid: Option<i32>,
    #[cfg(windows)]
    job: Option<win::Job>,
    _proxy: Option<proxy::Handle>,
}

impl Guarded {
    pub(super) fn new(child: Child, proxy: Option<proxy::Handle>) -> Self {
        #[cfg(unix)]
        let pgid = child.id().map(|p| p as i32);
        #[cfg(windows)]
        let job = win::Job::new().and_then(|j| j.assign(&child).then_some(j));
        Self {
            child,
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job,
            _proxy: proxy,
        }
    }

    /// Ends every process in the tree now.
    pub fn kill_tree(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            // SAFETY: plain syscall; a group that's already gone is ESRCH.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            self.job.take();
        }
        let _ = self.child.start_kill();
    }
}

impl Deref for Guarded {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for Guarded {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// A job object that kills its processes when its handle closes.
    pub struct Job(usize);

    impl Job {
        pub fn new() -> Option<Self> {
            // SAFETY: documented Win32 calls with a zeroed plain-data struct.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(h);
                    return None;
                }
                Some(Job(h as usize))
            }
        }

        pub fn assign(&self, child: &tokio::process::Child) -> bool {
            let Some(p) = child.raw_handle() else {
                return false;
            };
            // SAFETY: both handles are live for the call.
            unsafe { AssignProcessToJobObject(self.0 as HANDLE, p as HANDLE) != 0 }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle; closing it kills the job's processes.
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }
}
