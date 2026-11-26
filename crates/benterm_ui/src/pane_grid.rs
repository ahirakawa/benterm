//! The panes of a window: a tree of splits with a terminal or a browser
//! in each pane.
//!
//! The tree decides where every pane goes. The grid element lays the panes
//! out against the bounds it is given in the same frame, so a terminal is
//! never sized for the window's previous size.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use benterm_layout::{
    Direction, LayoutError, PaneBorder, PaneId, PaneTree, Size as TabSize, SplitDirection,
    TreeShape,
};
use benterm_terminal::{Notification, WindowSize};
use gpui::{
    AnyView, App, AppContext, AvailableSpace, BorderStyle, Bounds, ContentMask, Context,
    CursorStyle, DispatchPhase, Element, ElementId, Entity, EventEmitter, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, Point, Render, Style, Styled, Subscription, Window, actions, div, fill, outline, point,
    px, relative, size,
};

use crate::browser::{BrowserError, BrowserEvent, BrowserView, OpenBrowser};
use crate::terminal_view::{StartedTerminal, TerminalEvent, TerminalView, hsla};
use crate::theme::{PaneTheme, TerminalTheme, Theme};

actions!(
    pane,
    [
        /// Splits the focused pane and starts a shell on the right.
        SplitRight,
        /// Splits the focused pane and starts a shell below.
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        ResizeLeft,
        ResizeRight,
        ResizeUp,
        ResizeDown,
        ToggleZoom,
    ]
);

/// The key context of the grid, which its bindings are limited to.
const KEY_CONTEXT: &str = "PaneGrid";

/// How far a keyboard resize moves a split border, in pixels.
const RESIZE_STEP: i32 = 40;

/// Binds the pane keys, which follow Ghostty's macOS defaults.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-d", SplitRight, context),
        KeyBinding::new("cmd-shift-d", SplitDown, context),
        KeyBinding::new("cmd-alt-left", FocusLeft, context),
        KeyBinding::new("cmd-alt-right", FocusRight, context),
        KeyBinding::new("cmd-alt-up", FocusUp, context),
        KeyBinding::new("cmd-alt-down", FocusDown, context),
        KeyBinding::new("ctrl-cmd-left", ResizeLeft, context),
        KeyBinding::new("ctrl-cmd-right", ResizeRight, context),
        KeyBinding::new("ctrl-cmd-up", ResizeUp, context),
        KeyBinding::new("ctrl-cmd-down", ResizeDown, context),
        KeyBinding::new("cmd-shift-enter", ToggleZoom, context),
        // Not a key of Ghostty, which has no browser: the key that moves
        // to the address, with shift.
        KeyBinding::new("cmd-shift-l", OpenBrowser, context),
    ]);
}

/// No pane is made smaller than this, in pixels: a few cells inside the
/// terminal's padding.
const MIN_PANE: TabSize = TabSize::new(80, 48);

/// How thick the drawn lines are.
const LINE: Pixels = px(1.);

/// How far on each side of a border the mouse can grab it.
pub(crate) const GRAB: Pixels = px(3.);

/// Starts the terminal of a new pane, given the id the pane has in its
/// tree, so the program can be told which pane it runs in, the directory
/// to start in if there is one, and the size the terminal starts with.
/// Shared by the panes of every tab.
pub type SpawnTerminal =
    Rc<dyn Fn(PaneId, &TerminalTheme, Option<&Path>, WindowSize) -> io::Result<StartedTerminal>>;

/// Starts the terminal of `pane` in `directory`. If none starts there, as
/// when the directory has just been removed or may not be entered, starts
/// one where `spawn` puts a terminal that is given no directory.
pub(crate) fn spawn_in(
    spawn: &SpawnTerminal,
    pane: PaneId,
    theme: &TerminalTheme,
    directory: Option<&Path>,
    size: WindowSize,
) -> io::Result<StartedTerminal> {
    match spawn(pane, theme, directory, size) {
        Err(_) if directory.is_some() => spawn(pane, theme, None, size),
        result => result,
    }
}

/// The first pane of a new tab: its tree and the terminal started for it.
///
/// The tree comes first, because it gives the pane its id and the
/// terminal is started with that id. Both exist before the grid does, so
/// a program that fails to start is reported without a tab, or a window,
/// having been opened for it.
pub struct FirstPane {
    tree: PaneTree,
    started: StartedTerminal,
}

impl FirstPane {
    /// Makes the tree and starts the terminal of its pane with `spawn`,
    /// in `directory` if there is one and a terminal starts there. The
    /// terminal starts with `size`, which a window that has been drawn
    /// knows for its pane area; without one it starts with the usual 24
    /// rows and 80 columns and is fitted when it is first drawn.
    pub fn start(
        spawn: &SpawnTerminal,
        theme: &TerminalTheme,
        directory: Option<&Path>,
        size: Option<WindowSize>,
    ) -> io::Result<Self> {
        let size = size.unwrap_or_default();
        Self::start_with(|pane| spawn_in(spawn, pane, theme, directory, size))
    }

    /// Makes the tree and starts the terminal of its pane with `start`,
    /// which is given the pane's id.
    pub fn start_with(
        start: impl FnOnce(PaneId) -> io::Result<StartedTerminal>,
    ) -> io::Result<Self> {
        let tree = PaneTree::new(MIN_PANE);
        let started = start(tree.focused())?;
        Ok(Self { tree, started })
    }

    pub fn id(&self) -> PaneId {
        self.tree.focused()
    }
}

/// A terminal that has the id of a pane and is in no grid: it runs behind
/// the window until a tab is opened for it. It keeps the tree that gave
/// it its id, which becomes the tree of that tab, so the id the program
/// was told stays the id of its pane.
pub(crate) struct LoneTerminal {
    tree: PaneTree,
    view: Entity<TerminalView>,
}

impl LoneTerminal {
    pub(crate) fn id(&self) -> PaneId {
        self.tree.focused()
    }

    pub(crate) fn view(&self) -> &Entity<TerminalView> {
        &self.view
    }
}

impl FirstPane {
    /// Makes the view of the terminal without a grid to hold it.
    pub(crate) fn into_lone<V: 'static>(
        self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> LoneTerminal {
        let Self { tree, started } = self;
        let view = cx.new(|cx| TerminalView::new(started, theme, window, cx));
        LoneTerminal { tree, view }
    }
}

/// What a pane of a saved session held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedPane {
    /// A shell, in the directory if there is one.
    Terminal(Option<PathBuf>),
    /// A browser at the address, or one that shows no page.
    Browser(Option<String>),
}

/// What a pane made again starts with.
pub(crate) enum StartedPane {
    Terminal(Box<StartedTerminal>),
    /// The address of the page. The page itself is made with the grid,
    /// as it needs the window.
    Browser(Option<String>),
}

/// The panes of a tab made again from a saved session: the tree, and the
/// terminal started for each of its panes that has one.
///
/// As with [`FirstPane`], the tree comes first and gives every pane its
/// id, and all terminals have started before a grid is made for them.
pub struct RestoredPanes {
    tree: PaneTree,
    started: Vec<(PaneId, StartedPane)>,
}

impl RestoredPanes {
    /// Makes a tree with the splits of `shape` and starts a terminal for
    /// each pane that had one with `spawn`, in the pane's directory if it
    /// has one and a terminal starts there. `focused` counts the panes
    /// from 0 in tree order. If a terminal does not start, the ones
    /// started are dropped.
    pub fn start(
        shape: TreeShape<SavedPane>,
        focused: usize,
        spawn: &SpawnTerminal,
        theme: &TerminalTheme,
    ) -> io::Result<Self> {
        Self::start_with(shape, focused, |id, saved| match saved {
            SavedPane::Terminal(directory) => {
                let started = spawn_in(
                    spawn,
                    id,
                    theme,
                    directory.as_deref(),
                    WindowSize::default(),
                )?;
                Ok(StartedPane::Terminal(Box::new(started)))
            }
            SavedPane::Browser(url) => Ok(StartedPane::Browser(url)),
        })
    }

    pub(crate) fn start_with<T>(
        shape: TreeShape<T>,
        focused: usize,
        mut start: impl FnMut(PaneId, T) -> io::Result<StartedPane>,
    ) -> io::Result<Self> {
        let (tree, panes) = PaneTree::from_shape(shape, focused, MIN_PANE);
        let started = panes
            .into_iter()
            .map(|(id, saved)| start(id, saved).map(|pane| (id, pane)))
            .collect::<io::Result<_>>()?;
        Ok(Self { tree, started })
    }

    /// The id of the pane at place `index`, counted from 0 in tree
    /// order.
    pub fn id(&self, index: usize) -> Option<PaneId> {
        self.started.get(index).map(|(id, _)| *id)
    }

    /// Makes the pane at place `index` show `history` above the output
    /// of its program. See [`StartedTerminal::show_before`].
    pub fn show_before(&mut self, index: usize, history: Vec<u8>) {
        if let Some((_, StartedPane::Terminal(started))) = self.started.get_mut(index) {
            started.show_before(history);
        }
    }

    /// Makes the pane at place `index` type `command` into its shell
    /// once it shows what it showed before. See
    /// [`StartedTerminal::type_first`].
    pub fn type_first(&mut self, index: usize, command: String) {
        if let Some((_, StartedPane::Terminal(started))) = self.started.get_mut(index) {
            started.type_first(command);
        }
    }
}

/// Why a pane was not split.
#[derive(Debug)]
pub(crate) enum SplitError {
    /// The tree refused: the tab is too small for another pane.
    Layout(LayoutError),
    /// The terminal of the new pane did not start.
    Spawn(io::Error),
}

/// What a grid tells the tab it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneGridEvent {
    /// The focused pane, or its title, changed.
    TitleChanged,
    /// The program of a pane asked for a notification.
    Notified(PaneId, Notification),
    /// The last pane was closed, so the tab has nothing left to show. The
    /// grid still holds that pane until the grid is dropped.
    Emptied,
    /// A right click on a pane asked for its menu at this point in the
    /// window.
    MenuRequested(PaneId, Point<Pixels>),
    /// A command-click on a web address in the terminal of a pane. The
    /// window decides where it opens: the terminal may be in the peek.
    OpenUrl(PaneId, String),
    /// A command-click on an existing file or folder path in the terminal
    /// of a pane.
    OpenPath(PaneId, PathBuf),
    /// A pane's program exited with a non-zero status shortly after
    /// starting (or with 127, command not found), right before the pane
    /// closes.
    StartupFailed(PaneId, crate::toast::StartupFailure),
}

/// What a pane shows.
enum Content {
    Terminal(Entity<TerminalView>),
    Browser(Entity<BrowserView>),
}

struct Pane {
    content: Content,
    _events: Subscription,
}

impl Pane {
    fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match &self.content {
            Content::Terminal(view) => Some(view),
            Content::Browser(_) => None,
        }
    }

    fn browser(&self) -> Option<&Entity<BrowserView>> {
        match &self.content {
            Content::Terminal(_) => None,
            Content::Browser(view) => Some(view),
        }
    }

    /// The handle GPUI's focus is on while the pane has the keyboard.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.content {
            Content::Terminal(view) => view.read(cx).focus_handle().clone(),
            Content::Browser(view) => view.read(cx).focus_handle().clone(),
        }
    }

    fn view(&self) -> AnyView {
        match &self.content {
            Content::Terminal(view) => view.clone().into(),
            Content::Browser(view) => view.clone().into(),
        }
    }
}

/// The panes of a window and the terminals and browsers in them.
pub struct PaneGrid {
    tree: PaneTree,
    panes: HashMap<PaneId, Pane>,
    /// The pane that is drawn elsewhere, by the peek, while it is.
    hidden: Option<PaneId>,
    spawn: SpawnTerminal,
    theme: Theme,
    /// Where the grid was laid out in the last frame, in window
    /// coordinates.
    bounds: Bounds<Pixels>,
    /// The border being dragged, named by the pane and side that move it.
    drag: Option<(PaneId, Direction)>,
    /// The panes that need the user, outlined in the attention color.
    marked: HashSet<PaneId>,
}

impl PaneGrid {
    /// A grid with the one pane `first`. `spawn` starts the terminal of
    /// every pane split off later.
    pub fn new(
        first: FirstPane,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let FirstPane { tree, started } = first;
        let mut grid = Self {
            tree,
            panes: HashMap::new(),
            hidden: None,
            spawn,
            theme,
            bounds: Bounds::default(),
            drag: None,
            marked: HashSet::new(),
        };
        grid.add_pane(grid.tree.focused(), started, window, cx);
        grid
    }

    /// A grid with the one pane of `lone`, a terminal that has been
    /// running behind the window. The terminal is the same one: its
    /// program, its screen and its id stay.
    pub(crate) fn adopting(
        lone: LoneTerminal,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let LoneTerminal { tree, view } = lone;
        let id = tree.focused();
        // The theme may have changed while nothing drew the terminal.
        view.update(cx, |terminal, cx| terminal.set_theme(theme.clone(), cx));
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.on_terminal_event(id, event.clone(), window, cx);
        });
        let panes = HashMap::from([(
            id,
            Pane {
                content: Content::Terminal(view),
                _events: events,
            },
        )]);
        Self {
            tree,
            panes,
            hidden: None,
            spawn,
            theme,
            bounds: Bounds::default(),
            drag: None,
            marked: HashSet::new(),
        }
    }

    /// Closes a pane whatever its program is doing, as the user does.
    /// Leaves `pane` out of what the grid draws, for the peek that draws
    /// it, or draws every pane again.
    pub(crate) fn set_hidden(&mut self, pane: Option<PaneId>, cx: &mut Context<Self>) {
        if self.hidden != pane {
            self.hidden = pane;
            cx.notify();
        }
    }

    pub(crate) fn close_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.close(id, window, cx);
    }

    /// A grid with the splits and the terminals of `panes`. `spawn`
    /// starts the terminal of every pane split off later.
    pub fn restored(
        panes: RestoredPanes,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RestoredPanes { tree, started } = panes;
        let mut grid = Self {
            tree,
            panes: HashMap::new(),
            hidden: None,
            spawn,
            theme,
            bounds: Bounds::default(),
            drag: None,
            marked: HashSet::new(),
        };
        for (id, started) in started {
            match started {
                StartedPane::Terminal(started) => grid.add_pane(id, *started, window, cx),
                StartedPane::Browser(url) => grid.add_browser(id, url.as_deref(), window, cx),
            }
        }
        grid
    }

    /// The splits of the grid and their ratios, with the id of each pane.
    pub fn shape(&self) -> TreeShape<PaneId> {
        self.tree.shape()
    }

    /// The panes, from top left to bottom right in tree order.
    pub fn panes(&self) -> Vec<PaneId> {
        self.tree.panes()
    }

    pub fn focused(&self) -> PaneId {
        self.tree.focused()
    }

    /// The pane currently zoomed to fill the tab, if any.
    pub fn zoomed(&self) -> Option<PaneId> {
        self.tree.zoomed()
    }

    pub fn needs_attention(&self, id: PaneId) -> bool {
        self.marked.contains(&id)
    }

    /// Whether any pane of the grid needs the user.
    pub fn has_attention(&self) -> bool {
        !self.marked.is_empty()
    }

    /// Marks the panes of the grid that are among `marked`, and only
    /// those.
    pub(crate) fn set_attention(&mut self, marked: &HashSet<PaneId>, cx: &mut Context<Self>) {
        let mine: HashSet<PaneId> = self
            .panes
            .keys()
            .filter(|id| marked.contains(id))
            .copied()
            .collect();
        if mine != self.marked {
            self.marked = mine;
            cx.notify();
        }
    }

    pub fn terminal(&self, id: PaneId) -> Option<&Entity<TerminalView>> {
        self.panes.get(&id).and_then(Pane::terminal)
    }

    pub fn browser(&self, id: PaneId) -> Option<&Entity<BrowserView>> {
        self.panes.get(&id).and_then(Pane::browser)
    }

    /// Where each pane was put in the last frame, in window coordinates, in
    /// the order of [`panes`](Self::panes).
    pub fn pane_bounds(&self) -> Vec<(PaneId, Bounds<Pixels>)> {
        pane_bounds(&self.tree, self.bounds)
    }

    /// The theme the grid draws its lines with and starts new panes with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Draws every pane with `theme`, and starts new panes with it.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        for pane in self.panes.values() {
            let theme = theme.clone();
            match &pane.content {
                Content::Terminal(view) => {
                    view.update(cx, |terminal, cx| terminal.set_theme(theme, cx));
                }
                Content::Browser(view) => {
                    view.update(cx, |browser, cx| browser.set_theme(theme, cx));
                }
            }
        }
        self.theme = theme;
        cx.notify();
    }

    fn add_pane(
        &mut self,
        id: PaneId,
        started: StartedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let view = cx.new(|cx| TerminalView::new(started, theme, window, cx));
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.on_terminal_event(id, event.clone(), window, cx);
        });
        self.panes.insert(
            id,
            Pane {
                content: Content::Terminal(view),
                _events: events,
            },
        );
    }

    fn add_browser(
        &mut self,
        id: PaneId,
        url: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial_place = self
            .pane_bounds()
            .into_iter()
            .find(|(pane, _)| *pane == id)
            .and_then(|(_, pane)| crate::browser::initial_page_bounds(pane, &self.theme));
        let theme = self.theme.clone();
        let view = cx.new(|cx| BrowserView::new(url, theme, initial_place, window, cx));
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.on_browser_event(id, *event, window, cx);
        });
        self.panes.insert(
            id,
            Pane {
                content: Content::Browser(view),
                _events: events,
            },
        );
    }

    fn on_browser_event(
        &mut self,
        id: PaneId,
        event: BrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            BrowserEvent::TitleChanged if id == self.tree.focused() => {
                cx.emit(PaneGridEvent::TitleChanged);
                cx.notify();
            }
            BrowserEvent::TitleChanged => {}
            // The page was clicked, which the window does not see.
            BrowserEvent::TookKeyboard => self.focus(id, window, cx),
            BrowserEvent::CloseRequested => self.close(id, window, cx),
        }
    }

    fn on_terminal_event(
        &mut self,
        id: PaneId,
        event: TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::TitleChanged if id == self.tree.focused() => {
                cx.emit(PaneGridEvent::TitleChanged);
                cx.notify();
            }
            TerminalEvent::TitleChanged => {}
            TerminalEvent::Notified(notification) => {
                cx.emit(PaneGridEvent::Notified(id, notification));
            }
            TerminalEvent::Exited => {
                let failure = self.terminal(id).and_then(|view| {
                    view.read(cx)
                        .startup_failure(cx.background_executor().now())
                });
                if let Some(failure) = failure {
                    cx.emit(PaneGridEvent::StartupFailed(id, failure));
                }
                self.close(id, window, cx);
            }
            TerminalEvent::CommandFinished(_) | TerminalEvent::GaveWayToShell => {}
            TerminalEvent::OpenUrl(url) => cx.emit(PaneGridEvent::OpenUrl(id, url)),
            TerminalEvent::OpenPath(path) => cx.emit(PaneGridEvent::OpenPath(id, path)),
            TerminalEvent::MenuRequested(position) => {
                cx.emit(PaneGridEvent::MenuRequested(id, position));
            }
        }
    }

    /// Opens the page at `url` in the browser pane at the right of pane
    /// `id`, or in a new one split off there. The keyboard stays where it
    /// is.
    pub(crate) fn open_link(
        &mut self,
        id: PaneId,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let right = self.tree.neighbor(id, Direction::Right, self.tab_size());
        if let Some(browser) = right.and_then(|pane| self.browser(pane)).cloned() {
            // The address was checked to be a web page's when it was found.
            let _ = browser.update(cx, |browser, cx| browser.open(url, cx));
            return;
        }
        let shown = self.bounds;
        if let Err(error) =
            self.open_browser(id, SplitDirection::Right, Some(url), shown, window, cx)
        {
            eprintln!("benterm: could not open {url}: {error}");
        }
    }

    /// Where a terminal opened from the focused pane starts. See
    /// [`TerminalView::directory_for_new_terminal`].
    pub fn directory_for_new_terminal(&self, cx: &App) -> Option<PathBuf> {
        self.directory_of(self.tree.focused(), cx)
    }

    fn directory_of(&self, id: PaneId, cx: &App) -> Option<PathBuf> {
        self.terminal(id)
            .and_then(|view| view.read(cx).directory_for_new_terminal())
    }

    /// The title the program in the focused pane set, if it set one, or
    /// what the browser in it calls its page.
    pub fn title(&self, cx: &App) -> Option<String> {
        match &self.panes.get(&self.tree.focused())?.content {
            Content::Terminal(view) => view.read(cx).screen().title.clone(),
            Content::Browser(view) => Some(view.read(cx).title()),
        }
    }

    /// Moves the keyboard focus to the tree's focused pane.
    pub fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(&self.tree.focused()) {
            let handle = pane.focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.emit(PaneGridEvent::TitleChanged);
        cx.notify();
    }

    /// Whether the keyboard focus is in one of the grid's panes.
    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.panes
            .values()
            .any(|pane| pane.focus_handle(cx).contains_focused(window, cx))
    }

    /// Closes the focused pane and ends its program. Closing the last pane
    /// empties the grid.
    pub fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(self.tree.focused(), window, cx);
    }

    /// Makes `id` the focused pane and gives it the keyboard.
    pub(crate) fn focus(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let was_zoomed = self.tree.zoomed().is_some();
        if self.tree.focus(id).is_ok() {
            if was_zoomed && self.tree.zoomed().is_none() {
                self.fit_panes(cx);
            }
            self.focus_terminal(window, cx);
        }
    }

    /// The size the tree lays the panes out in: the grid's last bounds in
    /// whole pixels.
    fn tab_size(&self) -> TabSize {
        tab_size(self.bounds)
    }

    fn split(&mut self, direction: SplitDirection, window: &mut Window, cx: &mut Context<Self>) {
        self.split_at(self.tree.focused(), direction, window, cx);
    }

    /// Splits `id` as [`SplitRight`] and [`SplitDown`] split the focused
    /// pane, and gives the keyboard to the new pane.
    pub(crate) fn split_at(
        &mut self,
        id: PaneId,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.split_off(id, direction, window, cx) {
            Ok(_) => {}
            // A tab too small for another pane is left as it is.
            Err(SplitError::Layout(_)) => return,
            Err(SplitError::Spawn(error)) => {
                eprintln!("benterm: could not start a terminal for the new pane: {error}");
            }
        }
        self.focus_terminal(window, cx);
    }

    /// Splits a pane and starts a terminal in the new one, which the tree
    /// then has as its focused pane.
    fn split_off(
        &mut self,
        id: PaneId,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, SplitError> {
        let tab = self.tab_size();
        let new = self
            .tree
            .split(id, direction, tab)
            .map_err(SplitError::Layout)?;
        // The new pane starts where the pane it was split from is, at
        // the size of the place the tree has just given it.
        let directory = self.directory_of(id, cx);
        let theme = &self.theme.terminal;
        let size = self
            .pane_bounds()
            .into_iter()
            .find(|(pane, _)| *pane == new)
            .map(|(_, place)| TerminalView::size_in(place.size, theme, cx))
            .unwrap_or_default();
        match spawn_in(&self.spawn, new, theme, directory.as_deref(), size) {
            Ok(started) => {
                self.add_pane(new, started, window, cx);
                self.fit_panes(cx);
                Ok(new)
            }
            Err(error) => {
                // The pane that was split takes the space back and the
                // focus with it.
                let _ = self.tree.close(new, tab);
                Err(SplitError::Spawn(error))
            }
        }
    }

    /// Tells every terminal the size of the place its pane has now. A
    /// frame does the same for the grid on view; a grid that is not on
    /// view has no frame to wait for. A terminal that already has the
    /// size is left alone.
    fn fit_panes(&self, cx: &mut Context<Self>) {
        for (id, place) in self.pane_bounds() {
            if let Some(view) = self.terminal(id) {
                view.update(cx, |terminal, _| terminal.fit(place.size));
            }
        }
    }

    /// Lays the grid out in `area`, where the window shows its grids, if
    /// no frame and no split has laid it out yet, and tells its terminals
    /// their sizes. A grid that has a place keeps it until it is shown.
    pub(crate) fn lay_out_if_never_shown(&mut self, area: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.bounds.size.width <= px(0.) || self.bounds.size.height <= px(0.) {
            self.bounds = area;
            self.fit_panes(cx);
        }
    }

    /// Splits any pane of the grid for a caller outside the window, and
    /// returns the new pane. The focused pane and the keyboard stay where
    /// they are.
    ///
    /// `shown` is where the grid the window shows was laid out in the last
    /// frame. Every grid of a window is laid out in the same place, so
    /// the grid goes by it: a grid that is not on view was laid out for
    /// an older size of the window, or never. Its terminals are told
    /// their new sizes here, as no frame will.
    pub(crate) fn split_pane(
        &mut self,
        id: PaneId,
        direction: SplitDirection,
        shown: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, SplitError> {
        self.go_by(shown);
        let focused = self.tree.focused();
        let split = self.split_off(id, direction, window, cx);
        let _ = self.tree.focus(focused);
        cx.notify();
        split
    }

    /// Splits pane `id` for a browser that shows the page at `url`, or
    /// no page without one, and returns the new pane. The focused pane
    /// and the keyboard stay where they are. `shown` is as for
    /// [`split_pane`](Self::split_pane).
    pub(crate) fn open_browser(
        &mut self,
        id: PaneId,
        direction: SplitDirection,
        url: Option<&str>,
        shown: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, BrowserError> {
        if !self.panes.contains_key(&id) {
            return Err(BrowserError::UnknownPane);
        }
        self.go_by(shown);
        let focused = self.tree.focused();
        let tab = self.tab_size();
        let new = self
            .tree
            .split(id, direction, tab)
            .map_err(BrowserError::Layout)?;
        let _ = self.tree.focus(focused);
        self.add_browser(new, url, window, cx);
        self.fit_panes(cx);
        cx.notify();
        Ok(new)
    }

    /// Opens a browser pane at the right of the focused pane, as the
    /// user does, with the keyboard in its address. A tab too small for
    /// another pane is left as it is.
    fn open_browser_beside_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (focused, shown) = (self.tree.focused(), self.bounds);
        let opened = self.open_browser(focused, SplitDirection::Right, None, shown, window, cx);
        let Ok(new) = opened else {
            return;
        };
        let _ = self.tree.focus(new);
        if let Some(browser) = self.browser(new).cloned() {
            browser.update(cx, |browser, cx| browser.focus_address(window, cx));
        }
        cx.emit(PaneGridEvent::TitleChanged);
        cx.notify();
    }

    /// Lays the grid out where the window shows its grids, `shown`, as of
    /// the last frame, before the grid changes its panes. Without a frame
    /// the grid keeps the place it has.
    fn go_by(&mut self, shown: Bounds<Pixels>) {
        if shown.size.width > px(0.) && shown.size.height > px(0.) {
            self.bounds = shown;
        }
    }

    fn focus_neighbor(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_zoomed = self.tree.zoomed().is_some();
        if self
            .tree
            .focus_neighbor(direction, self.tab_size())
            .is_some()
        {
            if was_zoomed {
                self.fit_panes(cx);
            }
            self.focus_terminal(window, cx);
        }
    }

    fn resize_focused(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if self.tree.zoomed().is_some() {
            return;
        }
        let tab = self.tab_size();
        if self
            .tree
            .resize_towards(self.tree.focused(), direction, RESIZE_STEP, tab)
            .is_ok()
        {
            self.fit_panes(cx);
            cx.notify();
        }
    }

    fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        let before = self.tree.zoomed();
        self.tree.toggle_zoom();
        if self.tree.zoomed() != before {
            self.drag = None;
            self.fit_panes(cx);
            cx.notify();
        }
    }

    /// Closes a pane. The last pane is not taken out; the grid tells its
    /// tab that it is empty instead.
    ///
    /// The panes left are laid out as a split lays them out: where the
    /// window shows its grids now, and their terminals are told their new
    /// sizes, as no frame will for a grid that is not on view.
    fn close(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.panes() == [id] {
            cx.emit(PaneGridEvent::Emptied);
            return;
        }
        let was_focused = self.tree.focused() == id;
        // A grid in a tab that is not shown leaves the keyboard alone.
        let had_focus = self.has_focus(window, cx);
        self.go_by(crate::pane_area::pane_area(window, cx).unwrap_or_default());
        if self.tree.close(id, self.tab_size()).is_err() {
            return;
        }
        self.panes.remove(&id);
        self.marked.remove(&id);
        self.fit_panes(cx);
        if self.drag.is_some_and(|(pane, _)| pane == id) {
            self.drag = None;
        }
        if was_focused && had_focus {
            self.focus_terminal(window, cx);
        } else if was_focused {
            cx.emit(PaneGridEvent::TitleChanged);
            cx.notify();
        } else {
            cx.notify();
        }
    }

    /// Moves the dragged border to `position`, in window coordinates.
    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((pane, side)) = self.drag else {
            return;
        };
        let tab = self.tab_size();
        let Some(border) = self
            .tree
            .borders(tab)
            .into_iter()
            .find(|border| border.pane == pane && border.side == side)
        else {
            self.drag = None;
            return;
        };
        let local = position - self.bounds.origin;
        let (raw, extent, current) = match side {
            Direction::Right => (
                f32::from(local.x),
                tab.width as f32,
                i64::from(border.line.x),
            ),
            _ => (
                f32::from(local.y),
                tab.height as f32,
                i64::from(border.line.y),
            ),
        };
        let target = snap_position(raw, extent).round() as i64;
        let delta = (target - current).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        if delta != 0 && self.tree.resize(pane, side, delta, tab).is_ok() {
            cx.notify();
        }
    }

    /// Formats the split ratio badge while dragging a border (e.g. `"50% | 50%"`).
    fn drag_ratio_label(&self) -> Option<String> {
        let (pane, side) = self.drag?;
        let tab = self.tab_size();
        let border = self
            .tree
            .borders(tab)
            .into_iter()
            .find(|border| border.pane == pane && border.side == side)?;
        let (pos, extent) = match side {
            Direction::Right => (border.line.x as f32, tab.width as f32),
            _ => (border.line.y as f32, tab.height as f32),
        };
        if extent <= 0. {
            return None;
        }
        let first = ((pos / extent) * 100.).round().clamp(1., 99.) as u32;
        Some(format!("{first}% | {}%", 100 - first))
    }
}

/// Snaps a dragged border position to 33%, 50%, or 67% of `extent` when within 8px.
fn snap_position(pos: f32, extent: f32) -> f32 {
    if extent <= 0. {
        return pos;
    }
    const SNAP_PX: f32 = 8.;
    for fraction in [1.0 / 3.0, 0.5, 2.0 / 3.0] {
        let anchor = extent * fraction;
        if (pos - anchor).abs() <= SNAP_PX {
            return anchor;
        }
    }
    pos
}

impl EventEmitter<PaneGridEvent> for PaneGrid {}

fn tab_size(bounds: Bounds<Pixels>) -> TabSize {
    let whole = |length: Pixels| f32::from(length).max(0.).floor() as u32;
    TabSize::new(whole(bounds.size.width), whole(bounds.size.height))
}

fn pane_bounds(tree: &PaneTree, bounds: Bounds<Pixels>) -> Vec<(PaneId, Bounds<Pixels>)> {
    tree.layout(tab_size(bounds))
        .into_iter()
        .map(|(id, rect)| {
            let origin = bounds.origin + point(px(rect.x as f32), px(rect.y as f32));
            (
                id,
                Bounds::new(origin, size(px(rect.width as f32), px(rect.height as f32))),
            )
        })
        .collect()
}

/// Where a grid with one pane puts it when the grid is laid out in
/// `area`: the place of the first pane of a tab, and of a terminal that
/// has the pane area to itself. The area goes through the tree as in a
/// frame, which lays the panes out in whole pixels.
pub(crate) fn lone_pane_bounds(area: Bounds<Pixels>) -> Bounds<Pixels> {
    pane_bounds(&PaneTree::new(MIN_PANE), area)
        .first()
        .map_or(area, |(_, place)| *place)
}

/// The line a border is drawn as, and the area that grabs it.
fn border_bounds(border: &PaneBorder, origin: Point<Pixels>) -> (Bounds<Pixels>, Bounds<Pixels>) {
    let line = border.line;
    let start = origin + point(px(line.x as f32), px(line.y as f32));
    match border.side {
        Direction::Right => (
            Bounds::new(start, size(LINE, px(line.height as f32))),
            Bounds::new(
                start - point(GRAB, px(0.)),
                size(GRAB * 2., px(line.height as f32)),
            ),
        ),
        _ => (
            Bounds::new(start, size(px(line.width as f32), LINE)),
            Bounds::new(
                start - point(px(0.), GRAB),
                size(px(line.width as f32), GRAB * 2.),
            ),
        ),
    }
}

impl Render for PaneGrid {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panes = self
            .tree
            .panes()
            .into_iter()
            // A pane the peek shows is drawn there, and not here too.
            .filter(|id| Some(*id) != self.hidden)
            .filter_map(|id| Some((id, self.panes.get(&id)?.view())))
            .collect();
        let ratio_badge = self.drag_ratio_label().map(|label| {
            let palette = &self.theme.command_palette;
            div()
                .absolute()
                .top(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .relative()
                        .overflow_hidden()
                        .px(px(10.))
                        .py(px(3.))
                        .rounded_full()
                        .bg(hsla(palette.background))
                        .border_1()
                        .border_color(hsla(palette.accent))
                        .shadow_md()
                        .font_family(palette.font_family.clone())
                        .text_size(px(crate::theme::small_text(palette.font_size)))
                        .text_color(hsla(palette.text))
                        .child(crate::design::top_highlight_bar(palette.background))
                        .child(label),
                )
        });
        div()
            .relative()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                this.split(SplitDirection::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                this.split(SplitDirection::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusLeft, window, cx| {
                this.focus_neighbor(Direction::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusRight, window, cx| {
                this.focus_neighbor(Direction::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.focus_neighbor(Direction::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.focus_neighbor(Direction::Down, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ResizeLeft, _, cx| this.resize_focused(Direction::Left, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ResizeRight, _, cx| {
                    this.resize_focused(Direction::Right, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ResizeUp, _, cx| this.resize_focused(Direction::Up, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ResizeDown, _, cx| this.resize_focused(Direction::Down, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.toggle_zoom(cx)))
            .on_action(cx.listener(|this, _: &OpenBrowser, window, cx| {
                this.open_browser_beside_focused(window, cx)
            }))
            .child(GridElement {
                grid: cx.entity(),
                tree: self.tree.clone(),
                panes,
                dragging: self.drag.map(|(_, side)| side),
                marked: self.marked.clone(),
                colors: self.theme.panes.clone(),
            })
            .children(ratio_badge)
    }
}

/// Lays out and paints the panes and borders of a [`PaneGrid`].
struct GridElement {
    grid: Entity<PaneGrid>,
    /// The tree as of this frame.
    tree: PaneTree,
    panes: Vec<(PaneId, AnyView)>,
    /// The side of the border being dragged.
    dragging: Option<Direction>,
    /// The panes outlined in the attention color.
    marked: HashSet<PaneId>,
    /// The colors of the borders and the focus outline in this frame.
    colors: PaneTheme,
}

struct GridPrepaint {
    /// Each pane with its bounds, its element and the hitbox that tells
    /// whether a click is on it and not on what floats over it.
    panes: Vec<(PaneId, Bounds<Pixels>, gpui::AnyElement, Hitbox)>,
    borders: Vec<(PaneBorder, Bounds<Pixels>, Hitbox)>,
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = GridPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> GridPrepaint {
        self.grid.update(cx, |grid, _| grid.bounds = bounds);
        crate::pane_area::measured(bounds, window, cx);
        let placed = pane_bounds(&self.tree, bounds);
        let mut panes = Vec::with_capacity(placed.len());
        for (id, pane_bounds) in placed {
            let Some((_, view)) = self.panes.iter().find(|(pane, _)| *pane == id) else {
                continue;
            };
            // Under the pane's own hitboxes, which do not block it.
            let hitbox = window.insert_hitbox(pane_bounds, HitboxBehavior::Normal);
            let mut element = view.clone().into_any_element();
            let space = size(
                AvailableSpace::Definite(pane_bounds.size.width),
                AvailableSpace::Definite(pane_bounds.size.height),
            );
            element.prepaint_as_root(pane_bounds.origin, space, window, cx);
            panes.push((id, pane_bounds, element, hitbox));
        }
        // After the panes, so that a border is grabbed before the pane
        // under it.
        let borders = self
            .tree
            .borders(tab_size(bounds))
            .into_iter()
            .map(|border| {
                let (line, grab) = border_bounds(&border, bounds.origin);
                let hitbox = window.insert_hitbox(grab, HitboxBehavior::Normal);
                (border, line, hitbox)
            })
            .collect();
        GridPrepaint { panes, borders }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut GridPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focused = self.tree.focused();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (_, _, element, _) in &mut prepaint.panes {
                element.paint(window, cx);
            }
            for (border, line, hitbox) in &prepaint.borders {
                window.paint_quad(fill(*line, hsla(self.colors.divider)));
                window.set_cursor_style(resize_cursor(border.side), hitbox);
            }
            if prepaint.panes.len() > 1
                && let Some((_, pane_bounds, _, _)) =
                    prepaint.panes.iter().find(|(id, _, _, _)| *id == focused)
            {
                let ring = outline(
                    *pane_bounds,
                    hsla(self.colors.focus_outline),
                    BorderStyle::Solid,
                );
                window.paint_quad(ring.border_widths(LINE));
            }
            // Over the focus outline: a pane that needs the user says so
            // even while it has the focus in a window in the back.
            for (_, pane_bounds, _, _) in prepaint
                .panes
                .iter()
                .filter(|(id, _, _, _)| self.marked.contains(id))
            {
                let ring = outline(
                    *pane_bounds,
                    hsla(self.colors.attention),
                    BorderStyle::Solid,
                );
                window.paint_quad(ring.border_widths(LINE));
            }
        });
        if let Some(side) = self.dragging {
            window.set_window_cursor_style(resize_cursor(side));
        }

        let grabs: Vec<_> = prepaint
            .borders
            .iter()
            .map(|(border, _, hitbox)| (border.pane, border.side, hitbox.clone()))
            .collect();
        let panes: Vec<_> = prepaint
            .panes
            .iter()
            .map(|(id, _, _, hitbox)| (*id, hitbox.clone()))
            .collect();
        let grid = self.grid.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if let Some((pane, side, _)) = grabs.iter().find(|(_, _, hit)| hit.is_hovered(window)) {
                let (pane, side) = (*pane, *side);
                grid.update(cx, |grid, cx| {
                    grid.drag = Some((pane, side));
                    cx.notify();
                });
                cx.stop_propagation();
            } else if let Some((id, _)) = panes.iter().find(|(_, hit)| hit.is_hovered(window)) {
                // A click on what floats over the pane, such as the form
                // for a new task, is not one on the pane.
                let id = *id;
                grid.update(cx, |grid, cx| grid.focus(id, window, cx));
            }
        });
        let grid = self.grid.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || grid.read(cx).drag.is_none() {
                return;
            }
            grid.update(cx, |grid, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    grid.drag_to(event.position, cx);
                } else {
                    // The button was let go where no up event reached the
                    // window.
                    grid.drag = None;
                    cx.notify();
                }
            });
        });
        let grid = self.grid.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && grid.read(cx).drag.is_some()
            {
                grid.update(cx, |grid, cx| {
                    grid.drag = None;
                    cx.notify();
                });
            }
        });
    }
}

fn resize_cursor(side: Direction) -> CursorStyle {
    match side {
        Direction::Right | Direction::Left => CursorStyle::ResizeLeftRight,
        Direction::Up | Direction::Down => CursorStyle::ResizeUpDown,
    }
}

#[cfg(test)]
mod snap_tests {
    use super::snap_position;

    #[test]
    fn snaps_near_half_and_thirds_and_leaves_other_positions_alone() {
        assert_eq!(snap_position(496., 1000.), 500.);
        assert_eq!(snap_position(505., 1000.), 500.);
        assert!((snap_position(330., 1000.) - 1000. / 3.).abs() < 0.01);
        assert_eq!(snap_position(420., 1000.), 420.);
    }
}
