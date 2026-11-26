//! The command line for tasks: `benterm task ...` makes, lists and
//! closes tasks, each with a branch and a git worktree of its own.

mod common;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{CloseTask, CreateTask, ListTasks, PaneId, Request};
use common::{BRANCH_LEFT_TASK, DIRTY_TASK, TestServer, start_server};

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run(env: &CliEnv, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(&args, env, std::io::empty(), &mut stdout, &mut stderr);
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

/// The environment of a shell in pane 3 of the server's app.
fn pane_env(server: &TestServer) -> CliEnv {
    CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        cwd: Some("/work/benterm/src".into()),
        path: Some("/opt/bin:/bin".into()),
        ..CliEnv::default()
    }
}

fn ok(run: &Run) -> &str {
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    &run.stdout
}

#[test]
fn task_is_a_command() {
    assert!(cli::is_command("task"));
}

#[test]
fn create_makes_a_task_from_where_the_command_runs() {
    let server = start_server();

    let run = run(&pane_env(&server), &["task", "create", "kanban"]);

    // The task's number, its workspace, its first pane and its worktree.
    assert_eq!(ok(&run), "4 2 11 /trees/benterm/kanban\n");
    assert_eq!(
        server.app.requests(),
        [Request::CreateTask(CreateTask {
            name: "kanban".into(),
            branch: None,
            cwd: "/work/benterm/src".into(),
            role: None,
            work: None,
            caller: Some(PaneId::new(3)),
        })]
    );
}

#[test]
fn create_takes_a_branch_a_role_a_directory_and_the_work() {
    let server = start_server();

    let run = run(
        &pane_env(&server),
        &[
            "task",
            "create",
            "かんばん",
            "--branch",
            "issue-93-kanban",
            "--role",
            "reviewer",
            "--cwd",
            "../other",
            "--",
            "かんばんの列を",
            "足す",
        ],
    );

    assert_eq!(ok(&run), "4 2 11 /trees/benterm/issue-93-kanban\n");
    assert_eq!(
        server.app.requests(),
        [Request::CreateTask(CreateTask {
            name: "かんばん".into(),
            branch: Some("issue-93-kanban".into()),
            cwd: "/work/benterm/src/../other".into(),
            role: Some("reviewer".into()),
            work: Some("かんばんの列を 足す".into()),
            caller: Some(PaneId::new(3)),
        })]
    );
}

#[test]
fn list_prints_a_line_of_json_for_each_task() {
    let server = start_server();

    let run = run(&pane_env(&server), &["task", "list"]);

    let lines: Vec<serde_json::Value> = ok(&run)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[0]["closed"]["worktree"], "removed");
    assert!(lines[0].get("workspace").is_none());
    assert_eq!(lines[1]["id"], 4);
    assert_eq!(lines[1]["name"], "kanban");
    assert_eq!(lines[1]["repository"], "/work/benterm");
    assert_eq!(lines[1]["base_branch"], "main");
    assert_eq!(
        lines[1]["base_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(lines[1]["branch"], "kanban");
    assert_eq!(lines[1]["worktree"], "/trees/benterm/kanban");
    assert_eq!(lines[1]["workspace"], 2);
    assert_eq!(server.app.requests(), [Request::ListTasks(ListTasks {})]);
}

#[test]
fn close_keeps_the_worktree_unless_told_to_remove_it() {
    let server = start_server();
    let env = pane_env(&server);

    assert_eq!(ok(&run(&env, &["task", "close", "4"])), "");
    assert_eq!(ok(&run(&env, &["task", "close", "4", "--remove"])), "");
    let forced = run(&env, &["task", "close", "4", "--remove", "--force"]);
    assert_eq!(ok(&forced), "");

    let close = |remove, force| {
        Request::CloseTask(CloseTask {
            task: 4,
            remove,
            force,
        })
    };
    assert_eq!(
        server.app.requests(),
        [close(false, false), close(true, false), close(true, true)]
    );
}

#[test]
fn close_says_what_would_be_lost_and_removes_nothing() {
    let server = start_server();
    let task = DIRTY_TASK.to_string();

    let run = run(&pane_env(&server), &["task", "close", &task, "--remove"]);

    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("2 uncommitted files"), "{}", run.stderr);
    assert!(run.stderr.contains("--force"), "{}", run.stderr);
}

#[test]
fn close_says_when_the_branch_is_left() {
    let server = start_server();
    let task = BRANCH_LEFT_TASK.to_string();

    let run = run(&pane_env(&server), &["task", "close", &task, "--remove"]);

    assert_eq!(run.code, 0);
    assert!(run.stderr.contains("was not deleted"), "{}", run.stderr);
}

#[test]
fn what_makes_no_sense_is_refused_before_anything_is_sent() {
    let server = start_server();
    let env = pane_env(&server);
    for (args, word) in [
        (&["task"][..], "subcommand"),
        (&["task", "go"][..], "go"),
        (&["task", "create"][..], "NAME"),
        (&["task", "create", "a", "--remove"][..], "--remove"),
        (&["task", "list", "x"][..], "x"),
        (&["task", "close"][..], "TASK"),
        (&["task", "close", "kanban"][..], "kanban"),
        (&["task", "close", "4", "--force"][..], "--remove"),
        (&["task", "close", "4", "--role", "r"][..], "--role"),
    ] {
        let run = run(&env, args);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(run.stderr.contains(word), "{args:?}: {}", run.stderr);
    }
    assert_eq!(server.app.requests(), []);
}
