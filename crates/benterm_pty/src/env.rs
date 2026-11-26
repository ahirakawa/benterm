//! The environment the child starts with.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// The value of `TERM_PROGRAM_VERSION`. The app has no version of its own
/// yet, so this is the version of this crate.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Prefixes of names that other terminals and multiplexers set to point at
/// themselves. A name matches when it is the prefix without the trailing
/// `_`, or starts with the whole prefix.
const REMOVED_PREFIXES: &[&str] = &[
    "HC_",        // herdr's hc: the outer terminal and its socket
    "HERDR_",     // herdr
    "GHOSTTY_",   // Ghostty
    "ITERM_",     // iTerm2
    "WEZTERM_",   // WezTerm
    "KITTY_",     // kitty
    "ALACRITTY_", // Alacritty
    "ZELLIJ_",    // Zellij, which also sets ZELLIJ
];

/// Names that other terminals and multiplexers set, matched exactly.
const REMOVED_NAMES: &[&str] = &[
    "TERM_SESSION_ID",       // Terminal.app and iTerm2
    "LC_TERMINAL",           // iTerm2
    "LC_TERMINAL_VERSION",   // iTerm2
    "VTE_VERSION",           // GNOME Terminal and other VTE terminals
    "WINDOWID",              // X11 terminals
    "TMUX",                  // tmux
    "TMUX_PANE",             // tmux
    "STY",                   // GNU screen
    "BENTERM_PANE_TOKEN",    // Benterm per-pane IPC credential
    "BENTERM_CONTROL_TOKEN", // Benterm control IPC credential
];

/// Names that a running coding agent sets for the commands it runs,
/// matched exactly.
///
/// An agent started in Benterm is not part of the agent Benterm was started
/// from, so a name is removed when it points at a running session or the
/// process behind it: a session or thread id, a mark that the process
/// runs inside the agent, the agent's messaging socket and token, its
/// executable or pid. A name is kept when it holds a setting the user
/// chose, such as an API key, a provider, a model or a config directory
/// (`ANTHROPIC_*`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_EFFORT`,
/// `OPENAI_*`, `CODEX_HOME`). The names are listed one by one because the
/// agents put both kinds under the same prefixes.
const REMOVED_AGENT_NAMES: &[&str] = &[
    // Claude Code
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_CHILD_NOTES_DIR",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "AI_AGENT",
    // Codex
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_TOOL_CALL_ID",
    "CODEX_CI",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
];

/// Whether `name` was set by the terminal or the agent the app was
/// started from.
fn is_inherited_identity(name: &OsStr) -> bool {
    let name = name.as_bytes();
    REMOVED_NAMES
        .iter()
        .chain(REMOVED_AGENT_NAMES)
        .any(|removed| name == removed.as_bytes())
        || REMOVED_PREFIXES.iter().any(|prefix| {
            let prefix = prefix.as_bytes();
            name.starts_with(prefix) || name == &prefix[..prefix.len() - 1]
        })
}

/// The child's whole environment, sorted by name.
///
/// `inherited` is the app's own environment. Names that point at the
/// terminal or the agent the app was started from are dropped, Benterm's own terminal
/// names are set, and `extra`, which comes from the caller, replaces any
/// value set before it.
pub(crate) fn child_env(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    extra: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut vars: BTreeMap<OsString, OsString> = inherited
        .into_iter()
        .filter(|(name, _)| !is_inherited_identity(name))
        .collect();
    vars.insert("TERM".into(), "xterm-256color".into());
    vars.insert("COLORTERM".into(), "truecolor".into());
    vars.insert("TERM_PROGRAM".into(), "Benterm".into());
    vars.insert("TERM_PROGRAM_VERSION".into(), VERSION.into());
    vars.extend(extra.iter().cloned());
    vars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn sets_benterm_terminal_identity_over_inherited_values() {
        // (inherited, name, expected value in the child)
        type Row<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a str);
        let version = env!("CARGO_PKG_VERSION");
        let table: &[Row] = &[
            (&[], "TERM", "xterm-256color"),
            (&[("TERM", "xterm-ghostty")], "TERM", "xterm-256color"),
            (&[], "COLORTERM", "truecolor"),
            (&[("COLORTERM", "24bit")], "COLORTERM", "truecolor"),
            (&[], "TERM_PROGRAM", "Benterm"),
            (&[("TERM_PROGRAM", "ghostty")], "TERM_PROGRAM", "Benterm"),
            (
                &[("TERM_PROGRAM", "Apple_Terminal")],
                "TERM_PROGRAM",
                "Benterm",
            ),
            (&[], "TERM_PROGRAM_VERSION", version),
            (
                &[("TERM_PROGRAM_VERSION", "1.2.3")],
                "TERM_PROGRAM_VERSION",
                version,
            ),
        ];
        for (inherited, name, expected) in table {
            let env = child_env(vars(inherited), &[]);
            assert_eq!(
                get(&env, name),
                Some(*expected),
                "{name} from {inherited:?}"
            );
        }
    }

    #[test]
    fn removes_variables_other_terminals_set_and_keeps_the_rest() {
        // (inherited name, kept in the child)
        let table: &[(&str, bool)] = &[
            // herdr and its hc command
            ("HC_TERMINAL_ID", false),
            ("HC_TERMINAL_SOCKET", false),
            ("HC_SCOPE", false),
            ("HERDR_ENV", false),
            ("HERDR_PANE_ID", false),
            // Ghostty
            ("GHOSTTY_RESOURCES_DIR", false),
            ("GHOSTTY_BIN_DIR", false),
            ("GHOSTTY_SHELL_FEATURES", false),
            // Terminal.app and iTerm2
            ("TERM_SESSION_ID", false),
            ("ITERM_SESSION_ID", false),
            ("ITERM_PROFILE", false),
            ("LC_TERMINAL", false),
            ("LC_TERMINAL_VERSION", false),
            // WezTerm, kitty, Alacritty, VTE based terminals
            ("WEZTERM_PANE", false),
            ("WEZTERM_EXECUTABLE", false),
            ("KITTY_WINDOW_ID", false),
            ("KITTY_PID", false),
            ("ALACRITTY_WINDOW_ID", false),
            ("ALACRITTY_SOCKET", false),
            ("VTE_VERSION", false),
            ("WINDOWID", false),
            // terminal multiplexers
            ("TMUX", false),
            ("TMUX_PANE", false),
            ("STY", false),
            ("ZELLIJ", false),
            ("ZELLIJ_SESSION_NAME", false),
            // Benterm's own socket API: address and pane id are kept here,
            // while credentials are always stripped from inherited env.
            ("BENTERM_SOCKET", true),
            ("BENTERM_PANE_ID", true),
            ("BENTERM_PANE_TOKEN", false),
            ("BENTERM_CONTROL_TOKEN", false),
            // the user's own
            ("PATH", true),
            ("HOME", true),
            ("SHELL", true),
            ("LANG", true),
            ("LC_ALL", true),
            ("SSH_AUTH_SOCK", true),
            // names that only look alike
            ("HCX", true),
            ("TMUX_CONFIG", true),
            ("ZELLIJX", true),
            ("MY_HC_TERMINAL_ID", true),
        ];
        for (name, kept) in table {
            let env = child_env(vars(&[(name, "x")]), &[]);
            assert_eq!(get(&env, name).is_some(), *kept, "{name}");
        }
    }

    #[test]
    fn removes_agent_session_variables_and_keeps_agent_settings() {
        // (inherited name, kept in the child)
        let table: &[(&str, bool)] = &[
            // a running Claude Code session and the process that runs it
            ("CLAUDECODE", false),
            ("CLAUDE_CODE_ENTRYPOINT", false),
            ("CLAUDE_CODE_SESSION_ID", false),
            ("CLAUDE_CODE_CHILD_SESSION", false),
            ("CLAUDE_CODE_SESSION_ATTENDED", false),
            ("CLAUDE_CODE_BRIDGE_SESSION_ID", false),
            ("CLAUDE_CODE_HOST_SESSION_ID", false),
            ("CLAUDE_CODE_MESSAGING_SOCKET", false),
            ("CLAUDE_CODE_MESSAGING_TOKEN", false),
            ("CLAUDE_CODE_CHILD_NOTES_DIR", false),
            ("CLAUDE_CODE_EXECPATH", false),
            ("CLAUDE_PID", false),
            ("AI_AGENT", false),
            // a running Codex session and its sandbox
            ("CODEX_THREAD_ID", false),
            ("CODEX_SESSION_ID", false),
            ("CODEX_TOOL_CALL_ID", false),
            ("CODEX_CI", false),
            ("CODEX_SANDBOX", false),
            ("CODEX_SANDBOX_NETWORK_DISABLED", false),
            // settings the user chose
            ("ANTHROPIC_API_KEY", true),
            ("ANTHROPIC_MODEL", true),
            ("ANTHROPIC_BASE_URL", true),
            ("CLAUDE_CODE_USE_BEDROCK", true),
            ("CLAUDE_CODE_USE_VERTEX", true),
            ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", true),
            ("CLAUDE_CONFIG_DIR", true),
            ("CLAUDE_EFFORT", true),
            ("OPENAI_API_KEY", true),
            ("OPENAI_BASE_URL", true),
            ("CODEX_HOME", true),
        ];
        for (name, kept) in table {
            let env = child_env(vars(&[(name, "x")]), &[]);
            assert_eq!(get(&env, name).is_some(), *kept, "{name}");
        }
    }

    #[test]
    fn the_caller_can_set_a_removed_name() {
        let extra = vars(&[("TMUX", "mine")]);
        let env = child_env(vars(&[("TMUX", "outer")]), &extra);
        assert_eq!(get(&env, "TMUX"), Some("mine"));
    }

    #[test]
    fn inherited_benterm_tokens_are_stripped_and_fresh_token_can_be_supplied() {
        let inherited = vars(&[
            ("BENTERM_PANE_TOKEN", "outer-pane-secret"),
            ("BENTERM_CONTROL_TOKEN", "outer-control-secret"),
            ("BENTERM_SOCKET", "/tmp/outer.sock"),
        ]);
        let extra = vars(&[("BENTERM_PANE_TOKEN", "fresh-child-token")]);
        let env = child_env(inherited, &extra);
        assert_eq!(get(&env, "BENTERM_PANE_TOKEN"), Some("fresh-child-token"));
        assert_eq!(get(&env, "BENTERM_CONTROL_TOKEN"), None);
        assert_eq!(get(&env, "BENTERM_SOCKET"), Some("/tmp/outer.sock"));
    }

    #[test]
    fn the_callers_values_replace_benterms() {
        let extra = vars(&[("TERM_PROGRAM", "custom"), ("FOO", "bar")]);
        let env = child_env(vars(&[("TERM_PROGRAM", "ghostty")]), &extra);
        assert_eq!(get(&env, "TERM_PROGRAM"), Some("custom"));
        assert_eq!(get(&env, "FOO"), Some("bar"));
    }

    #[test]
    fn keeps_unrelated_variables_and_sorts_by_name() {
        let env = child_env(vars(&[("PATH", "/usr/bin"), ("HOME", "/h")]), &[]);
        assert_eq!(get(&env, "PATH"), Some("/usr/bin"));
        assert_eq!(get(&env, "HOME"), Some("/h"));
        let names: Vec<_> = env.iter().map(|(name, _)| name.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
