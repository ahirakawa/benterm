//! The environment of the user's shell in the window: taken in the
//! background at startup, given to every terminal started after it has
//! come, and waited for by the agents and roles started before.
//!
//! The window and its first shell do not wait: a pane's login shell reads
//! the user's shell files itself.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use benterm_terminal::login_env::{CAPTURE_TIMEOUT, capture, started_from_terminal};
pub use benterm_terminal::login_shell;
use futures::channel::oneshot;
use futures::future::{FutureExt, LocalBoxFuture, Shared};
use gpui::{Context, Window};

use crate::BentermWindow;

/// What taking the environment came to: the variables, or why there are
/// none.
pub type Taken = Result<Vec<(OsString, OsString)>, String>;

/// Starts taking the environment of `shell` on a thread of its own,
/// unless `vars`, the app's environment, says the app was started from a
/// terminal. The answer comes within the timeout of
/// [`benterm_terminal::login_env::capture`].
pub fn start(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    shell: PathBuf,
) -> Option<oneshot::Receiver<Taken>> {
    if started_from_terminal(vars) {
        return None;
    }
    let (sender, taken) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("benterm-login-shell".into())
        .spawn(move || {
            let result = capture(&shell, CAPTURE_TIMEOUT).map_err(|error| error.to_string());
            // The window may have closed by now.
            let _ = sender.send(result);
        });
    // Without a thread the receiver hears that the sender is gone.
    drop(thread);
    Some(taken)
}

/// The environment of the user's shell, as one of the window's states.
#[derive(Clone, Debug)]
enum State {
    /// The app's own: none was asked for, or none came.
    Own,
    Taken(Arc<Vec<(OsString, OsString)>>),
}

/// Where the window's terminals get the environment of the user's shell.
pub(crate) struct LoginEnv {
    /// Done once there is an answer. Every clone is done with the same.
    state: RefCell<Shared<LocalBoxFuture<'static, State>>>,
}

impl Default for LoginEnv {
    /// The app's own environment, with nothing to wait for.
    fn default() -> Self {
        Self {
            state: RefCell::new(futures::future::ready(State::Own).boxed_local().shared()),
        }
    }
}

impl LoginEnv {
    /// The environment of the user's shell, if it has come.
    pub(crate) fn now(&self) -> Option<Arc<Vec<(OsString, OsString)>>> {
        match self.state.borrow().clone().now_or_never()? {
            State::Own => None,
            State::Taken(env) => Some(env),
        }
    }

    /// The `PATH` of [`now`](Self::now).
    pub(crate) fn path(&self) -> Option<OsString> {
        let env = self.now()?;
        let (_, path) = env.iter().rev().find(|(name, _)| name == "PATH")?;
        Some(path.clone())
    }

    /// Whether the answer has not come yet.
    pub(crate) fn is_pending(&self) -> bool {
        self.state.borrow().clone().now_or_never().is_none()
    }

    /// Done once the answer has come.
    pub(crate) fn taken(&self) -> impl Future<Output = ()> + 'static {
        self.state.borrow().clone().map(drop)
    }
}

impl BentermWindow {
    /// Takes the environment that `taken` brings, for the terminals
    /// started after it has come. If none comes, the bar says why.
    pub fn take_login_env(
        &mut self,
        taken: oneshot::Receiver<Taken>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shells) = self.shells.clone() else {
            return;
        };
        let (why, failed) = oneshot::channel();
        let state = async move {
            let result = taken
                .await
                .unwrap_or_else(|_| Err("the login shell could not be run".to_owned()));
            match result {
                Ok(env) => State::Taken(Arc::new(env)),
                Err(reason) => {
                    let _ = why.send(reason);
                    State::Own
                }
            }
        }
        .boxed_local()
        .shared();
        *shells.login_env.state.borrow_mut() = state.clone();
        cx.spawn_in(window, async move |this, cx| {
            state.await;
            if let Ok(reason) = failed.await {
                let _ = this.update(cx, |this, cx| {
                    this.login_env_notice = Some(format!(
                        "{reason}. Programs start with the app's own environment, whose PATH \
                         lacks what the shell files add."
                    ));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Why the window's programs start with the app's own environment,
    /// as the bar shows it now.
    pub fn shown_login_env_notice(&self) -> Option<String> {
        self.login_env_notice.clone()
    }
}
