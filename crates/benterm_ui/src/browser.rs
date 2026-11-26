//! A pane that shows a web page.
//!
//! The bar with the address is drawn by GPUI like the rest of the window.
//! The page is not: WebKit draws it in a view of its own, which lies over
//! the window's drawing. A [`Page`] is what the pane asks of that view,
//! so that everything but the view itself works, and is tested, without
//! one.
//!
//! Because the page lies over the window's drawing, nothing GPUI draws
//! shows in front of it. The page is therefore hidden while something
//! that says so with [`covers`] is over it, and whenever its pane is not
//! drawn: in a tab or a workspace behind, or under another view of the
//! pane area. Which pages a frame drew is settled once after the frame.
//!
//! A page that could not be loaded is hidden too: the view would show
//! the page before it, or nothing, and what the pane draws in its place
//! says what happened. While a page waits for its server the view shows
//! what it showed before, so the bar says that the page is loading.
//!
//! The keyboard is in two places. GPUI's focus is on the pane, which is
//! where the app's keys are looked up. While it is, the page itself has
//! the keyboard of the system, so what is typed goes to it. A key with
//! command held still reaches the window's own view first, which is above
//! the page's view, and only goes on to the page if no key of the app is
//! bound to it.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_layout::LayoutError;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Global,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, WindowId, actions,
    canvas, div, point, px, size,
};

use crate::menu::{Cut, Redo, SelectAll, Undo};
use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::terminal_view::{Copy, Paste, hsla};
use crate::theme::Theme;

mod webkit;

/// Where [`BentermWindow::open_browser`](crate::BentermWindow::open_browser)
/// puts the new pane.
pub use benterm_layout::SplitDirection;
pub use benterm_session::is_page_url;
pub use webkit::page_in_view;

actions!(
    browser,
    [
        /// Opens a browser pane at the right of the focused pane, with
        /// the keyboard in its address.
        OpenBrowser,
        FocusAddress,
        /// Goes back to the page the browser pane showed before.
        GoBack,
        GoForward,
        Reload,
    ]
);

/// The key context of a browser pane, which its bindings are limited to.
const KEY_CONTEXT: &str = "Browser";

/// Binds the keys of a browser pane, which are those of Safari.
///
/// The keys of the Edit menu are bound too, so that the pane hands them
/// to the page itself. Left unbound, they went on to the menu bar, to be
/// sent to the view with the keyboard of the system, and in a real window
/// only Cmd-V reached the page that way.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-l", FocusAddress, context),
        KeyBinding::new("cmd-[", GoBack, context),
        KeyBinding::new("cmd-]", GoForward, context),
        KeyBinding::new("cmd-r", Reload, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-v", Paste, context),
        KeyBinding::new("cmd-a", SelectAll, context),
    ]);
}

/// The address of the page that `text`, typed as an address, names, or
/// none if it names no page. A host without `http://` or `https://` is
/// taken to be `https`, and one on this machine or without a dot, as a
/// server started here has, to be `http`. Nothing is looked up for text
/// that is no address.
pub fn address_url(text: &str) -> Option<String> {
    let text = text.trim();
    if is_page_url(text) {
        return Some(text.to_owned());
    }
    if text.contains("://") {
        return None;
    }
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = &text[..end];
    // The host, without a port: `[::1]:3000`, `localhost:3000`.
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (host, after) = rest.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let numbered = port.is_some_and(|port| !port.is_empty());
    if port.is_some_and(|port| !port.bytes().all(|b| b.is_ascii_digit())) || host.is_empty() {
        return None;
    }
    let local =
        host == "localhost" || host.contains(':') || host.parse::<std::net::Ipv4Addr>().is_ok();
    let scheme = match (local, host.contains('.')) {
        (true, _) => "http",
        (false, true) => "https",
        // A name without a dot is a word, unless a port says it is a
        // machine.
        (false, false) if numbered => "http",
        (false, false) => return None,
    };
    let url = format!("{scheme}://{text}");
    is_page_url(&url).then_some(url)
}

/// What a page says of itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The address of what the page shows, which changes as links are
    /// followed.
    pub url: Option<String>,
    pub title: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub loading: bool,
    /// Why the page asked for last is not shown, until another is asked
    /// for.
    pub failure: Option<PageFailure>,
}

/// A page that could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageFailure {
    /// The address that could not be loaded, which is the one a page
    /// was sent on to if it was.
    pub url: String,
    /// What the system says went wrong, in the language of the user.
    pub reason: String,
}

/// A milestone in loading a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadPhase {
    Requested,
    Started,
    Committed,
    Finished,
    Failed,
}

/// Timestamps recorded for a single page load.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadTiming {
    pub url: Option<String>,
    pub requested: Option<Instant>,
    pub started: Option<Instant>,
    pub committed: Option<Instant>,
    pub finished: Option<Instant>,
    pub failed: Option<Instant>,
}

impl LoadTiming {
    /// The earliest recorded timestamp, used as the baseline for elapsed durations.
    pub fn origin(&self) -> Option<Instant> {
        self.requested
            .or(self.started)
            .or(self.committed)
            .or(self.finished)
            .or(self.failed)
    }

    /// Formats a single-line summary with the origin of `url` and elapsed
    /// milliseconds from [`Self::origin`].
    pub fn format_log_line(&self) -> String {
        let origin = self.origin();
        let ms = |at: Option<Instant>| -> String {
            match (origin, at) {
                (Some(base), Some(at)) => {
                    format!("{}ms", at.saturating_duration_since(base).as_millis())
                }
                _ => "-".to_owned(),
            }
        };
        // Only where the page is from: the path, the query, the fragment
        // and the user of an address may hold what is secret.
        let page_origin = self
            .url
            .as_deref()
            .and_then(|url| url::Url::parse(url).ok())
            .map(|url| url.origin())
            .filter(url::Origin::is_tuple)
            .map_or_else(|| "-".to_owned(), |origin| origin.ascii_serialization());
        let end = if self.failed.is_some() {
            format!("fail={}", ms(self.failed))
        } else {
            format!("finish={}", ms(self.finished))
        };
        format!(
            "benterm_browser_timing origin={page_origin} start={} commit={} {end}",
            ms(self.started),
            ms(self.committed),
        )
    }
}

fn log_load_timing(timing: &LoadTiming) {
    let Some(target) = std::env::var_os("BENTERM_BROWSER_TIMING") else {
        return;
    };
    if target.is_empty() {
        return;
    }
    write_timing_line(&target, &timing.format_log_line());
}

/// Writes `line` to `target`: the standard error for `1` or `stderr`,
/// else the end of the file at that path.
fn write_timing_line(target: &std::ffi::OsStr, line: &str) {
    if target == "1" || target == "stderr" {
        eprintln!("{line}");
    } else if let Ok(mut file) = {
        use std::os::unix::fs::OpenOptionsExt;
        // The pages a user opens are the user's to read alone.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(target)
    } {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// What a page tells its pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEvent {
    /// Something of its [`PageState`] changed.
    Changed,
    /// WebKit lost the process that renders this page.
    ProcessTerminated,
    /// It took the keyboard for itself, as when it is clicked.
    TookKeyboard,
    /// A page load reached a milestone.
    LoadPhase { phase: LoadPhase, at: Instant },
}

/// A command of the Edit menu that a page does in what has its keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEdit {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// The view that shows a web page, as a browser pane uses it.
pub trait Page {
    /// Starts loading the page at `url`.
    fn load(&mut self, url: &str);
    fn go_back(&mut self);
    fn go_forward(&mut self);
    fn reload(&mut self);
    fn state(&self) -> PageState;
    /// Shows the page at `place`, in the window's coordinates, or hides
    /// it. A hidden page does not keep the keyboard.
    fn place(&mut self, place: Option<Bounds<Pixels>>);
    /// Gives the page the keyboard, or hands the keyboard back to the
    /// window if the page has it.
    fn set_keyboard(&mut self, on: bool);
    /// Whether the keyboard is in the page now.
    fn has_keyboard(&self) -> bool;
    /// Does `edit` in the page, as AppKit does a command of the Edit menu
    /// in the view with its keyboard.
    fn edit(&mut self, edit: PageEdit);
}

/// Makes the page of a new browser pane in a window. The page tells the
/// pane what happens to it through the sender.
pub type OpenPage = Rc<dyn Fn(&Window, UnboundedSender<PageEvent>) -> Box<dyn Page>>;

struct PageOpener(OpenPage);

impl Global for PageOpener {}

/// Makes the pages of the browser panes opened from now on with `open`
/// instead of WebKit.
pub fn set_page_opener(cx: &mut App, open: OpenPage) {
    cx.set_global(PageOpener(open));
}

/// A page where there is no WebKit view to show one: in a window that is
/// not on a screen. It keeps the address it was given.
#[derive(Default)]
struct NoPage(PageState);

impl Page for NoPage {
    fn load(&mut self, url: &str) {
        self.0.url = Some(url.to_owned());
    }

    fn go_back(&mut self) {}

    fn go_forward(&mut self) {}

    fn reload(&mut self) {}

    fn state(&self) -> PageState {
        self.0.clone()
    }

    fn place(&mut self, _: Option<Bounds<Pixels>>) {}

    fn set_keyboard(&mut self, _: bool) {}

    fn has_keyboard(&self) -> bool {
        false
    }

    fn edit(&mut self, _: PageEdit) {}
}

fn open_page(
    window: &Window,
    events: UnboundedSender<PageEvent>,
    initial_place: Option<Bounds<Pixels>>,
    cx: &App,
) -> Box<dyn Page> {
    if let Some(opener) = cx.try_global::<PageOpener>() {
        return (opener.0)(window, events);
    }
    webkit::page_in(window, events, initial_place).unwrap_or_else(|| Box::new(NoPage::default()))
}

/// Why no browser pane was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserError {
    /// The address is not one of a web page; see [`is_page_url`].
    NotAPage,
    UnknownPane,
    /// The tab is too small for another pane.
    Layout(LayoutError),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPage => {
                write!(f, "the address is not one of a web page (http or https)")
            }
            Self::UnknownPane => write!(f, "there is no such pane"),
            Self::Layout(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for BrowserError {}

/// What a browser pane tells the grid it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserEvent {
    /// The title or the address of the page changed.
    TitleChanged,
    /// The page took the keyboard, so its pane is the one in use.
    TookKeyboard,
    /// The cross of the bar was pressed: the pane is to be closed.
    CloseRequested,
}

/// How far the page stays inside its pane at the left, the right and the
/// bottom. The page takes every press on it, so it leaves free where a
/// border between panes is grabbed, and with it the outlines the grid
/// draws around a pane.
const OUTLINE: Pixels = crate::pane_grid::GRAB;

/// The height of the address bar at the top of a browser pane.
fn bar_height(theme: &Theme) -> Pixels {
    px(theme.sidebar.font_size + 17.)
}

/// Computes the initial page area inside `pane` (below the address bar and
/// inset by [`OUTLINE`]), or `None` if the resulting area has non-positive
/// width or height.
pub fn initial_page_bounds(pane: Bounds<Pixels>, theme: &Theme) -> Option<Bounds<Pixels>> {
    let bar = bar_height(theme);
    let width = pane.size.width - OUTLINE * 2.;
    let height = pane.size.height - bar - OUTLINE;
    if width <= px(0.) || height <= px(0.) {
        return None;
    }
    Some(Bounds::new(
        pane.origin + point(OUTLINE, bar),
        size(width, height),
    ))
}

/// Where a frame drew the page of a browser pane.
#[derive(Clone, Copy)]
struct Drawn {
    frame: u64,
    place: Bounds<Pixels>,
    /// Whether GPUI's focus was on the page.
    focused: bool,
}

/// A pane that shows a web page under a bar with its address.
pub struct BrowserView {
    page: Box<dyn Page>,
    /// What the page said of itself last.
    state: PageState,
    process_reloads: VecDeque<Instant>,
    process_failure: Option<PageFailure>,
    /// The address to load once the page is shown for the first time: a
    /// page nobody has looked at yet is not fetched.
    unloaded: Option<String>,
    focus: FocusHandle,
    theme: Theme,
    drawn: Option<Drawn>,
    /// Where the page was put last; `None` while it is hidden.
    placed: Option<Bounds<Pixels>>,
    /// Whether GPUI's focus was on the page when it was settled last.
    focused: bool,
    /// The field the address is typed in.
    address: Entity<NameEditor>,
    /// Whether the field is shown, in place of the page's address.
    editing: bool,
    /// Load timing currently in progress, if any.
    current_load: Option<LoadTiming>,
    /// Most recently completed (finished or failed) load timing.
    last_load: Option<LoadTiming>,
    _subscriptions: [Subscription; 2],
    _events: Task<()>,
}

impl BrowserView {
    /// A pane that shows the page at `url`, or no page without one.
    pub fn new(
        url: Option<&str>,
        theme: Theme,
        initial_place: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (sender, mut events) = unbounded();
        let page = open_page(window, sender, initial_place, cx);
        let unloaded = url.filter(|url| is_page_url(url)).map(str::to_owned);
        let state = PageState {
            url: unloaded.clone(),
            ..page.state()
        };
        let told = cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let alive = this.update(cx, |this: &mut Self, cx| this.on_page_event(event, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        let id = window.window_handle().window_id();
        let this = cx.weak_entity();
        cx.default_global::<Frames>().views.push((id, this));
        let field = theme.sidebar.clone();
        let address = cx.new(|cx| {
            NameEditor::new("", field, cx)
                .with_placeholder("Address")
                .without_underline()
        });
        let typed = cx.subscribe_in(&address, window, |this, _, event, window, cx| match event {
            NameEditorEvent::Confirmed(text) => this.open_typed(text, window, cx),
            NameEditorEvent::Cancelled => this.focus_page(window, cx),
        });
        let handle = address.read(cx).focus_handle().clone();
        // The keyboard left the field: the page's address shows again.
        let left = cx.on_blur(&handle, window, |this, _, cx| {
            this.editing = false;
            cx.notify();
        });
        Self {
            page,
            state,
            process_reloads: VecDeque::new(),
            process_failure: None,
            unloaded,
            focus: cx.focus_handle(),
            theme,
            drawn: None,
            placed: None,
            focused: false,
            address,
            editing: false,
            current_load: None,
            last_load: None,
            _subscriptions: [typed, left],
            _events: told,
        }
    }

    /// The handle GPUI's focus is on while the page has the keyboard.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The address of what the page shows.
    pub fn url(&self) -> Option<String> {
        self.state.url.clone()
    }

    /// What a tab calls the pane: the title of the page, or its address
    /// while it has none.
    pub fn title(&self) -> String {
        let title = self
            .state
            .title
            .as_deref()
            .filter(|title| !title.is_empty());
        let named = title.or(self.state.url.as_deref());
        named.unwrap_or("Browser").to_owned()
    }

    /// Why the page asked for last is not shown, if it could not be
    /// loaded.
    pub fn failure(&self) -> Option<&PageFailure> {
        self.state.failure.as_ref()
    }

    /// Where the page is shown, in the window's coordinates; `None` while
    /// it is hidden.
    pub fn page_bounds(&self) -> Option<Bounds<Pixels>> {
        self.placed
    }

    /// The timing of the page load currently in progress, if any.
    pub fn current_load_timing(&self) -> Option<&LoadTiming> {
        self.current_load.as_ref()
    }

    /// The timing of the most recently completed (or failed) page load.
    pub fn last_load_timing(&self) -> Option<&LoadTiming> {
        self.last_load.as_ref()
    }

    /// Shows the page at `url`. An address that is not one of a web page
    /// is not opened.
    pub fn open(&mut self, url: &str, cx: &mut Context<Self>) -> Result<(), BrowserError> {
        if !is_page_url(url) {
            return Err(BrowserError::NotAPage);
        }
        self.load(url, cx);
        Ok(())
    }

    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        let field = theme.sidebar.clone();
        self.address
            .update(cx, |address, cx| address.set_theme(field, cx));
        self.theme = theme;
        cx.notify();
    }

    /// The handle GPUI's focus is on while the address is typed.
    pub fn address_focus(&self, cx: &App) -> FocusHandle {
        self.address.read(cx).focus_handle().clone()
    }

    /// What the bar shows as the address: what is typed while the
    /// keyboard is in the field, and the address of the page otherwise.
    pub fn address_text(&self, cx: &App) -> String {
        match self.editing {
            true => self.address.read(cx).text().to_owned(),
            false => self.state.url.clone().unwrap_or_default(),
        }
    }

    /// Moves the keyboard to the field for the address, with the address
    /// of the page in it to be typed over.
    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.state.url.clone().unwrap_or_default();
        self.address
            .update(cx, |address, cx| address.set_text(&url, cx));
        self.editing = true;
        let handle = self.address_focus(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Gives the keyboard to the page.
    fn focus_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens what was typed as an address. What is no address stays in
    /// the field.
    fn open_typed(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = address_url(text) {
            self.load(&url, cx);
            self.focus_page(window, cx);
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.state.can_go_back {
            self.process_failure = None;
            self.page.go_back();
            self.read_state(cx);
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if self.state.can_go_forward {
            self.process_failure = None;
            self.page.go_forward();
            self.read_state(cx);
        }
    }

    /// Does a command of the Edit menu in the page while the page has the
    /// keyboard and is not hidden behind why it could not be loaded. What is typed in the address is not the page's: the
    /// command goes on to the address and the views around it.
    fn edit(&mut self, edit: PageEdit, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) && self.state.failure.is_none() {
            self.page.edit(edit);
        } else {
            cx.propagate();
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(failure) = self.process_failure.take() {
            self.load(&failure.url, cx);
            return;
        }
        self.page.reload();
        self.read_state(cx);
    }

    fn on_page_event(&mut self, event: PageEvent, cx: &mut Context<Self>) {
        match event {
            PageEvent::Changed => self.read_state(cx),
            PageEvent::ProcessTerminated => self.recover_page(cx),
            // The page says so a moment after it took the keyboard. A
            // page that has been hidden since, or that the keyboard has
            // left again, does not take the focus from where it went.
            PageEvent::TookKeyboard => {
                if self.placed.is_some() && self.page.has_keyboard() {
                    cx.emit(BrowserEvent::TookKeyboard);
                }
            }
            PageEvent::LoadPhase { phase, at } => self.record_load_phase(phase, at),
        }
    }

    fn record_load_phase(&mut self, phase: LoadPhase, at: Instant) {
        let state = self.page.state();
        let current_url = || state.url.clone().or_else(|| self.state.url.clone());
        match phase {
            LoadPhase::Requested => {
                self.current_load = Some(LoadTiming {
                    url: current_url(),
                    requested: Some(at),
                    started: None,
                    committed: None,
                    finished: None,
                    failed: None,
                });
            }
            LoadPhase::Started => {
                if let Some(timing) = self
                    .current_load
                    .as_mut()
                    .filter(|timing| timing.requested.is_some() && timing.started.is_none())
                {
                    timing.started = Some(at);
                    if let Some(url) = current_url() {
                        timing.url = Some(url);
                    }
                } else {
                    self.current_load = Some(LoadTiming {
                        url: current_url(),
                        requested: None,
                        started: Some(at),
                        committed: None,
                        finished: None,
                        failed: None,
                    });
                }
            }
            LoadPhase::Committed => {
                let timing = self.current_load.get_or_insert_with(LoadTiming::default);
                timing.committed = Some(at);
                if let Some(url) = current_url() {
                    timing.url = Some(url);
                }
            }
            LoadPhase::Finished => {
                let mut timing = self.current_load.take().unwrap_or_default();
                timing.finished = Some(at);
                if let Some(url) = current_url() {
                    timing.url = Some(url);
                }
                log_load_timing(&timing);
                self.last_load = Some(timing);
            }
            LoadPhase::Failed => {
                let mut timing = self.current_load.take().unwrap_or_default();
                timing.failed = Some(at);
                if let Some(url) = state
                    .failure
                    .map(|failure| failure.url)
                    .filter(|url| !url.is_empty())
                    .or_else(current_url)
                {
                    timing.url = Some(url);
                }
                log_load_timing(&timing);
                self.last_load = Some(timing);
            }
        }
    }

    fn load(&mut self, url: &str, cx: &mut Context<Self>) {
        self.process_failure = None;
        self.unloaded = None;
        self.page.load(url);
        self.read_state(cx);
    }

    fn read_state(&mut self, cx: &mut Context<Self>) {
        let mut state = self.page.state();
        if let Some(failure) = &self.process_failure {
            state.failure = Some(failure.clone());
            state.loading = false;
        }
        if let Some(url) = &self.unloaded {
            // The page has not been asked for it yet.
            state.url = Some(url.clone());
        }
        if state != self.state {
            self.state = state;
            cx.emit(BrowserEvent::TitleChanged);
            cx.notify();
        }
    }

    fn recover_page(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self.page.state().url.or_else(|| self.state.url.clone()) else {
            return;
        };
        let now = Instant::now();
        self.process_reloads
            .retain(|attempt| now.duration_since(*attempt) < Duration::from_secs(30));
        if self.process_reloads.len() < 3 {
            self.process_reloads.push_back(now);
            self.load(&url, cx);
        } else {
            self.process_failure = Some(PageFailure {
                url,
                reason: "The page process stopped. Click Reload (↻) to try again.".into(),
            });
            self.read_state(cx);
        }
    }

    /// Puts the page where frame `frame` drew it, or hides it if the
    /// frame did not draw it or drew one of `covers` over it, or if the
    /// page could not be loaded, and gives it the keyboard if GPUI's
    /// focus went to it.
    fn settle(&mut self, frame: u64, covers: &[Bounds<Pixels>], cx: &mut Context<Self>) {
        let drawn = self.drawn.filter(|drawn| drawn.frame == frame);
        let place = drawn
            .map(|drawn| drawn.place)
            .filter(|place| place.size.width > px(0.) && place.size.height > px(0.))
            .filter(|place| !covers.iter().any(|cover| cover.intersects(place)))
            .filter(|_| self.state.failure.is_none());
        if place != self.placed {
            self.placed = place;
            self.page.place(place);
        }
        if place.is_some()
            && let Some(url) = self.unloaded.take()
        {
            self.load(&url, cx);
        }
        // Only when GPUI's focus comes or goes: a page that was clicked
        // has the keyboard before GPUI's focus follows, and keeps it
        // meanwhile.
        let focused = place.is_some() && drawn.is_some_and(|drawn| drawn.focused);
        if focused != self.focused {
            self.focused = focused;
            self.page.set_keyboard(focused);
        }
    }

    fn bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme.sidebar;
        let tooltip_theme = self.theme.menu.clone();
        let button = |name: &'static str, label: &'static str, enabled: bool| {
            let color = match enabled {
                true => theme.item_text,
                false => theme.item_secondary_text,
            };
            div()
                .id(name)
                .debug_selector(move || name.into())
                .flex_none()
                .px_1()
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .text_color(hsla(color))
                .when(enabled, |button| {
                    button
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(theme.item_hover_background)))
                })
                .child(label)
        };
        let address = match self.editing {
            true => self.address.clone().into_any_element(),
            false => div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(self.state.url.clone().unwrap_or_default())
                .into_any_element(),
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .h(bar_height(&self.theme))
            .bg(hsla(theme.background))
            .border_b_1()
            .border_color(hsla(theme.border))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .text_color(hsla(theme.item_text))
            .child(
                button("browser-back", "←", self.state.can_go_back)
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                button("browser-forward", "→", self.state.can_go_forward)
                    .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            )
            .child(
                button("browser-reload", "↻", true)
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
            .child(
                div()
                    .id("browser-address")
                    .debug_selector(|| "browser-address".into())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .px_2()
                    .py(px(1.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .bg(hsla(theme.field_background))
                    // A field is known by its edge, which is the accent
                    // while it is typed into.
                    .border_1()
                    .border_color(hsla(if self.editing {
                        theme.item_active_indicator
                    } else {
                        theme.border
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if !this.editing {
                                // After the grid, which gives the page
                                // the keyboard at a press in its pane.
                                cx.defer_in(window, Self::focus_address);
                            }
                        }),
                    )
                    .child(address),
            )
            .when(self.state.loading, |bar| {
                bar.child(
                    div()
                        .debug_selector(|| "browser-loading".into())
                        .flex_none()
                        .text_color(hsla(theme.item_secondary_text))
                        .child("Loading…"),
                )
            })
            // Closes this pane, as Cmd-W does the focused one.
            .child(
                button("browser-close", "✕", true)
                    .tooltip(crate::tooltip::Tooltip::with_shortcut(
                        "Close",
                        "⌘W",
                        tooltip_theme,
                    ))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(BrowserEvent::CloseRequested))),
            )
    }

    /// What shows in place of the page while it is hidden: why it could
    /// not be loaded, or else its address.
    fn note(&self) -> gpui::AnyElement {
        let Some(failure) = &self.state.failure else {
            return match &self.state.url {
                Some(url) => url.clone().into_any_element(),
                None => "Type an address in the bar above".into_any_element(),
            };
        };
        div()
            .debug_selector(|| "browser-failure".into())
            .max_w_full()
            .px_4()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_color(hsla(self.theme.sidebar.item_text))
                    .child("This page could not be opened"),
            )
            .child(failure.url.clone())
            .child(failure.reason.clone())
            .into_any_element()
    }
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let focus = self.focus.clone();
        let note = self.note();
        let bar = self.bar(cx).into_any_element();
        let theme = &self.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &FocusAddress, window, cx| this.focus_address(window, cx)),
            )
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.reload(cx)))
            .on_action(
                cx.listener(|this, _: &Undo, window, cx| this.edit(PageEdit::Undo, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Redo, window, cx| this.edit(PageEdit::Redo, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Cut, window, cx| this.edit(PageEdit::Cut, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Copy, window, cx| this.edit(PageEdit::Copy, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Paste, window, cx| this.edit(PageEdit::Paste, window, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectAll, window, cx| {
                this.edit(PageEdit::SelectAll, window, cx)
            }))
            .bg(hsla(theme.terminal.background))
            .child(bar)
            .child(
                // What shows while the page is hidden, and where it is
                // put while it is not.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .font_family(theme.sidebar.font_family.clone())
                    .text_size(px(theme.sidebar.font_size))
                    .text_color(hsla(theme.sidebar.item_secondary_text))
                    .child(note)
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let place = Bounds::new(
                                    bounds.origin + point(OUTLINE, px(0.)),
                                    size(
                                        bounds.size.width - OUTLINE * 2.,
                                        bounds.size.height - OUTLINE,
                                    ),
                                );
                                let focused = focus.is_focused(window);
                                let id = window.window_handle().window_id();
                                let frame = cx.default_global::<Frames>().frame(id);
                                view.update(cx, |view, _| {
                                    view.drawn = Some(Drawn {
                                        frame,
                                        place,
                                        focused,
                                    });
                                });
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
    }
}

/// The frames of the windows, as far as their browser panes go.
#[derive(Default)]
struct Frames {
    /// The number of the frame each window draws or drew last.
    frames: HashMap<WindowId, u64>,
    /// What the frame drew over the pages, in the window's coordinates.
    covers: HashMap<WindowId, Vec<Bounds<Pixels>>>,
    /// The browser panes, each with its window.
    views: Vec<(WindowId, WeakEntity<BrowserView>)>,
}

impl Global for Frames {}

impl Frames {
    fn frame(&self, window: WindowId) -> u64 {
        self.frames.get(&window).copied().unwrap_or(0)
    }
}

/// Starts a frame of `window`. Call it when the window's root is drawn.
/// Once the frame is drawn, every page of the window is put where the
/// frame drew its pane, and the pages the frame did not draw are hidden.
pub(crate) fn begin_frame(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let frames = cx.default_global::<Frames>();
    frames.views.retain(|(_, view)| view.upgrade().is_some());
    if !frames.views.iter().any(|(window, _)| *window == id) {
        // No browser pane: nothing to settle after the frame.
        return;
    }
    let frame = frames.frame(id) + 1;
    frames.frames.insert(id, frame);
    frames.covers.remove(&id);
    // After the frame: the views cannot be updated while they are drawn.
    window.defer(cx, move |_, cx| settle(id, frame, cx));
}

fn settle(window: WindowId, frame: u64, cx: &mut App) {
    let frames = cx.default_global::<Frames>();
    if frames.frame(window) != frame {
        // A later frame was drawn already, and settles for this one too.
        return;
    }
    let covers = frames.covers.get(&window).cloned().unwrap_or_default();
    let views: Vec<_> = frames
        .views
        .iter()
        .filter(|(of, _)| *of == window)
        .filter_map(|(_, view)| view.upgrade())
        .collect();
    for view in views {
        view.update(cx, |view, cx| view.settle(frame, &covers, cx));
    }
}

/// An element that tells the pages of the window that what it is a
/// child of is drawn over them: a page under it is hidden for as long as
/// it is drawn, since nothing shows in front of a page. Its parent must
/// be positioned.
pub(crate) fn covers() -> impl IntoElement {
    canvas(
        |bounds, window, cx| {
            let id = window.window_handle().window_id();
            let frames = cx.default_global::<Frames>();
            frames.covers.entry(id).or_default().push(bounds);
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_page_bounds_insets_pane_below_bar() {
        let theme = Theme::default();
        let pane = Bounds::new(point(px(200.), px(40.)), size(px(600.), px(500.)));
        let place = initial_page_bounds(pane, &theme).expect("non-empty pane");
        let bar = bar_height(&theme);
        assert_eq!(place.origin, point(px(200.) + OUTLINE, px(40.) + bar));
        assert_eq!(place.size.width, px(600.) - OUTLINE * 2.);
        assert_eq!(place.size.height, px(500.) - bar - OUTLINE);
    }

    #[test]
    fn initial_page_bounds_returns_none_when_zero_or_too_small() {
        let theme = Theme::default();
        assert_eq!(initial_page_bounds(Bounds::default(), &theme), None);
        let tiny = Bounds::new(point(px(0.), px(0.)), size(px(4.), px(20.)));
        assert_eq!(initial_page_bounds(tiny, &theme), None);
    }

    #[test]
    fn load_timing_formats_success_and_failure_lines() {
        let t0 = Instant::now();
        let ok = LoadTiming {
            url: Some("https://example.com".into()),
            requested: Some(t0),
            started: Some(t0 + Duration::from_millis(10)),
            committed: Some(t0 + Duration::from_millis(45)),
            finished: Some(t0 + Duration::from_millis(120)),
            failed: None,
        };
        assert_eq!(
            ok.format_log_line(),
            "benterm_browser_timing origin=https://example.com start=10ms commit=45ms finish=120ms"
        );

        let failed = LoadTiming {
            url: Some("https://example.com/bad".into()),
            requested: Some(t0),
            started: Some(t0 + Duration::from_millis(5)),
            committed: None,
            finished: None,
            failed: Some(t0 + Duration::from_millis(30)),
        };
        assert_eq!(
            failed.format_log_line(),
            "benterm_browser_timing origin=https://example.com start=5ms commit=- fail=30ms"
        );

        let in_page = LoadTiming {
            url: Some("https://example.com/next".into()),
            requested: None,
            started: Some(t0),
            committed: Some(t0 + Duration::from_millis(20)),
            finished: Some(t0 + Duration::from_millis(80)),
            failed: None,
        };
        assert_eq!(
            in_page.format_log_line(),
            "benterm_browser_timing origin=https://example.com start=0ms commit=20ms finish=80ms"
        );
    }

    /// The query, the fragment, the path and the user of an address may
    /// hold what is secret; the log keeps only where the page is from.
    #[test]
    fn load_timing_logs_only_the_origin_of_the_address() {
        let t0 = Instant::now();
        let timing = |url: &str| LoadTiming {
            url: Some(url.into()),
            started: Some(t0),
            finished: Some(t0),
            ..LoadTiming::default()
        };
        assert_eq!(
            timing("https://user:pw@example.com:8443/reset?token=s3cret#k=v").format_log_line(),
            "benterm_browser_timing origin=https://example.com:8443 start=0ms commit=- finish=0ms"
        );
        assert_eq!(
            timing("about:blank").format_log_line(),
            "benterm_browser_timing origin=- start=0ms commit=- finish=0ms"
        );
    }

    #[test]
    fn the_timing_log_file_is_made_for_the_user_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timing.log");
        write_timing_line(path.as_os_str(), "one");
        write_timing_line(path.as_os_str(), "two");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
