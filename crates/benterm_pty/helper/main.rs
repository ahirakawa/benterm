//! The first program of every child that benterm_pty starts.
//!
//! posix_spawn(2) starts the child in a new session with the terminal on
//! descriptors 0, 1 and 2, but does not make the terminal the session's
//! controlling terminal. This program does that with TIOCSCTTY and then
//! replaces itself with the requested program, which keeps the
//! controlling terminal.
//!
//! Usage: `benterm-pty-helper PROGRAM ARGV0 [ARG...]`, where PROGRAM is
//! a path. On failure it prints why to the terminal and exits with 127.
//!
//! It defines its own `main`, so the Rust runtime does not start: that
//! runtime would ignore SIGPIPE, and the program would inherit that.

#![no_main]

use std::ffi::{c_char, c_int, c_ulong};

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn execv(path: *const c_char, argv: *const *const c_char) -> c_int;
    fn perror(message: *const c_char);
    fn _exit(status: c_int) -> !;
}

/// `TIOCSCTTY` from `<sys/ttycom.h>`: `_IO('t', 97)`.
const TIOCSCTTY: c_ulong = 0x2000_7461;

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: c_int, argv: *const *const c_char) -> c_int {
    unsafe {
        if argc < 3 {
            perror(c"benterm-pty-helper: usage: PROGRAM ARGV0 [ARG...]".as_ptr());
            _exit(127);
        }
        if ioctl(0, TIOCSCTTY, 0) == -1 {
            perror(c"benterm-pty-helper: TIOCSCTTY".as_ptr());
            _exit(127);
        }
        let program = *argv.add(1);
        execv(program, argv.add(2));
        perror(program);
        _exit(127)
    }
}
