//! Thin wrappers over the system calls this crate needs.

use std::cell::UnsafeCell;
use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr;

use crate::WindowSize;

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn winsize(size: WindowSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = check(unsafe { libc::fcntl(fd, libc::F_GETFD) })?;
    check(unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) })?;
    Ok(())
}

/// A pseudo terminal pair, both ends close-on-exec.
pub struct Pty {
    pub master: File,
    pub slave: OwnedFd,
    /// The device path of the slave, for the child to open.
    pub slave_path: CString,
}

/// How many times [`openpty`] tries when the kernel reports the
/// transient `-ENXIO`.
const OPENPTY_TRIES: usize = 10;

/// Opens a pseudo terminal pair.
///
/// While other threads or processes open terminals at the same moment,
/// opening /dev/ptmx sometimes fails with errno `-ENXIO`, a negative
/// value no documented error has. A second try then works,
/// so that error is retried. Plain `ENXIO`, meaning no terminal is free,
/// is not.
pub fn openpty(size: WindowSize) -> io::Result<Pty> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut ws = winsize(size);
    let mut tries = 0;
    loop {
        tries += 1;
        let ret = check(unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut ws,
            )
        });
        match ret {
            Err(e) if e.raw_os_error() == Some(-libc::ENXIO) && tries < OPENPTY_TRIES => continue,
            Err(e) => return Err(e),
            Ok(_) => break,
        }
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_cloexec(master.as_raw_fd())?;
    set_cloexec(slave.as_raw_fd())?;
    let slave_path = slave_name(master.as_raw_fd())?;
    Ok(Pty {
        master: File::from(master),
        slave,
        slave_path,
    })
}

/// The device path of the slave whose master is `master`, from the kernel.
///
/// ttyname(3) on the slave is not used: it searches /dev for the device,
/// and fails with ERANGE when the search misses it, which happens while
/// other processes open and close terminals.
fn slave_name(master: RawFd) -> io::Result<CString> {
    // TIOCPTYGNAME takes no length; the kernel writes up to 128 bytes.
    let mut buf = [0 as libc::c_char; 128];
    check(unsafe { libc::ioctl(master, libc::TIOCPTYGNAME.into(), buf.as_mut_ptr()) })?;
    Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

/// Creates a close-on-exec pipe and returns (read end, write end).
/// A write after the read end is closed fails with EPIPE and sends no
/// SIGPIPE, whatever the process does with that signal.
pub fn pipe() -> io::Result<(File, File)> {
    let mut fds = [-1; 2];
    check(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
    let read = unsafe { File::from_raw_fd(fds[0]) };
    let write = unsafe { File::from_raw_fd(fds[1]) };
    set_cloexec(read.as_raw_fd())?;
    set_cloexec(write.as_raw_fd())?;
    check(unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETNOSIGPIPE, 1) })?;
    Ok((read, write))
}

/// Sets the window size; the kernel then sends SIGWINCH to the
/// foreground process group.
pub fn set_window_size(fd: RawFd, size: WindowSize) -> io::Result<()> {
    let ws = winsize(size);
    check(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) })?;
    Ok(())
}

/// `POSIX_SPAWN_SETSID` from `<sys/spawn.h>`; the libc crate lacks it.
const POSIX_SPAWN_SETSID: libc::c_int = 0x0400;

/// What [`spawn`] runs.
pub struct SpawnArgs<'a> {
    /// Looked up in the app's `PATH` if it has no slash.
    pub program: &'a CStr,
    /// `argv`, starting with `argv[0]`.
    pub argv: &'a [CString],
    /// `NAME=value` entries: the child's whole environment.
    pub env: &'a [CString],
    pub cwd: Option<&'a CStr>,
}

/// Starts a child on the terminal whose slave device is `slave_path`, and
/// returns its pid.
///
/// The child is created by posix_spawn(2), not fork(2). After fork, the
/// child runs libSystem's fork handlers, which crash if another thread
/// was in the middle of setting up a system library at that moment; the
/// kernel then kills the child. posix_spawn builds the child
/// in the kernel and runs none of the parent's code in it.
///
/// The child starts a new session, so its pid is also its process group
/// id. It opens the slave by name and puts it on descriptors 0, 1 and 2.
/// It inherits no other descriptor, and starts with every signal at its
/// default action and none blocked.
///
/// posix_spawn opens the slave before the child leads its session, so
/// the slave does not become its controlling terminal. The
/// child therefore runs the helper of [`crate::helper`] first, which
/// makes the slave its controlling terminal with TIOCSCTTY and then
/// execs the program. The program is looked up here, so that a missing
/// program fails this call as it did with posix_spawnp.
pub fn spawn(slave_path: &CStr, args: &SpawnArgs) -> io::Result<libc::pid_t> {
    let attr = SpawnAttr::new()?;
    let actions = FileActions::new()?;
    unsafe {
        let flags = POSIX_SPAWN_SETSID
            | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_SETSIGDEF
            | libc::POSIX_SPAWN_SETSIGMASK;
        check_spawn(libc::posix_spawnattr_setflags(
            attr.ptr(),
            flags as libc::c_short,
        ))?;
        let mut all: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        libc::sigdelset(&mut all, libc::SIGKILL);
        libc::sigdelset(&mut all, libc::SIGSTOP);
        check_spawn(libc::posix_spawnattr_setsigdefault(attr.ptr(), &all))?;
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        check_spawn(libc::posix_spawnattr_setsigmask(attr.ptr(), &none))?;

        check_spawn(libc::posix_spawn_file_actions_addopen(
            actions.ptr(),
            0,
            slave_path.as_ptr(),
            libc::O_RDWR,
            0,
        ))?;
        check_spawn(libc::posix_spawn_file_actions_adddup2(actions.ptr(), 0, 1))?;
        check_spawn(libc::posix_spawn_file_actions_adddup2(actions.ptr(), 0, 2))?;
        if let Some(cwd) = args.cwd {
            check_spawn(libc::posix_spawn_file_actions_addchdir_np(
                actions.ptr(),
                cwd.as_ptr(),
            ))?;
        }
    }
    let helper = crate::helper::path()?;
    let program = find_program(args.program, args.cwd)?;
    let helper_argv: Vec<CString> = [helper.clone(), program]
        .into_iter()
        .chain(args.argv.iter().cloned())
        .collect();
    let argv = null_terminated(&helper_argv);
    let env = null_terminated(args.env);
    let mut pid = 0;
    check_spawn(unsafe {
        libc::posix_spawn(
            &mut pid,
            helper.as_ptr(),
            actions.ptr(),
            attr.ptr(),
            argv.as_ptr(),
            env.as_ptr(),
        )
    })?;
    Ok(pid)
}

/// The path of `program` as the helper should exec it: `program` itself
/// if it has a slash, else the first executable file named `program` in
/// the app's `PATH` (`/usr/bin:/bin` if unset), as posix_spawnp looks it
/// up. A relative path is checked from `cwd`, where the child will be.
fn find_program(program: &CStr, cwd: Option<&CStr>) -> io::Result<CString> {
    let name = Path::new(OsStr::from_bytes(program.to_bytes()));
    let is_executable = |path: &Path| {
        let path = match cwd {
            Some(cwd) if path.is_relative() => {
                Path::new(OsStr::from_bytes(cwd.to_bytes())).join(path)
            }
            _ => path.to_owned(),
        };
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let found = if program.to_bytes().contains(&b'/') {
        is_executable(name).then(|| name.to_owned())
    } else {
        let path = env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    };
    let found = found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no executable program {name:?}"),
        )
    })?;
    Ok(CString::new(found.into_os_string().into_vec()).expect("came from a C string"))
}

/// posix_spawn and its helpers return the error number instead of
/// setting errno.
fn check_spawn(ret: libc::c_int) -> io::Result<()> {
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(ret))
    }
}

fn null_terminated(strings: &[CString]) -> Vec<*mut libc::c_char> {
    strings
        .iter()
        .map(|s| s.as_ptr().cast_mut())
        .chain(std::iter::once(ptr::null_mut()))
        .collect()
}

/// A `posix_spawnattr_t`, destroyed on drop.
struct SpawnAttr(UnsafeCell<libc::posix_spawnattr_t>);

impl SpawnAttr {
    fn new() -> io::Result<Self> {
        let mut attr = ptr::null_mut();
        check_spawn(unsafe { libc::posix_spawnattr_init(&mut attr) })?;
        Ok(Self(UnsafeCell::new(attr)))
    }

    fn ptr(&self) -> *mut libc::posix_spawnattr_t {
        self.0.get()
    }
}

impl Drop for SpawnAttr {
    fn drop(&mut self) {
        unsafe { libc::posix_spawnattr_destroy(self.ptr()) };
    }
}

/// A `posix_spawn_file_actions_t`, destroyed on drop.
struct FileActions(UnsafeCell<libc::posix_spawn_file_actions_t>);

impl FileActions {
    fn new() -> io::Result<Self> {
        let mut actions = ptr::null_mut();
        check_spawn(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
        Ok(Self(UnsafeCell::new(actions)))
    }

    fn ptr(&self) -> *mut libc::posix_spawn_file_actions_t {
        self.0.get()
    }
}

impl Drop for FileActions {
    fn drop(&mut self) {
        unsafe { libc::posix_spawn_file_actions_destroy(self.ptr()) };
    }
}

/// Readiness of one file descriptor after `poll`.
#[derive(Clone, Copy, Default)]
pub struct Ready {
    /// Readable, or hung up / errored so that `read` will not block.
    pub readable: bool,
}

/// Polls `fds` for input. A negative `timeout_ms` waits forever.
pub fn poll_read<const N: usize>(fds: [RawFd; N], timeout_ms: i32) -> io::Result<[Ready; N]> {
    let mut pfds = fds.map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    loop {
        let ret = unsafe { libc::poll(pfds.as_mut_ptr(), N as libc::nfds_t, timeout_ms) };
        match check(ret) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(pfds.map(|p| Ready {
        readable: p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0,
    }))
}

/// Blocks until `pid`, our child, has exited and is a zombie, without
/// reaping it. A stop or a continue of the child does not end the wait.
///
/// `waitid(WEXITED | WNOWAIT)` alone does not do: on macOS it returns
/// for a stopped child too, with `CLD_STOPPED`, and keeps returning at
/// once while the child stays stopped. So this first waits on a kqueue
/// for `NOTE_EXIT`, which comes only when the child ends, and then on
/// `waitid` for the zombie, which no stop can come before any more.
pub fn wait_exit_no_reap(pid: libc::pid_t) -> io::Result<()> {
    wait_note_exit(pid)?;
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        match check(ret) {
            Ok(_) if exited(info.si_code) => return Ok(()),
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Whether a `si_code` from `waitid` tells that the child has ended.
fn exited(code: libc::c_int) -> bool {
    matches!(code, libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED)
}

/// Blocks until the kernel posts `NOTE_EXIT` for `pid`, our child, or
/// returns at once if the child has already ended: the kernel refuses to
/// watch a zombie with `ESRCH`.
fn wait_note_exit(pid: libc::pid_t) -> io::Result<()> {
    let kq = unsafe { OwnedFd::from_raw_fd(check(libc::kqueue())?) };
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: ptr::null_mut(),
    };
    let ret = unsafe { libc::kevent(kq.as_raw_fd(), &change, 1, ptr::null_mut(), 0, ptr::null()) };
    match check(ret) {
        Ok(_) => {}
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
        Err(e) => return Err(e),
    }
    loop {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let ret =
            unsafe { libc::kevent(kq.as_raw_fd(), ptr::null(), 0, &mut event, 1, ptr::null()) };
        match check(ret) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Reaps `pid`, which must have exited (see [`wait_exit_no_reap`]), and
/// returns its exit status. This never blocks: if the child has not
/// exited after all, it is an error, so that the caller does not wait
/// for the child under a lock.
pub fn reap(pid: libc::pid_t) -> io::Result<ExitStatus> {
    let mut status = 0;
    loop {
        match check(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }) {
            Ok(0) => return Err(io::Error::other("the child has not exited")),
            Ok(_) => return Ok(ExitStatus::from_raw(status)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Sends `signal` to the process group `pgid`.
pub fn killpg(pgid: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    check(unsafe { libc::killpg(pgid, signal) })?;
    Ok(())
}

/// The process group in the foreground of the terminal whose master is
/// `master`: the one that reads the keyboard. `None` if the terminal has
/// none, as after its session has ended.
pub fn foreground_group(master: RawFd) -> Option<libc::pid_t> {
    let group = unsafe { libc::tcgetpgrp(master) };
    (group > 0).then_some(group)
}

/// The working directory of the process `pid`, with every symbolic link
/// resolved, or `None` if the kernel does not tell: the process is gone,
/// is a zombie, or belongs to another user.
///
/// The kernel keeps the path as it was when the process entered the
/// directory or the directory was last renamed. After the directory has
/// been removed, the path of what was removed is still returned.
pub fn working_directory(pid: libc::pid_t) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // The kernel writes the whole record or reports an error.
    if written != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    let path = info.pvi_cdir.vip_path.as_flattened();
    let path: Vec<u8> = path
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    (path.first() == Some(&b'/')).then(|| PathBuf::from(std::ffi::OsString::from_vec(path)))
}

/// The arguments the process `pid` was started with, `argv[0]` first,
/// or `None` if the kernel does not tell: the process is gone, is a
/// zombie, or belongs to another user.
///
/// `argv[0]` is what the starter passed, usually the command as typed,
/// not the name of the file: Claude Code's file is named after its
/// version, while a shell starts it as `claude`. A script started by
/// its `#!` line has its interpreter first and its own path second.
pub fn process_args(pid: libc::pid_t) -> Option<Vec<std::ffi::OsString>> {
    if pid <= 0 {
        return None;
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let mut sysctl = |buf: *mut libc::c_void, size: &mut libc::size_t| unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf,
            size,
            ptr::null_mut(),
            0,
        )
    };
    // The kernel tells the size of the whole area, environment included.
    if sysctl(ptr::null_mut(), &mut size) != 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if sysctl(buf.as_mut_ptr().cast(), &mut size) != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs(&buf)
}

/// Reads the arguments out of what `KERN_PROCARGS2` gives: `argc` as a
/// C int, the path of the executable, NUL padding, then `argc` strings
/// each ended by a NUL, and the environment after them.
fn parse_procargs(buf: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    let (argc, rest) = buf.split_first_chunk::<{ size_of::<libc::c_int>() }>()?;
    let argc = usize::try_from(libc::c_int::from_ne_bytes(*argc)).ok()?;
    if argc == 0 {
        return Some(Vec::new());
    }
    let path_end = rest.iter().position(|&b| b == 0)?;
    let mut rest = &rest[path_end..];
    let first = rest.iter().position(|&b| b != 0)?;
    rest = &rest[first..];
    let mut args = Vec::with_capacity(argc);
    for _ in 0..argc {
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        args.push(std::ffi::OsString::from_vec(rest[..end].to_vec()));
        rest = rest.get(end + 1..).unwrap_or_default();
    }
    Some(args)
}

/// The login shell in the current user's passwd entry.
pub fn passwd_shell() -> Option<PathBuf> {
    passwd_field(|pwd| pwd.pw_shell)
}

/// The home directory in the current user's passwd entry.
pub fn passwd_home() -> Option<PathBuf> {
    passwd_field(|pwd| pwd.pw_dir)
}

/// One path of the current user's passwd entry.
fn passwd_field(field: impl FnOnce(&libc::passwd) -> *mut libc::c_char) -> Option<PathBuf> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result = ptr::null_mut();
    let ret = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if ret != 0 || result.is_null() {
        return None;
    }
    let field = field(&pwd);
    if field.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so the field points at a C string in
    // `buf`, which lives until the end of this function.
    let path = unsafe { CStr::from_ptr(field) };
    Some(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}
