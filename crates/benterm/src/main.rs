//! `benterm` opens the app. `benterm <command>` talks to the app the
//! command runs in over its socket; see `benterm --help`.

use std::ffi::OsString;
use std::process::ExitCode;

use benterm_ipc::cli::{self, CliEnv};
use gpui::App;

mod nofile;
mod notify_check;

fn opens_app(args: &[OsString]) -> bool {
    args.first().is_none_or(|first| first == notify_check::FLAG)
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // The notification check is a way to open the app, not a command.
    if opens_app(&args) {
        run_app();
        return ExitCode::SUCCESS;
    }
    // A command never starts GPUI: it is over before a window could open.
    let hook = cli::is_hook(&args);
    let args: Result<Vec<String>, _> = args.into_iter().map(|arg| arg.into_string()).collect();
    let Ok(args) = args else {
        eprintln!("benterm: arguments must be UTF-8");
        // A hook never fails: the agent would take it for a reason to stop.
        return ExitCode::from(if hook { 0 } else { 2 });
    };
    ExitCode::from(cli::run(
        &args,
        &CliEnv::from_process(),
        std::io::stdin(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ))
}

fn run_app() {
    // Raise a low file descriptor limit (launchd gives GUI apps 256) before
    // starting the login shell probe or opening terminals. Children inherit
    // the raised 8192 soft limit rather than falling back to 256.
    nofile::raise_and_log();
    // Opened from the Finder, the app takes the environment of the user's
    // shell, without making the window wait for it.
    let mut login_env =
        benterm_ui::login_env::start(std::env::vars_os(), benterm_ui::login_env::login_shell());
    gpui_platform::application().run(move |cx: &mut App| {
        // The window closes when its last shell exits.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        benterm_ui::bind_keys(cx);
        // A click on a notification of a pane shows the pane.
        benterm_ui::attention::answer_notifications(cx);
        // The menu shows the keys bound by now.
        benterm_ui::menu::install(cx);
        benterm_ui::open_main_window(login_env.take(), cx).expect("failed to open the main window");
        cx.activate(true);
        notify_check::post_if_requested(std::env::args().skip(1), cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_app_launch_and_notify_check_open_the_app() {
        assert!(opens_app(&args(&[])));
        assert!(opens_app(&args(&[notify_check::FLAG])));
        assert!(!opens_app(&args(&["--help"])));
        assert!(!opens_app(&args(&["list"])));
        assert!(!opens_app(&args(&["hook"])));
        assert!(!opens_app(&args(&["split", "--down"])));
    }
}
