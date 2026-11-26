//! The view that draws one terminal and passes input to it.
//!
//! Input reaches the program three ways. A key that [`key_bytes`] encodes
//! is sent from the key event. A key that types text is not handled there,
//! so the platform gives it to the input method, and the view sends what
//! the input method commits. A paste is sent as [`paste_bytes`] has it.

use std::io;
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_config::PtyLogSettings;
use benterm_core::{CellWidth, CursorShape, Damage, GridSize, MouseTracking, Rgb, Row, Underline};
use benterm_terminal::{
    CommandEnd, ForegroundProcess, LockMeter, Notification, PtyConfig, Recorder, Screen,
    SyncOutcome, Terminal, TerminalConfig, WindowSize, WriteError,
};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, DispatchPhase,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, ExternalPaths, FocusHandle,
    Font, FontStyle, FontWeight, Global, Hsla, InteractiveElement, IntoElement, KeyBinding,
    KeyDownEvent, Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, Rgba, ScrollEvent, ShapedLine,
    SharedString, Size, StrikethroughStyle, Styled, Subscription, Task, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, canvas, div, fill, font, outline, point, px,
    size,
};

use crate::agent_watch::{AgentListener, AgentStatus, AgentWatch};
use crate::ime::Composition;
use crate::input::mouse::{self, MouseReporter};
use crate::input::{
    Key, KeyNameError, PageScroll, dropped_path_bytes, key_bytes, named_key_bytes, paste_bytes,
    scroll_key,
};
use crate::layout::{BackgroundSpan, GlyphStyle, StyleRun, layout_row};
use crate::links::{Link, PathCandidate, link_at, path_at, resolve_existing_path, resolve_path};
use crate::menu::{Cut, Redo, SelectAll, Undo};
use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::recording::start_recorder;
use crate::redraw_stats::{self, RedrawStats};
use crate::selection::{
    Change, DRAG_SCROLL_INTERVAL, Hit, MAX_CHECKED_LINES, Selection, Unit, drag_scroll_lines,
    fingerprint, hit, selects_on_drag, survives,
};
use crate::terminal_search::{self, FindNext, FindPrevious, OpenSearch, SearchMatch, SearchState};
use crate::theme::{RADIUS_CARD, RADIUS_CONTROL, TerminalTheme, Theme};
use crate::viewport::{Scroll, terminal_size};

actions!(
    terminal,
    [
        Copy,
        /// Types the text on the clipboard into the terminal.
        Paste,
    ]
);

/// The key context of a terminal, which its bindings are limited to.
pub(crate) const KEY_CONTEXT: &str = "Terminal";

/// Binds the terminal keys, which follow Ghostty's macOS defaults.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-v", Paste, context),
        // A terminal has nothing to undo, cut or select all of, as in
        // Ghostty. The keys are bound so that the terminal takes them
        // and sends nothing, and so that the Edit menu shows them.
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-a", SelectAll, context),
    ]);
}

/// Rows kept above the screen.
const SCROLLBACK_LIMIT: usize = 10_000;

/// Thickness of the bar and underline cursors and of the hollow block.
const CURSOR_THICKNESS: Pixels = px(1.5);

/// How much of the foreground color covers selected cells.
const SELECTION_OPACITY: f32 = 0.3;

/// Opacity of non-active search match highlights.
const SEARCH_MATCH_OPACITY: f32 = 0.35;

/// Opacity of the currently selected search match highlight.
const SEARCH_CURRENT_OPACITY: f32 = 0.7;

/// How thick the line under a link under the pointer is.
const LINK_THICKNESS: Pixels = px(1.);

/// How many rows above and below the pointer are read to find the whole
/// of a link that wraps.
const MAX_LINK_ROWS: usize = 16;

/// How long a synchronized update (mode 2026) may defer updating the view's
/// screen copy before the view shows the latest screen anyway.
pub const SYNC_UPDATE_TIMEOUT: Duration = Duration::from_secs(1);

/// A running terminal together with the wakes its worker thread sends.
///
/// It is started before the window exists, so that a failure to start the
/// program is reported without opening a window.
pub struct StartedTerminal {
    terminal: Terminal,
    /// What the terminal shows above the output of its program. The
    /// output is held back until a view takes the terminal.
    history: Vec<u8>,
    /// A command line typed into the program once the history is shown.
    typed_first: Option<String>,
    wakes: UnboundedReceiver<()>,
    wake_pending: Arc<AtomicBool>,
    /// The size the terminal started with.
    size: WindowSize,
    agent_listener: Option<AgentListener>,
    pub(crate) restricted: Option<benterm_session::RestrictedPane>,
}

/// The size a terminal starts with: the one `pty` asks for, with at
/// least the smallest grid.
fn start_size(pty: &PtyConfig) -> (GridSize, WindowSize) {
    let grid = GridSize::new(pty.size.rows, pty.size.cols).at_least_min();
    let size = WindowSize {
        rows: grid.rows,
        cols: grid.cols,
        ..pty.size
    };
    (grid, size)
}

impl StartedTerminal {
    /// Starts `pty` on a terminal that reports the theme's colors. The
    /// terminal has the size `pty` says from the start, so a program that
    /// is not drawn at once still runs at the size of its pane.
    pub fn spawn(pty: PtyConfig, theme: &TerminalTheme) -> io::Result<Self> {
        Self::spawn_logged(pty, theme, None)
    }

    /// Like [`spawn`](Self::spawn), and records the terminal's PTY log
    /// if `settings` enable it. A log that cannot be started is reported
    /// on stderr, and the terminal runs without one.
    pub fn spawn_recorded(
        pty: PtyConfig,
        theme: &TerminalTheme,
        settings: &PtyLogSettings,
        home: Option<&Path>,
    ) -> io::Result<Self> {
        let log = start_recorder(settings, home, start_size(&pty).0).unwrap_or_else(|error| {
            eprintln!("benterm: not recording the PTY log: {error}");
            None
        });
        Self::spawn_logged(pty, theme, log)
    }

    fn spawn_logged(
        pty: PtyConfig,
        theme: &TerminalTheme,
        log: Option<Recorder>,
    ) -> io::Result<Self> {
        let (sender, wakes) = unbounded();
        let wake_pending = Arc::new(AtomicBool::new(false));
        // At most one wake is in the channel, however fast the output arrives.
        let waker = {
            let wake_pending = Arc::clone(&wake_pending);
            move || {
                if !wake_pending.swap(true, Ordering::AcqRel) {
                    let _ = sender.unbounded_send(());
                }
            }
        };
        let (grid, size) = start_size(&pty);
        let config = TerminalConfig {
            pty,
            size: grid,
            scrollback_limit: SCROLLBACK_LIMIT,
            colors: Some(theme.reported_colors()),
            log,
        };
        Ok(Self {
            terminal: Terminal::spawn_held(config, waker)?,
            history: Vec::new(),
            typed_first: None,
            wakes,
            wake_pending,
            size,
            agent_listener: None,
            restricted: None,
        })
    }

    /// The process of the program the terminal started. `None` once it
    /// has exited.
    pub fn process_id(&self) -> Option<u32> {
        self.terminal.process_id()
    }

    /// Has `listener` told whenever the agent of the terminal's pane or
    /// its state changes, and when the pane is gone.
    pub fn tell_agent_news(mut self, listener: AgentListener) -> Self {
        self.agent_listener = Some(listener);
        self
    }
}

impl StartedTerminal {
    /// Makes the terminal show `history`, what the terminal of the same
    /// pane showed before a restart, above the output of its program.
    /// See [`Terminal::release`] for what is taken as history.
    pub fn show_before(&mut self, history: Vec<u8>) {
        self.history = history;
    }

    /// Types `command` and Enter into the program, after the history is
    /// shown, as if the user had typed it. A shell that has not started
    /// to read yet finds it waiting.
    pub fn type_first(&mut self, command: String) {
        self.typed_first = Some(command);
    }
}

/// What a [`TerminalView`] tells the view that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The program changed the title; read it from
    /// [`TerminalView::screen`].
    TitleChanged,
    /// The program asked for a notification.
    Notified(Notification),
    /// The shell told that a command ended. See
    /// [`TerminalView::last_command_end`].
    CommandFinished(CommandEnd),
    Exited,
    /// The agent the terminal was started for has ended, and the shell
    /// it gave way to runs in the terminal now. See
    /// [`TerminalView::give_way_to`].
    GaveWayToShell,
    /// The user command-clicked a link to this web page.
    OpenUrl(String),
    /// The user command-clicked a link to this existing file or folder.
    OpenPath(PathBuf),
    /// A right click that does not go to the program asked for a menu at
    /// this point in the window. A program that asks for mouse reports
    /// gets the right click, unless Shift is held.
    MenuRequested(Point<Pixels>),
}

/// Draws a terminal and sends it keys.
pub struct TerminalView {
    pub(crate) restricted: Option<benterm_session::RestrictedPane>,
    terminal: Terminal,
    /// The size the terminal was last given, at its start or by
    /// [`fit`](Self::fit). The program may have set another since then. The view
    /// does not set its own again for that, or the two would take turns.
    fitted: WindowSize,
    /// This thread's copy of the terminal's screen.
    screen: Screen,
    /// What was shaped for each screen row, until the row changes.
    row_cache: Vec<Option<Rc<ShapedRow>>>,
    scroll: Scroll,
    theme: Theme,
    metrics: Metrics,
    focus: FocusHandle,
    /// What the input method is composing. It is drawn at the cursor and
    /// never sent.
    composition: Composition,
    /// Where the cursor's cell was in the last frame, in window
    /// coordinates, if it was on view. The input method puts its
    /// candidate window there.
    cursor_cell: Option<Bounds<Pixels>>,
    /// The title as of the last [`TerminalEvent::TitleChanged`].
    reported_title: Option<String>,
    /// Whether [`TerminalEvent::Exited`] has been sent.
    reported_exit: bool,
    selection: Option<Selection>,
    /// The [`fingerprint`] of the selected lines when the selection last
    /// changed, unless it has more lines than are checked.
    selected_lines: Option<u64>,
    /// The left button went down here and has not come up; moving the
    /// pointer moves the end of the selection.
    selecting: bool,
    /// Where the pointer was last seen while selecting.
    pointer: Point<Pixels>,
    /// Repeats scrolling while a selection drag is held above or below the
    /// pane.
    drag_scroll_timer: Option<Task<()>>,
    /// Set when an auto-scroll tick notifies for a redraw and cleared in
    /// [`Render::render`], so a hidden pane stops scrolling.
    awaiting_drag_scroll_frame: bool,
    /// The link under the pointer while command is held, which a click
    /// opens.
    hovered_link: Option<Link>,
    /// The verified file or folder path for `hovered_link`, when it refers
    /// to a local path rather than a web address.
    hovered_path: Option<PathBuf>,
    hovered_path_candidate: Option<PathCandidate>,
    /// The path candidate currently being verified off the UI thread.
    pending_path_probe: Option<PathCandidate>,
    /// Monotonic generation to discard stale background path checks.
    path_probe_gen: u64,
    /// Bounds of the pane in the last frame, in window coordinates.
    pane_bounds: Option<Bounds<Pixels>>,
    /// Where the top left cell of the grid was in the last frame when the
    /// view is not scrolled, in window coordinates.
    grid_origin: Point<Pixels>,
    /// Turns the pointer into reports for a program that asked for them.
    mouse: MouseReporter,
    /// The button whose press went to the program and has not come up.
    /// Its moves and its release go to the program too, even outside the
    /// pane.
    reported_button: Option<mouse::Button>,
    /// Which agent runs here and what it is doing.
    agent: AgentWatch,
    /// Looks at the screen again when a state it shows is due; see
    /// [`watch_agent`](Self::watch_agent).
    agent_timer: Option<Task<()>>,
    /// How many letters the user has typed or pasted here and neither
    /// sent off with Enter, given up with control-C, nor erased since:
    /// more than none, and there may be a draft at the program's prompt.
    draft: usize,
    /// When the program in front was last read.
    front_read: Option<Instant>,
    /// The terminal changed since the program in front was last read.
    front_stale: bool,
    /// Reads the program in front again; see
    /// [`schedule_front_look`](Self::schedule_front_look).
    front_timer: Option<Task<()>>,
    /// The shell the agent of the terminal gives way to, until it has.
    shell_after: Option<ShellAfter>,
    /// When the view started watching this terminal, for detecting
    /// immediate non-zero exits.
    started_at: Instant,
    /// Counts the wakes, when the app counts redraws.
    counting: Option<WakeCounting>,
    /// The in-pane search bar, while open.
    search: Option<OpenSearchBar>,
    /// When the current synchronized update (mode 2026) first deferred a sync.
    sync_update_started: Option<Instant>,
    /// Whether the last sync was deferred by an active synchronized update
    /// (mode 2026), meaning `self.screen` may be behind the core.
    sync_deferred: bool,
    /// Forces a sync once [`SYNC_UPDATE_TIMEOUT`] has elapsed since
    /// `sync_update_started`.
    sync_update_timer: Option<Task<()>>,
}

/// The open in-pane search bar: its input field, match state, and event
/// subscriptions.
struct OpenSearchBar {
    editor: Entity<NameEditor>,
    state: SearchState,
    _subscriptions: [Subscription; 2],
}

/// What a view counts of its wakes; see [`redraw_stats`].
struct WakeCounting {
    stats: Rc<RedrawStats>,
    /// Counts the locks the view takes of the terminal's state.
    locks: LockMeter,
    /// The frame the view was last drawn in, if it has been drawn.
    drawn_in: Option<u64>,
}

/// The shell an agent gives way to; see [`TerminalView::give_way_to`].
struct ShellAfter {
    /// The arguments the shell is started with, `argv[0]` first.
    argv: Vec<std::ffi::OsString>,
    /// The last conversation the agent's hooks told. Kept here because
    /// the agent watch forgets it when the session ends, which Claude
    /// Code tells right before it exits.
    conversation: Option<benterm_session::Conversation>,
}

/// How often the program in front of a terminal is read at most, and how
/// long a pane with an agent goes without reading it. An agent that ends
/// is noticed within this time even if nothing is printed after it.
///
/// A reading takes about 25 µs of the UI thread. Read on every change of
/// a busy terminal, ten panes would take a few percent of it; at this
/// pace they take about a thousandth.
const FRONT_INTERVAL: Duration = Duration::from_millis(250);

impl EventEmitter<TerminalEvent> for TerminalView {}

/// The font and the size of a cell.
struct Metrics {
    font: Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
}

impl Metrics {
    /// The size of a terminal in a pane of `pane` pixels with `padding`.
    fn terminal_size(&self, pane: Size<Pixels>, padding: f32) -> WindowSize {
        terminal_size(
            (pane.width.into(), pane.height.into()),
            padding,
            (self.cell_width.into(), self.line_height.into()),
        )
    }

    fn new(theme: &TerminalTheme, cx: &App) -> Self {
        let text_system = cx.text_system();
        let family = theme.installed_font_family(&text_system.all_font_names());
        let font = font(SharedString::from(family.to_owned()));
        let font_size = px(theme.font_size);
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(px(0.), |advance| advance.width);
        Self {
            font,
            font_size,
            // At least a pixel, so that a font without metrics (as in
            // tests) cannot ask for an endless grid.
            cell_width: cell_width.max(px(1.)),
            line_height: px(theme.row_height()),
        }
    }
}

/// A row ready to paint.
struct ShapedRow {
    backgrounds: Vec<BackgroundSpan>,
    /// Shaped text and the column it starts at.
    segments: Vec<(u16, ShapedLine)>,
}

/// Everything one frame paints, computed in the prepaint phase.
struct Frame {
    clip: Bounds<Pixels>,
    /// Where the top left cell of the first row goes.
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    /// The rows, top to bottom.
    rows: Vec<Rc<ShapedRow>>,
    selection: Option<SelectionPaint>,
    search_matches: Option<SelectionPaint>,
    current_search_match: Option<SelectionPaint>,
    /// The line under the link under the pointer.
    link: Option<SelectionPaint>,
    cursor: Option<CursorPaint>,
    composition: Option<CompositionPaint>,
}

/// Rectangles filled with one color: the selected cells, covered with a
/// see-through color, or the line under a link.
struct SelectionPaint {
    rects: Vec<Bounds<Pixels>>,
    color: Hsla,
}

/// The text being composed, drawn over the cells from the cursor on.
struct CompositionPaint {
    /// The cells the text covers.
    bounds: Bounds<Pixels>,
    /// Shaped text and the column it starts at, counted from the cursor.
    segments: Vec<(usize, ShapedLine)>,
    background: Hsla,
    /// The color of the underline that marks the text as not yet sent.
    underline: Hsla,
    /// The color of the bar at the end of the text.
    caret: Hsla,
}

struct CursorPaint {
    /// The cell the cursor is on.
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    /// A block cursor is hollow while the window is not active.
    active: bool,
    color: Hsla,
    /// The character under a filled block cursor, in the background color.
    text: Option<ShapedLine>,
}

pub(crate) fn hsla(color: Rgb) -> Hsla {
    Rgba {
        r: f32::from(color.0) / 255.,
        g: f32::from(color.1) / 255.,
        b: f32::from(color.2) / 255.,
        a: 1.,
    }
    .into()
}

#[derive(Default)]
struct TerminalReapers {
    quitting: bool,
    pending: Vec<mpsc::Receiver<()>>,
}

impl Global for TerminalReapers {}

fn install_terminal_reapers(cx: &mut App) {
    if !cx.has_global::<TerminalReapers>() {
        cx.default_global::<TerminalReapers>();
        cx.on_app_quit(|cx| {
            let reapers = cx.global_mut::<TerminalReapers>();
            reapers.quitting = true;
            // Closures already in flight run together. Give them the PTY
            // grace period plus a short window for its threads to finish.
            let deadline = Instant::now() + Duration::from_millis(2500);
            for done in reapers.pending.drain(..) {
                let _ = done.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            }
            std::future::ready(())
        })
        .detach();
    }
}

fn reap_terminal(terminal: &mut Terminal, cx: &mut App) {
    let reapers = cx.global_mut::<TerminalReapers>();
    // On application exit preserve the synchronous cleanup of still-open
    // terminals. The interactive close path below never waits for a PTY.
    if reapers.quitting {
        return;
    }
    reapers
        .pending
        .retain(|done| matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let (send_job, job) = mpsc::sync_channel::<Box<dyn FnOnce() + Send>>(1);
    let (done, completed) = mpsc::channel();
    if std::thread::Builder::new()
        .name("benterm-terminal-reaper".into())
        .spawn(move || {
            if let Ok(job) = job.recv() {
                job();
            }
            let _ = done.send(());
        })
        .is_ok()
    {
        let job = Box::new(terminal.take_shutdown());
        if let Err(unsent) = send_job.send(job) {
            (unsent.0)();
        }
        reapers.pending.push(completed);
    }
}

impl TerminalView {
    pub fn new(
        started: StartedTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        install_terminal_reapers(cx);
        cx.on_release(|view, cx| {
            view.stop_selecting();
            reap_terminal(&mut view.terminal, cx);
        })
        .detach();
        let StartedTerminal {
            mut terminal,
            history,
            typed_first,
            mut wakes,
            wake_pending,
            size: fitted,
            agent_listener,
            restricted,
        } = started;
        let counting = redraw_stats::installed(cx).map(|stats| {
            let locks = LockMeter::new();
            terminal.meter_locks(locks.clone());
            WakeCounting {
                stats,
                locks,
                drawn_in: None,
            }
        });
        // From here on the output of the program is fed, below what the
        // terminal is to show first.
        terminal.release(history);
        if let Some(command) = typed_first {
            let _ = terminal.write(format!("{command}\r").into_bytes());
        }

        // The flag is cleared before the state
        // is read, so a change made after the read sends a new wake. The
        // channel closes when the terminal, and with it the worker, ends.
        cx.spawn_in(window, async move |this, cx| {
            while wakes.next().await.is_some() {
                wake_pending.store(false, Ordering::Release);
                let woken = this.update_in(cx, |this, window, cx| this.woken(window, cx));
                if woken.is_err() {
                    break;
                }
            }
        })
        .detach();
        // The cursor is drawn hollow while the window is not active or
        // another pane has the focus.
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.stop_selecting();
            }
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        cx.on_focus_in(&focus, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_focus_out(&focus, window, |this, _, _, cx| {
            // The input method goes with the focus, and would never
            // finish what it was composing here.
            this.composition.clear();
            this.stop_selecting();
            cx.notify();
        })
        .detach();

        let metrics = Metrics::new(&theme.terminal, cx);
        Self {
            restricted,
            terminal,
            fitted,
            screen: Screen::default(),
            row_cache: Vec::new(),
            scroll: Scroll::default(),
            theme,
            metrics,
            focus,
            composition: Composition::default(),
            cursor_cell: None,
            reported_title: None,
            reported_exit: false,
            selection: None,
            selected_lines: None,
            selecting: false,
            pointer: Point::default(),
            drag_scroll_timer: None,
            awaiting_drag_scroll_frame: false,
            hovered_link: None,
            hovered_path: None,
            hovered_path_candidate: None,
            pending_path_probe: None,
            path_probe_gen: 0,
            pane_bounds: None,
            grid_origin: Point::default(),
            mouse: MouseReporter::default(),
            reported_button: None,
            agent: AgentWatch::new(agent_listener),
            agent_timer: None,
            draft: 0,
            front_read: None,
            front_stale: false,
            front_timer: None,
            shell_after: None,
            started_at: cx.background_executor().now(),
            counting,
            search: None,
            sync_update_started: None,
            sync_deferred: false,
            sync_update_timer: None,
        }
    }

    /// Returns the startup failure details if the terminal's program has
    /// exited with a non-zero code within [`crate::toast::STARTUP_FAILURE_WINDOW`]
    /// (or with 127, command not found).
    pub(crate) fn startup_failure(&self, now: Instant) -> Option<crate::toast::StartupFailure> {
        let exit_code = self.screen.exit.and_then(|status| status.code());
        let elapsed = now.saturating_duration_since(self.started_at);
        if !crate::toast::is_startup_failure(exit_code, elapsed) {
            return None;
        }
        let output = self.text(Some(100));
        crate::toast::StartupFailure::from_exit(exit_code, elapsed, &output)
    }

    /// Tells the view that its program runs an agent and then gives way
    /// to the shell started with `argv`, in the same process. Once that
    /// shell is the program in front, the view types the command that
    /// takes up the agent's last conversation, without Enter, and tells
    /// [`TerminalEvent::GaveWayToShell`].
    pub(crate) fn give_way_to(&mut self, argv: Vec<std::ffi::OsString>) {
        self.shell_after = Some(ShellAfter {
            argv,
            conversation: None,
        });
    }

    /// Keeps the conversation of the agent that gives way to a shell.
    fn keep_conversation(&mut self) {
        if let Some(after) = &mut self.shell_after
            && let Some(conversation) = self.agent.conversation()
        {
            after.conversation = Some(conversation);
        }
    }

    /// The link under the pointer while command is held, which a click
    /// opens.
    pub fn hovered_link(&self) -> Option<&Link> {
        self.hovered_link.as_ref()
    }

    /// The verified local path under the pointer while command is held, if
    /// the hovered link is a file or folder.
    pub fn hovered_path(&self) -> Option<&Path> {
        self.hovered_path.as_deref()
    }

    /// The theme the view draws with.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Draws with `theme` from the next frame on. A change of font or
    /// padding also changes the grid, which the next frame fits to the
    /// pane.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            let search_theme = theme.search.clone();
            search
                .editor
                .update(cx, |editor, cx| editor.set_theme(search_theme, cx));
        }
        if theme.terminal == self.theme.terminal {
            self.theme = theme;
            cx.notify();
            return;
        }
        let metrics = Metrics::new(&theme.terminal, cx);
        if metrics.line_height != self.metrics.line_height {
            // The scroll offset is in pixels of the old row height.
            self.scroll = Scroll::default();
        }
        self.metrics = metrics;
        self.terminal
            .set_reported_colors(theme.terminal.reported_colors());
        // Every shaped row has the old colors or font.
        self.row_cache.iter_mut().for_each(|row| *row = None);
        self.theme = theme;
        cx.notify();
    }

    /// The directory of the program the terminal started. See
    /// [`Terminal::working_directory`].
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.terminal.working_directory()
    }

    /// The directory of the program in front. See
    /// [`Terminal::foreground_working_directory`].
    pub(crate) fn foreground_working_directory(&self) -> Option<PathBuf> {
        self.terminal.foreground_working_directory()
    }

    /// What the terminal shows, to be shown again after a restart. See
    /// [`Terminal::history`].
    pub fn history(&self, limit: usize) -> Option<Vec<Row>> {
        self.terminal.history(limit)
    }

    /// A number that goes up whenever [`history`](Self::history) may
    /// have changed.
    pub fn changes(&self) -> u64 {
        self.terminal.changes()
    }

    /// Where a terminal opened beside this one starts. See
    /// [`Terminal::directory_for_new_terminal`].
    pub fn directory_for_new_terminal(&self) -> Option<PathBuf> {
        self.terminal.directory_for_new_terminal()
    }

    /// The handle that receives key input.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The screen as of the last wake or frame.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The text the input method is composing, which is shown at the
    /// cursor and has not been sent.
    pub fn marked_text(&self) -> Option<&str> {
        self.composition.marked()
    }

    /// The size a terminal drawn with `theme` has in a pane of `pane`
    /// pixels. A new terminal is started with it, so that its program
    /// never sees another size.
    pub(crate) fn size_in(pane: Size<Pixels>, theme: &TerminalTheme, cx: &App) -> WindowSize {
        Metrics::new(theme, cx).terminal_size(pane, theme.padding)
    }

    /// Gives the terminal the size it has in a pane of `pane` pixels, and
    /// tells the program if that is not the size it was last given. Every
    /// frame fits the terminal to the pane it is drawn in; the grid of a
    /// pane that is not on view calls this when it gives the pane another
    /// place.
    pub(crate) fn fit(&mut self, pane: Size<Pixels>) {
        let size = self
            .metrics
            .terminal_size(pane, self.theme.terminal.padding);
        if size != self.fitted {
            self.fitted = size;
            self.terminal.resize(
                GridSize::new(size.rows, size.cols),
                (size.pixel_width, size.pixel_height),
            );
        }
    }

    /// The size of a place in which the terminal has the rows and the
    /// columns it was last given, for one who draws it without changing
    /// them. Half a cell is added each way, so that rounding cannot cost
    /// a row or a column.
    pub(crate) fn fitted_place(&self) -> Size<Pixels> {
        let padding = px(2. * self.theme.terminal.padding);
        let cells = |count: u16, cell: Pixels| cell * (f32::from(count) + 0.5) + padding;
        size(
            cells(self.fitted.cols, self.metrics.cell_width),
            cells(self.fitted.rows, self.metrics.line_height),
        )
    }

    pub fn cell_size(&self) -> Size<Pixels> {
        size(self.metrics.cell_width, self.metrics.line_height)
    }

    /// Which agent runs in the terminal and what it is doing, judged from
    /// the screen and from the hook events reported for the pane. A view
    /// that observes this one is notified when it changes.
    pub fn agent_status(&self) -> AgentStatus {
        self.agent.status()
    }

    /// How many times a turn began, asked the user, or stopped by an
    /// error, also when OSC 7501 work ended before the status showed
    /// it. A view that observes this one is notified when it changes.
    pub(crate) fn agent_turns_begun(&self) -> u64 {
        self.agent.turns_begun()
    }

    /// Shows the agent watch what the terminal already holds of its
    /// program, so a caller about to send input or weigh the agent's
    /// state sees reports that arrived before the next wake.
    pub(crate) fn observe_program(&mut self, cx: &mut Context<Self>) {
        if self.agent.observe_program(self.terminal.program_status()) {
            cx.notify();
        }
    }

    /// The end of the last command, as the shell told it. `None` before
    /// the first, and with a shell that does not tell. Benterm's shell
    /// integration for zsh and bash tells it.
    pub fn last_command_end(&self) -> Option<CommandEnd> {
        self.terminal.last_command_end()
    }

    /// The conversation of the agent in front, as its hooks reported it,
    /// to take up again after a restart.
    pub fn conversation(&self) -> Option<benterm_session::Conversation> {
        self.agent.conversation()
    }

    /// What the agent in front means to do, as it keeps it with its own
    /// tool: `None` for an agent whose hooks told no plan.
    pub fn agent_plan(&self) -> Option<benterm_agent::Plan> {
        self.agent.plan().cloned()
    }

    /// What the terminal's program told of itself with OSC 7501 (`msg`),
    /// on one line without control characters, while the pane waits for
    /// the user or has stopped with an error.
    pub fn agent_msg(&self) -> Option<String> {
        self.agent.msg().map(str::to_owned)
    }

    /// Takes an event an agent's hook reported for this pane.
    pub fn report_hook(&mut self, event: &benterm_agent::HookEvent, cx: &mut Context<Self>) {
        let now = Instant::now();
        // The hook is weighed against the program in front as it is now:
        // an agent that has just started sends its first hook at once.
        if self.look_at_front(now, true, cx) {
            cx.notify();
        }
        // What the terminal holds of the program first: work the
        // program told before this hook is older than the hook.
        self.observe_program(cx);
        if self.agent.observe_hook(event, now) {
            cx.notify();
        }
        self.keep_conversation();
        // The screen is weighed against the hook from now on, even if
        // nothing more is printed: a screen that keeps saying otherwise
        // puts a wrong or outdated hook right.
        self.watch_agent(cx);
    }

    /// Shows the agent watch the screen as it is now. This runs for every
    /// change of the terminal, whether the pane is on view or not.
    fn watch_agent(&mut self, cx: &mut Context<Self>) {
        // One reading of the clock for looking and for the timer. After a
        // look at `now` a pending state is always due later than `now`, so
        // the wait below is never nothing and no deadline slips through
        // between the look and the timer.
        let now = Instant::now();
        // The program in front first: it decides whether the screen may
        // name an agent.
        let front_changed = self.look_at_front(now, false, cx);
        // What the program tells of itself comes before the screen.
        let told = self.agent.observe_program(self.terminal.program_status());
        let latest_rows;
        let rows = if self.sync_deferred {
            latest_rows = self.terminal.screen_rows();
            &latest_rows
        } else {
            &self.screen.rows
        };
        if self.agent.observe_rows(rows, now) || front_changed || told {
            cx.notify();
        }
        self.schedule_agent_look(now, cx);
        self.schedule_front_look(now, cx);
    }

    /// Reads the program in front and tells the agent watch, if `force`
    /// or if it was last read [`FRONT_INTERVAL`] ago. Returns whether
    /// the agent's status changed.
    ///
    /// The shell an agent gives way to coming to the front is the end
    /// of the agent: see [`give_way_to`](Self::give_way_to).
    fn look_at_front(&mut self, now: Instant, force: bool, cx: &mut Context<Self>) -> bool {
        let due = self
            .front_read
            .is_none_or(|read| now.saturating_duration_since(read) >= FRONT_INTERVAL);
        if !force && !due {
            self.front_stale = true;
            return false;
        }
        self.front_read = Some(now);
        self.front_stale = false;
        let front = self.terminal.foreground_process();
        self.take_front(front, cx)
    }

    /// Tells the agent watch the program in front as it was just read.
    /// Returns whether the agent's status changed.
    fn take_front(&mut self, front: Option<ForegroundProcess>, cx: &mut Context<Self>) -> bool {
        let shell_came = self
            .shell_after
            .as_ref()
            .is_some_and(|after| front.as_ref().is_some_and(|front| front.args == after.argv));
        let changed = self.agent.observe_front(front);
        if shell_came && let Some(after) = self.shell_after.take() {
            if let Some(conversation) = after.conversation {
                let _ = self
                    .terminal
                    .write(conversation.resume_command().into_bytes());
            }
            cx.emit(TerminalEvent::GaveWayToShell);
        }
        changed
    }

    /// Reads the program in front now, and returns the id of the agent
    /// it is: `claude` or `codex`. `None` for a shell and any other
    /// program that is no agent, and when the program could not be read.
    ///
    /// For input that only an agent may be given, asked for in the
    /// update that writes it. What the agent watch held before is not
    /// enough for that: the program in front changes without the
    /// terminal changing, and an agent the screen alone names may be a
    /// banner an agent left behind. The watch is told what was read, so
    /// the state it tells after this is the state of this process.
    pub(crate) fn agent_in_front(&mut self, cx: &mut Context<Self>) -> Option<&'static str> {
        self.front_read = Some(Instant::now());
        self.front_stale = false;
        let front = self.terminal.foreground_process();
        let pid = front.as_ref().map(|front| front.pid);
        if self.take_front(front, cx) {
            cx.notify();
        }
        let (agent, seen) = self.agent.agent_in_front()?;
        (Some(seen) == pid).then_some(agent)
    }

    /// The program in front can change without the terminal changing,
    /// as when an agent ends and the program after it prints nothing.
    /// So while the pane has an agent, the view reads it again by itself
    /// every [`FRONT_INTERVAL`]. It does too after a change that came
    /// too soon after the last reading to read it again.
    fn schedule_front_look(&mut self, now: Instant, cx: &mut Context<Self>) {
        if !self.front_stale && self.agent.status().agent.is_none() && self.shell_after.is_none() {
            self.front_timer = None;
            return;
        }
        if self.front_timer.is_some() {
            return;
        }
        let due = self.front_read.map_or(now, |read| read + FRONT_INTERVAL);
        let wait = due.saturating_duration_since(now);
        self.front_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.front_timer = None;
                // Due now, so the look below reads it.
                this.front_read = None;
                this.sync(cx);
                this.watch_agent(cx);
            });
        }));
    }

    /// A state the screen shows counts once it has been shown for a
    /// while, and a screen that has stopped changing sends no wake. So
    /// the view looks again by itself when the state is due.
    fn schedule_agent_look(&mut self, now: Instant, cx: &mut Context<Self>) {
        let wait = self
            .agent
            .pending_deadline()
            .map(|deadline| deadline.saturating_duration_since(now))
            // Never the case after a look at `now`; it keeps a timer that
            // fires at once from being set over and over.
            .filter(|wait| !wait.is_zero());
        self.agent_timer = wait.map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    this.sync(cx);
                    this.watch_agent(cx);
                });
            })
        });
    }

    /// The terminal changed: copy the changes and ask for a frame.
    fn woken(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let locks_before = self
            .counting
            .as_ref()
            .map(|counting| counting.locks.count());
        self.sync(cx);
        // Before the agent watch looks at the front again: the shell
        // tells the end of an agent right as it is back in front, and
        // the error comes before the agent is gone.
        self.take_command_ends(cx);
        self.watch_agent(cx);
        self.emit_screen_events(cx);
        for notification in self.terminal.take_notifications() {
            cx.emit(TerminalEvent::Notified(notification));
        }
        if !self.sync_deferred {
            cx.notify();
        }
        if let (Some(counting), Some(before)) = (&self.counting, locks_before) {
            // Drawn in the last frame: the window draws the view again.
            let visible = counting.drawn_in == Some(counting.stats.frames());
            counting
                .stats
                .wake(visible, counting.locks.count() - before);
        }
    }

    /// Emits title and exit events for any changes now reflected on
    /// `self.screen`.
    fn emit_screen_events(&mut self, cx: &mut Context<Self>) {
        if self.reported_title != self.screen.title {
            self.reported_title.clone_from(&self.screen.title);
            cx.emit(TerminalEvent::TitleChanged);
        }
        if self.screen.exit.is_some() && !self.reported_exit {
            self.reported_exit = true;
            cx.emit(TerminalEvent::Exited);
        }
    }

    /// Hands the ends of commands the shell told to the agent watch and
    /// to whoever listens to the view.
    fn take_command_ends(&mut self, cx: &mut Context<Self>) {
        let ends = self.terminal.take_command_ends();
        if ends.is_empty() {
            return;
        }
        let front = self.terminal.foreground_process();
        for end in ends {
            if self.agent.observe_command_end(end, front.clone()) {
                cx.notify();
            }
            cx.emit(TerminalEvent::CommandFinished(end));
        }
    }

    /// Copies what changed in the terminal and forgets what was shaped
    /// for the rows that changed.
    ///
    /// While the program is inside a synchronized update (mode 2026) and
    /// [`SYNC_UPDATE_TIMEOUT`] has not elapsed, leaves `self.screen` as it was
    /// and schedules a timer so the update cannot hold drawing back forever.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.sync_at(now, cx);
    }

    fn sync_at(&mut self, now: Instant, cx: &mut Context<Self>) {
        let force = self
            .sync_update_started
            .is_some_and(|started| now.saturating_duration_since(started) >= SYNC_UPDATE_TIMEOUT);
        let old_scrollback = self.screen.scrollback_len;
        let old_evicted = self.screen.scrollback_evicted;
        let old_size = self.screen.size;
        let old_modes = self.screen.modes;
        let damage = match self.terminal.sync_if_ready(&mut self.screen, force) {
            SyncOutcome::Unchanged => return,
            SyncOutcome::Deferred => {
                self.sync_deferred = true;
                let started = *self.sync_update_started.get_or_insert(now);
                if self.sync_update_timer.is_none() {
                    let wait =
                        SYNC_UPDATE_TIMEOUT.saturating_sub(now.saturating_duration_since(started));
                    self.sync_update_timer = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(wait).await;
                        let _ = this.update(cx, |this, cx| {
                            this.sync_update_timer = None;
                            this.sync(cx);
                            this.emit_screen_events(cx);
                            cx.notify();
                        });
                    }));
                }
                return;
            }
            SyncOutcome::Updated(damage) => {
                self.sync_deferred = false;
                if !self.screen.modes.synchronized_update {
                    self.sync_update_started = None;
                    self.sync_update_timer = None;
                }
                damage
            }
        };
        let evicted = usize::try_from(self.screen.scrollback_evicted.saturating_sub(old_evicted))
            .unwrap_or(usize::MAX);
        if self.screen.modes.alternate_screen != old_modes.alternate_screen
            || self.screen.modes.mouse_tracking != old_modes.mouse_tracking
        {
            self.mouse.reset_wheel();
        }
        match &damage {
            Damage::Full => {
                self.row_cache.clear();
                self.row_cache.resize(self.screen.rows.len(), None);
            }
            Damage::Rows(rows) => {
                for &row in rows {
                    self.row_cache[usize::from(row)] = None;
                }
            }
        }
        self.scroll.scrollback_changed(
            old_scrollback,
            self.screen.scrollback_len,
            evicted,
            self.metrics.line_height.into(),
        );
        if let Some(mut selection) = self.selection {
            let change = Change {
                resized: self.screen.size != old_size,
                old_scrollback,
                new_scrollback: self.screen.scrollback_len,
                evicted,
                damage,
            };
            let then = self.selected_lines;
            if survives(selection.lines(), &change, then, |lines| {
                self.line_rows(lines)
            }) {
                if !change.resized && evicted > 0 {
                    selection.shift_up(evicted);
                    self.selection = Some(selection);
                }
            } else {
                self.selection = None;
                self.selected_lines = None;
                self.stop_selecting();
            }
        }
        if let Some(query) = self
            .search
            .as_ref()
            .filter(|search| !search.state.query().is_empty())
            .map(|search| search.state.query().to_owned())
        {
            self.refresh_search(&query, false);
        }
    }

    /// The rows of `lines`, from the scrollback and the screen.
    fn line_rows(&self, lines: RangeInclusive<usize>) -> Vec<Row> {
        let history = self.screen.scrollback_len;
        let (start, end) = (*lines.start(), lines.end().saturating_add(1));
        let mut rows = if start < history {
            self.terminal.scrollback_rows(start..end.min(history))
        } else {
            Vec::new()
        };
        let screen = &self.screen.rows;
        let on_screen = |line: usize| line.saturating_sub(history).min(screen.len());
        rows.extend_from_slice(&screen[on_screen(start)..on_screen(end)]);
        rows
    }

    /// The selected cells, if any.
    fn selected_span(&self) -> Option<crate::selection::Span> {
        self.selection?
            .span(|line| self.line_rows(line..=line).pop())
    }

    /// The cell under `position`, a point in the window.
    fn hit_at(&self, position: Point<Pixels>) -> Hit {
        let line_height = self.metrics.line_height;
        let rows = self.screen.size.rows;
        let history = self.screen.scrollback_len;
        let visible = self.scroll.visible(line_height.into(), rows, history);
        let offset = position - self.grid_origin;
        hit(
            f32::from(offset.x) / f32::from(self.metrics.cell_width),
            (f32::from(offset.y) + visible.shift) / f32::from(line_height),
            visible.first_line,
            self.screen.size.cols,
            history + usize::from(rows),
        )
    }

    /// Makes `selection` the selection, and remembers what its lines hold.
    fn set_selection(&mut self, selection: Selection) {
        let lines = selection.lines();
        let same_lines = self.selection.is_some_and(|old| old.lines() == lines);
        if !same_lines {
            self.selected_lines = (lines.clone().count() <= MAX_CHECKED_LINES)
                .then(|| fingerprint(&self.line_rows(lines)));
        }
        self.selection = Some(selection);
    }

    /// Where `position`, a point in the window, is on the screen, in
    /// cells. While the view is scrolled back, rows above the screen are
    /// negative.
    fn pointer_at(&self, position: Point<Pixels>) -> mouse::Pointer {
        let line_height = f32::from(self.metrics.line_height);
        let history = self.screen.scrollback_len;
        let visible = self
            .scroll
            .visible(line_height, self.screen.size.rows, history);
        let offset = position - self.grid_origin;
        mouse::Pointer {
            x: f32::from(offset.x) / f32::from(self.metrics.cell_width),
            y: (f32::from(offset.y) + visible.shift) / line_height + visible.first_line as f32
                - history as f32,
        }
    }

    /// The rows of the wrapped logical line under `position`, the first
    /// line index of that slice, and the cell point at `position`.
    fn line_rows_around(
        &self,
        position: Point<Pixels>,
    ) -> Option<(Vec<Row>, usize, crate::selection::Point)> {
        let offset = position - self.grid_origin;
        let width = self.metrics.cell_width * f32::from(self.screen.size.cols);
        let height = self.metrics.line_height * f32::from(self.screen.size.rows);
        if offset.x < px(0.) || offset.y < px(0.) || offset.x >= width || offset.y >= height {
            return None;
        }
        let at = self.hit_at(position).point;
        // The rows around the pointer that may hold the rest of its line
        // of text. An address longer than this many rows is not one that
        // is clicked.
        let first = at.line.saturating_sub(MAX_LINK_ROWS);
        let rows = self.line_rows(first..=at.line + MAX_LINK_ROWS);
        let index = at.line - first;
        let start = rows[..index]
            .iter()
            .rposition(|row| !row.end.wraps())
            .map_or(0, |before| before + 1);
        let end = rows[index..]
            .iter()
            .position(|row| !row.end.wraps())
            .map_or(rows.len(), |last| index + last + 1);
        Some((rows[start..end].to_vec(), first + start, at))
    }

    /// The link under `position`, a point in the window, if the pointer
    /// is over the grid there.
    fn link_under(&self, position: Point<Pixels>) -> Option<Link> {
        let (rows, first_line, at) = self.line_rows_around(position)?;
        link_at(&rows, first_line, at)
    }

    /// The file or folder path candidate under `position`, without
    /// touching the file system.
    fn path_candidate_under(&self, position: Point<Pixels>) -> Option<PathCandidate> {
        let (rows, first_line, at) = self.line_rows_around(position)?;
        path_at(&rows, first_line, at)
    }

    /// The directory against which relative paths on the terminal screen
    /// are resolved: the foreground program's working directory, falling
    /// back to the pane's initial shell directory.
    fn link_base_dir(&self) -> Option<PathBuf> {
        self.foreground_working_directory()
            .or_else(|| self.working_directory())
    }

    /// Where the link under the pointer goes, as it is shown while
    /// command is held: the address a click opens, which for an OSC 8
    /// hyperlink need not be the text on the screen, after its host
    /// (see [`crate::links::shown_target`]).
    pub fn link_target(&self) -> Option<String> {
        let link = self.hovered_link.as_ref()?;
        Some(crate::links::shown_target(&link.url))
    }

    /// The line at the bottom of the pane that shows [`Self::link_target`].
    fn render_link_target(&self) -> Option<impl IntoElement + use<>> {
        let target = self.link_target()?;
        let theme = &self.theme.search;
        Some(
            div()
                .debug_selector(|| "terminal-link-target".into())
                .absolute()
                .left_2()
                .bottom_2()
                .max_w(gpui::relative(0.9))
                .px_2()
                .py_0p5()
                .rounded(px(RADIUS_CONTROL))
                .bg(hsla(theme.background))
                .border_1()
                .border_color(hsla(theme.border))
                .font_family(SharedString::from(theme.font_family.clone()))
                .text_size(px(theme.font_size))
                .text_color(hsla(theme.text))
                .truncate()
                .child(target),
        )
    }

    fn clear_hovered_link(&mut self, cx: &mut Context<Self>) {
        self.path_probe_gen += 1;
        self.pending_path_probe = None;
        self.hovered_path = None;
        self.hovered_path_candidate = None;
        if self.hovered_link.take().is_some() {
            cx.notify();
        }
    }

    /// Marks the link under `position` while command is held, or none.
    /// Web addresses are checked synchronously; local paths are verified
    /// off the UI thread.
    fn hover(&mut self, position: Point<Pixels>, modifiers: Modifiers, cx: &mut Context<Self>) {
        if !modifiers.platform || self.selecting {
            self.clear_hovered_link(cx);
            return;
        }
        if let Some(link) = self.link_under(position) {
            self.path_probe_gen += 1;
            self.pending_path_probe = None;
            self.hovered_path = None;
            self.hovered_path_candidate = None;
            if self.hovered_link.as_ref() != Some(&link) {
                self.hovered_link = Some(link);
                cx.notify();
            }
            return;
        }
        let Some(candidate) = self.path_candidate_under(position) else {
            self.clear_hovered_link(cx);
            return;
        };
        if self.hovered_path_candidate.as_ref() == Some(&candidate) {
            return;
        }
        if self.pending_path_probe.as_ref() == Some(&candidate) {
            return;
        }
        self.clear_hovered_link(cx);
        let base = self.link_base_dir();
        if resolve_path(&candidate.path, base.as_deref()).is_none() {
            return;
        }
        self.pending_path_probe = Some(candidate.clone());
        let generation = self.path_probe_gen;
        let raw = candidate.path.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move { resolve_existing_path(&raw, base.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.path_probe_gen != generation {
                    return;
                }
                this.pending_path_probe = None;
                if let Some(path) = found {
                    this.hovered_link = Some(Link {
                        url: path.to_string_lossy().into_owned(),
                        start: candidate.start,
                        end: candidate.end,
                    });
                    this.hovered_path = Some(path);
                    this.hovered_path_candidate = Some(candidate);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover(window.mouse_position(), event.modifiers, cx);
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A command-click on a link opens it, even for a program that
        // asked for mouse reports: a report never carries command.
        if event.button == MouseButton::Left && event.modifiers.platform {
            if let Some(link) = self.link_under(event.position) {
                cx.emit(TerminalEvent::OpenUrl(link.url));
                return;
            }
            if let Some(candidate) = self.path_candidate_under(event.position) {
                if self.hovered_path_candidate.as_ref() == Some(&candidate)
                    && let Some(path) = self.hovered_path.clone()
                {
                    cx.emit(TerminalEvent::OpenPath(path));
                    return;
                }
                let base = self.link_base_dir();
                if resolve_path(&candidate.path, base.as_deref()).is_some() {
                    let raw = candidate.path;
                    cx.spawn(async move |this, cx| {
                        let found = cx
                            .background_spawn(async move {
                                resolve_existing_path(&raw, base.as_deref())
                            })
                            .await;
                        if let Some(path) = found {
                            let _ = this.update(cx, |_, cx| {
                                cx.emit(TerminalEvent::OpenPath(path));
                            });
                        }
                    })
                    .detach();
                    return;
                }
            }
        }
        if !selects_on_drag(self.screen.modes, event.modifiers.shift) {
            self.report_press(event, cx);
            return;
        }
        if event.button == MouseButton::Right {
            cx.emit(TerminalEvent::MenuRequested(event.position));
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let at = self.hit_at(event.position);
        self.selection = None;
        self.set_selection(Selection::new(at, Unit::for_clicks(event.click_count)));
        self.stop_selecting();
        self.selecting = true;
        self.pointer = event.position;
        cx.notify();
    }

    /// Sends a press to the program, which asked for mouse reports.
    fn report_press(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(button) = report_button(event.button) else {
            return;
        };
        let bytes = self.mouse.press(
            button,
            self.pointer_at(event.position),
            report_mods(&event.modifiers),
            self.screen.modes,
            self.screen.size,
        );
        if let Some(bytes) = bytes {
            self.reported_button = Some(button);
            self.send(bytes, cx);
        }
    }

    /// Moves the end of the selection to `position` during a drag.
    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.pointer = position;
        let Some(mut selection) = self.selection else {
            return;
        };
        let before = selection;
        selection.extend_to(self.hit_at(position));
        if selection != before {
            self.set_selection(selection);
            cx.notify();
        }
    }

    /// Ends an active selection drag and cancels any auto-scroll timer.
    fn stop_selecting(&mut self) {
        self.selecting = false;
        self.drag_scroll_timer = None;
        self.awaiting_drag_scroll_frame = false;
    }

    /// Signed lines to scroll on one auto-scroll tick for the current pointer.
    fn drag_scroll_step(&self) -> i32 {
        let Some(bounds) = self.pane_bounds else {
            return 0;
        };
        drag_scroll_lines(
            f32::from(self.pointer.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
            f32::from(self.metrics.line_height),
        )
    }

    /// Starts or stops the auto-scroll timer according to whether a selection
    /// drag is currently held above or below the pane.
    fn update_drag_scroll(&mut self, cx: &mut Context<Self>) {
        if !self.selecting || self.drag_scroll_step() == 0 {
            self.drag_scroll_timer = None;
            self.awaiting_drag_scroll_frame = false;
            return;
        }
        if self.drag_scroll_timer.is_some() {
            return;
        }
        self.awaiting_drag_scroll_frame = false;
        self.drag_scroll_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DRAG_SCROLL_INTERVAL).await;
                let keep_going = this
                    .update(cx, |this, cx| this.drag_scroll_tick(cx))
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// One tick of selection drag auto-scroll. Returns `false` when the timer
    /// should stop.
    fn drag_scroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.selecting || self.awaiting_drag_scroll_frame {
            self.stop_selecting();
            return false;
        }
        let lines = self.drag_scroll_step();
        if lines == 0 {
            self.drag_scroll_timer = None;
            return false;
        }
        let line_height = f32::from(self.metrics.line_height);
        let before = self.scroll;
        self.scroll.scroll_by(
            lines as f32 * line_height,
            line_height,
            self.screen.scrollback_len,
        );
        if self.scroll != before {
            self.drag_to(self.pointer, cx);
            self.awaiting_drag_scroll_frame = true;
            cx.notify();
        }
        true
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        self.hover(event.position, event.modifiers, cx);
        if !self.selecting {
            self.report_move(event, cx);
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            self.drag_to(event.position, cx);
            self.update_drag_scroll(cx);
        } else {
            // The button was let go where no up event reached the window.
            self.stop_selecting();
        }
    }

    /// Sends a move to the program if it asked for moves. Moves come from
    /// the whole window: a drag is reported only if its press was, and a
    /// move without a button only over this pane's screen.
    fn report_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button.is_none() {
            // The button was let go where no up event reached the window.
            self.reported_button = None;
        } else if self.reported_button.is_none() {
            // A drag that another pane, or a selection, started.
            return;
        }
        let bytes = self.mouse.motion(
            self.reported_button,
            self.pointer_at(event.position),
            report_mods(&event.modifiers),
            self.screen.modes,
            self.screen.size,
        );
        if let Some(bytes) = bytes {
            self.send(bytes, cx);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.selecting && event.button == MouseButton::Left {
            self.drag_to(event.position, cx);
            self.stop_selecting();
            return;
        }
        let button = report_button(event.button);
        if button.is_none() || button != self.reported_button {
            return;
        }
        self.reported_button = None;
        let bytes = self.mouse.release(
            button.unwrap(),
            self.pointer_at(event.position),
            report_mods(&event.modifiers),
            self.screen.modes,
            self.screen.size,
        );
        if let Some(bytes) = bytes {
            self.send(bytes, cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let key = Key {
            key: &keystroke.key,
            key_char: keystroke.key_char.as_deref(),
            shift: keystroke.modifiers.shift,
            alt: keystroke.modifiers.alt,
            control: keystroke.modifiers.control,
            platform: keystroke.modifiers.platform,
        };
        if let Some(direction) = scroll_key(&key, self.screen.modes) {
            self.scroll_page(direction, cx);
            cx.stop_propagation();
            return;
        }
        // A key that types text is left alone: the platform gives it to
        // the input method, which commits the text to
        // `replace_text_in_range`.
        if let Some(bytes) = key_bytes(&key, self.screen.modes, self.theme.keys) {
            self.send_typed(bytes, true, cx);
            cx.stop_propagation();
        }
    }

    /// Scrolls by the height of the screen, as Ghostty does.
    fn scroll_page(&mut self, direction: PageScroll, cx: &mut Context<Self>) {
        let line_height = f32::from(self.metrics.line_height);
        let page = f32::from(self.screen.size.rows) * line_height;
        let delta = match direction {
            PageScroll::Up => page,
            PageScroll::Down => -page,
        };
        let before = self.scroll;
        self.scroll
            .scroll_by(delta, line_height, self.screen.scrollback_len);
        if self.scroll != before {
            cx.notify();
        }
    }

    /// The text of the terminal as the socket API gives it: the screen,
    /// or with a number the last lines of the scrollback and the screen
    /// together. It does not depend on where the view is scrolled to.
    pub fn text(&self, lines: Option<usize>) -> String {
        self.terminal.text(lines)
    }

    fn write_if_running(&self, bytes: Vec<u8>) -> Result<(), WriteError> {
        if self.screen.exit.is_some() {
            return Err(WriteError::Closed);
        }
        self.terminal.write(bytes)
    }

    /// Types `text` into the program exactly as it is, for the socket
    /// API. Unlike a key it leaves the view where it is scrolled to.
    /// Returns false when the program has exited or its input queue is full.
    pub fn send_text(&self, text: &str) -> bool {
        self.try_send_text(text).is_ok()
    }

    /// Like [`send_text`](Self::send_text), distinguishing a full input
    /// queue from a program that has exited.
    pub(crate) fn try_send_text(&self, text: &str) -> Result<(), WriteError> {
        self.write_if_running(text.as_bytes().to_vec())
    }

    /// Sends the program what the key named `key`, such as `escape`,
    /// sends when it is pressed in the pane with no modifier: in the
    /// keyboard protocol the program asked for. Returns false when the
    /// key sends nothing by itself, the program has exited, or its input
    /// queue is full.
    pub(crate) fn send_key(&self, key: &str) -> bool {
        let key = Key {
            key,
            key_char: None,
            shift: false,
            alt: false,
            control: false,
            platform: false,
        };
        let Some(bytes) = key_bytes(&key, self.screen.modes, self.theme.keys) else {
            return false;
        };
        self.write_if_running(bytes).is_ok()
    }

    /// Presses the keys named in `names`, such as `up` or `ctrl-c`, as
    /// they are pressed in the pane: in the keyboard protocol the program
    /// asked for, all in one write. A name that is no key, or a key that
    /// types text, sends none of them.
    pub(crate) fn press_keys(
        &self,
        names: &[String],
    ) -> Result<Result<(), WriteError>, KeyNameError> {
        let bytes = named_key_bytes(names, self.screen.modes, self.theme.keys)?;
        Ok(self.write_if_running(bytes))
    }

    /// Gives the program `text` as one message: pasted, as the user
    /// would paste it, with Enter after it, in one write, so nothing
    /// typed gets between the two.
    pub(crate) fn enter_message(&self, text: &str) -> Result<(), WriteError> {
        let mut bytes = paste_bytes(text, self.screen.modes);
        bytes.push(b'\r');
        self.write_if_running(bytes)
    }

    /// Whether the program has exited. Its pane, if it has one, is about
    /// to close.
    pub(crate) fn has_exited(&self) -> bool {
        self.screen.exit.is_some()
    }

    /// Whether the program has asked for pastes to be marked, so that it
    /// can tell a line break in one from Enter.
    pub(crate) fn takes_marked_pastes(&self) -> bool {
        self.screen.modes.bracketed_paste
    }

    /// Whether input from somebody else would get in the user's way now:
    /// the input method is composing, or the user has typed something
    /// and not sent it off or taken it back, so a draft may be open.
    pub(crate) fn user_is_typing(&self) -> bool {
        self.composition.marked().is_some() || self.draft > 0
    }

    /// Sends what the user typed or pasted, and notes whether it leaves
    /// a draft behind. `key` is for a key, as opposed to text from the
    /// input method or the clipboard.
    fn send_typed(&mut self, bytes: Vec<u8>, key: bool, cx: &mut Context<Self>) {
        // Enter sends a draft off, control-C gives it up and control-U
        // erases it; backspace takes one letter back. What a program
        // makes of them is its own business; this is the usual. Keys
        // that move the cursor leave the draft as long as it is. A line
        // break in pasted text may as well stay in the draft.
        let letters = || {
            let text = String::from_utf8_lossy(&bytes);
            text.chars().filter(|ch| !ch.is_control()).count()
        };
        self.draft = match bytes[..] {
            [b'\r'] | [0x03] | [0x15] if key => 0,
            [0x7f] | [0x08] if key => self.draft.saturating_sub(1),
            [0x1b, ..] if key => self.draft,
            _ if key => self.draft + letters(),
            _ => self.draft + letters().max(1),
        };
        self.send(bytes, cx);
    }

    /// Whether the view shows older lines instead of following the
    /// screen.
    pub fn is_scrolled_back(&self) -> bool {
        !self.scroll.is_at_bottom()
    }

    /// The range of lines currently visible in the viewport (0 = oldest scrollback row).
    pub fn visible_lines(&self) -> Range<usize> {
        let visible = self.scroll.visible(
            self.metrics.line_height.into(),
            self.screen.size.rows,
            self.screen.scrollback_len,
        );
        visible.first_line..visible.first_line + visible.line_count
    }

    /// Whether the view is currently auto-scrolling for a selection drag
    /// held outside the pane.
    pub fn is_drag_scrolling(&self) -> bool {
        self.drag_scroll_timer.is_some()
    }

    /// Whether the in-pane search bar is currently open.
    pub fn is_search_open(&self) -> bool {
        self.search.is_some()
    }

    /// The current search query if the search bar is open.
    pub fn search_query(&self, cx: &App) -> Option<String> {
        self.search
            .as_ref()
            .map(|search| search.editor.read(cx).text().to_owned())
    }

    /// The total number of search matches currently found.
    pub fn search_match_count(&self) -> usize {
        self.search
            .as_ref()
            .map_or(0, |search| search.state.matches().len())
    }

    /// The zero-based index of the active search match, if any.
    pub fn search_current_index(&self) -> Option<usize> {
        self.search
            .as_ref()
            .and_then(|search| search.state.current_index())
    }

    /// Human-readable match counter such as `"1/3"` or `"0/0"`.
    pub fn search_status_label(&self) -> String {
        self.search
            .as_ref()
            .map_or_else(String::new, |search| search.state.status_label())
    }

    pub fn search_matches(&self) -> &[SearchMatch] {
        self.search
            .as_ref()
            .map_or(&[], |search| search.state.matches())
    }

    /// Opens the search bar for this pane, or re-focuses and selects its text
    /// if it is already open.
    fn on_open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            let current = search.editor.read(cx).text().to_owned();
            search
                .editor
                .update(cx, |editor, cx| editor.set_text(&current, cx));
            let focus = search.editor.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            cx.notify();
            return;
        }
        let theme = self.theme.search.clone();
        let editor = cx.new(|cx| {
            NameEditor::new("", theme, cx)
                .without_underline()
                .with_placeholder("Find...")
        });
        let observed = cx.observe(&editor, |this, editor, cx| {
            let query = editor.read(cx).text().to_owned();
            let changed = this
                .search
                .as_ref()
                .is_some_and(|search| search.state.query() != query);
            if changed {
                this.refresh_search(&query, true);
            }
            cx.notify();
        });
        let events = cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            NameEditorEvent::Confirmed(_) => this.find_next(cx),
            NameEditorEvent::Cancelled => this.close_search(window, cx),
        });
        let focus = editor.read(cx).focus_handle().clone();
        self.search = Some(OpenSearchBar {
            editor,
            state: SearchState::default(),
            _subscriptions: [observed, events],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes the search bar, clears highlights, and returns keyboard focus to
    /// the terminal.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn on_find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.find_next(cx);
    }

    fn on_find_previous(&mut self, _: &FindPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.find_previous(cx);
    }

    fn find_next(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.editor.read(cx).is_converting() {
            return;
        }
        let target = search.state.next().map(|m| m.start_line);
        if let Some(line) = target {
            self.scroll_to_match_line(line);
        }
        cx.notify();
    }

    fn find_previous(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.editor.read(cx).is_converting() {
            return;
        }
        let target = search.state.previous().map(|m| m.start_line);
        if let Some(line) = target {
            self.scroll_to_match_line(line);
        }
        cx.notify();
    }

    fn toggle_search_case(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        search.state.toggle_case_sensitive();
        let query = search.state.query().to_owned();
        self.refresh_search(&query, true);
        cx.notify();
    }

    /// Collects all rows from scrollback and the active screen in order.
    fn all_rows(&self) -> Vec<Row> {
        let history = self.screen.scrollback_len;
        let mut rows = if history > 0 {
            self.terminal.scrollback_rows(0..history)
        } else {
            Vec::new()
        };
        rows.extend_from_slice(&self.screen.rows);
        rows
    }

    /// Recomputes search matches for `query` and optionally scrolls to the
    /// active match.
    fn refresh_search(&mut self, query: &str, scroll_to_current: bool) {
        let rows = if query.is_empty() {
            Vec::new()
        } else {
            self.all_rows()
        };
        let target = {
            let Some(search) = &mut self.search else {
                return;
            };
            search.state.update(query, &rows, 0, scroll_to_current);
            scroll_to_current
                .then(|| search.state.current_match().map(|m| m.start_line))
                .flatten()
        };
        if let Some(line) = target {
            self.scroll_to_match_line(line);
        }
    }

    fn scroll_to_match_line(&mut self, line: usize) {
        self.scroll.scroll_to_line(
            line,
            self.metrics.line_height.into(),
            self.screen.size.rows,
            self.screen.scrollback_len,
        );
    }

    /// Renders the floating search bar over the top-right corner of the pane.
    fn render_search_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let search = self.search.as_ref()?;
        let theme = &self.theme.search;
        let label = search.state.status_label();
        let no_match = !search.state.query().is_empty() && search.state.matches().is_empty();
        let case_sensitive = search.state.case_sensitive();
        let counter_color = if no_match {
            hsla(theme.no_match_text)
        } else {
            hsla(theme.secondary_text)
        };
        let case_bg = if case_sensitive {
            hsla(theme.accent)
        } else {
            hsla(theme.input_background)
        };
        let case_fg = if case_sensitive {
            hsla(theme.input_background)
        } else {
            hsla(theme.secondary_text)
        };

        Some(
            div()
                .id("terminal-search")
                .debug_selector(|| "terminal-search".into())
                .key_context(terminal_search::KEY_CONTEXT)
                .on_action(cx.listener(Self::on_open_search))
                .on_action(cx.listener(Self::on_find_next))
                .on_action(cx.listener(Self::on_find_previous))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .absolute()
                .overflow_hidden()
                .top_2()
                .right_2()
                .w(px(320.))
                .px_2()
                .py_1()
                .rounded(px(RADIUS_CARD))
                .bg(hsla(theme.background))
                .border_1()
                .border_color(hsla(theme.border))
                .shadow_lg()
                .font_family(SharedString::from(theme.font_family.clone()))
                .text_size(px(theme.font_size))
                .text_color(hsla(theme.text))
                .flex()
                .items_center()
                .gap_1()
                .child(crate::design::top_highlight_bar(theme.background))
                .child(crate::icons::search(hsla(theme.secondary_text), 13.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .py_0p5()
                        .rounded(px(RADIUS_CONTROL))
                        .bg(hsla(theme.input_background))
                        .border_1()
                        .border_color(hsla(theme.input_border))
                        .child(search.editor.clone()),
                )
                .when(!label.is_empty(), |bar| {
                    bar.child(
                        div()
                            .flex_none()
                            .px_1()
                            .text_color(counter_color)
                            .child(label),
                    )
                })
                .child(
                    div()
                        .id("terminal-search-case")
                        .flex_none()
                        .px_1()
                        .rounded(px(RADIUS_CONTROL))
                        .bg(case_bg)
                        .text_color(case_fg)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_search_case(cx);
                            }),
                        )
                        .child("Aa"),
                )
                .child(
                    div()
                        .id("terminal-search-prev")
                        .flex_none()
                        .px_1()
                        .rounded(px(RADIUS_CONTROL))
                        .text_color(hsla(theme.secondary_text))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.find_previous(cx);
                            }),
                        )
                        .child("↑"),
                )
                .child(
                    div()
                        .id("terminal-search-next")
                        .flex_none()
                        .px_1()
                        .rounded(px(RADIUS_CONTROL))
                        .text_color(hsla(theme.secondary_text))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.find_next(cx);
                            }),
                        )
                        .child("↓"),
                )
                .child(
                    div()
                        .id("terminal-search-close")
                        .flex_none()
                        .px_1()
                        .rounded(px(RADIUS_CONTROL))
                        .text_color(hsla(theme.secondary_text))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_search(window, cx);
                            }),
                        )
                        .child("×"),
                ),
        )
    }

    /// Sends input to the program.
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let _ = self.terminal.write(bytes);
        // Typing brings the view back to the cursor.
        self.scroll = Scroll::default();
        cx.notify();
    }

    /// Whether some text is selected, which [`Copy`] copies.
    pub fn has_selection(&self) -> bool {
        self.selected_span().is_some()
    }

    pub(crate) fn selection_text(&self) -> Option<String> {
        let span = self.selected_span()?;
        let rows = self.line_rows(span.start.line..=span.end.line);
        let colors = self.theme.terminal.reported_colors();
        Some(span.text_with(&rows, Some(&colors)))
    }

    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy(cx);
    }

    /// Copies the selected text, as [`Copy`] does.
    pub(crate) fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        self.paste(cx);
    }

    /// Pastes text, or asks the agent in front to attach the clipboard
    /// image: neither Claude Code nor Codex reads an image from a paste,
    /// and both read it from the native clipboard on Ctrl+V.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            let bytes = paste_bytes(&text, self.screen.modes);
            if !bytes.is_empty() {
                self.send_typed(bytes, false, cx);
            }
        } else if self.agent_status().agent.is_some()
            && item
                .entries()
                .iter()
                .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
        {
            self.send_typed(vec![0x16], true, cx);
        }
    }

    fn on_drop_paths(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        let bytes = dropped_path_bytes(paths.paths(), self.screen.modes);
        if !bytes.is_empty() {
            self.send_typed(bytes, false, cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.metrics.line_height;
        if self.screen.modes.mouse_tracking != MouseTracking::Off
            || self.screen.modes.alternate_screen
        {
            // The program asked for the wheel, or runs on the alternate
            // screen where there is no scrollback, so scrolling goes to the
            // program instead of the view.
            let delta = event.delta.pixel_delta(line_height);
            let lines = (
                f32::from(delta.x) / f32::from(line_height),
                f32::from(delta.y) / f32::from(line_height),
            );
            let bytes = self.mouse.wheel(
                lines,
                self.pointer_at(event.position),
                report_mods(&event.modifiers),
                self.screen.modes,
                self.screen.size,
            );
            if !bytes.is_empty() {
                self.send(bytes, cx);
            }
            return;
        }
        // A positive delta moves the content down, toward older lines.
        let delta = event.delta.pixel_delta(line_height).y;
        let before = self.scroll;
        self.scroll
            .scroll_by(delta.into(), line_height.into(), self.screen.scrollback_len);
        if self.scroll != before {
            if self.selecting {
                // The lines move under a pointer that holds still.
                self.drag_to(self.pointer, cx);
            }
            cx.notify();
        }
    }

    /// Fits the grid to `bounds`, brings the screen up to date and shapes
    /// the visible rows.
    fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Frame {
        let cell_width = self.metrics.cell_width;
        let line_height = self.metrics.line_height;
        let padding = self.theme.terminal.padding;

        self.fit(bounds.size);
        self.sync(cx);
        if self.hovered_path.is_some() {
            if self.path_candidate_under(window.mouse_position()) != self.hovered_path_candidate {
                self.hovered_link = None;
                self.hovered_path = None;
                self.hovered_path_candidate = None;
                self.pending_path_probe = None;
                self.path_probe_gen += 1;
            }
        } else if self.hovered_link.is_some() {
            // The text under the pointer may have changed.
            self.hovered_link = self.link_under(window.mouse_position());
        }

        let scrollback_len = self.screen.scrollback_len;
        let visible =
            self.scroll
                .visible(line_height.into(), self.screen.size.rows, scrollback_len);
        let last_line = visible.first_line + visible.line_count;
        let history = if visible.first_line < scrollback_len {
            self.terminal
                .scrollback_rows(visible.first_line..last_line.min(scrollback_len))
        } else {
            Vec::new()
        };

        let theme = &self.theme.terminal;
        let metrics = &self.metrics;
        let mut rows = Vec::with_capacity(visible.line_count);
        for line in visible.first_line..last_line {
            let shaped = if line < scrollback_len {
                Rc::new(shape_row(
                    &history[line - visible.first_line],
                    theme,
                    metrics,
                    window,
                ))
            } else {
                let index = line - scrollback_len;
                let Some(row) = self.screen.rows.get(index) else {
                    break;
                };
                self.row_cache[index]
                    .get_or_insert_with(|| Rc::new(shape_row(row, theme, metrics, window)))
                    .clone()
            };
            rows.push(shaped);
        }

        self.pane_bounds = Some(bounds);
        self.grid_origin = bounds.origin + point(px(padding), px(padding));
        let origin = self.grid_origin - point(px(0.), px(visible.shift));
        let selection = self.selected_span().map(|span| {
            let cols = self.screen.size.cols;
            let rects = (visible.first_line..last_line)
                .enumerate()
                .filter_map(|(index, line)| {
                    let columns = span.columns(line, cols);
                    (!columns.is_empty()).then(|| {
                        Bounds::new(
                            origin
                                + point(
                                    cell_width * f32::from(columns.start),
                                    line_height * index as f32,
                                ),
                            size(cell_width * f32::from(columns.len() as u16), line_height),
                        )
                    })
                })
                .collect();
            let mut color = hsla(self.theme.terminal.foreground);
            color.a = SELECTION_OPACITY;
            SelectionPaint { rects, color }
        });
        let (search_matches, current_search_match) = match &self.search {
            Some(search) if !search.state.matches().is_empty() => {
                let current_idx = search.state.current_index();
                let mut other_rects = Vec::new();
                let mut current_rects = Vec::new();
                for (idx, m) in search.state.matches().iter().enumerate() {
                    let is_current = current_idx == Some(idx);
                    for &(line, ref cols) in &m.spans {
                        if line < visible.first_line || line >= last_line || cols.is_empty() {
                            continue;
                        }
                        let row_offset = line - visible.first_line;
                        let rect = Bounds::new(
                            origin
                                + point(
                                    cell_width * f32::from(cols.start),
                                    line_height * row_offset as f32,
                                ),
                            size(cell_width * f32::from(cols.len() as u16), line_height),
                        );
                        if is_current {
                            current_rects.push(rect);
                        } else {
                            other_rects.push(rect);
                        }
                    }
                }
                let mut match_color = hsla(self.theme.search.match_highlight);
                match_color.a = SEARCH_MATCH_OPACITY;
                let mut current_color = hsla(self.theme.search.current_match_highlight);
                current_color.a = SEARCH_CURRENT_OPACITY;
                (
                    (!other_rects.is_empty()).then_some(SelectionPaint {
                        rects: other_rects,
                        color: match_color,
                    }),
                    (!current_rects.is_empty()).then_some(SelectionPaint {
                        rects: current_rects,
                        color: current_color,
                    }),
                )
            }
            _ => (None, None),
        };
        let link = self.hovered_link.as_ref().map(|link| {
            let cols = self.screen.size.cols;
            let rects = (visible.first_line..last_line)
                .enumerate()
                .filter_map(|(index, line)| {
                    let columns = link.columns(line, cols);
                    (!columns.is_empty()).then(|| {
                        let top = line_height * (index + 1) as f32 - LINK_THICKNESS;
                        Bounds::new(
                            origin + point(cell_width * f32::from(columns.start), top),
                            size(cell_width * f32::from(columns.len() as u16), LINK_THICKNESS),
                        )
                    })
                })
                .collect();
            let color = hsla(self.theme.terminal.foreground);
            SelectionPaint { rects, color }
        });
        let cursor_line = scrollback_len + usize::from(self.screen.cursor.row);
        let cursor_on_view = (visible.first_line..last_line).contains(&cursor_line);
        // Kept whether or not the program shows its cursor: programs that
        // draw a cursor of their own still park the real one where text
        // goes.
        self.cursor_cell = cursor_on_view.then(|| {
            let cell = point(
                cell_width * f32::from(self.screen.cursor.col),
                line_height * (cursor_line - visible.first_line) as f32,
            );
            Bounds::new(origin + cell, size(cell_width, line_height))
        });
        let composition = self
            .cursor_cell
            .and_then(|cell| self.composition_paint(cell, window));
        // The composition has a caret of its own.
        let cursor = (self.screen.cursor.visible && cursor_on_view && composition.is_none())
            .then(|| self.cursor_paint(origin, cursor_line - visible.first_line, window))
            .flatten();

        Frame {
            clip: bounds,
            origin,
            cell_width,
            line_height,
            rows,
            selection,
            search_matches,
            current_search_match,
            link,
            cursor,
            composition,
        }
    }

    /// The text being composed, starting at the cursor's `cell`.
    fn composition_paint(
        &self,
        cell: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<CompositionPaint> {
        self.composition.marked()?;
        let theme = &self.theme.terminal;
        let style = GlyphStyle {
            fg: theme.foreground,
            bold: false,
            italic: false,
            underline: Underline::None,
            strikeout: false,
        };
        let segments = self
            .composition
            .segments()
            .into_iter()
            .map(|(col, text)| {
                let run = StyleRun {
                    len: text.len(),
                    style,
                };
                (
                    col,
                    shape_text(text.to_owned(), &[run], &self.metrics, window),
                )
            })
            .collect();
        let width = self.metrics.cell_width * self.composition.columns() as f32;
        Some(CompositionPaint {
            bounds: Bounds::new(cell.origin, size(width, cell.size.height)),
            segments,
            background: hsla(theme.background),
            underline: hsla(theme.foreground),
            caret: hsla(theme.cursor),
        })
    }

    /// The cursor, `line` rows below `origin`.
    fn cursor_paint(
        &self,
        origin: Point<Pixels>,
        line: usize,
        window: &mut Window,
    ) -> Option<CursorPaint> {
        let cursor = self.screen.cursor;
        let row = self.screen.rows.get(usize::from(cursor.row))?;
        // On the second half of a wide character, cover the whole
        // character.
        let mut col = usize::from(cursor.col);
        if row.cells.get(col)?.width == CellWidth::WideSpacer {
            col = col.saturating_sub(1);
        }
        let cell = &row.cells[col];
        let columns = if cell.width == CellWidth::Wide {
            2.
        } else {
            1.
        };

        let theme = &self.theme.terminal;
        let active = window.is_window_active() && self.focus.is_focused(window);
        let text = (cursor.shape == CursorShape::Block && active && cell.ch != ' ').then(|| {
            let mut text = String::from(cell.ch);
            text.extend(&cell.zero_width);
            let style = GlyphStyle {
                fg: theme.cursor_text,
                bold: cell.attrs.bold,
                italic: cell.attrs.italic,
                underline: Underline::None,
                strikeout: false,
            };
            let run = StyleRun {
                len: text.len(),
                style,
            };
            shape_text(text, &[run], &self.metrics, window)
        });
        Some(CursorPaint {
            bounds: Bounds::new(
                origin
                    + point(
                        self.metrics.cell_width * col as f32,
                        self.metrics.line_height * line as f32,
                    ),
                size(self.metrics.cell_width * columns, self.metrics.line_height),
            ),
            shape: cursor.shape,
            active,
            color: hsla(theme.cursor),
            text,
        })
    }
}

/// The button a mouse report names, if it names this one.
fn report_button(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Middle => Some(mouse::Button::Middle),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

/// The modifiers a mouse report carries.
fn report_mods(modifiers: &Modifiers) -> mouse::Mods {
    mouse::Mods {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    }
}

fn shape_row(
    row: &Row,
    theme: &TerminalTheme,
    metrics: &Metrics,
    window: &mut Window,
) -> ShapedRow {
    let layout = layout_row(row, theme);
    ShapedRow {
        backgrounds: layout.backgrounds,
        segments: layout
            .segments
            .into_iter()
            .map(|segment| {
                let line = shape_text(segment.text, &segment.runs, metrics, window);
                (segment.col, line)
            })
            .collect(),
    }
}

/// Shapes `text` with every glyph placed on the cell grid.
fn shape_text(
    text: String,
    runs: &[StyleRun],
    metrics: &Metrics,
    window: &mut Window,
) -> ShapedLine {
    let runs: Vec<TextRun> = runs
        .iter()
        .map(|run| {
            let style = run.style;
            let mut font = metrics.font.clone();
            if style.bold {
                font.weight = FontWeight::BOLD;
            }
            if style.italic {
                font.style = FontStyle::Italic;
            }
            TextRun {
                len: run.len,
                font,
                color: hsla(style.fg),
                background_color: None,
                // GPUI draws straight and wavy underlines only; double,
                // dotted and dashed ones are drawn straight.
                underline: (style.underline != Underline::None).then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: style.underline == Underline::Curly,
                }),
                strikethrough: style.strikeout.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
            }
        })
        .collect();
    window.text_system().shape_line(
        text.into(),
        metrics.font_size,
        &runs,
        Some(metrics.cell_width),
    )
}

impl Frame {
    fn paint(&self, window: &mut Window, cx: &mut App) {
        let mask = ContentMask { bounds: self.clip };
        window.with_content_mask(Some(mask), |window| {
            let row_origin =
                |index: usize| self.origin + point(px(0.), self.line_height * index as f32);
            // Edges are rounded to device pixels, so that backgrounds side by
            // side meet without a seam even though a cell is not a whole
            // number of pixels wide.
            let scale = window.scale_factor();
            let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
            // All backgrounds first: a glyph may reach into the next cell.
            for (index, row) in self.rows.iter().enumerate() {
                let origin = row_origin(index);
                let (top, bottom) = (snap(origin.y), snap(origin.y + self.line_height));
                for span in &row.backgrounds {
                    let left = snap(origin.x + self.cell_width * f32::from(span.start));
                    let right = snap(origin.x + self.cell_width * f32::from(span.end));
                    let bounds = Bounds::new(point(left, top), size(right - left, bottom - top));
                    window.paint_quad(fill(bounds, hsla(span.color)));
                }
            }
            // Under the text, so that the text stays readable.
            if let Some(selection) = &self.selection {
                for rect in &selection.rects {
                    window.paint_quad(fill(*rect, selection.color));
                }
            }
            if let Some(matches) = &self.search_matches {
                for rect in &matches.rects {
                    window.paint_quad(fill(*rect, matches.color));
                }
            }
            if let Some(current) = &self.current_search_match {
                for rect in &current.rects {
                    window.paint_quad(fill(*rect, current.color));
                }
            }
            for (index, row) in self.rows.iter().enumerate() {
                let origin = row_origin(index);
                for (col, line) in &row.segments {
                    let origin = origin + point(self.cell_width * f32::from(*col), px(0.));
                    // Fails only if a glyph cannot be rasterized; the rest
                    // of the frame is still worth drawing.
                    let _ = line.paint(origin, self.line_height, TextAlign::Left, None, window, cx);
                }
            }
            if let Some(link) = &self.link {
                for rect in &link.rects {
                    window.paint_quad(fill(*rect, link.color));
                }
            }
            if let Some(cursor) = &self.cursor {
                cursor.paint(self.line_height, window, cx);
            }
            if let Some(composition) = &self.composition {
                composition.paint(self.cell_width, self.line_height, window, cx);
            }
        });
    }
}

impl CursorPaint {
    fn paint(&self, line_height: Pixels, window: &mut Window, cx: &mut App) {
        let Bounds { origin, size: cell } = self.bounds;
        match self.shape {
            CursorShape::Block if self.active => {
                window.paint_quad(fill(self.bounds, self.color));
                if let Some(text) = &self.text {
                    let _ = text.paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
            }
            CursorShape::Block => {
                let hollow = outline(self.bounds, self.color, gpui::BorderStyle::Solid);
                window.paint_quad(hollow.border_widths(CURSOR_THICKNESS));
            }
            CursorShape::Bar => {
                let bar = Bounds::new(origin, size(CURSOR_THICKNESS, cell.height));
                window.paint_quad(fill(bar, self.color));
            }
            CursorShape::Underline => {
                let line = Bounds::new(
                    origin + point(px(0.), cell.height - CURSOR_THICKNESS),
                    size(cell.width, CURSOR_THICKNESS),
                );
                window.paint_quad(fill(line, self.color));
            }
        }
    }
}

impl CompositionPaint {
    fn paint(&self, cell_width: Pixels, line_height: Pixels, window: &mut Window, cx: &mut App) {
        let Bounds { origin, size: area } = self.bounds;
        // The cells under the text must not show through.
        window.paint_quad(fill(self.bounds, self.background));
        for (col, line) in &self.segments {
            let origin = origin + point(cell_width * *col as f32, px(0.));
            let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
        let underline = Bounds::new(
            origin + point(px(0.), area.height - CURSOR_THICKNESS),
            size(area.width, CURSOR_THICKNESS),
        );
        window.paint_quad(fill(underline, self.underline));
        let caret = Bounds::new(
            origin + point(area.width, px(0.)),
            size(CURSOR_THICKNESS, area.height),
        );
        window.paint_quad(fill(caret, self.caret));
    }
}

/// The input method's side of the terminal: it shows marked text through
/// the view and types committed text into the program.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (range, text) = self.composition.text_in_range_utf16(range);
        *adjusted_range = Some(range);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition.selected_range_utf16(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.composition.marked_range_utf16()
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        cx.notify();
    }

    /// The input method commits `text`, or a key typed it without an
    /// input method in between.
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.composition.commit(text) {
            Some(bytes) => self.send_typed(bytes, false, cx),
            None => cx.notify(),
        }
    }

    /// The input method shows `text` as what is being composed.
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.mark(text);
        // Composing brings the view back to the cursor, where the text is
        // shown.
        self.scroll = Scroll::default();
        cx.notify();
    }

    /// Where the candidate window goes: the cell at the cursor, moved
    /// right by the marked text before `range`.
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cell = self.cursor_cell?;
        let columns = self.composition.columns_before_utf16(range.start);
        let offset = point(self.metrics.cell_width * columns as f32, px(0.));
        Some(Bounds::new(cell.origin + offset, cell.size))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.awaiting_drag_scroll_frame = false;
        if let Some(counting) = &mut self.counting {
            counting.drawn_in = Some(counting.stats.frames());
        }
        let view = cx.entity();
        let input_view = view.clone();
        let mouse_view = view.clone();
        let focus = self.focus.clone();
        let search_bar = self.render_search_bar(cx);
        let link_target = self.render_link_target();
        let search_ticks = self.search.as_ref().and_then(|search| {
            let matches = search.state.matches();
            if matches.is_empty() {
                return None;
            }
            let screen = self.screen();
            let total = (screen.scrollback_len + usize::from(screen.size.rows)).max(1);
            let current = search.state.current_index();
            let theme = &self.theme.search;
            let ticks = matches.iter().enumerate().take(120).map(|(idx, m)| {
                let ratio = (m.start_line as f32 / total as f32).clamp(0.0, 0.99);
                let color = if current == Some(idx) {
                    hsla(theme.current_match_highlight)
                } else {
                    crate::design::with_alpha(theme.match_highlight, 0.85)
                };
                div()
                    .absolute()
                    .top(gpui::relative(ratio))
                    .left(px(1.))
                    .right(px(1.))
                    .h(px(3.))
                    .rounded(px(1.))
                    .bg(color)
            });
            Some(
                div()
                    .debug_selector(|| "terminal-search-ticks".into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(2.))
                    .w(px(6.))
                    .children(ticks),
            )
        });
        div()
            .relative()
            .size_full()
            .bg(hsla(self.theme.terminal.background))
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .when(self.hovered_link.is_some(), |this| this.cursor_pointer())
            .on_action(cx.listener(Self::on_open_search))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_paste))
            .on_drop(cx.listener(Self::on_drop_paths))
            .on_action(|_: &Undo, _, _| {})
            .on_action(|_: &Redo, _, _| {})
            .on_action(|_: &Cut, _, _| {})
            .on_action(|_: &SelectAll, _, _| {})
            .on_scroll(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        view.update(cx, |this, cx| this.prepare_frame(bounds, window, cx))
                    },
                    move |bounds, frame, window, cx| {
                        // Text input goes to this view while it has the
                        // focus.
                        let handler = ElementInputHandler::new(bounds, input_view);
                        window.handle_input(&focus, handler, cx);
                        // A drag goes on outside the pane, so its moves
                        // are taken from the whole window.
                        let view = mouse_view.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                view.update(cx, |this, cx| this.on_mouse_move(event, cx));
                            }
                        });
                        let view = mouse_view.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                view.update(cx, |this, cx| this.on_mouse_up(event, cx));
                            }
                        });
                        frame.paint(window, cx);
                    },
                )
                .size_full(),
            )
            .when_some(search_ticks, |this, ticks| this.child(ticks))
            .when_some(search_bar, |this, bar| this.child(bar))
            .when_some(link_target, |this, target| this.child(target))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use benterm_agent::AgentState;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle};

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn screen_text(view: &Entity<TerminalView>, cx: &VisualTestContext) -> String {
        view.read_with(cx, |view, _| {
            view.screen()
                .rows
                .iter()
                .map(Row::text)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn open_view(
        pty: PtyConfig,
        cx: &mut TestAppContext,
    ) -> (WindowHandle<TerminalView>, VisualTestContext) {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let theme = Theme::default();
        let started = StartedTerminal::spawn(pty, &theme.terminal).unwrap();
        let window = cx.add_window(|window, cx| TerminalView::new(started, theme, window, cx));
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        (window, vcx)
    }

    fn wait_until(
        vcx: &mut VisualTestContext,
        what: &str,
        advance: Duration,
        mut done: impl FnMut(&mut VisualTestContext) -> bool,
    ) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !done(vcx) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            vcx.run_until_parked();
            vcx.update(|window, cx| window.draw(cx).clear(cx));
            std::thread::sleep(Duration::from_millis(10));
            if !advance.is_zero() {
                vcx.executor().advance_clock(advance);
            }
            vcx.run_until_parked();
        }
    }

    #[gpui::test]
    fn synchronized_update_defers_screen_until_end_sequence(cx: &mut TestAppContext) {
        let script = r#"stty -echo; printf before; while IFS= read -r line; do eval "$line"; done"#;
        let (window, mut vcx) = open_view(PtyConfig::command("/bin/sh", ["-c", script]), cx);
        let view = window.entity(&vcx).unwrap();

        wait_until(&mut vcx, "initial screen", Duration::ZERO, |vcx| {
            screen_text(&view, vcx).contains("before")
        });

        // Send start of mode 2026 and new content without the end sequence.
        assert!(view.read_with(&vcx, |view, _| {
            view.send_text("printf '\\033[?2026h\\033[H\\033[2Jafter_sync'\n")
        }));

        // Core (API read) sees the new content immediately.
        wait_until(&mut vcx, "core update", Duration::ZERO, |vcx| {
            view.read_with(vcx, |view, _| view.text(None).contains("after_sync"))
        });

        // Even after drawing frames, the UI's screen copy remains at "before".
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        let shown = screen_text(&view, &vcx);
        assert!(shown.contains("before"), "screen changed early: {shown:?}");
        assert!(
            !shown.contains("after_sync"),
            "partial sync update leaked to screen: {shown:?}"
        );

        // Sending the end sequence flushes the new content to the UI screen.
        assert!(view.read_with(&vcx, |view, _| view.send_text("printf '\\033[?2026l'\n")));
        wait_until(&mut vcx, "screen after 2026l", Duration::ZERO, |vcx| {
            screen_text(&view, vcx).contains("after_sync")
        });
    }

    #[gpui::test]
    fn synchronized_update_flushes_after_timeout(cx: &mut TestAppContext) {
        let script = r#"stty -echo; printf before; while IFS= read -r line; do eval "$line"; done"#;
        let (window, mut vcx) = open_view(PtyConfig::command("/bin/sh", ["-c", script]), cx);
        let view = window.entity(&vcx).unwrap();

        wait_until(&mut vcx, "initial screen", Duration::ZERO, |vcx| {
            screen_text(&view, vcx).contains("before")
        });

        // Start mode 2026 and never send the end sequence.
        assert!(view.read_with(&vcx, |view, _| {
            view.send_text("printf '\\033[?2026h\\033[H\\033[2Jtimed_out'\n")
        }));
        wait_until(&mut vcx, "core update", Duration::ZERO, |vcx| {
            view.read_with(vcx, |view, _| view.text(None).contains("timed_out"))
        });

        // Just before the timeout, the screen copy is still deferred.
        vcx.executor()
            .advance_clock(SYNC_UPDATE_TIMEOUT - Duration::from_millis(50));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        let before_deadline = screen_text(&view, &vcx);
        assert!(
            before_deadline.contains("before") && !before_deadline.contains("timed_out"),
            "screen flushed before timeout: {before_deadline:?}"
        );

        // Once the 1-second timeout elapses, the screen copy updates.
        vcx.executor().advance_clock(Duration::from_millis(100));
        vcx.run_until_parked();
        let after_deadline = screen_text(&view, &vcx);
        assert!(
            after_deadline.contains("timed_out"),
            "screen did not flush after timeout: {after_deadline:?}"
        );
    }

    #[gpui::test]
    fn synchronized_update_reads_latest_core_for_api_and_agent_state(cx: &mut TestAppContext) {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let script_path = dir.path().join("claude");
        let script = r#"#!/bin/sh
stty -echo
printf '\033[H\033[2JClaude Code\r\n❯ \r\n? for shortcuts'
while IFS= read -r line; do eval "$line"; done
"#;
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (window, mut vcx) =
            open_view(PtyConfig::command(script_path, Vec::<String>::new()), cx);
        let view = window.entity(&vcx).unwrap();

        wait_until(
            &mut vcx,
            "claude done state",
            Duration::from_millis(25),
            |vcx| {
                let status = view.read_with(vcx, |view, _| view.agent_status());
                status.agent == Some("claude") && status.state == AgentState::Done
            },
        );
        assert!(screen_text(&view, &vcx).contains("? for shortcuts"));

        // Enter mode 2026 (without ending it) and print a Working screen.
        assert!(view.read_with(&vcx, |view, _| {
            view.send_text(
                "printf '\\033[?2026h\\033[H\\033[2JClaude Code\\r\\nThinking (esc to interrupt)'\n",
            )
        }));

        // API read sees the new screen immediately.
        wait_until(&mut vcx, "core working text", Duration::ZERO, |vcx| {
            view.read_with(vcx, |view, _| view.text(None).contains("esc to interrupt"))
        });

        // Let real time pass the agent detector's settle window (500ms), then
        // advance the test clock past the agent timer (500ms) while staying
        // under SYNC_UPDATE_TIMEOUT (1s). `agent_status` must become Working
        // even though `self.screen` is still frozen at the pre-2026 screen.
        std::thread::sleep(Duration::from_millis(550));
        vcx.executor().advance_clock(Duration::from_millis(600));
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));

        let status = view.read_with(&vcx, |view, _| view.agent_status());
        assert_eq!(status.agent, Some("claude"));
        assert_eq!(status.state, AgentState::Working);

        let shown = screen_text(&view, &vcx);
        assert!(
            shown.contains("? for shortcuts") && !shown.contains("esc to interrupt"),
            "screen should still be deferred while agent_status updated: {shown:?}"
        );
    }
}
