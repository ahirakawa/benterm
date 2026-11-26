//! Saving the session of a window, and making a window again from a saved
//! session.
//!
//! The window is compared with what was last saved once every
//! [`SAVE_INTERVAL`], and saved if it differs. This catches every kind of
//! change in one place, also a shell that moved to another directory,
//! which nothing tells the app. The session is saved once more when the
//! window is about to close and when the app quits. Closing the last
//! pane removes the saved session: what the user closed does not come
//! back.
//!
//! What each pane showed is saved too, in a file of its own under a key
//! that the pane keeps. Copying the rows of a pane takes a millisecond
//! or two on this thread, so at most one pane is saved in a round: of
//! those that changed, the one that has waited longest. All that
//! changed are saved when the window closes or the app quits.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use benterm_config::Role;
use benterm_core::history_bytes_within;
use benterm_layout::{PaneId, SplitDirection, TreeShape, WorkspaceColor, WorkspaceList};
use benterm_session::{
    Axis, Browser, MAX_SCROLLBACK_BYTES, Node, Session, SessionStore, Split, Tab, Terminal, Ticket,
    Workspace, new_scrollback_key,
};
use gpui::{App, AppContext, Context, Entity, Subscription, Task, Window, WindowHandle};

use crate::ipc::Shells;
use crate::pane_grid::{FirstPane, RestoredPanes, SpawnTerminal, StartedPane, spawn_in};
use crate::settings::ConfigFeed;
use crate::tab_group::TabGroup;
use crate::terminal_view::TerminalView;
use crate::theme::TerminalTheme;
use crate::{BentermWindow, window_options};

/// How long a change waits at most before it is saved.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Saves the session of a window for as long as the window is open.
pub(crate) struct SessionSaver {
    store: Arc<SessionStore>,
    /// The session last given to the writer. `None` when the writer
    /// failed, so that the next round tries again.
    last: Option<Session>,
    /// The ticket of the save given to the writer last. The outcome of
    /// an earlier one says nothing about the session as it is now.
    latest: Option<Ticket>,
    /// Where the shell of each pane was when it last told, for a shell
    /// that does not tell for a moment.
    directories: HashMap<PaneId, PathBuf>,
    /// What is kept of what each pane showed. A pane is in here from
    /// the first save after it was made.
    scrollback: HashMap<PaneId, Kept>,
    /// Counts the saves, so that the panes take turns.
    round: u64,
    /// The keys of panes that are gone, whose files are still to be
    /// removed because the session file could not be written without
    /// them.
    gone: Vec<String>,
    /// The last save failed, and the bar says so.
    failed: bool,
    /// The window is closing or has closed its last pane: what was
    /// saved or removed then stays.
    done: bool,
    _tick: Task<()>,
    _quit: Subscription,
}

/// The file that holds what a pane showed, and how far it is behind.
struct Kept {
    /// Names the file. The pane has it for as long as it lives, also
    /// when the window is made again from the saved session.
    key: String,
    /// [`TerminalView::changes`] of the rows last given to the writer,
    /// and how many rows were asked for. `None` before the first save
    /// and after one that failed.
    saved: Option<(u64, usize)>,
    /// The round in which the pane was last looked at.
    turn: u64,
    /// The writer has rows of the pane that are not on disk yet.
    writing: bool,
}

impl Kept {
    fn new(key: String) -> Self {
        Self {
            key,
            saved: None,
            turn: 0,
            writing: false,
        }
    }
}

/// The terminals of a saved session, started and not yet in a window.
struct StartedSession {
    /// The scrollback keys the session file has for the panes.
    keys: Vec<(PaneId, String)>,
    workspaces: Vec<StartedWorkspace>,
    selected: usize,
    made: u64,
    /// The repositories whose tasks were folded in the sidebar.
    folded: Vec<String>,
}

struct StartedWorkspace {
    name: String,
    /// The task the workspace shows, if the tasks file has it open.
    task: Option<u64>,
    color: Option<WorkspaceColor>,
    tabs: Vec<RestoredPanes>,
    /// The names the user gave the tabs, in the same order.
    tab_names: Vec<Option<String>>,
    selected_tab: usize,
}

/// Opens the main window with the session saved at `path`: the same
/// workspaces, tabs and splits, each pane with a terminal that `spawn`
/// starts in the directory the pane's shell was in, and shows what the
/// pane showed above the output of its shell. A pane whose agent had a
/// conversation has the agent's resume command typed into its shell. Panes get new ids, which
/// `spawn` is given. The window then keeps the file up to date.
///
/// Without a file, or without a `path`, the window starts with one pane.
/// It does so too when the session cannot be made again, and the bar
/// says why. See [`benterm_session::open`] for when the file is left
/// as it is and nothing is saved.
pub fn open_session_window(
    spawn: SpawnTerminal,
    feed: ConfigFeed,
    path: Option<&Path>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    open_with_shells(spawn, None, feed, path, cx)
}

pub(crate) fn open_with_shells(
    spawn: SpawnTerminal,
    shells: Option<&Rc<Shells>>,
    feed: ConfigFeed,
    path: Option<&Path>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    let theme = feed.settings().theme().terminal.clone();
    // The tasks are beside the session, and outlive it.
    let tasks = benterm_task::TaskStore::open(path.map(benterm_task::tasks_path_beside).as_deref());
    let tasks = crate::tasks::Tasks::new(tasks);
    let opened = path.map(benterm_session::open);
    let (saved, mut notice, mut store) = match opened {
        Some(opened) => (opened.session, opened.notice, opened.store),
        None => (None, None, None),
    };
    // A task may have been closed after the session was last saved: its
    // workspace is not made again, and its agent's conversation is not
    // taken up.
    let (saved, pruned) = match saved {
        Some(saved) => tasks.without_closed(saved),
        None => (None, false),
    };
    // With no rows to keep, nothing of what the panes showed is shown
    // again, and the files go.
    let scrollback = feed.settings().scrollback_lines() > 0;
    if let Some(store) = &store {
        // No leaf of the session names the other files, so they would
        // never be read again.
        let kept = saved.as_ref().filter(|_| scrollback);
        if let Err(error) = store.keep_scrollback_of(kept) {
            eprintln!("benterm: could not remove old scrollback: {error}");
        }
    }
    let shown = store.as_ref().filter(|_| scrollback);
    // Every terminal is started before the window is made, so that a
    // session that cannot be made again leaves nothing half made.
    let started = saved.as_ref().map(|saved| {
        start(
            saved,
            &spawn,
            &theme,
            shown,
            &tasks,
            shells,
            feed.settings().roles(),
        )
    });
    let started = match started {
        Some(Ok(started)) => Some(started),
        Some(Err(error)) => {
            // The file may be good for another start, so it stays.
            store = None;
            notice = Some(format!(
                "The saved session was not restored, because a terminal could not be started \
                 ({}). It is left as it is, and this session is not saved.",
                error.kind()
            ));
            None
        }
        None => None,
    };
    let restored = started.is_some();
    let keys = started.as_ref().map(|started| started.keys.clone());
    let first = match &started {
        Some(_) => None,
        None => Some(FirstPane::start(&spawn, &theme, None, None)?),
    };
    let options = window_options(cx);
    cx.open_window(options, |window, cx| {
        let root = cx.new(|cx| {
            let mut root = match (started, first) {
                (Some(started), _) => BentermWindow::restored(started, spawn, feed, window, cx),
                (None, Some(first)) => BentermWindow::new(first, spawn, feed, window, cx),
                (None, None) => unreachable!("a window without a session has a first terminal"),
            };
            root.session_notice = notice;
            root.tasks = tasks;
            if let Some(store) = store {
                // What was restored is in the file already.
                let last = saved.filter(|_| restored && !pruned);
                root.keep_session(store, last, keys.unwrap_or_default(), window, cx);
            }
            root
        });
        let grid = root.read(cx).grid(cx);
        grid.update(cx, |grid, cx| grid.focus_terminal(window, cx));
        root
    })
}

/// Starts the terminals of `session`. If one of them starts neither in
/// its directory nor elsewhere, the ones started so far are dropped.
///
/// With a `store`, each pane keeps the scrollback key the session has
/// for it and shows what the store holds under that key.
fn start(
    session: &Session,
    spawn: &SpawnTerminal,
    theme: &TerminalTheme,
    store: Option<&SessionStore>,
    tasks: &crate::tasks::Tasks,
    shells: Option<&Rc<Shells>>,
    roles: &[Role],
) -> io::Result<StartedSession> {
    let mut keys = Vec::new();
    let mut workspaces = Vec::new();
    for workspace in &session.workspaces {
        let mut tabs = Vec::new();
        for tab in &workspace.tabs {
            let shape = shape_of(&tab.root);
            let mut panes = RestoredPanes::start_with(shape, tab.focused_pane, |id, node| {
                let terminal = match node {
                    Node::Terminal(terminal) => terminal,
                    Node::Browser(browser) => {
                        return Ok(StartedPane::Browser(browser.url().map(str::to_owned)));
                    }
                    Node::Split(_) => unreachable!("only leaves are panes"),
                };
                let started = if let Some(restricted) = &terminal.restricted {
                    let shells = shells.ok_or_else(|| {
                        io::Error::other("this window cannot restore a pane without control")
                    })?;
                    shells.restore_restricted(id, theme, terminal, restricted, roles)?
                } else {
                    let mut started =
                        spawn_in(spawn, id, theme, terminal.directory(), Default::default())?;
                    if let Some(conversation) = &terminal.conversation {
                        started.type_first(conversation.resume_command());
                    }
                    started
                };
                Ok(StartedPane::Terminal(Box::new(started)))
            })?;
            let mut saved = Vec::new();
            keys_of(&tab.root, &mut saved);
            for (index, key) in saved.into_iter().enumerate() {
                let (Some(key), Some(store), Some(pane)) = (key, store, panes.id(index)) else {
                    continue;
                };
                if let Some(history) = store.scrollback(key) {
                    panes.show_before(index, history);
                }
                keys.push((pane, key.to_owned()));
            }
            tabs.push(panes);
        }
        workspaces.push(StartedWorkspace {
            name: workspace.name.clone(),
            // A task that the tasks file does not have leaves a
            // workspace like any other.
            task: workspace.task.filter(|task| tasks.keeps_link(*task)),
            color: workspace.color.and_then(WorkspaceColor::from_index),
            tabs,
            tab_names: workspace.tabs.iter().map(|tab| tab.name.clone()).collect(),
            selected_tab: workspace.selected_tab,
        });
    }
    Ok(StartedSession {
        keys,
        workspaces,
        selected: session.selected_workspace,
        made: session.workspaces_made,
        folded: session.folded_repositories.clone(),
    })
}

/// The scrollback key of each pane of a saved tab, in tree order.
fn keys_of<'a>(node: &'a Node, keys: &mut Vec<Option<&'a str>>) {
    match node {
        Node::Terminal(terminal) => keys.push(terminal.scrollback.as_deref()),
        Node::Browser(_) => keys.push(None),
        Node::Split(split) => {
            keys_of(&split.first, keys);
            keys_of(&split.second, keys);
        }
    }
}

/// The splits of a saved tab, with the saved node at each leaf.
fn shape_of(node: &Node) -> TreeShape<&Node> {
    match node {
        Node::Terminal(_) | Node::Browser(_) => TreeShape::Pane(node),
        Node::Split(split) => TreeShape::Split {
            direction: match split.axis {
                Axis::Horizontal => SplitDirection::Right,
                Axis::Vertical => SplitDirection::Down,
            },
            ratio: split.ratio,
            first: Box::new(shape_of(&split.first)),
            second: Box::new(shape_of(&split.second)),
        },
    }
}

/// The splits of a grid as the session file has them. `leaf` is asked
/// for each pane in tree order.
fn node_of(shape: TreeShape<PaneId>, leaf: &mut impl FnMut(PaneId) -> Node) -> Node {
    match shape {
        TreeShape::Pane(pane) => leaf(pane),
        TreeShape::Split {
            direction,
            ratio,
            first,
            second,
        } => Node::Split(Split {
            axis: match direction {
                SplitDirection::Right => Axis::Horizontal,
                SplitDirection::Down => Axis::Vertical,
            },
            ratio,
            first: Box::new(node_of(*first, leaf)),
            second: Box::new(node_of(*second, leaf)),
        }),
    }
}

impl BentermWindow {
    /// A window with the workspaces of a saved session, whose terminals
    /// have already started.
    fn restored(
        started: StartedSession,
        spawn: SpawnTerminal,
        feed: ConfigFeed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = feed.settings().theme().clone();
        let mut names = Vec::new();
        let mut contents = Vec::new();
        for workspace in started.workspaces {
            let (spawn, theme) = (spawn.clone(), theme.clone());
            let tab_names = workspace.tab_names;
            let tabs = cx.new(|cx| {
                let mut group = TabGroup::restored(
                    workspace.tabs,
                    workspace.selected_tab,
                    spawn,
                    theme,
                    window,
                    cx,
                );
                for (id, name) in group.tabs().into_iter().zip(tab_names) {
                    if let Some(name) = name {
                        group.rename_tab(id, &name, cx);
                    }
                }
                group
            });
            names.push(workspace.name);
            let tabs = crate::WorkspaceTabs {
                task: std::cell::Cell::new(workspace.task),
                ..Self::workspace_tabs_of(tabs, window, cx)
            };
            contents.push((workspace.color, tabs));
        }
        let saved = names
            .iter()
            .zip(contents)
            .map(|(name, (color, tabs))| (Some(name.as_str()), color, tabs))
            .collect();
        let workspaces = WorkspaceList::from_saved(saved, started.selected, started.made)
            .expect("a session that was read has a workspace");
        let mut root = Self::with_workspaces(workspaces, spawn, feed, window, cx);
        root.sidebar.folded = started.folded.into_iter().map(PathBuf::from).collect();
        root
    }

    /// The session as it would be saved now: the workspaces, their tabs,
    /// the splits of each tab, the directory of each pane's shell, the
    /// conversation of each pane's agent, and the scrollback key of each
    /// pane that has been given one.
    pub fn session(&self, cx: &App) -> Session {
        self.session_and_directories(cx).0
    }

    /// The session, and where the shell of each pane is. A shell that
    /// does not tell is taken to be where it last was.
    fn session_and_directories(&self, cx: &App) -> (Session, HashMap<PaneId, PathBuf>) {
        let known = self.session.as_ref().map(|saver| &saver.directories);
        let kept = self.session.as_ref().map(|saver| &saver.scrollback);
        let mut directories = HashMap::new();
        let mut workspaces = Vec::new();
        for workspace in self.workspaces.iter() {
            let group = workspace.content().tabs.read(cx);
            let ids = group.tabs();
            let mut tabs = Vec::new();
            for (id, grid) in ids.iter().filter_map(|id| Some((*id, group.grid(*id)?))) {
                let grid = grid.read(cx);
                let root = node_of(grid.shape(), &mut |pane| {
                    if let Some(browser) = grid.browser(pane) {
                        // Where the page is now, not where it started.
                        let url = browser.read(cx).url().unwrap_or_default();
                        return Node::Browser(Browser::at(&url));
                    }
                    let view = grid.terminal(pane).map(|view| view.read(cx));
                    let directory = view
                        .and_then(TerminalView::working_directory)
                        .or_else(|| known?.get(&pane).cloned());
                    let mut terminal = Terminal::in_directory(directory.as_deref());
                    terminal.conversation = view.and_then(TerminalView::conversation);
                    terminal.restricted = view.and_then(|view| view.restricted.clone());
                    let key = kept.and_then(|kept| kept.get(&pane));
                    terminal.scrollback = key.map(|kept| kept.key.clone());
                    directories.extend(directory.map(|directory| (pane, directory)));
                    Node::Terminal(terminal)
                });
                let focused = grid.focused();
                tabs.push(Tab {
                    name: group.name(id).map(str::to_owned),
                    focused_pane: grid
                        .panes()
                        .iter()
                        .position(|pane| *pane == focused)
                        .unwrap_or(0),
                    root,
                });
            }
            let selected = group.selected();
            workspaces.push(Workspace {
                task: workspace.content().task.get(),
                name: workspace.name().to_owned(),
                color: Some(workspace.color().index()),
                selected_tab: ids.iter().position(|id| *id == selected).unwrap_or(0),
                tabs,
            });
        }
        let session = Session {
            selected_workspace: self.workspaces.selected_index(),
            workspaces_made: self.workspaces.made(),
            folded_repositories: self.folded_repositories(),
            workspaces,
        };
        (session, directories)
    }

    /// Every pane of the window with its terminal.
    fn panes(&self, cx: &App) -> Vec<(PaneId, Entity<TerminalView>)> {
        let mut panes = Vec::new();
        for workspace in self.workspaces.iter() {
            let group = workspace.content().tabs.read(cx);
            let ids = group.tabs();
            for grid in ids.iter().filter_map(|id| group.grid(*id)) {
                let grid = grid.read(cx);
                let views = grid.panes().into_iter();
                panes.extend(views.filter_map(|pane| Some((pane, grid.terminal(pane)?.clone()))));
            }
        }
        panes
    }

    /// Gives every pane that has none a scrollback key, and forgets the
    /// panes that are gone; all of them when no rows are to be kept.
    /// Returns the keys of the forgotten panes, whose files are to be
    /// removed once the session file no longer names them.
    fn keep_keys(&mut self, cx: &App) -> Vec<String> {
        let lines = self.settings.scrollback_lines();
        let panes: HashSet<PaneId> = self.panes(cx).into_iter().map(|(pane, _)| pane).collect();
        let Some(saver) = &mut self.session else {
            return Vec::new();
        };
        let mut gone = std::mem::take(&mut saver.gone);
        saver.scrollback.retain(|pane, kept| {
            let stays = lines > 0 && panes.contains(pane);
            if !stays {
                gone.push(kept.key.clone());
            }
            stays
        });
        if lines > 0 {
            for pane in panes {
                if saver.scrollback.contains_key(&pane) {
                    continue;
                }
                // The key the history has for the pane, as when rows
                // are kept again after a time without: the pane has
                // one key. Without random bytes the pane goes without.
                let recorded = self.agent_history.key(pane).map(str::to_owned);
                if let Some(key) = recorded.or_else(|| new_scrollback_key().ok()) {
                    saver.scrollback.insert(pane, Kept::new(key));
                }
            }
        }
        gone
    }

    /// The store of the session, if this window saves its session.
    pub(crate) fn session_store(&self) -> Option<Arc<SessionStore>> {
        self.session.as_ref().map(|saver| saver.store.clone())
    }

    /// The scrollback key of `pane`, which it is given now if it has
    /// none yet, for something that is kept under the key before the
    /// next save. `None` when the session is not saved or no rows are
    /// kept: the session file then has no keys.
    pub(crate) fn scrollback_key(&mut self, pane: PaneId) -> Option<String> {
        let lines = self.settings.scrollback_lines();
        let saver = self.session.as_mut().filter(|saver| !saver.done)?;
        if lines == 0 {
            return None;
        }
        if let Some(kept) = saver.scrollback.get(&pane) {
            return Some(kept.key.clone());
        }
        let key = new_scrollback_key().ok()?;
        saver.scrollback.insert(pane, Kept::new(key.clone()));
        Some(key)
    }

    /// Why a saved session was not made again, or why this one is not
    /// saved, until the bar is closed.
    pub fn shown_session_notice(&self) -> Option<String> {
        self.session_notice.clone().filter(|_| !self.dismissed)
    }

    /// Saves the session to `store` from now on. `last` is the session
    /// the file holds already, if the window was made from it, and
    /// `keys` are the scrollback keys it has for the panes.
    fn keep_session(
        &mut self,
        store: SessionStore,
        last: Option<Session>,
        keys: Vec<(PaneId, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SAVE_INTERVAL).await;
                if this.update(cx, |this, cx| this.save_session(cx)).is_err() {
                    break;
                }
            }
        });
        // Both run while the terminals are still there to be asked.
        let quit = cx.on_app_quit(|this, cx| {
            this.finish_session(cx);
            async {}
        });
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = this.update(cx, |this, cx| this.finish_session(cx));
            true
        });
        self.session = Some(SessionSaver {
            store: Arc::new(store),
            last,
            latest: None,
            directories: HashMap::new(),
            scrollback: keys
                .into_iter()
                .map(|(pane, key)| (pane, Kept::new(key)))
                .collect(),
            round: 0,
            gone: Vec::new(),
            failed: false,
            done: false,
            _tick: tick,
            _quit: quit,
        });
    }

    /// Saves the session if it is not the one saved last. The file is
    /// written on another thread, since waiting for the disk takes
    /// longer than a frame.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        if self.session.as_ref().is_none_or(|saver| saver.done) {
            return;
        }
        let gone = self.keep_keys(cx);
        self.save_scrollback(cx);
        let (session, directories) = self.session_and_directories(cx);
        let Some(saver) = &mut self.session else {
            return;
        };
        saver.directories = directories;
        if saver.last.as_ref() == Some(&session) {
            return;
        }
        saver.last = Some(session.clone());
        // Taken here, so that this session does not replace one that the
        // window saves later and the writer gets to first.
        let ticket = saver.store.ticket();
        saver.latest = Some(ticket);
        let store = saver.store.clone();
        let written = cx.background_spawn(async move {
            let written = store.write(ticket, &session);
            // Only once the file no longer names them: a crash before
            // that brings the panes back, and what they showed with them.
            let left = match written {
                Ok(()) => {
                    remove_scrollback(&store, ticket, &gone);
                    Vec::new()
                }
                Err(_) => gone,
            };
            (written, left)
        });
        cx.spawn(async move |this, cx| {
            let (written, left) = written.await;
            let _ = this.update(cx, |this, cx| {
                // Removed with the next session that is written.
                if let Some(saver) = &mut this.session {
                    saver.gone.extend(left);
                }
                this.session_saved(ticket, written, cx);
            });
        })
        .detach();
    }

    /// Saves what one pane shows: of the panes that changed since they
    /// were saved, the one that has waited longest. A pane that has
    /// nothing to take now, as while it shows the alternate screen,
    /// keeps what was saved of it and lets the next pane go first.
    fn save_scrollback(&mut self, cx: &mut Context<Self>) {
        let lines = self.settings.scrollback_lines();
        let panes = self.panes(cx);
        let Some(saver) = &mut self.session else {
            return;
        };
        saver.round += 1;
        let mut waiting: Vec<_> = panes
            .iter()
            .filter_map(|(pane, view)| {
                let kept = saver.scrollback.get(pane)?;
                let changes = view.read(cx).changes();
                let behind = !kept.writing && kept.saved != Some((changes, lines));
                behind.then_some((kept.turn, *pane, view, changes))
            })
            .collect();
        waiting.sort_by_key(|(turn, ..)| *turn);
        for (_, pane, view, changes) in waiting {
            let Some(kept) = saver.scrollback.get_mut(&pane) else {
                continue;
            };
            kept.turn = saver.round;
            // Output that arrives after `changes` was read is in the
            // rows already and is saved once more: never the other way.
            let Some(rows) = view.read(cx).history(lines) else {
                continue;
            };
            kept.saved = Some((changes, lines));
            kept.writing = true;
            let key = kept.key.clone();
            let ticket = saver.store.ticket();
            let store = saver.store.clone();
            let written = cx.background_spawn(async move {
                let bytes = history_bytes_within(&rows, MAX_SCROLLBACK_BYTES);
                store.write_scrollback(ticket, &key, &bytes)
            });
            cx.spawn(async move |this, cx| {
                let written = written.await;
                let _ = this.update(cx, |this, _| this.scrollback_saved(pane, written));
            })
            .detach();
            break;
        }
    }

    /// Takes the outcome of a save of what `pane` showed. After a
    /// failure the pane is saved again when its turn comes.
    fn scrollback_saved(&mut self, pane: PaneId, written: io::Result<()>) {
        let saver = self.session.as_mut();
        let Some(kept) = saver.and_then(|saver| saver.scrollback.get_mut(&pane)) else {
            return;
        };
        kept.writing = false;
        if let Err(error) = written {
            kept.saved = None;
            eprintln!("benterm: could not save what a pane showed: {error}");
        }
    }

    /// Takes the outcome of the save with `ticket`. A failure is shown
    /// in the bar, and the next round saves again. The outcome of a save
    /// that a later one has followed is passed over, in whatever order
    /// the two come back.
    fn session_saved(&mut self, ticket: Ticket, written: io::Result<()>, cx: &mut Context<Self>) {
        let Some(saver) = &mut self.session else {
            return;
        };
        if saver.done || saver.latest != Some(ticket) {
            return;
        }
        match written {
            Ok(()) if saver.failed => {
                saver.failed = false;
                self.session_notice = None;
                cx.notify();
            }
            Ok(()) => {}
            Err(error) => {
                saver.last = None;
                if !saver.failed && self.session_notice.is_none() {
                    saver.failed = true;
                    self.session_notice = Some(format!(
                        "The session could not be saved ({}).",
                        error.kind()
                    ));
                    self.dismissed = false;
                    cx.notify();
                }
            }
        }
    }

    /// Saves the session before the window or the app goes away, and
    /// waits until it is on disk. Nothing is saved after that.
    fn finish_session(&mut self, cx: &App) {
        // While the keys of the panes are still known.
        self.finish_agent_history();
        if self.session.as_ref().is_none_or(|saver| saver.done) {
            return;
        }
        let gone = self.keep_keys(cx);
        let lines = self.settings.scrollback_lines();
        let panes = self.panes(cx);
        let (session, _) = self.session_and_directories(cx);
        let Some(saver) = &mut self.session else {
            return;
        };
        saver.done = true;
        // What every pane shows that is not on disk yet. A write that
        // is still on its way may not get there before the app is gone.
        for (pane, view) in panes {
            let Some(kept) = saver.scrollback.get(&pane) else {
                continue;
            };
            let view = view.read(cx);
            if !kept.writing && kept.saved == Some((view.changes(), lines)) {
                continue;
            }
            let Some(rows) = view.history(lines) else {
                continue;
            };
            let ticket = saver.store.ticket();
            let written = saver.store.write_scrollback(
                ticket,
                &kept.key,
                &history_bytes_within(&rows, MAX_SCROLLBACK_BYTES),
            );
            if let Err(error) = written {
                eprintln!("benterm: could not save what a pane showed: {error}");
            }
        }
        let ticket = saver.store.ticket();
        match saver.store.write(ticket, &session) {
            Ok(()) => remove_scrollback(&saver.store, ticket, &gone),
            Err(error) => eprintln!("benterm: could not save the session: {error}"),
        }
    }

    /// Removes the saved session and what its panes showed, as the user
    /// closed the last pane. The next start has one pane.
    pub(crate) fn clear_session(&mut self) {
        self.finish_agent_history();
        let Some(saver) = self.session.as_mut().filter(|saver| !saver.done) else {
            return;
        };
        saver.done = true;
        if let Err(error) = saver.store.clear(saver.store.ticket()) {
            eprintln!("benterm: could not remove the saved session: {error}");
        }
    }
}

/// Removes what the panes with `keys` showed. A file that stays is
/// removed at the next start.
fn remove_scrollback(store: &SessionStore, ticket: Ticket, keys: &[String]) {
    for key in keys {
        if let Err(error) = store.remove_scrollback(ticket, key) {
            eprintln!("benterm: could not remove what a closed pane showed: {error}");
        }
    }
}
