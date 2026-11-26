//! Roles in the window: the menu bar and the command palette list the
//! roles of the roles directory, choosing one starts its agent in a new
//! tab, a file that is not a role is named in the bar, and a saved file
//! is listed without a restart.
//!
//! The window is a real one, the agent is a made-up `claude` that writes
//! down its arguments, and the roles are files the window watches.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::HookEvent;
use benterm_session::{Node, Session};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, LaunchRole, StartedTerminal, bind_keys, ipc, menu};
use gpui::{Action, OwnedMenuItem, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

const REVIEWER: &str = r#"
name = "reviewer"
description = "PR を読む"
agent = "claude"
model = "opus"
permission_mode = "auto"
prompt = "差分を読んで指摘する"
"#;

/// A role with variables, one of them a name every pane gets, and more
/// arguments for its agent.
const PLANNER: &str = r#"
name = "planner"
agent = "claude"
prompt = "計画を立てる"
args = ["--add-dir", "../docs"]

[env]
FOO = "bar"
BENTERM_SOCKET = "mine"
"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    roles: PathBuf,
    /// Where the made-up claude writes its arguments, one file for each
    /// terminal it runs in.
    args: PathBuf,
    _dir: tempfile::TempDir,
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn open(cx: &mut TestAppContext, roles: &[(&str, &str)]) -> App {
    cx.executor().allow_parking();
    cx.update(|cx| {
        bind_keys(cx);
        menu::install(cx);
    });
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let config = root.join("config");
    let roles_dir = config.join("roles");
    std::fs::create_dir_all(&roles_dir).unwrap();
    for (name, text) in roles {
        std::fs::write(roles_dir.join(name), text).unwrap();
    }
    let fake = root.join("fake");
    let args = root.join("args");
    std::fs::create_dir_all(&args).unwrap();
    std::fs::create_dir(&fake).unwrap();
    executable(
        &fake.join("claude"),
        &format!(
            "#!/bin/sh\nf={}/$BENTERM_PANE_ID\nenv > $f.env\n\
             printf '%s\\0' \"$@\" > $f.part && mv $f.part $f\n\
             echo started\nexec sleep 100\n",
            args.display()
        ),
    );
    let (window, vcx) = open_saved(cx, &root, "benterm.sock");
    App {
        window,
        cx: vcx,
        roles: roles_dir,
        args,
        _dir: dir,
    }
}

fn open_saved(
    cx: &mut TestAppContext,
    root: &Path,
    socket: &str,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
    pty.cwd = Some(root.to_owned());
    pty.env = vec![(
        "PATH".into(),
        format!("{}:/usr/bin:/bin", root.join("fake").display()).into(),
    )];
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let feed = ConfigFeed::watch(&root.join("config/config.toml"));
    let window = cx
        .update(|cx| {
            ipc::open_session_window(
                pty,
                start,
                Some(root.join(socket)),
                Some(root.join("session.json")),
                feed,
                cx,
            )
        })
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

impl App {
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
        self.cx.executor().advance_clock(Duration::from_millis(10));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    fn tab_count(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.tabs().read(cx).tabs().len()
            })
            .unwrap()
    }

    /// The id of the focused pane of the selected tab.
    fn focused_pane(&mut self) -> u64 {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.grid(cx).read(cx).focused().get()
            })
            .unwrap()
    }

    /// The arguments the made-up claude was started with in `pane`.
    fn args_in(&mut self, pane: u64) -> Vec<String> {
        let file = self.args.join(pane.to_string());
        self.wait_until("the agent's arguments", |_| file.exists());
        std::fs::read(&file)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect()
    }

    /// The variables the made-up claude was started with in `pane`.
    fn env_in(&mut self, pane: u64) -> Vec<String> {
        self.args_in(pane);
        let file = self.args.join(format!("{pane}.env"));
        std::fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn agent_names(&mut self) -> Vec<(String, bool)> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.agents(cx)
                    .into_iter()
                    .map(|row| (row.name, row.background))
                    .collect()
            })
            .unwrap()
    }

    fn palette_names(&mut self) -> Vec<String> {
        self.cx.run_until_parked();
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.command_palette()
                    .expect("the palette is open")
                    .read(cx)
                    .shown(cx)
                    .into_iter()
                    .map(|command| command.name)
                    .collect()
            })
            .unwrap()
    }

    fn shown_messages(&mut self) -> Vec<String> {
        self.window
            .update(&mut self.cx, |root, _, _| root.shown_messages())
            .unwrap()
    }
}

/// The items of the menu bar that start a role, as the menu's name, the
/// item's name and the action.
fn role_items(cx: &mut TestAppContext) -> Vec<(String, String, Box<dyn Action>)> {
    fn walk(
        menu: &str,
        items: &[OwnedMenuItem],
        found: &mut Vec<(String, String, Box<dyn Action>)>,
    ) {
        for item in items {
            match item {
                OwnedMenuItem::Action { name, action, .. }
                    if action.as_any().is::<LaunchRole>() =>
                {
                    found.push((menu.to_owned(), name.to_string(), action.boxed_clone()))
                }
                OwnedMenuItem::Submenu(submenu) => walk(menu, &submenu.items, found),
                _ => {}
            }
        }
    }
    let menus = cx.update(|cx| cx.get_menus()).expect("the menus are set");
    let mut found = Vec::new();
    for menu in &menus {
        walk(&menu.name, &menu.items, &mut found);
    }
    found
}

/// The arguments the role reviewer starts claude with from the window,
/// where there is no task.
fn reviewer_args() -> Vec<String> {
    [
        "--model",
        "opus",
        "--permission-mode",
        "auto",
        "--",
        "差分を読んで指摘する",
    ]
    .map(String::from)
    .to_vec()
}

#[gpui::test]
fn the_tab_menu_lists_the_roles(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    app.pass_time();
    let items = role_items(cx);
    assert_eq!(
        items.len(),
        1,
        "{:?}",
        items.iter().map(|i| &i.1).collect::<Vec<_>>()
    );
    let (menu, name, action) = &items[0];
    assert_eq!(menu, "Tab");
    assert!(name.contains("reviewer"), "{name}");
    assert!(action.partial_eq(&LaunchRole("reviewer".into())));
}

#[gpui::test]
fn choosing_a_role_from_the_menu_starts_its_agent_in_a_new_tab(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    app.pass_time();
    app.cx.update(|window, _| window.activate_window());
    app.pass_time();
    let (_, _, action) = role_items(cx).pop().expect("the role is in the menu");
    cx.update(|cx| cx.dispatch_action(action.as_ref()));
    app.pass_time();

    assert_eq!(app.tab_count(), 2);
    let pane = app.focused_pane();
    assert_eq!(app.args_in(pane), reviewer_args());
    assert_eq!(app.agent_names(), [("reviewer".to_owned(), false)]);
}

#[gpui::test]
fn choosing_a_role_from_the_palette_starts_its_agent_in_a_new_tab(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    app.pass_time();
    app.cx.simulate_keystrokes("cmd-k");
    app.cx.simulate_input("reviewer");
    let names = app.palette_names();
    assert_eq!(
        names.first().map(String::as_str),
        Some("Role: reviewer — PR を読む")
    );
    app.cx.simulate_keystrokes("enter");
    app.pass_time();

    assert_eq!(app.tab_count(), 2);
    let pane = app.focused_pane();
    assert_eq!(app.args_in(pane), reviewer_args());
    assert_eq!(app.agent_names(), [("reviewer".to_owned(), false)]);
}

#[gpui::test]
fn a_role_started_from_the_window_gets_its_variables_and_arguments(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("planner.toml", PLANNER)]);
    app.pass_time();
    app.window
        .update(&mut app.cx, |_, window, cx| {
            window.dispatch_action(LaunchRole("planner".into()).boxed_clone(), cx)
        })
        .unwrap();
    app.pass_time();

    let pane = app.focused_pane();
    assert_eq!(
        app.args_in(pane),
        ["--add-dir", "../docs", "--", "計画を立てる"]
    );
    let env = app.env_in(pane);
    assert!(env.contains(&"FOO=bar".to_owned()), "{env:?}");
    // The role's value is put over the pane's.
    assert!(env.contains(&"BENTERM_SOCKET=mine".to_owned()), "{env:?}");
}

fn save_role_conversation(app: &mut App) -> u64 {
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.launch_role("reviewer", window, cx)
        })
        .unwrap();
    let initial = app.focused_pane();
    let env = app.env_in(initial);
    assert!(
        env.iter()
            .any(|line| line.starts_with("BENTERM_PANE_TOKEN="))
    );
    assert!(
        !env.iter()
            .any(|line| line.starts_with("BENTERM_CONTROL_TOKEN="))
    );
    app.window
        .update(&mut app.cx, |root, _, cx| {
            root.terminal(cx).update(cx, |view, cx| {
                view.report_hook(
                    &HookEvent::new("claude", "SessionStart").with_session("abc"),
                    cx,
                );
            });
        })
        .unwrap();

    let root = app._dir.path().canonicalize().unwrap();
    let file = root.join("session.json");
    app.wait_until("the role conversation is saved", |_| {
        let Ok(text) = std::fs::read(&file) else {
            return false;
        };
        let session = Session::from_json(&text).unwrap();
        matches!(&session.workspaces[0].tabs[1..], [tab]
            if matches!(&tab.root, Node::Terminal(pane)
                if pane.conversation.as_ref().is_some_and(|c| c.id() == "abc")))
    });
    app.window
        .update(&mut app.cx, |_, window, _| window.remove_window())
        .unwrap();
    initial
}

#[gpui::test]
fn a_role_resumes_with_its_settings_and_without_control(cx: &mut TestAppContext) {
    let role = format!("{REVIEWER}\nargs = [\"--add-dir\", \"../docs\"]\n[env]\nFOO = \"before\"");
    let mut app = open(cx, &[("reviewer.toml", &role)]);
    let initial = save_role_conversation(&mut app);
    let root = app._dir.path().canonicalize().unwrap();
    let role = role.replace("opus", "sonnet").replace("before", "after");
    let role = format!("{role}\nBENTERM_CONTROL_TOKEN = \"ignored\"");
    std::fs::write(app.roles.join("reviewer.toml"), role).unwrap();
    (app.window, app.cx) = open_saved(cx, &root, "reopened.sock");
    let resumed = app.focused_pane();
    assert_ne!(resumed, initial);
    let env = app.env_in(resumed);
    assert!(
        env.iter()
            .any(|line| line.starts_with("BENTERM_PANE_TOKEN="))
    );
    assert!(
        !env.iter()
            .any(|line| line.starts_with("BENTERM_CONTROL_TOKEN="))
    );
    assert!(env.contains(&"FOO=after".to_owned()));
    assert_eq!(
        app.args_in(resumed),
        [
            "--model",
            "sonnet",
            "--permission-mode",
            "auto",
            "--add-dir",
            "../docs",
            "--resume",
            "abc"
        ]
    );
}

#[gpui::test]
fn a_deleted_role_restores_only_a_shell_without_control(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    save_role_conversation(&mut app);
    std::fs::remove_file(app.roles.join("reviewer.toml")).unwrap();
    let root = app._dir.path().canonicalize().unwrap();
    (app.window, app.cx) = open_saved(cx, &root, "reopened.sock");
    let resumed = app.focused_pane();
    app.window
        .read_with(&app.cx, |root, cx| {
            assert!(root.terminal(cx).read(cx).send_text(concat!(
                "printf 'pane-token:%s control-token:%s\\n' ",
                "\"${BENTERM_PANE_TOKEN:+present}\" \"${BENTERM_CONTROL_TOKEN:+present}\"\r"
            )));
        })
        .unwrap();
    app.wait_until("the restricted shell runs the command", |app| {
        app.window
            .read_with(&app.cx, |root, cx| {
                root.terminal(cx)
                    .read(cx)
                    .text(None)
                    .lines()
                    .any(|line| line.trim() == "pane-token:present control-token:")
            })
            .unwrap()
    });
    assert!(!app.args.join(resumed.to_string()).exists());
}

#[gpui::test]
fn a_role_started_twice_gives_its_second_agent_a_name_of_its_own(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    app.pass_time();
    for _ in 0..2 {
        app.window
            .update(&mut app.cx, |_, window, cx| {
                window.dispatch_action(LaunchRole("reviewer".into()).boxed_clone(), cx)
            })
            .unwrap();
        app.pass_time();
    }
    assert_eq!(app.tab_count(), 3);
    assert_eq!(
        app.agent_names(),
        [
            ("reviewer".to_owned(), false),
            ("reviewer-2".to_owned(), false)
        ]
    );
}

#[gpui::test]
fn a_file_that_is_not_a_role_is_named_in_the_bar(cx: &mut TestAppContext) {
    let bad = "name = \"bad\"\nagent = \"gemini\"\nprompt = \"p\"\n";
    let mut app = open(cx, &[("reviewer.toml", REVIEWER), ("bad.toml", bad)]);
    app.pass_time();
    let messages = app.shown_messages();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("roles/bad.toml: ") && m.contains("gemini")),
        "{messages:?}"
    );
    assert_eq!(role_items(cx).len(), 1);
}

#[gpui::test]
fn a_role_file_saved_while_the_app_runs_is_listed_without_a_restart(cx: &mut TestAppContext) {
    let mut app = open(cx, &[("reviewer.toml", REVIEWER)]);
    app.pass_time();
    let coder = REVIEWER
        .replace("\"reviewer\"", "\"coder\"")
        .replace("PR を読む", "書く");
    std::fs::write(app.roles.join("coder.toml"), coder).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while role_items(cx).len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the new role never reached the menu"
        );
        app.pass_time();
    }
    app.cx.simulate_keystrokes("cmd-k");
    app.cx.simulate_input("role");
    let names = app.palette_names();
    assert!(
        names.contains(&"Role: coder — 書く".to_owned()),
        "{names:?}"
    );

    // A file that goes away takes its role with it, and a broken file
    // is named in the bar.
    std::fs::remove_file(app.roles.join("coder.toml")).unwrap();
    std::fs::write(app.roles.join("bad.toml"), "agent = 1").unwrap();
    app.wait_until("the bar to name the broken file", |app| {
        app.shown_messages()
            .iter()
            .any(|m| m.starts_with("roles/bad.toml"))
    });
    assert_eq!(role_items(cx).len(), 1);
}
