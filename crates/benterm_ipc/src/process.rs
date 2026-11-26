//! Whether a client of the socket is one of the processes of a pane.
//!
//! A pane's credentials are in the environment of its program, where
//! another process of the same user can read them. So the server also asks
//! the kernel who the client is: a credential of a pane is taken only from
//! the program the pane started and from what that program started.

/// How far up the parents of a process are followed. Deeper than any tree
/// of processes a pane has; a longer chain is not followed to its end.
const MAX_DEPTH: usize = 256;

/// What the kernel tells of one process.
struct Lineage {
    parent: u32,
    /// The session the process is in: the process id of its leader.
    session: Option<u32>,
}

/// Whether the process `pid` belongs to the pane whose program is the
/// process `root`: it is that program, one of its descendants, or it or
/// one of its ancestors is still in the session `root` leads. `None` when
/// that cannot be told, as when `pid` has ended.
///
/// The session is what keeps a job whose parent has ended: it is adopted
/// by the first process of the system, and stays in the session of the
/// pane's program, which no process of another pane can enter. A process
/// that left both, as a daemon does, is not the pane's any more.
pub(crate) fn belongs_to(pid: u32, root: u32) -> Option<bool> {
    if pid == root {
        return Some(true);
    }
    let mut current = pid;
    for _ in 0..MAX_DEPTH {
        let lineage = lineage(current)?;
        if lineage.parent == root || lineage.session == Some(root) {
            return Some(true);
        }
        // The first process of the system, or none: the top.
        if lineage.parent <= 1 {
            return Some(false);
        }
        current = lineage.parent;
    }
    None
}

#[cfg(target_vendor = "apple")]
fn lineage(pid: u32) -> Option<Lineage> {
    let pid = libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 0)?;
    // SAFETY: the structure is plain data, for which all zeroes is valid.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is valid for writes of `size` bytes.
    let read =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if read != size {
        return None;
    }
    // SAFETY: getsid has no preconditions.
    let session = unsafe { libc::getsid(pid) };
    Some(Lineage {
        parent: info.pbi_ppid,
        session: u32::try_from(session).ok().filter(|session| *session > 0),
    })
}

#[cfg(target_os = "linux")]
fn lineage(pid: u32) -> Option<Lineage> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name of the program, in parentheses, may itself have spaces and
    // parentheses; the fields after it do not.
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace();
    let parent = fields.nth(1)?.parse().ok()?;
    let session = fields.nth(1)?.parse::<u32>().ok();
    Some(Lineage {
        parent,
        session: session.filter(|session| *session > 0),
    })
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn lineage(_pid: u32) -> Option<Lineage> {
    None
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use super::*;

    /// Starts `script` under `/bin/sh` as the leader of a session of its
    /// own, as the program of a pane is.
    fn leader(script: &str) -> Child {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            command.pre_exec(|| match libc::setsid() {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        command.spawn().unwrap()
    }

    fn first_number(child: &mut Child) -> u32 {
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        line.trim().parse().unwrap()
    }

    fn end(mut child: Child) {
        // SAFETY: the child leads its group, which holds only what the
        // test started.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.wait();
    }

    #[test]
    fn a_program_and_what_it_started_belong_to_it() {
        // The shell starts a shell that starts the sleep.
        let mut root = leader("/bin/sh -c 'sleep 60 & echo $!; wait'; :");
        let grandchild = first_number(&mut root);

        assert_eq!(belongs_to(root.id(), root.id()), Some(true));
        assert_eq!(belongs_to(grandchild, root.id()), Some(true));
        end(root);
    }

    #[test]
    fn a_process_of_another_tree_does_not_belong() {
        let mut root = leader("sleep 60 & echo $!; wait");
        let mut other = leader("sleep 60 & echo $!; wait");
        let of_other = first_number(&mut other);
        first_number(&mut root);

        assert_eq!(belongs_to(of_other, root.id()), Some(false));
        assert_eq!(belongs_to(other.id(), root.id()), Some(false));
        // The process that started the program is not the program's.
        assert_eq!(belongs_to(std::process::id(), root.id()), Some(false));
        end(root);
        end(other);
    }

    #[test]
    fn a_job_whose_parent_ended_still_belongs_by_its_session() {
        // The inner shell ends at once and leaves the sleep to the first
        // process of the system.
        let mut root = leader("/bin/sh -c 'sleep 60 & echo $!'; sleep 60");
        let orphan = first_number(&mut root);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lineage(orphan).is_some_and(|lineage| lineage.parent != 1) {
            assert!(std::time::Instant::now() < deadline, "never adopted");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(belongs_to(orphan, root.id()), Some(true));
        end(root);
    }

    #[test]
    fn a_process_that_has_ended_cannot_be_told() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();

        assert_eq!(belongs_to(pid, std::process::id()), None);
    }
}
