//! Checks that every command and option documented in `--help` is accepted
//! by the parser, that unknown options fail with exit status 2, and that
//! `VALUE_OPTIONS` stays in sync with the help text.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::setup::{self, SetupEnv};
use benterm_ipc::{AgentTarget, Request, WaitForAgents};
use common::start_server;

#[derive(Clone, Debug, PartialEq, Eq)]
struct HelpOption {
    name: String,
    value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HelpCommand {
    path: Vec<String>,
    options: Vec<HelpOption>,
}

#[derive(Debug)]
struct ParsedHelp {
    global_options: Vec<HelpOption>,
    commands: Vec<HelpCommand>,
}

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_cli(roles: &Path, env: &CliEnv, args: &[String]) -> Run {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run_with_roles(
        args,
        env,
        Some(roles),
        std::io::empty(),
        &mut stdout,
        &mut stderr,
    );
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

fn run_setup(env: &SetupEnv, args: &[String]) -> Run {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = setup::run(args, env, std::io::empty(), &mut stdout, &mut stderr);
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

/// Splits a synopsis fragment into tokens, separating `[` and `]` while
/// keeping `a|b` value choices intact.
fn tokenize_synopsis(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    for mut word in text.split_whitespace() {
        while let Some(rest) = word.strip_prefix('[') {
            tokens.push("[");
            word = rest;
        }
        let mut closings = 0;
        while let Some(rest) = word.strip_suffix(']') {
            closings += 1;
            word = rest;
        }
        if !word.is_empty() {
            tokens.push(word);
        }
        tokens.extend(std::iter::repeat_n("]", closings));
    }
    tokens
}

fn is_command_word(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Extracts `--option` and `--option PLACEHOLDER` entries from synopsis tokens.
fn extract_options(tokens: &[&str]) -> Vec<HelpOption> {
    let mut options = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token.starts_with("--") && token != "--" {
            let next = tokens.get(i + 1).copied();
            let value = match next {
                Some(val) if !matches!(val, "[" | "]" | "|") && !val.starts_with('-') => {
                    i += 1;
                    Some(val.to_string())
                }
                _ => None,
            };
            options.push(HelpOption {
                name: token.to_string(),
                value,
            });
        }
        i += 1;
    }
    options
}

fn parse_synopsis_line(line: &str) -> HelpCommand {
    let tokens = tokenize_synopsis(line);
    let mut path = Vec::new();
    let mut i = 0;
    while i < tokens.len() && is_command_word(tokens[i]) {
        path.push(tokens[i].to_string());
        i += 1;
    }
    assert!(!path.is_empty(), "synopsis line has no command: {line:?}");
    let options = extract_options(&tokens[i..]);
    HelpCommand { path, options }
}

/// Parses `benterm --help` dynamically: the global options from the
/// `usage:` line and every command/subcommand synopsis under `commands:`.
fn parse_main_help(help: &str) -> ParsedHelp {
    let usage_line = help
        .lines()
        .find(|line| line.starts_with("usage: benterm "))
        .expect("help has a usage line");
    let header_rest = usage_line.strip_prefix("usage: benterm ").unwrap();
    let global_options = extract_options(&tokenize_synopsis(header_rest));

    let mut commands = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if !in_commands {
            if line == "commands:" {
                in_commands = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            break;
        }
        // Command synopses are indented by two spaces; their descriptions by six.
        if let Some(synopsis) = line.strip_prefix("  ")
            && !synopsis.starts_with(' ')
        {
            commands.push(parse_synopsis_line(synopsis));
        }
    }
    assert!(!commands.is_empty(), "no commands parsed from help");
    ParsedHelp {
        global_options,
        commands,
    }
}

fn parse_setup_help(help: &str) -> Vec<HelpCommand> {
    let mut commands = Vec::new();
    for line in help.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some((_, rest)) = line.split_once("benterm ") {
            commands.push(parse_synopsis_line(rest));
        }
    }
    assert!(!commands.is_empty(), "no commands parsed from setup help");
    commands
}

/// Picks a valid argument value from a help placeholder such as `SECONDS`,
/// `claude|codex`, `DIR`, or `STATES`.
fn sample_value(placeholder: &str, temp_dir: &Path) -> String {
    if let Some((first, _)) = placeholder.split_once('|') {
        return first.to_string();
    }
    match placeholder {
        "SECONDS" | "N" | "ID" | "MS" => "1".to_string(),
        "STATES" => "done".to_string(),
        "DIR" => temp_dir.join("dir").to_string_lossy().into_owned(),
        "PATH" => temp_dir.join("missing.sock").to_string_lossy().into_owned(),
        _ => "sample".to_string(),
    }
}

fn isolated_cli_env(temp_dir: &Path) -> CliEnv {
    CliEnv {
        socket: Some(temp_dir.join("missing.sock").into_os_string()),
        pane: Some("3".into()),
        cwd: Some(temp_dir.to_path_buf()),
        path: None,
        report_check: Some("off".into()),
        state_dir: Some(temp_dir.join("state")),
        config_file: Some(temp_dir.join("config.toml")),
        mailbox_dir: None,
        ..CliEnv::default()
    }
}

fn isolated_setup_env(temp_dir: &Path) -> SetupEnv {
    let home = temp_dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let exe = temp_dir.join("app/benterm");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, "binary").unwrap();
    SetupEnv {
        home,
        path: None,
        exe,
        skill: None,
    }
}

fn is_hook_command(path: &[String]) -> bool {
    matches!(
        path.first().map(String::as_str),
        Some("hook" | "report-check")
    )
}

fn is_setup_command(path: &[String]) -> bool {
    path.first().map(String::as_str) == Some("setup")
}

#[track_caller]
fn assert_option_was_understood(args: &[String], opt: &HelpOption, run: &Run) {
    let forbidden = [
        "unknown option",
        "does not apply to",
        "needs a value",
        "needs a directory",
        "unknown command",
        "unknown subcommand",
        "unexpected argument",
        "cannot be combined",
    ];
    for needle in forbidden {
        assert!(
            !run.stderr.contains(needle),
            "option {} was rejected for {args:?} (exit {}): {}",
            opt.name,
            run.code,
            run.stderr
        );
    }
}

#[test]
fn every_option_in_help_is_understood_by_its_command() {
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join("roles");
    fs::create_dir_all(&roles).unwrap();
    let env = isolated_cli_env(dir.path());
    let setup_env = isolated_setup_env(dir.path());

    let main_help = run_cli(&roles, &env, &["--help".to_string()]);
    assert_eq!(main_help.code, 0, "{}", main_help.stderr);
    let parsed = parse_main_help(&main_help.stdout);

    // Every top-level command listed in help must be known to `is_command`.
    for command in &parsed.commands {
        assert!(
            cli::is_command(&command.path[0]),
            "top-level command {:?} is in help but not is_command",
            command.path[0]
        );
    }

    // `setup --help` must match the `setup` commands listed in main `--help`.
    let setup_help = run_setup(&setup_env, &["--help".to_string()]);
    assert_eq!(setup_help.code, 0, "{}", setup_help.stderr);
    let setup_from_setup_help = parse_setup_help(&setup_help.stdout);
    let setup_from_main_help: Vec<HelpCommand> = parsed
        .commands
        .iter()
        .filter(|command| is_setup_command(&command.path))
        .cloned()
        .collect();
    assert_eq!(
        setup_from_main_help, setup_from_setup_help,
        "setup commands in main --help and setup --help drifted apart"
    );

    // For every command/subcommand, pass each of its documented options one by one.
    for command in &parsed.commands {
        for opt in &command.options {
            let mut args = command.path.clone();
            args.push(opt.name.clone());
            if let Some(placeholder) = &opt.value {
                args.push(sample_value(placeholder, dir.path()));
            }
            let ran = if is_setup_command(&command.path) {
                run_setup(&setup_env, &args[1..])
            } else {
                run_cli(&roles, &env, &args)
            };
            assert_option_was_understood(&args, opt, &ran);

            // A value-taking option without its value must be reported as missing one.
            if opt.value.is_some() {
                let mut missing = command.path.clone();
                missing.push(opt.name.clone());
                let ran = if is_setup_command(&command.path) {
                    run_setup(&setup_env, &missing[1..])
                } else {
                    run_cli(&roles, &env, &missing)
                };
                if is_hook_command(&command.path) {
                    assert_eq!(ran.code, 0, "{missing:?}: {}", ran.stderr);
                } else {
                    assert_eq!(ran.code, 2, "{missing:?}: {}", ran.stderr);
                }
                assert!(
                    ran.stderr.contains(&opt.name),
                    "missing value for {} should mention it in {missing:?}: {}",
                    opt.name,
                    ran.stderr
                );
            }
        }

        // Global options (`--socket`, `--timeout`) apply to every non-setup command,
        // both before and after the command name.
        if !is_setup_command(&command.path) {
            for opt in &parsed.global_options {
                let value = opt.value.as_deref().map(|p| sample_value(p, dir.path()));

                let mut before = vec![opt.name.clone()];
                if let Some(v) = &value {
                    before.push(v.clone());
                }
                before.extend(command.path.iter().cloned());
                let ran_before = run_cli(&roles, &env, &before);
                assert_option_was_understood(&before, opt, &ran_before);

                let mut after = command.path.clone();
                after.push(opt.name.clone());
                if let Some(v) = &value {
                    after.push(v.clone());
                }
                let ran_after = run_cli(&roles, &env, &after);
                assert_option_was_understood(&after, opt, &ran_after);
            }
        }
    }
}

#[test]
fn unknown_option_is_rejected_for_every_command_in_help() {
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join("roles");
    fs::create_dir_all(&roles).unwrap();
    let env = isolated_cli_env(dir.path());
    let setup_env = isolated_setup_env(dir.path());

    let main_help = run_cli(&roles, &env, &["--help".to_string()]);
    assert_eq!(main_help.code, 0, "{}", main_help.stderr);
    let parsed = parse_main_help(&main_help.stdout);

    const UNKNOWN: &str = "--definitely-unknown-option";

    for command in &parsed.commands {
        let mut args = command.path.clone();
        args.push(UNKNOWN.to_string());

        let ran = if is_setup_command(&command.path) {
            run_setup(&setup_env, &args[1..])
        } else {
            run_cli(&roles, &env, &args)
        };

        if is_hook_command(&command.path) {
            // Hooks never fail the agent, but still report the unknown option on stderr.
            assert_eq!(ran.code, 0, "{args:?}: {}", ran.stderr);
        } else {
            assert_eq!(ran.code, 2, "{args:?}: {}", ran.stderr);
        }
        assert!(
            ran.stderr.contains("unknown option") && ran.stderr.contains(UNKNOWN),
            "expected unknown option error for {args:?}, got: {:?}",
            ran.stderr
        );
    }
}

#[test]
fn value_options_match_help_in_hook_detection() {
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join("roles");
    fs::create_dir_all(&roles).unwrap();
    let env = isolated_cli_env(dir.path());

    let main_help = run_cli(&roles, &env, &["--help".to_string()]);
    let parsed = parse_main_help(&main_help.stdout);

    // Collect every non-setup option and check that help agrees on whether it takes a value.
    let mut takes_value: BTreeMap<String, bool> = BTreeMap::new();
    let mut record = |opt: &HelpOption| {
        let has_value = opt.value.is_some();
        if let Some(&prev) = takes_value.get(&opt.name) {
            assert_eq!(
                prev, has_value,
                "option {} is documented both with and without a value in help",
                opt.name
            );
        } else {
            takes_value.insert(opt.name.clone(), has_value);
        }
    };
    for opt in &parsed.global_options {
        record(opt);
    }
    for command in &parsed.commands {
        if !is_setup_command(&command.path) {
            for opt in &command.options {
                record(opt);
            }
        }
    }

    assert!(!takes_value.is_empty());
    let mut value_opts = BTreeSet::new();
    let mut flag_opts = BTreeSet::new();
    for (name, has_value) in takes_value {
        if has_value {
            value_opts.insert(name);
        } else {
            flag_opts.insert(name);
        }
    }

    for opt in &value_opts {
        assert!(
            !cli::is_hook(&[opt.as_str(), "hook", "list"]),
            "value option {opt} must consume \"hook\" as its value (missing from VALUE_OPTIONS?)"
        );
        assert!(
            cli::is_hook(&[opt.as_str(), "value", "hook"]),
            "value option {opt} must skip its value to find \"hook\" (missing from VALUE_OPTIONS?)"
        );
    }
    for opt in &flag_opts {
        assert!(
            cli::is_hook(&[opt.as_str(), "hook"]),
            "flag option {opt} must not consume \"hook\" as a value (wrongly in VALUE_OPTIONS?)"
        );
    }
}

#[test]
fn agent_wait_help_options_timeout_and_wait_are_parsed_with_values() {
    let server = start_server();
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join("roles");
    fs::create_dir_all(&roles).unwrap();
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        ..CliEnv::default()
    };

    let help = run_cli(
        &roles,
        &env,
        &["agent", "wait", "--help"].map(str::to_string),
    );
    assert_eq!(help.code, 0, "{}", help.stderr);
    assert!(help.stdout.contains("--timeout SECONDS"), "{}", help.stdout);
    assert!(help.stdout.contains("--wait SECONDS"), "{}", help.stdout);

    let waited = run_cli(
        &roles,
        &env,
        &[
            "agent",
            "wait",
            "w1",
            "--timeout",
            "2",
            "--wait",
            "5",
            "--settle",
            "0.5",
            "--until",
            "done",
        ]
        .map(str::to_string),
    );
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    assert_eq!(
        server.app.requests(),
        [Request::WaitForAgents(WaitForAgents {
            agent: Some(AgentTarget::Name("w1".into())),
            children_of: None,
            until: vec![benterm_ipc::AgentActivity::Done],
            wait_ms: Some(5_000),
            settle_ms: Some(500),
        })]
    );
}
