//! `benterm setup`: the hooks, the link on the PATH and the skill, in a
//! home directory made for each test. The real `~/.claude` and `~/.codex`
//! are never read or written.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use benterm_ipc::setup::{self, CLAUDE_HOOK_EVENTS, CODEX_HOOK_EVENTS, SetupEnv};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The hooks as an earlier install wrote them, with `benterm` looked up
/// on the PATH each time a hook runs. Install now writes the path of
/// the link; see [`Home::hook`].
const EARLIER_CLAUDE_HOOK: &str = "benterm hook --agent claude";
const EARLIER_CLAUDE_CHECK: &str = "benterm report-check --agent claude";

/// A home directory, and a `benterm` and a skill outside of it, as a
/// checkout or Benterm.app has them.
struct Home {
    _dir: TempDir,
    home: PathBuf,
    exe: PathBuf,
    skill: PathBuf,
}

struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // The temporary directory may be behind a symbolic link, as /var
        // is on macOS; the links are compared with the real paths.
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        let exe = root.join("app/benterm");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "binary").unwrap();
        let skill = root.join("app/skills/benterm");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: benterm\n---\n").unwrap();
        Self {
            _dir: dir,
            home,
            exe,
            skill,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }

    /// The command install writes for an event of `agent`: the link in
    /// `~/.local/bin` by its whole path. The path of a test's home needs
    /// no quotes.
    fn hook(&self, agent: &str) -> String {
        let link = self.path(".local/bin/benterm");
        format!("{} hook --agent {agent}", link.display())
    }

    /// The command install writes for the check of the report.
    fn check(&self, agent: &str) -> String {
        let link = self.path(".local/bin/benterm");
        format!("{} report-check --agent {agent}", link.display())
    }

    fn claude_entries(&self, event: &str) -> Value {
        entries(event, &self.hook("claude"), &self.check("claude"))
    }

    fn settings(&self) -> PathBuf {
        self.path(".claude/settings.json")
    }

    fn codex_hooks(&self) -> PathBuf {
        self.path(".codex/hooks.json")
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn json(&self, path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The environment, with `~/.local/bin` on the PATH.
    fn env(&self) -> SetupEnv {
        let path =
            std::env::join_paths([self.path(".local/bin"), PathBuf::from("/usr/bin")]).unwrap();
        self.env_with_path(path)
    }

    fn env_with_path(&self, path: OsString) -> SetupEnv {
        SetupEnv {
            home: self.home.clone(),
            path: Some(path),
            exe: self.exe.clone(),
            skill: Some(self.skill.clone()),
        }
    }

    fn run(&self, args: &[&str], answer: &str) -> Ran {
        self.run_in(&self.env(), args, answer)
    }

    fn run_in(&self, env: &SetupEnv, args: &[&str], answer: &str) -> Ran {
        self.run_with_stdin(env, args, answer.as_bytes())
    }

    fn run_with_stdin(&self, env: &SetupEnv, args: &[&str], stdin: impl std::io::Read) -> Ran {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = setup::run(&args, env, stdin, &mut stdout, &mut stderr);
        Ran {
            code,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
        }
    }

    /// Every file and link under the home directory, with what it holds
    /// or points to, to tell that nothing changed.
    fn snapshot(&self) -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, into: &mut Vec<(PathBuf, String)>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
            entries.sort_by_key(|entry| entry.path());
            for entry in entries {
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_symlink() {
                    let target = fs::read_link(&path).unwrap();
                    into.push((path, format!("-> {}", target.display())));
                } else if kind.is_dir() {
                    into.push((path.clone(), "dir".into()));
                    walk(&path, into);
                } else {
                    into.push((path.clone(), fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let mut all = Vec::new();
        walk(&self.home, &mut all);
        all
    }
}

fn benterm_entry(command: &str) -> Value {
    json!({"hooks": [{"type": "command", "command": command}]})
}

/// The entries install writes for an event: the Stop goes to the check
/// of the report, which tells the app of the Stop itself once the agent
/// may stop, and a tool call is told to both.
fn entries(event: &str, hook: &str, check: &str) -> Value {
    match event {
        "Stop" => json!([benterm_entry(check)]),
        "PostToolUse" => json!([benterm_entry(hook), benterm_entry(check)]),
        _ => json!([benterm_entry(hook)]),
    }
}

fn event_names(hooks: &Value) -> Vec<String> {
    hooks.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn install_adds_the_hook_to_every_event_of_settings_without_hooks() {
    let home = Home::new();
    home.write(".claude/settings.json", r#"{"model": "opus"}"#);

    let ran = home.run(&["install"], "y\n");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let settings = home.json(&home.settings());
    assert_eq!(event_names(&settings["hooks"]), CLAUDE_HOOK_EVENTS);
    for event in CLAUDE_HOOK_EVENTS {
        assert_eq!(settings["hooks"][event], home.claude_entries(event));
    }
    assert_eq!(settings["model"], "opus");
    let codex = home.json(&home.codex_hooks());
    assert_eq!(event_names(&codex["hooks"]), CODEX_HOOK_EVENTS);
    for event in CODEX_HOOK_EVENTS {
        assert_eq!(
            codex["hooks"][event],
            entries(event, &home.hook("codex"), &home.check("codex"))
        );
    }
}

#[test]
fn install_keeps_the_other_settings_in_their_order() {
    let home = Home::new();
    home.write(
        ".claude/settings.json",
        r#"{"theme": "dark", "hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "echo hi"}]}]}, "model": "opus"}"#,
    );

    home.run(&["install"], "y\n");

    let settings = home.json(&home.settings());
    assert_eq!(event_names(&settings), ["theme", "hooks", "model"]);
    assert_eq!(
        settings["hooks"]["PreToolUse"],
        json!([
            benterm_entry("echo hi"),
            benterm_entry(&home.hook("claude"))
        ])
    );
    assert_eq!(event_names(&settings["hooks"])[0], "PreToolUse");
}

#[test]
fn install_run_twice_adds_each_hook_once() {
    let home = Home::new();
    home.run(&["install"], "y\n");
    let before = home.snapshot();

    let again = home.run(&["install"], "");

    assert_eq!(again.code, 0, "{}{}", again.stdout, again.stderr);
    assert_eq!(home.snapshot(), before);
    let settings = home.json(&home.settings());
    for event in CLAUDE_HOOK_EVENTS {
        assert_eq!(settings["hooks"][event], home.claude_entries(event));
    }
}

#[test]
fn install_takes_a_hook_written_by_hand_as_there() {
    let home = Home::new();
    home.write(
        ".claude/settings.json",
        r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "/opt/bin/benterm hook --agent claude"}]}], "Stop": [{"hooks": [{"type": "command", "command": "/opt/bin/benterm report-check --agent claude"}]}]}}"#,
    );

    home.run(&["install"], "y\n");

    let settings = home.json(&home.settings());
    assert_eq!(
        settings["hooks"]["PreToolUse"],
        json!([benterm_entry("/opt/bin/benterm hook --agent claude")])
    );
    assert_eq!(
        settings["hooks"]["Stop"],
        json!([benterm_entry(
            "/opt/bin/benterm report-check --agent claude"
        )])
    );
}

/// The hooks as install wrote them before the check of reports: every
/// event runs `benterm hook`.
fn hooks_before_the_check(home: &Home) {
    let mut hooks = serde_json::Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            json!([benterm_entry(EARLIER_CLAUDE_HOOK)]),
        );
    }
    hooks.insert(
        "Stop".into(),
        json!([
            benterm_entry("say done"),
            benterm_entry(EARLIER_CLAUDE_HOOK)
        ]),
    );
    home.write(
        ".claude/settings.json",
        &json!({"hooks": hooks}).to_string(),
    );
}

/// A Stop that both commands were run for would be told to the app
/// before the check has decided, and a blocked agent would show as
/// done. So the check takes the place of the hook at the Stop.
#[test]
fn install_gives_the_stop_of_an_earlier_install_to_the_check() {
    let home = Home::new();
    hooks_before_the_check(&home);

    let ran = home.run(&["install"], "y\n");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let settings = home.json(&home.settings());
    assert_eq!(event_names(&settings["hooks"]), CLAUDE_HOOK_EVENTS);
    assert_eq!(
        settings["hooks"]["Stop"],
        json!([
            benterm_entry("say done"),
            benterm_entry(&home.check("claude"))
        ])
    );
    for event in CLAUDE_HOOK_EVENTS.iter().filter(|event| **event != "Stop") {
        assert_eq!(
            settings["hooks"][event],
            home.claude_entries(event),
            "{event}"
        );
    }
}

/// A file that has the check at the Stop and still the hook an earlier
/// install wrote, as after the check was added by hand.
#[test]
fn install_takes_the_earlier_stop_hook_out_where_the_check_is_there() {
    let home = Home::new();
    hooks_before_the_check(&home);
    home.run(&["install"], "y\n");
    let mut settings = home.json(&home.settings());
    settings["hooks"]["Stop"]
        .as_array_mut()
        .unwrap()
        .push(benterm_entry(EARLIER_CLAUDE_HOOK));
    home.write(".claude/settings.json", &settings.to_string());

    let ran = home.run(&["install"], "y\n");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        home.json(&home.settings())["hooks"]["Stop"],
        json!([
            benterm_entry("say done"),
            benterm_entry(&home.check("claude"))
        ])
    );
}

/// A Stop hook that is not as install wrote it is the user's: it stays,
/// and status says what it does.
#[test]
fn status_tells_of_a_stop_hook_written_by_hand_that_tells_the_app_too_early() {
    let home = Home::new();
    home.write(
        ".claude/settings.json",
        r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/opt/bin/benterm hook --agent claude"}]}]}}"#,
    );
    home.run(&["install"], "y\n");
    assert_eq!(
        home.json(&home.settings())["hooks"]["Stop"],
        json!([
            benterm_entry("/opt/bin/benterm hook --agent claude"),
            benterm_entry(&home.check("claude"))
        ])
    );

    let ran = home.run(&["status"], "");

    let claude = ran
        .stdout
        .lines()
        .find(|line| line.contains("Claude Code"))
        .unwrap_or_else(|| panic!("{}", ran.stdout));
    assert!(claude.contains("Stop also runs benterm hook"), "{claude}");
    assert!(
        !ran.stdout.contains("everything is set up"),
        "{}",
        ran.stdout
    );
}

#[test]
fn status_names_the_events_the_check_is_not_run_for() {
    let home = Home::new();
    hooks_before_the_check(&home);

    let ran = home.run(&["status"], "");

    let claude = ran
        .stdout
        .lines()
        .find(|line| line.contains("Claude Code"))
        .unwrap_or_else(|| panic!("{}", ran.stdout));
    assert!(
        claude.ends_with("missing PostToolUse (report-check), Stop (report-check)"),
        "{claude}"
    );
}

#[test]
fn uninstall_takes_out_the_hooks_of_an_earlier_install_too() {
    let home = Home::new();
    hooks_before_the_check(&home);

    home.run(&["uninstall"], "y\n");

    assert_eq!(
        home.json(&home.settings()),
        json!({"hooks": {"Stop": [benterm_entry("say done")]}})
    );
}

#[test]
fn install_links_benterm_and_the_skill() {
    let home = Home::new();

    let ran = home.run(&["install"], "y\n");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        fs::read_link(home.path(".local/bin/benterm")).unwrap(),
        home.exe
    );
    assert_eq!(
        fs::read_link(home.path(".claude/skills/benterm")).unwrap(),
        home.skill
    );
    assert_eq!(
        fs::read_link(home.path(".codex/skills/benterm")).unwrap(),
        home.skill
    );
}

#[test]
fn install_puts_the_link_in_the_bin_dir_it_is_given() {
    let home = Home::new();
    let bin = home.path("bin");
    let path = std::env::join_paths([&bin]).unwrap();

    let ran = home.run_in(
        &home.env_with_path(path),
        &["install", "--bin-dir", bin.to_str().unwrap()],
        "y\n",
    );

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(fs::read_link(bin.join("benterm")).unwrap(), home.exe);
    assert!(!home.path(".local/bin/benterm").exists());
    assert!(!ran.stdout.contains("not on your PATH"), "{}", ran.stdout);
}

#[test]
fn install_shows_the_lines_it_adds_before_it_asks() {
    let home = Home::new();
    home.write(".claude/settings.json", "{}");

    let ran = home.run(&["install"], "n\n");

    let lines: Vec<&str> = ran.stdout.lines().collect();
    let added = lines
        .iter()
        .position(|line| line.starts_with('+') && line.contains(&home.hook("claude")))
        .unwrap_or_else(|| panic!("the hook is not shown:\n{}", ran.stdout));
    let asked = lines
        .iter()
        .position(|line| line.contains("[y/N]"))
        .unwrap_or_else(|| panic!("it does not ask:\n{}", ran.stdout));
    assert!(added < asked, "{}", ran.stdout);
    assert!(
        ran.stdout.contains("~/.claude/settings.json"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains("~/.codex/hooks.json"), "{}", ran.stdout);
}

#[test]
fn install_preview_shows_large_numbers_and_duplicate_keys_from_the_original_file() {
    let home = Home::new();
    let original = r#"{
  "limit": 18446744073709551617,
  "theme": "light",
  "theme": "dark"
}"#;
    home.write(".claude/settings.json", original);

    let ran = home.run(&["install"], "n\n");

    assert!(
        ran.stdout.contains(r#"-   "limit": 18446744073709551617,"#),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout.contains(r#"-   "theme": "light","#),
        "{}",
        ran.stdout
    );
    assert_eq!(fs::read_to_string(home.settings()).unwrap(), original);
}

#[test]
fn install_answered_no_changes_nothing() {
    let home = Home::new();
    home.write(".claude/settings.json", r#"{"model": "opus"}"#);
    let before = home.snapshot();

    let ran = home.run(&["install"], "n\n");

    assert_ne!(ran.code, 0);
    assert_eq!(home.snapshot(), before);
}

#[test]
fn install_not_answered_changes_nothing() {
    let home = Home::new();
    let before = home.snapshot();

    let ran = home.run(&["install"], "");

    assert_ne!(ran.code, 0);
    assert_eq!(home.snapshot(), before);
}

#[test]
fn install_with_yes_does_not_ask() {
    let home = Home::new();

    let ran = home.run(&["install", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(!ran.stdout.contains("[y/N]"), "{}", ran.stdout);
    assert!(home.settings().exists());
}

#[test]
fn install_keeps_a_dated_copy_of_a_file_it_changes() {
    let home = Home::new();
    let original = r#"{"model": "opus"}"#;
    home.write(".claude/settings.json", original);

    home.run(&["install"], "y\n");

    let copies: Vec<PathBuf> = fs::read_dir(home.path(".claude"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with("settings.json.") && name.chars().any(|c| c.is_ascii_digit())
        })
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(fs::read_to_string(&copies[0]).unwrap(), original);
}

#[test]
fn install_refuses_settings_that_are_not_json() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\"model\": ");
    let before = home.snapshot();

    let ran = home.run(&["install", "--yes"], "y\n");

    assert_ne!(ran.code, 0);
    assert!(
        ran.stderr.contains("~/.claude/settings.json"),
        "{}",
        ran.stderr
    );
    assert_eq!(home.snapshot(), before);
}

#[test]
fn install_refuses_hooks_that_are_not_an_object() {
    let home = Home::new();
    home.write(".codex/hooks.json", r#"{"hooks": []}"#);
    let before = home.snapshot();

    let ran = home.run(&["install", "--yes"], "");

    assert_ne!(ran.code, 0);
    assert!(ran.stderr.contains("~/.codex/hooks.json"), "{}", ran.stderr);
    assert_eq!(home.snapshot(), before);
}

#[test]
fn install_tells_how_to_put_the_bin_dir_on_the_path() {
    let home = Home::new();
    let env = home.env_with_path(OsString::from("/usr/bin:/bin"));

    let ran = home.run_in(&env, &["install", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains("~/.local/bin is not on your PATH"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stdout
            .contains("export PATH=\"$HOME/.local/bin:$PATH\""),
        "{}",
        ran.stdout
    );
}

#[test]
fn install_says_that_codex_asks_to_review_the_hooks() {
    let home = Home::new();

    let ran = home.run(&["install", "--yes"], "");

    assert!(ran.stdout.contains("/hooks"), "{}", ran.stdout);
}

#[test]
fn install_leaves_a_file_where_the_link_goes_alone() {
    let home = Home::new();
    home.write(".local/bin/benterm", "my own");

    let ran = home.run(&["install", "--yes"], "");

    assert_ne!(ran.code, 0);
    assert_eq!(
        fs::read_to_string(home.path(".local/bin/benterm")).unwrap(),
        "my own"
    );
    assert!(
        ran.stderr.contains("~/.local/bin/benterm"),
        "{}",
        ran.stderr
    );
    // The rest is done all the same.
    assert!(home.settings().exists());
}

#[test]
fn install_moves_a_link_that_points_elsewhere() {
    let home = Home::new();
    fs::create_dir_all(home.path(".local/bin")).unwrap();
    std::os::unix::fs::symlink("/old/benterm", home.path(".local/bin/benterm")).unwrap();

    let ran = home.run(&["install", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        fs::read_link(home.path(".local/bin/benterm")).unwrap(),
        home.exe
    );
    assert!(ran.stdout.contains("/old/benterm"), "{}", ran.stdout);
}

#[test]
fn uninstall_removes_only_what_install_added() {
    let home = Home::new();
    home.write(
        ".claude/settings.json",
        r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "echo hi"}]}]}, "model": "opus"}"#,
    );
    home.write(
        ".codex/hooks.json",
        r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}}"#,
    );
    home.run(&["install"], "y\n");

    let ran = home.run(&["uninstall"], "y\n");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        home.json(&home.settings()),
        json!({"hooks": {"PreToolUse": [benterm_entry("echo hi")]}, "model": "opus"})
    );
    assert_eq!(
        home.json(&home.codex_hooks()),
        json!({"hooks": {"Stop": [benterm_entry("say done")]}})
    );
    for link in [
        ".local/bin/benterm",
        ".claude/skills/benterm",
        ".codex/skills/benterm",
    ] {
        assert!(
            fs::symlink_metadata(home.path(link)).is_err(),
            "{link} is left"
        );
    }
}

#[test]
fn uninstall_shows_what_it_removes_and_answered_no_changes_nothing() {
    let home = Home::new();
    home.run(&["install"], "y\n");
    let before = home.snapshot();

    let ran = home.run(&["uninstall"], "n\n");

    assert_ne!(ran.code, 0);
    assert_eq!(home.snapshot(), before);
    assert!(
        ran.stdout
            .lines()
            .any(|line| line.starts_with('-') && line.contains(&home.hook("claude"))),
        "{}",
        ran.stdout
    );
}

#[test]
fn uninstall_leaves_a_link_to_another_benterm() {
    let home = Home::new();
    fs::create_dir_all(home.path(".local/bin")).unwrap();
    std::os::unix::fs::symlink("/old/benterm", home.path(".local/bin/benterm")).unwrap();

    home.run(&["uninstall", "--yes"], "");

    assert_eq!(
        fs::read_link(home.path(".local/bin/benterm")).unwrap(),
        Path::new("/old/benterm")
    );
}

#[test]
fn status_names_the_events_that_have_no_hook_and_writes_nothing() {
    let home = Home::new();
    let mut hooks = serde_json::Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        if event != "Stop" && event != "SessionEnd" {
            hooks.insert(
                event.to_string(),
                json!([benterm_entry(EARLIER_CLAUDE_HOOK)]),
            );
        }
    }
    home.write(
        ".claude/settings.json",
        &json!({"hooks": hooks}).to_string(),
    );
    let before = home.snapshot();

    let ran = home.run(&["status"], "");

    assert_eq!(home.snapshot(), before);
    let claude = ran
        .stdout
        .lines()
        .find(|line| line.contains("Claude Code"))
        .unwrap_or_else(|| panic!("{}", ran.stdout));
    assert!(
        claude.contains("Stop") && claude.contains("SessionEnd"),
        "{claude}"
    );
    assert!(!claude.contains("PreToolUse"), "{claude}");
    let codex = ran
        .stdout
        .lines()
        .find(|line| line.contains("Codex"))
        .unwrap_or_else(|| panic!("{}", ran.stdout));
    assert!(codex.contains("Interrupt"), "{codex}");
}

#[test]
fn status_after_install_says_everything_is_in_place() {
    let home = Home::new();
    home.run(&["install"], "y\n");

    let ran = home.run(&["status"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stdout.contains("everything is set up"),
        "{}",
        ran.stdout
    );
}

#[test]
fn status_tells_when_the_benterm_on_the_path_is_another_one() {
    let home = Home::new();
    let other = home.path("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("benterm"), "another").unwrap();
    let env = home.env_with_path(std::env::join_paths([&other]).unwrap());

    let ran = home.run_in(&env, &["status"], "");

    assert!(ran.stdout.contains("~/other/benterm"), "{}", ran.stdout);
    assert!(ran.stdout.contains("another"), "{}", ran.stdout);
    assert!(
        !ran.stdout.contains("everything is set up"),
        "{}",
        ran.stdout
    );
}

#[test]
fn status_tells_that_the_bin_dir_is_not_on_the_path() {
    let home = Home::new();
    home.run(&["install", "--yes"], "");
    let env = home.env_with_path(OsString::from("/usr/bin:/bin"));

    let ran = home.run_in(&env, &["status"], "");

    assert!(
        ran.stdout.contains("~/.local/bin is not on your PATH"),
        "{}",
        ran.stdout
    );
}

#[test]
fn the_skill_is_found_in_the_app_and_in_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let app_skill = root.join("Benterm.app/Contents/Resources/skills/benterm");
    fs::create_dir_all(&app_skill).unwrap();
    fs::write(app_skill.join("SKILL.md"), "").unwrap();
    assert_eq!(
        setup::skill_source(&root.join("Benterm.app/Contents/MacOS/benterm")),
        Some(app_skill)
    );

    let checkout_skill = root.join("checkout/skills/benterm");
    fs::create_dir_all(&checkout_skill).unwrap();
    fs::write(checkout_skill.join("SKILL.md"), "").unwrap();
    fs::write(root.join("checkout/Cargo.toml"), WORKSPACE).unwrap();
    assert_eq!(
        setup::skill_source(&root.join("checkout/target/debug/benterm")),
        Some(checkout_skill)
    );

    assert_eq!(setup::skill_source(&root.join("elsewhere/benterm")), None);
}

#[test]
fn the_skill_is_not_taken_from_a_directory_above_the_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A directory above the binary that someone else can write to, as
    // /tmp or /Users/Shared is, with a skill of theirs in it.
    let planted = root.join("skills/benterm");
    fs::create_dir_all(&planted).unwrap();
    fs::write(planted.join("SKILL.md"), "").unwrap();

    // Without a checkout around the binary, no skill is its own.
    assert_eq!(setup::skill_source(&root.join("bin/benterm")), None);
    assert_eq!(setup::skill_source(&root.join("a/b/c/benterm")), None);

    // The checkout is where the search ends, with a skill or without.
    fs::create_dir_all(root.join("checkout")).unwrap();
    fs::write(root.join("checkout/Cargo.toml"), WORKSPACE).unwrap();
    fs::write(root.join("Cargo.toml"), WORKSPACE).unwrap();
    assert_eq!(
        setup::skill_source(&root.join("checkout/target/debug/benterm")),
        None
    );
}

/// The manifest at the top of a checkout of Benterm.
const WORKSPACE: &str = "[workspace]\nmembers = [\"crates/*\"]\n";

#[test]
fn the_skill_is_taken_only_from_the_workspace_the_binary_was_built_in() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let skill = root.join("shared/skills/benterm");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "").unwrap();

    // A binary put alone in a shared folder, under a manifest someone
    // else wrote there.
    fs::write(root.join("shared/Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    assert_eq!(setup::skill_source(&root.join("shared/bin/benterm")), None);
    assert_eq!(
        setup::skill_source(&root.join("shared/target/debug/benterm")),
        None
    );

    // Even a workspace's manifest is not the binary's checkout when the
    // binary is not in its target folder.
    fs::write(root.join("shared/Cargo.toml"), WORKSPACE).unwrap();
    assert_eq!(setup::skill_source(&root.join("shared/bin/benterm")), None);
    assert_eq!(
        setup::skill_source(&root.join("shared/target/debug/benterm")),
        Some(skill.clone())
    );
    assert_eq!(
        setup::skill_source(&root.join("shared/target/aarch64-apple-darwin/release/benterm")),
        Some(skill)
    );
}

#[test]
fn install_does_not_write_through_a_link_at_the_name_of_an_earlier_temporary_file() {
    let home = Home::new();
    home.write(".codex/hooks.json", "{}\n");
    // The name the file beside the settings had: the number of the
    // process is all that changed in it, so it could be made in advance.
    let victim = home.path("victim");
    fs::write(&victim, "kept").unwrap();
    let guessed = home.path(&format!(
        ".codex/.hooks.json.benterm-{}",
        std::process::id()
    ));
    std::os::unix::fs::symlink(&victim, &guessed).unwrap();

    let ran = home.run(&["install", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(fs::read_to_string(&victim).unwrap(), "kept");
    let hooks = fs::symlink_metadata(home.codex_hooks()).unwrap();
    assert!(hooks.file_type().is_file(), "the hooks are a file");
    assert!(home.json(&home.codex_hooks())["hooks"].is_object());
}

#[test]
fn install_makes_a_settings_file_that_only_its_owner_reads() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    home.write(".claude/settings.json", "{}\n");
    fs::set_permissions(home.settings(), fs::Permissions::from_mode(0o640)).unwrap();

    let ran = home.run(&["install", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    // A file that was not there is the owner's alone; one that was
    // keeps the permissions it had.
    assert_eq!(mode(&home.codex_hooks()), 0o600);
    assert_eq!(mode(&home.settings()), 0o640);
    // Nothing is left beside the files.
    for dir in [".claude", ".codex"] {
        let left: Vec<_> = fs::read_dir(home.path(dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[test]
fn a_hook_runs_the_link_and_not_a_benterm_put_in_front_on_the_path() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let program = |path: &Path, says: &str| {
        fs::write(path, format!("#!/bin/sh\necho {says} \"$@\"\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    program(&home.exe, "real");
    // A directory of a repository that comes first on the PATH the
    // agent runs its hooks with, and holds a `benterm` of its own.
    let repository = home.path("repository/bin");
    fs::create_dir_all(&repository).unwrap();
    program(&repository.join("benterm"), "planted");
    // The quoting holds for a directory with a blank and a quote in it.
    let bin_dir = home.path("my bin's");

    let ran = home.run(
        &["install", "--yes", "--bin-dir", bin_dir.to_str().unwrap()],
        "",
    );
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);

    let settings = home.json(&home.settings());
    for (event, says) in [
        ("PreToolUse", "real hook --agent claude\n"),
        ("Stop", "real report-check --agent claude\n"),
    ] {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let path = std::env::join_paths([
            repository.as_path(),
            Path::new("/usr/bin"),
            Path::new("/bin"),
        ])
        .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", command])
            .env("PATH", path)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), says, "{command}");
    }
}

#[test]
fn the_path_in_a_hook_is_quoted_only_where_a_shell_needs_it() {
    let command = |bin_dir: &Path| {
        let home = Home::new();
        let ran = home.run(
            &["install", "--yes", "--bin-dir", bin_dir.to_str().unwrap()],
            "",
        );
        assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
        home.json(&home.codex_hooks())["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    // A path of letters, digits and / . _ - + is written as it is, so
    // that it also runs where the command is not read by a shell.
    let plain = root.join("opt/bin_1.2-x+y");
    assert_eq!(
        command(&plain),
        format!("{}/benterm hook --agent codex", plain.display())
    );
    // Any other character, as a blank, is inside single quotes.
    let blank = root.join("my bin");
    assert_eq!(
        command(&blank),
        format!("'{}/benterm' hook --agent codex", blank.display())
    );
}

#[test]
fn install_gives_the_path_of_the_link_to_hooks_that_name_only_benterm() {
    let home = Home::new();
    // An earlier install, a hook written by hand with a matcher, and one
    // that names another benterm by its path.
    home.write(
        ".claude/settings.json",
        &json!({"hooks": {
            "PreToolUse": [
                benterm_entry(EARLIER_CLAUDE_HOOK),
                {"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "echo hi"},
                    {"type": "command", "command": "benterm  hook --agent claude", "timeout": 5},
                ]},
            ],
            "Stop": [benterm_entry(EARLIER_CLAUDE_CHECK)],
            "SessionEnd": [benterm_entry("/opt/bin/benterm hook --agent claude")],
        }})
        .to_string(),
    );

    let status = home.run(&["status"], "");
    assert!(
        status
            .stdout
            .contains("missing SessionStart, UserPromptSubmit"),
        "{}",
        status.stdout
    );

    let ran = home.run(&["install", "--yes"], "");
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);

    let settings = home.json(&home.settings());
    assert_eq!(
        settings["hooks"]["PreToolUse"],
        json!([
            benterm_entry(&home.hook("claude")),
            {"matcher": "Bash", "hooks": [
                {"type": "command", "command": "echo hi"},
                {"type": "command", "command": home.hook("claude"), "timeout": 5},
            ]},
        ])
    );
    assert_eq!(
        settings["hooks"]["Stop"],
        json!([benterm_entry(&home.check("claude"))])
    );
    assert_eq!(
        settings["hooks"]["SessionEnd"],
        json!([benterm_entry("/opt/bin/benterm hook --agent claude")])
    );
    assert_eq!(
        home.run(&["install", "--yes"], "").stdout,
        "Nothing to change.\n"
    );
}

#[test]
fn status_tells_of_hooks_that_look_benterm_up_on_the_path() {
    let home = Home::new();
    let mut hooks = serde_json::Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            entries(event, EARLIER_CLAUDE_HOOK, EARLIER_CLAUDE_CHECK),
        );
    }
    home.write(
        ".claude/settings.json",
        &json!({"hooks": hooks}).to_string(),
    );

    let before = home.run(&["status"], "");
    let claude = before.stdout.lines().next().unwrap();
    assert!(
        claude.contains("all 10 events, but 11 of the hooks do not name benterm by its whole path"),
        "{claude}"
    );
    assert!(!before.stdout.contains("everything is set up."));

    home.run(&["install", "--yes"], "");
    let after = home.run(&["status"], "");
    let claude = after.stdout.lines().next().unwrap();
    assert!(claude.ends_with("all 10 events"), "{claude}");
}

/// A hook that names benterm by a relative path runs the program of
/// that name in the folder the agent runs in, which may be a repository.
#[test]
fn status_tells_of_and_install_rewrites_hooks_with_a_relative_path() {
    let home = Home::new();
    let mut hooks = serde_json::Map::new();
    for event in CLAUDE_HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            entries(
                event,
                "./benterm hook --agent claude",
                "bin/benterm report-check --agent claude",
            ),
        );
    }
    home.write(
        ".claude/settings.json",
        &json!({"hooks": hooks}).to_string(),
    );

    let before = home.run(&["status"], "");
    let claude = before.stdout.lines().next().unwrap();
    assert!(
        claude.contains("all 10 events, but 11 of the hooks do not name benterm by its whole path"),
        "{claude}"
    );

    let ran = home.run(&["install", "--yes"], "");
    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    let settings = home.json(&home.settings());
    assert_eq!(
        settings["hooks"]["Stop"],
        json!([benterm_entry(&home.check("claude"))])
    );
    assert_eq!(
        settings["hooks"]["SessionStart"],
        json!([benterm_entry(&home.hook("claude"))])
    );
    let after = home.run(&["status"], "");
    let claude = after.stdout.lines().next().unwrap();
    assert!(claude.ends_with("all 10 events"), "{claude}");
}

#[test]
fn uninstall_takes_out_the_hooks_with_the_path_and_the_ones_without() {
    let home = Home::new();
    home.run(&["install", "--yes"], "");
    let mut settings = home.json(&home.settings());
    settings["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(benterm_entry(EARLIER_CLAUDE_HOOK));
    home.write(".claude/settings.json", &settings.to_string());

    let ran = home.run(&["uninstall", "--yes"], "");

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(home.json(&home.settings()), json!({}));
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let home = Home::new();

    let ran = home.run(&["explode"], "");

    assert_eq!(ran.code, 2);
}

/// A stdin that runs `before_first_answer` once, right when setup waits for
/// the first answer (after it has shown the diff), and then feeds `answers`.
struct EditingStdin<F> {
    before_first_answer: Option<F>,
    answers: std::io::Cursor<Vec<u8>>,
}

impl<F: FnOnce()> EditingStdin<F> {
    fn new(answers: &str, before_first_answer: F) -> Self {
        Self {
            before_first_answer: Some(before_first_answer),
            answers: std::io::Cursor::new(answers.as_bytes().to_vec()),
        }
    }
}

impl<F: FnOnce()> std::io::Read for EditingStdin<F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(edit) = self.before_first_answer.take() {
            edit();
        }
        self.answers.read(buf)
    }
}

#[test]
fn install_rechecks_and_shows_new_diff_when_settings_change_before_answer() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\n  \"model\": \"opus\"\n}\n");
    let edited = "{\n  \"model\": \"opus\",\n  \"theme\": \"dark\"\n}\n";
    let settings_path = home.settings();

    let stdin = EditingStdin::new("y\nn\n", || {
        fs::write(&settings_path, edited).unwrap();
    });
    let ran = home.run_with_stdin(&home.env(), &["install"], stdin);

    assert_ne!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("[y/N]").count(),
        2,
        "should ask again with a refreshed diff:\n{}",
        ran.stdout
    );
    let after_first_prompt = ran.stdout.split_once("[y/N]").unwrap().1;
    assert!(
        after_first_prompt.contains("\"theme\": \"dark\""),
        "new diff should include the added line:\n{}",
        after_first_prompt
    );
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), edited);
    assert!(fs::symlink_metadata(home.path(".local/bin/benterm")).is_err());
    let backups: Vec<_> = fs::read_dir(home.path(".claude"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("settings.json."))
        .collect();
    assert!(
        backups.is_empty(),
        "no backup should be created: {backups:?}"
    );
}

#[test]
fn install_writes_merged_settings_when_confirmed_after_change() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\n  \"model\": \"opus\"\n}\n");
    let edited = "{\n  \"model\": \"opus\",\n  \"theme\": \"dark\"\n}\n";
    let settings_path = home.settings();

    let stdin = EditingStdin::new("y\ny\n", || {
        fs::write(&settings_path, edited).unwrap();
    });
    let ran = home.run_with_stdin(&home.env(), &["install"], stdin);

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("[y/N]").count(),
        2,
        "should ask again before writing:\n{}",
        ran.stdout
    );
    let settings = home.json(&settings_path);
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["theme"], "dark");
    assert_eq!(event_names(&settings["hooks"]), CLAUDE_HOOK_EVENTS);
    let copies: Vec<PathBuf> = fs::read_dir(home.path(".claude"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.")
        })
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(fs::read_to_string(&copies[0]).unwrap(), edited);
}

#[test]
fn uninstall_rechecks_and_shows_new_diff_when_settings_change_before_answer() {
    let home = Home::new();
    home.run(&["install", "--yes"], "");
    let settings_path = home.settings();
    let hooks = home.json(&settings_path)["hooks"].clone();
    let mut updated = serde_json::Map::new();
    updated.insert("theme".into(), json!("dark"));
    updated.insert("hooks".into(), hooks);
    let edited = format!("{}\n", serde_json::to_string_pretty(&updated).unwrap());

    let stdin = EditingStdin::new("y\nn\n", || {
        fs::write(&settings_path, &edited).unwrap();
    });
    let ran = home.run_with_stdin(&home.env(), &["uninstall"], stdin);

    assert_ne!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("[y/N]").count(),
        2,
        "should ask again with a refreshed diff:\n{}",
        ran.stdout
    );
    let after_first_prompt = ran.stdout.split_once("[y/N]").unwrap().1;
    assert!(
        after_first_prompt.contains("\"theme\": \"dark\""),
        "new diff should include the added line:\n{}",
        after_first_prompt
    );
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), edited);
    assert!(fs::symlink_metadata(home.path(".local/bin/benterm")).is_ok());
}

#[test]
fn uninstall_writes_merged_settings_when_confirmed_after_change() {
    let home = Home::new();
    home.run(&["install", "--yes"], "");
    let settings_path = home.settings();
    let hooks = home.json(&settings_path)["hooks"].clone();
    let mut updated = serde_json::Map::new();
    updated.insert("theme".into(), json!("dark"));
    updated.insert("hooks".into(), hooks);
    let edited = format!("{}\n", serde_json::to_string_pretty(&updated).unwrap());

    let stdin = EditingStdin::new("y\ny\n", || {
        fs::write(&settings_path, &edited).unwrap();
    });
    let ran = home.run_with_stdin(&home.env(), &["uninstall"], stdin);

    assert_eq!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert_eq!(
        ran.stdout.matches("[y/N]").count(),
        2,
        "should ask again before writing:\n{}",
        ran.stdout
    );
    assert_eq!(home.json(&settings_path), json!({"theme": "dark"}));
    assert!(fs::symlink_metadata(home.path(".local/bin/benterm")).is_err());
}

#[test]
fn install_writes_nothing_when_settings_become_invalid_json_before_answer() {
    let home = Home::new();
    home.write(".claude/settings.json", "{\n  \"model\": \"opus\"\n}\n");
    let settings_path = home.settings();

    let stdin = EditingStdin::new("y\ny\n", || {
        fs::write(&settings_path, "{invalid").unwrap();
    });
    let ran = home.run_with_stdin(&home.env(), &["install"], stdin);

    assert_ne!(ran.code, 0, "{}{}", ran.stdout, ran.stderr);
    assert!(
        ran.stderr.contains("~/.claude/settings.json"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("nothing was written"), "{}", ran.stderr);
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), "{invalid");
    assert!(fs::symlink_metadata(home.path(".local/bin/benterm")).is_err());
}
