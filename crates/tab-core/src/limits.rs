//! Process limits tab's children inherit.

/// Raises the soft open-file limit to the hard one, so agents and the tools
/// they start inherit room for many open files. An app opened from Finder
/// gets launchd's soft limit of 256, which a busy Claude Code or language
/// server can run through; a terminal's `ulimit -n` never reaches it.
/// Best effort: nothing changes if the limit can't be read or set.
pub fn raise_open_file_limit() {
    #[cfg(unix)]
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut want = lim.rlim_max;
        // macOS refuses a soft limit above the kernel's per-process cap
        // (and RLIM_INFINITY), though it reports the hard limit as unlimited.
        #[cfg(target_os = "macos")]
        {
            let mut cap: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>();
            let name = c"kern.maxfilesperproc";
            if libc::sysctlbyname(
                name.as_ptr(),
                (&mut cap as *mut libc::c_int).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            ) == 0
                && cap > 0
            {
                want = want.min(cap as libc::rlim_t);
            } else {
                // OPEN_MAX in <sys/syslimits.h>.
                want = want.min(10240);
            }
        }
        if want <= lim.rlim_cur {
            return;
        }
        lim.rlim_cur = want;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) == 0 {
            tracing::debug!(limit = want, "raised the open-file limit");
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn raises_the_soft_limit_to_the_hard_one() {
        unsafe {
            let mut lim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
            let low = lim.rlim_max.min(256);
            let lowered = libc::rlimit {
                rlim_cur: low,
                rlim_max: lim.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lowered), 0);
            super::raise_open_file_limit();
            let mut after = lim;
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut after), 0);
            assert!(after.rlim_cur >= low);
            if lim.rlim_max > 256 {
                assert!(after.rlim_cur > 256, "still {}", after.rlim_cur);
            }
        }
    }
}
