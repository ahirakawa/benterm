//! Benterm's shell integration: zsh and bash started on the PTY tell where
//! each command ends, with its exit code (OSC 133 ; D), and where each
//! prompt starts (OSC 133 ; A), without any setting of the user's. The
//! user's startup files are not changed.
//!
//! The start of the prompt is told from the same hook as the end of the
//! command, right after it, as Orca does: before each prompt, the first
//! one too. The prompt itself is left as the user set it.
//!
//! zsh is started with `ZDOTDIR` pointing at a directory of Benterm's
//! whose `.zshenv` puts `ZDOTDIR` back, reads the user's `.zshenv` and
//! adds the hooks; zsh then reads the user's other startup files from
//! the user's `ZDOTDIR`, as it would have. This is how Ghostty and Orca
//! put theirs in.
//!
//! bash 3.2, the one macOS ships, has no hook to load a file into a
//! login shell before the user's files: Ghostty's way, POSIX mode and
//! `ENV`, needs bash 4. bash reads `PROMPT_COMMAND` from the
//! environment, though, so the hook is given there. A startup file that
//! sets `PROMPT_COMMAND` without keeping the value it had drops it.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The shells Benterm has an integration for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shell {
    Zsh,
    Bash,
}

impl Shell {
    /// The shell `program` runs, by the name of its file.
    pub(crate) fn of(program: &Path) -> Option<Self> {
        match program.file_name()?.to_str()? {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            _ => None,
        }
    }
}

/// Where the user's `ZDOTDIR` waits while zsh reads Benterm's `.zshenv`.
/// Unset when the user had none.
const ZSH_ZDOTDIR: &str = "BENTERM_ZSH_ZDOTDIR";

/// Benterm's `.zshenv`.
const ZSHENV: &str = r#"# Benterm's shell integration for zsh, written by Benterm on each start of
# a shell. Benterm starts zsh with ZDOTDIR set to this file's directory.
# This file sets ZDOTDIR back and reads the user's .zshenv; zsh then
# reads the user's other startup files.
if [[ -n ${BENTERM_ZSH_ZDOTDIR+x} ]]; then
  builtin export ZDOTDIR="$BENTERM_ZSH_ZDOTDIR"
  builtin unset BENTERM_ZSH_ZDOTDIR
else
  builtin unset ZDOTDIR
fi

if [[ -o interactive ]]; then
  # Tells the end of each command line run, with its exit code, and then
  # the start of the prompt. An empty line runs nothing and tells no end.
  __benterm_preexec() { __benterm_ran=1 }
  __benterm_precmd() {
    builtin local __benterm_status=$?
    if [[ -n ${__benterm_ran-} ]]; then
      __benterm_ran=
      builtin printf '\033]133;D;%s\007' "$__benterm_status"
    fi
    builtin printf '\033]133;A\007'
  }
  # First, so that it sees the exit code before any other hook runs.
  precmd_functions=(__benterm_precmd ${precmd_functions[@]})
  preexec_functions+=(__benterm_preexec)

  # Codex runs the commands of every Codex in one shared background
  # server, with the environment of the Codex that started it, so its hooks
  # would tell another pane. A codex function of the user's, which the
  # user's startup files define after this one, wins.
  if (( ! ${+functions[codex]} )); then
    function codex {
      builtin local __benterm_arg __benterm_alone=1
      for __benterm_arg in "$@"; do
        case $__benterm_arg in
          --no-daemon|agents|queue|--remote|--remote=*) __benterm_alone= ;;
        esac
      done
      if [[ -n $__benterm_alone ]]; then
        case "$(command codex --help 2>/dev/null </dev/null)" in
          *--no-daemon*) builtin set -- --no-daemon "$@" ;;
        esac
      fi
      command codex "$@"
    }
  fi
fi

if [[ -r ${ZDOTDIR:-$HOME}/.zshenv ]]; then
  builtin source "${ZDOTDIR:-$HOME}/.zshenv"
fi
"#;

/// bash's hook, run before each prompt. It tells the exit code of the
/// command before, except at the first prompt, then the start of the
/// prompt, and leaves `$?` as it found it for whatever runs after it.
///
/// It also defines a `codex` function, as the integration for zsh does,
/// unless there is one: bash has no file of Benterm's to define it in.
const BASH_PROMPT_COMMAND: &str = r#"__benterm_status=$?; declare -F codex >/dev/null 2>&1 || function codex { local __benterm_arg __benterm_alone=1; for __benterm_arg in "$@"; do case $__benterm_arg in --no-daemon|agents|queue|--remote|--remote=*) __benterm_alone= ;; esac; done; if [ -n "$__benterm_alone" ]; then case "$(command codex --help 2>/dev/null </dev/null)" in *--no-daemon*) set -- --no-daemon "$@" ;; esac; fi; command codex "$@"; }; __benterm_return() { return "$1"; }; if [ -n "${__benterm_prompted-}" ]; then builtin printf '\033]133;D;%s\007' "$__benterm_status"; fi; builtin printf '\033]133;A\007'; __benterm_prompted=1; __benterm_return "$__benterm_status""#;

/// Changes `env`, the child's whole environment, so that `shell` starts
/// with the integration. `dir` is where [`write_files`] put the files.
pub(crate) fn integrate(shell: Shell, env: &mut Vec<(OsString, OsString)>, dir: &Path) {
    match shell {
        Shell::Zsh => {
            let user = take(env, "ZDOTDIR");
            take(env, ZSH_ZDOTDIR);
            if let Some(user) = user {
                env.push((ZSH_ZDOTDIR.into(), user));
            }
            env.push(("ZDOTDIR".into(), dir.join("zsh").into()));
        }
        Shell::Bash => {
            let value = match take(env, "PROMPT_COMMAND") {
                // Started from a shell of Benterm's, which has the hook.
                Some(theirs) if starts_with(&theirs, BASH_PROMPT_COMMAND) => theirs,
                Some(theirs) if !theirs.is_empty() => {
                    let mut value = OsString::from(BASH_PROMPT_COMMAND);
                    value.push("\n");
                    value.push(theirs);
                    value
                }
                _ => BASH_PROMPT_COMMAND.into(),
            };
            env.push(("PROMPT_COMMAND".into(), value));
        }
    }
    env.sort();
}

fn take(env: &mut Vec<(OsString, OsString)>, name: &str) -> Option<OsString> {
    let index = env.iter().position(|(n, _)| n == name)?;
    Some(env.remove(index).1)
}

fn starts_with(value: &OsStr, prefix: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().starts_with(prefix.as_bytes())
}

/// The directory of the integration's files: one of this user's alone,
/// in [`private_dir`](crate::private_dir).
pub(crate) fn default_dir() -> io::Result<PathBuf> {
    Ok(crate::private_dir::private_dir()?
        .join(format!("shell-integration-{}", env!("CARGO_PKG_VERSION"))))
}

/// Puts the integration's files in `dir`, unless they are there already.
///
/// The files are sourced by the user's shells, so `dir` must be the
/// user's own and nobody else's to write in; if it is not, nothing is
/// written and an error is returned.
pub(crate) fn write_files(dir: &Path) -> io::Result<()> {
    crate::private_dir::make_private(dir)?;
    let zsh = dir.join("zsh");
    crate::private_dir::make_private(&zsh)?;
    write_if_changed(&zsh.join(".zshenv"), ZSHENV)
}

/// Writes `text` to `path` unless it holds it already. The text goes to
/// a file of its own first and is then renamed into place, so that a
/// shell starting meanwhile reads either the old file or the new one.
fn write_if_changed(path: &Path, text: &str) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(());
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}-{n}", std::process::id()));
    fs::write(&temporary, text)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn get<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a str> {
        env.iter()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value.to_str().unwrap())
    }

    #[test]
    fn the_shell_is_known_by_the_name_of_its_file() {
        assert_eq!(Shell::of(Path::new("/bin/zsh")), Some(Shell::Zsh));
        assert_eq!(
            Shell::of(Path::new("/opt/homebrew/bin/bash")),
            Some(Shell::Bash)
        );
        assert_eq!(Shell::of(Path::new("bash")), Some(Shell::Bash));
        assert_eq!(Shell::of(Path::new("/bin/sh")), None);
        assert_eq!(Shell::of(Path::new("/usr/local/bin/fish")), None);
        assert_eq!(Shell::of(Path::new("/usr/bin/claude")), None);
    }

    #[test]
    fn zsh_starts_in_benterms_zdotdir_and_keeps_the_users() {
        let dir = Path::new("/x/integration");
        let mut env = vars(&[("HOME", "/h"), ("ZDOTDIR", "/h/conf")]);
        integrate(Shell::Zsh, &mut env, dir);
        assert_eq!(get(&env, "ZDOTDIR"), Some("/x/integration/zsh"));
        assert_eq!(get(&env, ZSH_ZDOTDIR), Some("/h/conf"));
        assert_eq!(get(&env, "HOME"), Some("/h"));

        let mut env = vars(&[("HOME", "/h"), (ZSH_ZDOTDIR, "/stale")]);
        integrate(Shell::Zsh, &mut env, dir);
        assert_eq!(get(&env, "ZDOTDIR"), Some("/x/integration/zsh"));
        assert_eq!(get(&env, ZSH_ZDOTDIR), None);
    }

    #[test]
    fn bash_gets_the_hook_before_its_own_prompt_command() {
        let dir = Path::new("/x");
        let mut env = vars(&[]);
        integrate(Shell::Bash, &mut env, dir);
        assert_eq!(get(&env, "PROMPT_COMMAND"), Some(BASH_PROMPT_COMMAND));

        let mut env = vars(&[("PROMPT_COMMAND", "history -a")]);
        integrate(Shell::Bash, &mut env, dir);
        let expected = format!("{BASH_PROMPT_COMMAND}\nhistory -a");
        assert_eq!(get(&env, "PROMPT_COMMAND"), Some(expected.as_str()));

        // Once is enough for a bash started from a shell of Benterm's.
        integrate(Shell::Bash, &mut env, dir);
        assert_eq!(get(&env, "PROMPT_COMMAND"), Some(expected.as_str()));
        assert_eq!(get(&env, "ZDOTDIR"), None);
    }

    #[test]
    fn the_files_are_written_once_into_a_private_directory() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("integration");
        write_files(&dir).unwrap();
        let zshenv = dir.join("zsh").join(".zshenv");
        assert_eq!(fs::read_to_string(&zshenv).unwrap(), ZSHENV);
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);

        fs::write(&zshenv, "old").unwrap();
        write_files(&dir).unwrap();
        assert_eq!(fs::read_to_string(&zshenv).unwrap(), ZSHENV);
        let names: Vec<_> = fs::read_dir(dir.join("zsh")).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left");
    }

    #[test]
    fn a_directory_others_may_write_in_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("integration");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(write_files(&dir).is_err());
        assert!(!dir.join("zsh").exists());
    }
}
