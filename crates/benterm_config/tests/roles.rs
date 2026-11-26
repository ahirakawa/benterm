//! Roles: one TOML file each in the `roles` directory beside the config
//! file, read into what an agent is started with.

use benterm_config::{Agent, Problem, Role, Roles, load_roles, roles_dir_from, worker_rules_line};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const REVIEWER: &str = r#"
name = "reviewer"
description = "PR を読む"
agent = "claude"
model = "opus"
permission_mode = "auto"
prompt = "差分を読んで指摘する"
"#;

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

fn reviewer() -> Role {
    Role {
        name: "reviewer".into(),
        description: "PR を読む".into(),
        agent: Agent::Claude,
        model: Some("opus".into()),
        permission_mode: Some("auto".into()),
        prompt: "差分を読んで指摘する".into(),
        env: Vec::new(),
        args: Vec::new(),
        worker_rules: None,
    }
}

/// [`reviewer`] as read from the roles directory `dir`, which knows where
/// the rules for workers are.
fn read_reviewer(dir: &Path) -> Role {
    Role {
        worker_rules: Some(dir.parent().unwrap().join("worker.md")),
        ..reviewer()
    }
}

fn names(roles: &Roles) -> Vec<&str> {
    roles.roles.iter().map(|role| role.name.as_str()).collect()
}

#[test]
fn a_role_file_is_read_into_one_role() {
    let dir = dir_with(&[("reviewer.toml", REVIEWER)]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.warnings, []);
    let worker_rules = Some(dir.path().parent().unwrap().join("worker.md"));
    assert_eq!(
        roles.roles,
        [Role {
            worker_rules,
            ..reviewer()
        }]
    );
}

#[test]
fn a_role_with_an_unknown_agent_is_left_out_with_a_warning_naming_its_file() {
    let bad = REVIEWER
        .replace("\"reviewer\"", "\"bad\"")
        .replace("\"claude\"", "\"gemini\"");
    let dir = dir_with(&[("reviewer.toml", REVIEWER), ("bad.toml", &bad)]);
    let roles = load_roles(dir.path());
    assert_eq!(names(&roles), ["reviewer"]);
    assert_eq!(roles.warnings.len(), 1);
    let warning = &roles.warnings[0];
    assert_eq!(warning.key, "roles/bad.toml");
    let text = warning.to_string();
    assert!(text.starts_with("roles/bad.toml: "), "{text}");
    assert!(text.contains("gemini"), "{text}");
}

#[test]
fn a_file_that_is_not_toml_is_a_warning_and_the_other_roles_are_read() {
    let dir = dir_with(&[("reviewer.toml", REVIEWER), ("broken.toml", "name = ")]);
    let roles = load_roles(dir.path());
    assert_eq!(names(&roles), ["reviewer"]);
    assert_eq!(roles.warnings.len(), 1);
    assert_eq!(roles.warnings[0].key, "roles/broken.toml");
    assert!(matches!(roles.warnings[0].problem, Problem::Syntax(_)));
}

#[test]
fn a_role_needs_a_name_an_agent_and_a_prompt() {
    for key in ["name", "agent", "prompt"] {
        let text: String = REVIEWER
            .lines()
            .filter(|line| !line.starts_with(key))
            .map(|line| format!("{line}\n"))
            .collect();
        let dir = dir_with(&[("reviewer.toml", &text)]);
        let roles = load_roles(dir.path());
        assert_eq!(roles.roles, [], "without {key}");
        assert!(
            roles.warnings[0].to_string().contains(key),
            "without {key}: {}",
            roles.warnings[0]
        );
    }
}

#[test]
fn description_model_and_permission_mode_may_be_left_out() {
    let dir = dir_with(&[(
        "plain.toml",
        "name = \"plain\"\nagent = \"codex\"\nprompt = \"p\"\n",
    )]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.warnings, []);
    let role = &roles.roles[0];
    assert_eq!(role.description, "");
    assert_eq!(role.model, None);
    assert_eq!(role.permission_mode, None);
    assert_eq!(role.env, []);
    assert_eq!(role.args, Vec::<String>::new());
    assert_eq!(role.command(""), ["codex", "--no-daemon", "--", "p"]);
}

#[test]
fn a_misspelt_key_leaves_the_role_out() {
    let text = REVIEWER.replace("model =", "modle =");
    let dir = dir_with(&[("reviewer.toml", &text)]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.roles, []);
    assert!(roles.warnings[0].to_string().contains("modle"));
}

#[test]
fn a_name_must_do_as_the_name_of_an_agent() {
    let text = REVIEWER.replace("\"reviewer\"", "\"Report Reviewer\"");
    let dir = dir_with(&[("reviewer.toml", &text)]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.roles, []);
    assert!(roles.warnings[0].to_string().contains("Report Reviewer"));
}

#[test]
fn a_permission_mode_must_be_one_the_agent_has() {
    let claude = REVIEWER.replace("\"auto\"", "\"workspace-write\"");
    let codex = REVIEWER
        .replace("\"claude\"", "\"codex\"")
        .replace("\"reviewer\"", "\"coder\"")
        .replace("\"auto\"", "\"workspace-write\"");
    let dir = dir_with(&[("reviewer.toml", &claude), ("coder.toml", &codex)]);
    let roles = load_roles(dir.path());
    assert_eq!(names(&roles), ["coder"]);
    assert_eq!(roles.warnings.len(), 1);
    assert_eq!(roles.warnings[0].key, "roles/reviewer.toml");
    assert!(roles.warnings[0].to_string().contains("workspace-write"));
}

#[test]
fn a_second_role_with_the_same_name_is_left_out() {
    let dir = dir_with(&[("a.toml", REVIEWER), ("b.toml", REVIEWER)]);
    let roles = load_roles(dir.path());
    assert_eq!(names(&roles), ["reviewer"]);
    assert_eq!(roles.warnings.len(), 1);
    assert_eq!(roles.warnings[0].key, "roles/b.toml");
}

#[test]
fn roles_come_in_order_of_name_and_other_files_are_passed_over() {
    let coder = REVIEWER.replace("\"reviewer\"", "\"coder\"");
    let dir = dir_with(&[
        ("x.toml", REVIEWER),
        ("y.toml", &coder),
        ("README.md", "not a role"),
        (".x.toml.swp", "not a role"),
    ]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.warnings, []);
    assert_eq!(names(&roles), ["coder", "reviewer"]);
}

#[test]
fn a_missing_directory_has_no_roles_and_no_warnings() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(load_roles(&dir.path().join("roles")), Roles::default());
}

#[test]
fn the_first_prompt_is_the_instructions_then_the_task() {
    let role = reviewer();
    assert_eq!(
        role.first_prompt("PR #1 を見る"),
        "差分を読んで指摘する\n\nPR #1 を見る"
    );
    assert_eq!(role.first_prompt(""), "差分を読んで指摘する");
    assert_eq!(role.first_prompt("  "), "差分を読んで指摘する");
}

/// A config directory with a roles directory in it, and the file of the
/// rules for workers if `rules` is set.
fn config_with(rules: bool) -> (tempfile::TempDir, PathBuf) {
    let config = tempfile::tempdir().unwrap();
    let roles = config.path().join("roles");
    fs::create_dir(&roles).unwrap();
    fs::write(roles.join("reviewer.toml"), REVIEWER).unwrap();
    let file = config.path().join("worker.md");
    if rules {
        fs::write(&file, "- 報告は agent report で返す\n").unwrap();
    }
    (config, file)
}

fn loaded_reviewer(config: &tempfile::TempDir) -> Role {
    load_roles(&config.path().join("roles")).roles.remove(0)
}

#[test]
fn the_rules_line_names_the_file_and_asks_to_pass_it_on() {
    let (_config, file) = config_with(true);
    let line = worker_rules_line(&file).unwrap();
    assert_eq!(
        line,
        format!(
            "Read {} before you start. When you start a worker, put this line first in its first message.",
            file.display()
        )
    );
    assert!(file.is_absolute());
    assert!(!line.contains('\n'));
}

#[test]
fn there_is_no_rules_line_without_the_file() {
    let (_config, file) = config_with(false);
    assert_eq!(worker_rules_line(&file), None);
}

#[test]
fn with_the_rules_file_the_first_prompt_starts_with_the_line() {
    let (config, file) = config_with(true);
    let line = worker_rules_line(&file).unwrap();
    let role = loaded_reviewer(&config);
    assert_eq!(
        role.first_prompt("PR #1 を見る"),
        format!("{line}\n差分を読んで指摘する\n\nPR #1 を見る")
    );
    assert_eq!(
        role.first_prompt(""),
        format!("{line}\n差分を読んで指摘する")
    );
    assert_eq!(role.first_prompt("PR #1 を見る").matches(&line).count(), 1);
}

#[test]
fn without_the_rules_file_the_first_prompt_is_as_before() {
    let (config, _file) = config_with(false);
    let role = loaded_reviewer(&config);
    assert_eq!(
        role.first_prompt("PR #1 を見る"),
        "差分を読んで指摘する\n\nPR #1 を見る"
    );
    assert_eq!(role.first_prompt(""), "差分を読んで指摘する");
}

/// The file is looked for when the agent starts, so one put there after
/// the roles were read is not missed.
#[test]
fn the_rules_file_is_looked_for_when_the_prompt_is_made() {
    let (config, file) = config_with(false);
    let role = loaded_reviewer(&config);
    fs::write(&file, "決まり\n").unwrap();
    let line = worker_rules_line(&file).unwrap();
    assert_eq!(
        role.first_prompt(""),
        format!("{line}\n差分を読んで指摘する")
    );
}

#[test]
fn the_rules_line_comes_after_the_end_of_the_options() {
    let (config, file) = config_with(true);
    let line = worker_rules_line(&file).unwrap();
    let command = loaded_reviewer(&config).command("作業");
    let (last, rest) = command.split_last().unwrap();
    assert_eq!(rest.last().map(String::as_str), Some("--"));
    assert_eq!(last, &format!("{line}\n差分を読んで指摘する\n\n作業"));
}

#[test]
fn a_claude_role_starts_claude_with_its_model_mode_and_prompt() {
    assert_eq!(
        reviewer().command("PR #1 を見る"),
        [
            "claude",
            "--model",
            "opus",
            "--permission-mode",
            "auto",
            "--",
            "差分を読んで指摘する\n\nPR #1 を見る",
        ]
    );
}

#[test]
fn a_codex_role_starts_codex_with_its_model_sandbox_and_prompt() {
    let role = Role {
        agent: Agent::Codex,
        model: Some("gpt-5-codex".into()),
        permission_mode: Some("workspace-write".into()),
        ..reviewer()
    };
    assert_eq!(
        role.command("PR #1 を見る"),
        [
            "codex",
            "--no-daemon",
            "--model",
            "gpt-5-codex",
            "--sandbox",
            "workspace-write",
            "--",
            "差分を読んで指摘する\n\nPR #1 を見る",
        ]
    );
}

/// A prompt that starts with `-`, such as a Markdown list, is not taken
/// for an option.
#[test]
fn the_prompt_comes_after_the_end_of_the_options() {
    let role = Role {
        model: None,
        permission_mode: None,
        prompt: "- 差分を読む".into(),
        ..reviewer()
    };
    assert_eq!(role.command(""), ["claude", "--", "- 差分を読む"]);
}

const PLANNER: &str = r#"
name = "planner"
agent = "codex"
prompt = "計画を立てる"
args = ["-c", "tools.update_plan.enabled=true"]

[env]
CLAUDE_CODE_ENABLE_TODO_TOOLS = "1"
"#;

#[test]
fn a_role_may_have_variables_and_more_arguments_for_its_agent() {
    let dir = dir_with(&[("planner.toml", PLANNER)]);
    let roles = load_roles(dir.path());
    assert_eq!(roles.warnings, []);
    let role = &roles.roles[0];
    assert_eq!(
        role.env,
        [("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned())]
    );
    assert_eq!(role.args, ["-c", "tools.update_plan.enabled=true"]);
}

#[test]
fn the_arguments_come_after_the_agent_and_before_the_prompt() {
    // In a directory of its own, whose parent has no rules for workers.
    let (config, _) = config_with(false);
    let dir = config.path().join("roles");
    fs::write(dir.join("planner.toml"), PLANNER).unwrap();
    let role = load_roles(&dir).roles.remove(0);
    assert_eq!(
        role.command("作業"),
        [
            "codex",
            "--no-daemon",
            "-c",
            "tools.update_plan.enabled=true",
            "--",
            "計画を立てる\n\n作業",
        ]
    );
}

#[test]
fn the_arguments_come_after_the_model_and_the_mode() {
    let role = Role {
        args: vec!["--add-dir".into(), "../docs".into()],
        ..reviewer()
    };
    assert_eq!(
        role.command(""),
        [
            "claude",
            "--model",
            "opus",
            "--permission-mode",
            "auto",
            "--add-dir",
            "../docs",
            "--",
            "差分を読んで指摘する",
        ]
    );
}

#[test]
fn a_variable_with_a_bad_name_leaves_the_role_out() {
    for (name, why) in [("\"1BAD\"", "1BAD"), ("\"\"", "empty"), ("\"A=B\"", "A=B")] {
        let bad = format!(
            "{}\n[env]\n{name} = \"x\"\n",
            REVIEWER.replace("\"reviewer\"", "\"bad\"")
        );
        let dir = dir_with(&[("reviewer.toml", REVIEWER), ("bad.toml", &bad)]);
        let roles = load_roles(dir.path());
        assert_eq!(names(&roles), ["reviewer"], "{name}");
        assert_eq!(roles.warnings.len(), 1, "{name}");
        let text = roles.warnings[0].to_string();
        assert!(text.starts_with("roles/bad.toml: "), "{text}");
        assert!(text.contains(why), "{name}: {text}");
    }
}

#[test]
fn variables_and_arguments_must_be_strings() {
    for (tail, why) in [
        ("args = \"-c\"\n", "args"),
        ("args = [\"-c\", 1]\n", "args"),
        ("env = \"FOO=bar\"\n", "env"),
        ("[env]\nFOO = 1\n", "FOO"),
    ] {
        let bad = format!("{REVIEWER}{tail}");
        let dir = dir_with(&[("bad.toml", &bad)]);
        let roles = load_roles(dir.path());
        assert_eq!(roles.roles, [], "{tail}");
        let text = roles.warnings[0].to_string();
        assert!(text.contains(why), "{tail}: {text}");
    }
}

#[test]
fn the_roles_directory_is_beside_the_config_file() {
    fn os(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }
    assert_eq!(
        roles_dir_from(os("/x"), os("/h")),
        Some(PathBuf::from("/x/benterm/roles"))
    );
    assert_eq!(
        roles_dir_from(None, os("/h")),
        Some(Path::new("/h/.config/benterm/roles").to_path_buf())
    );
    assert_eq!(roles_dir_from(None, None), None);
}

mod watching {
    use super::*;
    use benterm_config::RolesWatcher;
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn start(dir: &Path) -> (RolesWatcher, Roles, Receiver<Roles>) {
        let (tx, rx) = mpsc::channel();
        let (watcher, roles) = RolesWatcher::start(dir, move |roles| {
            let _ = tx.send(roles);
        })
        .unwrap();
        (watcher, roles, rx)
    }

    fn wait_for(rx: &Receiver<Roles>, pred: impl Fn(&Roles) -> bool) -> Roles {
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(roles) if pred(&roles) => return roles,
                Ok(roles) => seen.push(roles),
                Err(e) => panic!("no matching roles ({e:?}); saw {seen:?}"),
            }
        }
    }

    #[test]
    fn the_directory_is_made_so_that_files_put_there_later_are_seen() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("roles");
        let (_watcher, roles, _rx) = start(&dir);
        assert_eq!(roles, Roles::default());
        assert!(dir.is_dir());
    }

    #[test]
    fn a_role_file_saved_while_watching_is_read() {
        let dir = dir_with(&[]);
        let (_watcher, _, rx) = start(dir.path());
        fs::write(dir.path().join("reviewer.toml"), REVIEWER).unwrap();
        let roles = wait_for(&rx, |roles| !roles.roles.is_empty());
        assert_eq!(
            roles.roles,
            [read_reviewer(&dir.path().canonicalize().unwrap())]
        );
    }

    #[test]
    fn the_roles_at_start_are_returned_and_a_removed_file_takes_its_role_away() {
        let dir = dir_with(&[("reviewer.toml", REVIEWER)]);
        let (_watcher, roles, rx) = start(dir.path());
        assert_eq!(
            roles.roles,
            [read_reviewer(&dir.path().canonicalize().unwrap())]
        );
        fs::remove_file(dir.path().join("reviewer.toml")).unwrap();
        wait_for(&rx, |roles| roles.roles.is_empty());
    }

    #[test]
    fn a_broken_file_saved_while_watching_is_a_warning() {
        let dir = dir_with(&[("reviewer.toml", REVIEWER)]);
        let (_watcher, _, rx) = start(dir.path());
        fs::write(dir.path().join("bad.toml"), "agent = 1").unwrap();
        let roles = wait_for(&rx, |roles| !roles.warnings.is_empty());
        assert_eq!(
            roles.roles,
            [read_reviewer(&dir.path().canonicalize().unwrap())]
        );
        assert_eq!(roles.warnings[0].key, "roles/bad.toml");
    }
}

/// A directory with a `codex` whose `--help` prints `help`.
fn fake_codex(help: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    fs::write(&codex, format!("#!/bin/sh\nprintf '%s\\n' '{help}'\n")).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn codex_launch() -> Vec<String> {
    let role = Role {
        agent: Agent::Codex,
        ..reviewer()
    };
    role.command("PR #1 を見る")
}

#[test]
fn a_codex_that_takes_no_daemon_keeps_it() {
    let dir = fake_codex("  --no-daemon  Run without the shared background server");
    let mut command = codex_launch();
    benterm_config::fit_codex_launch(&mut command, Some(dir.path().as_os_str()));
    assert_eq!(command, codex_launch());
    assert_eq!(command[1], "--no-daemon");
}

#[test]
fn a_codex_that_does_not_take_no_daemon_starts_without_it() {
    let dir = fake_codex("  --oss  Use an open source model");
    let mut command = codex_launch();
    benterm_config::fit_codex_launch(&mut command, Some(dir.path().as_os_str()));
    let mut expected = codex_launch();
    expected.remove(1);
    assert_eq!(command, expected);
}

#[test]
fn a_claude_launch_is_left_as_it_is() {
    let dir = fake_codex("");
    let mut command = reviewer().command("x");
    let before = command.clone();
    benterm_config::fit_codex_launch(&mut command, Some(dir.path().as_os_str()));
    assert_eq!(command, before);
}

#[test]
fn a_codex_that_does_not_answer_its_help_is_taken_not_to_take_no_daemon() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    fs::write(&codex, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    assert!(!benterm_config::codex_takes_no_daemon(Some(
        dir.path().as_os_str()
    )));
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(1500),
        "{waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
}
