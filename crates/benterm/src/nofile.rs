//! Reads `RLIMIT_NOFILE` at app startup and raises a low soft limit so
//! opening many terminals does not run out of file descriptors.

use std::io;

/// The soft limit Benterm asks for at startup, matching herdr's cap.
pub const TARGET_SOFT_LIMIT: libc::rlim_t = 8192;

/// The soft and hard `RLIMIT_NOFILE` values of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub soft: libc::rlim_t,
    pub hard: libc::rlim_t,
}

/// Reads the current process's `RLIMIT_NOFILE` soft and hard limits.
pub fn read_limits() -> io::Result<Limits> {
    let mut rlim = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `getrlimit` initializes `rlim` on success (return value 0).
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, rlim.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let rlim = unsafe { rlim.assume_init() };
    Ok(Limits {
        soft: rlim.rlim_cur,
        hard: rlim.rlim_max,
    })
}

/// Sets the current process's `RLIMIT_NOFILE` soft and hard limits.
pub fn write_limits(limits: Limits) -> io::Result<()> {
    let rlim = libc::rlimit {
        rlim_cur: limits.soft,
        rlim_max: limits.hard,
    };
    // SAFETY: `rlim` is a valid `libc::rlimit` struct.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rlim) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Decides what soft limit to raise to, or `None` when the current soft
/// limit should be left as it is.
pub fn next_soft_limit(current: Limits, target: libc::rlim_t) -> Option<libc::rlim_t> {
    let cap = if current.hard == libc::RLIM_INFINITY {
        target
    } else {
        target.min(current.hard)
    };
    (current.soft < cap).then_some(cap)
}

/// Reads `RLIMIT_NOFILE`, logs its initial values to stderr, and raises
/// the soft limit up to [`TARGET_SOFT_LIMIT`] within the hard limit.
/// Failures are logged as warnings so the app still starts.
pub fn raise_and_log() {
    raise_with(
        TARGET_SOFT_LIMIT,
        read_limits,
        write_limits,
        &mut io::stderr().lock(),
    );
}

fn format_limit(limit: libc::rlim_t) -> String {
    if limit == libc::RLIM_INFINITY {
        "unlimited".to_string()
    } else {
        limit.to_string()
    }
}

fn raise_with<R, W, O>(target: libc::rlim_t, read: R, write: W, log: &mut O)
where
    R: FnOnce() -> io::Result<Limits>,
    W: FnOnce(Limits) -> io::Result<()>,
    O: io::Write,
{
    let current = match read() {
        Ok(limits) => limits,
        Err(error) => {
            let _ = writeln!(
                log,
                "benterm: warning: could not read RLIMIT_NOFILE: {error}"
            );
            return;
        }
    };
    let soft = format_limit(current.soft);
    let hard = format_limit(current.hard);
    let Some(new_soft) = next_soft_limit(current, target) else {
        let _ = writeln!(
            log,
            "benterm: RLIMIT_NOFILE soft={soft} hard={hard} (unchanged)"
        );
        return;
    };
    let updated = Limits {
        soft: new_soft,
        hard: current.hard,
    };
    match write(updated) {
        Ok(()) => {
            let _ = writeln!(
                log,
                "benterm: RLIMIT_NOFILE soft={soft} hard={hard} -> raised soft to {new_soft}"
            );
        }
        Err(error) => {
            let _ = writeln!(
                log,
                "benterm: warning: RLIMIT_NOFILE soft={soft} hard={hard}; could not raise soft limit to {new_soft}: {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raises_low_soft_limit_to_target_when_hard_is_unlimited() {
        let current = Limits {
            soft: 256,
            hard: libc::RLIM_INFINITY,
        };
        assert_eq!(next_soft_limit(current, TARGET_SOFT_LIMIT), Some(8192));
    }

    #[test]
    fn keeps_soft_limit_when_already_above_target() {
        let unlimited = Limits {
            soft: 10_240,
            hard: libc::RLIM_INFINITY,
        };
        assert_eq!(next_soft_limit(unlimited, TARGET_SOFT_LIMIT), None);

        let finite = Limits {
            soft: 10_240,
            hard: 10_240,
        };
        assert_eq!(next_soft_limit(finite, TARGET_SOFT_LIMIT), None);

        let at_target = Limits {
            soft: TARGET_SOFT_LIMIT,
            hard: libc::RLIM_INFINITY,
        };
        assert_eq!(next_soft_limit(at_target, TARGET_SOFT_LIMIT), None);
    }

    #[test]
    fn caps_raised_soft_limit_at_finite_hard_limit() {
        let low = Limits {
            soft: 256,
            hard: 4096,
        };
        assert_eq!(next_soft_limit(low, TARGET_SOFT_LIMIT), Some(4096));

        let already_at_hard = Limits {
            soft: 4096,
            hard: 4096,
        };
        assert_eq!(next_soft_limit(already_at_hard, TARGET_SOFT_LIMIT), None);
    }

    #[test]
    fn logs_initial_limits_and_raises_when_below_target() {
        let mut written = None;
        let mut log = Vec::new();
        raise_with(
            TARGET_SOFT_LIMIT,
            || {
                Ok(Limits {
                    soft: 256,
                    hard: libc::RLIM_INFINITY,
                })
            },
            |limits| {
                written = Some(limits);
                Ok(())
            },
            &mut log,
        );

        assert_eq!(
            written,
            Some(Limits {
                soft: 8192,
                hard: libc::RLIM_INFINITY,
            })
        );
        let log = String::from_utf8(log).unwrap();
        assert!(log.contains("soft=256"), "{log}");
        assert!(log.contains("hard=unlimited"), "{log}");
        assert!(log.contains("8192"), "{log}");
    }

    #[test]
    fn logs_initial_limits_without_writing_when_already_high_enough() {
        let mut wrote = false;
        let mut log = Vec::new();
        raise_with(
            TARGET_SOFT_LIMIT,
            || {
                Ok(Limits {
                    soft: 10_240,
                    hard: libc::RLIM_INFINITY,
                })
            },
            |_| {
                wrote = true;
                Ok(())
            },
            &mut log,
        );

        assert!(!wrote);
        let log = String::from_utf8(log).unwrap();
        assert!(log.contains("soft=10240"), "{log}");
        assert!(log.contains("hard=unlimited"), "{log}");
    }

    #[test]
    fn logs_warning_and_continues_when_raising_fails() {
        let mut log = Vec::new();
        raise_with(
            TARGET_SOFT_LIMIT,
            || {
                Ok(Limits {
                    soft: 256,
                    hard: libc::RLIM_INFINITY,
                })
            },
            |_| Err(io::Error::from_raw_os_error(libc::EPERM)),
            &mut log,
        );

        let log = String::from_utf8(log).unwrap();
        assert!(log.contains("warning"), "{log}");
        assert!(log.contains("soft=256"), "{log}");
        assert!(log.contains("hard=unlimited"), "{log}");
        assert!(log.contains("8192"), "{log}");
    }

    #[test]
    fn logs_warning_and_continues_when_reading_fails() {
        let mut wrote = false;
        let mut log = Vec::new();
        raise_with(
            TARGET_SOFT_LIMIT,
            || Err(io::Error::from_raw_os_error(libc::EINVAL)),
            |_| {
                wrote = true;
                Ok(())
            },
            &mut log,
        );

        assert!(!wrote);
        let log = String::from_utf8(log).unwrap();
        assert!(log.contains("warning"), "{log}");
        assert!(log.contains("RLIMIT_NOFILE"), "{log}");
    }

    #[test]
    fn reads_process_nofile_limits() {
        let limits = read_limits().expect("getrlimit(RLIMIT_NOFILE) should succeed");
        assert!(limits.soft > 0);
        assert!(limits.hard == libc::RLIM_INFINITY || limits.soft <= limits.hard);
    }
}
