//! The progress board: the states of every agent of the window over
//! time, one row per pane, as in the mock's progress board.
//!
//! The board takes the records of the agents' history from the window:
//! all of them when it is shown, and those recorded since whenever the
//! window records more while it is shown. Time moves the bands on by
//! itself, with a timer that runs only while the board is shown and
//! wakes once for every pixel or so the present moves.
//!
//! The same panel shows the graph of the agents instead, when the user
//! chooses it: who started whom, and what they told each other; see
//! [`relations`]. The timeline does not follow the time meanwhile.
//!
//! Under the panel, whichever of the two it shows, the kanban of the
//! tasks puts each task in a column by what its agents and its review
//! did; see [`columns`].
//!
//! The board is a cached view: it is drawn again when it changes, and
//! not with the output of the terminals. A band that begins while the
//! board is shown pulses twice, so that a change catches the eye.

mod columns;
mod relations;

use std::cell::Cell;
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use benterm_layout::PaneId;
use benterm_session::history::Record;
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, ElementId, EventEmitter, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, actions, div,
    px, relative,
};

use crate::graph::{Message, Node};
use crate::kanban::{BoardTask, Card};
use crate::terminal_view::hsla;
use crate::theme::BoardTheme;
use crate::timeline::{
    BandState, Lane, TimeSpan, band_x, lanes, stalled_stretch, tick_label, time_x, visible_lanes,
};

actions!(
    progress_board,
    [
        WiderSpan,
        NarrowerSpan,
        SelectNextLane,
        SelectPreviousLane,
        /// Goes to the pane of the selected row.
        OpenLane,
        /// Shows the terminal panes again.
        LeaveProgressBoard,
        ShowTimeline,
        ShowRelations,
        ToggleEndedAgents,
        /// Selects the next arrow of the graph.
        SelectNextMessage,
    ]
);

/// The key context of the board, which its keys are limited to.
const KEY_CONTEXT: &str = "ProgressBoard";

const PADDING_X: f32 = 18.;
const PADDING_Y: f32 = 14.;
/// The width of the agents' names left of the timeline.
const LABEL_WIDTH: f32 = 150.;
/// The height of a row, and of the band in it, as in the mock.
const ROW_HEIGHT: f32 = 35.;
const BAND_HEIGHT: f32 = 22.;
/// The opacity of a band: done is faint, the others strong.
const BAND_OPACITY: f32 = 0.9;
const DONE_OPACITY: f32 = 0.35;
/// The space between a band and the next.
const BAND_GAP: f32 = 1.;
/// How long a band that just began pulses.
const PULSE: Duration = Duration::from_millis(1400);
/// The least time between two moves of the bands with time.
const LEAST_TICK: Duration = Duration::from_secs(1);
/// About how many pixels wide a timeline is, for how often it moves.
const TYPICAL_WIDTH: u32 = 1000;
/// How often the graph looks whether an arrow has grown fainter.
const GRAPH_TICK: Duration = Duration::from_secs(5);

/// Binds the keys of the board while it has the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("-", WiderSpan, context),
        KeyBinding::new("=", NarrowerSpan, context),
        KeyBinding::new("down", SelectNextLane, context),
        KeyBinding::new("up", SelectPreviousLane, context),
        KeyBinding::new("enter", OpenLane, context),
        KeyBinding::new("escape", LeaveProgressBoard, context),
        KeyBinding::new("t", ShowTimeline, context),
        KeyBinding::new("r", ShowRelations, context),
        KeyBinding::new("h", ToggleEndedAgents, context),
        KeyBinding::new("tab", SelectNextMessage, context),
    ]);
}

/// What the board tells the window.
pub enum BoardEvent {
    /// The panes should be shown again.
    Leave,
    /// The user chose the row of this pane.
    Open(PaneId),
    /// The user chose the node of the terminal whose pane id has this
    /// number, in a pane or behind the window.
    OpenTerminal(u64),
    /// The board wants the graph of the window as it is now.
    Refresh,
}

/// What the panel of the board shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BoardMode {
    #[default]
    Timeline,
    /// The graph of the agents: who started whom, and their messages.
    Relations,
}

impl BoardMode {
    const ALL: [BoardMode; 2] = [Self::Timeline, Self::Relations];

    fn label(self) -> &'static str {
        match self {
            Self::Timeline => "Timeline",
            Self::Relations => "Relations",
        }
    }

    /// The name of the mode's button in tests.
    fn selector(self) -> &'static str {
        match self {
            Self::Timeline => "board-mode-timeline",
            Self::Relations => "board-mode-relations",
        }
    }
}

/// Where the pane of a row is now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LanePlace {
    pub(crate) pane: PaneId,
    /// The name of the workspace, or of the agent behind the window.
    pub(crate) name: String,
    /// The last change of the pane's agent, in milliseconds since 1970,
    /// when the agent is marked as stalled.
    pub(crate) stalled_since_ms: Option<u64>,
}

/// The board as a view.
pub struct ProgressBoard {
    records: Vec<Record>,
    /// Made from `records` at `now_ms`.
    lanes: Vec<Lane>,
    /// The panes of this run, by their key in the history.
    places: HashMap<String, LanePlace>,
    span: TimeSpan,
    now_ms: u64,
    /// When the board was last shown: bands that begin later pulse.
    shown_ms: Option<u64>,
    /// The row the keyboard is on, by its key.
    selected: Option<String>,
    mode: BoardMode,
    /// The graph, as the window handed it last.
    nodes: Vec<Node>,
    messages: Vec<Message>,
    /// Since when the graph has been in view; none while it is not.
    graph_since_ms: Option<u64>,
    /// The present of the graph, which the arrows grow fainter by.
    graph_now_ms: u64,
    /// How much each arrow stood out when the graph was last drawn.
    looks: Vec<f32>,
    hide_ended: bool,
    /// The selected arrow, by its two ends.
    selected_arrow: Option<(u64, u64)>,
    selected_node: Option<u64>,
    /// When the state of a node last changed while the board had it.
    changed: HashMap<u64, u64>,
    /// Where in the window the graph was last drawn, for the clicks on
    /// its arrows.
    graph_origin: Rc<Cell<Option<Point<Pixels>>>>,
    /// How wide the place of the graph was when it was last drawn; 0
    /// before that.
    graph_width: Rc<Cell<f32>>,
    /// The tasks of the window, as it handed them last.
    tasks: Vec<BoardTask>,
    show_completed: bool,
    repository: Option<std::path::PathBuf>,
    pull_requests: HashMap<u64, benterm_github::PullRequest>,
    pr_store: Option<std::sync::Arc<benterm_task::TaskStore>>,
    pr_branches: Vec<(u64, String, String)>,
    last_pr_refresh: Option<std::time::Instant>,
    pr_refresh: Option<Task<()>>,
    /// Made from `tasks` and `records`.
    cards: Vec<Card>,
    /// When each task last changed its column, while the board had it.
    moved: HashMap<u64, u64>,
    /// How far the clock of the user is ahead of UTC, in seconds.
    utc_offset: i64,
    theme: BoardTheme,
    focus: FocusHandle,
    _tick: Option<Task<()>>,
}

impl EventEmitter<BoardEvent> for ProgressBoard {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// How many seconds the clock of the user is ahead of UTC now.
fn utc_offset() -> i64 {
    // SAFETY: `time` takes a null pointer for "do not store", and
    // `localtime_r` writes only into the `tm` it is given.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut local: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut local).is_null() {
            return 0;
        }
        local.tm_gmtoff as i64
    }
}

impl ProgressBoard {
    pub(crate) fn new(theme: BoardTheme, cx: &mut Context<Self>) -> Self {
        Self {
            records: Vec::new(),
            lanes: Vec::new(),
            places: HashMap::new(),
            span: TimeSpan::default(),
            now_ms: now_ms(),
            shown_ms: None,
            selected: None,
            mode: BoardMode::default(),
            nodes: Vec::new(),
            messages: Vec::new(),
            graph_since_ms: None,
            graph_now_ms: now_ms(),
            looks: Vec::new(),
            hide_ended: false,
            selected_arrow: None,
            selected_node: None,
            changed: HashMap::new(),
            graph_origin: Rc::default(),
            graph_width: Rc::default(),
            tasks: Vec::new(),
            show_completed: false,
            repository: None,
            pull_requests: HashMap::new(),
            pr_store: None,
            pr_branches: Vec::new(),
            last_pr_refresh: None,
            pr_refresh: None,
            cards: Vec::new(),
            moved: HashMap::new(),
            utc_offset: utc_offset(),
            theme,
            focus: cx.focus_handle(),
            _tick: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Whether the panel shows the timeline or the graph.
    pub fn mode(&self) -> BoardMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: BoardMode, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.graph_since_ms = self.graph_in_view().then(now_ms);
        if self.shown_ms.is_some() {
            // Each of the two follows the time in its own way.
            self.tick(cx);
            self.start_ticking(cx);
        }
        cx.notify();
    }

    fn graph_in_view(&self) -> bool {
        self.shown_ms.is_some() && self.mode == BoardMode::Relations
    }

    /// How far back the timeline goes.
    pub fn span(&self) -> TimeSpan {
        self.span
    }

    pub fn set_span(&mut self, span: TimeSpan, cx: &mut Context<Self>) {
        if span != self.span {
            self.span = span;
            if self.shown_ms.is_some() {
                self.start_ticking(cx);
            }
            cx.notify();
        }
    }

    /// The cards of the kanban, in the order of the tasks.
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// Takes made-up tasks, and records of their agents among those the
    /// board has, for a picture of the board.
    pub fn take_made_up_tasks(
        &mut self,
        tasks: Vec<BoardTask>,
        records: Vec<Record>,
        cx: &mut Context<Self>,
    ) {
        self.records.extend(records);
        self.records.sort_by_key(|record| record.at_ms);
        self.now_ms = now_ms();
        self.lanes = lanes(&self.records, self.now_ms);
        self.take_tasks(tasks, cx);
        cx.notify();
    }

    /// Takes the tasks of the window as they are now.
    pub(crate) fn take_tasks(&mut self, tasks: Vec<BoardTask>, cx: &mut Context<Self>) {
        if tasks == self.tasks {
            return;
        }
        self.tasks = tasks;
        self.deal();
        cx.notify();
    }

    /// The rows the board shows now, top to bottom.
    pub fn shown_lanes(&self) -> Vec<&Lane> {
        visible_lanes(&self.lanes, self.now_ms, self.span.length()).collect()
    }

    /// The time of the last record the board has.
    pub(crate) fn last_record_ms(&self) -> Option<u64> {
        self.records.last().map(|record| record.at_ms)
    }

    /// Takes the records made since [`last_record_ms`], and where the
    /// panes of this run are now.
    ///
    /// [`last_record_ms`]: Self::last_record_ms
    pub(crate) fn take_records(
        &mut self,
        records: Vec<Record>,
        places: HashMap<String, LanePlace>,
        cx: &mut Context<Self>,
    ) {
        if records.is_empty() && places == self.places {
            return;
        }
        self.records.extend(records);
        self.places = places;
        if self.deal() {
            cx.notify();
        }
        if self.mode == BoardMode::Timeline {
            self.now_ms = now_ms();
            self.lanes = lanes(&self.records, self.now_ms);
            cx.notify();
        }
    }

    /// Starts or stops following the time, as the board is shown or
    /// hidden.
    pub(crate) fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown == self.shown_ms.is_some() {
            return;
        }
        if shown {
            self.shown_ms = Some(now_ms());
            self.graph_since_ms = self.graph_in_view().then(now_ms);
            self.utc_offset = utc_offset();
            self.tick(cx);
            self.start_ticking(cx);
        } else {
            self.shown_ms = None;
            self.graph_since_ms = None;
            self._tick = None;
        }
    }

    pub(crate) fn set_theme(&mut self, theme: BoardTheme, cx: &mut Context<Self>) {
        if theme != self.theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// Wakes the board as often as the present moves by about a pixel
    /// of the timeline, or as an arrow of the graph may grow fainter.
    fn start_ticking(&mut self, cx: &mut Context<Self>) {
        let every = match self.mode {
            BoardMode::Timeline => (self.span.length() / TYPICAL_WIDTH).max(LEAST_TICK),
            BoardMode::Relations => GRAPH_TICK,
        };
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Moves the bands on to the present, or lets the arrows of the
    /// graph grow fainter.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.refresh_pull_requests(cx);
        // How long an agent has worked goes on in either mode.
        if self.deal() {
            cx.notify();
        }
        if self.mode == BoardMode::Relations {
            return self.age_graph(cx);
        }
        self.now_ms = now_ms();
        self.lanes = lanes(&self.records, self.now_ms);
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.shown_lanes().iter().map(|l| l.key.clone()).collect();
        if keys.is_empty() {
            return;
        }
        let position = self
            .selected
            .as_ref()
            .and_then(|key| keys.iter().position(|k| k == key));
        let next = match position {
            None if step > 0 => 0,
            None => keys.len() - 1,
            Some(position) => position.saturating_add_signed(step).min(keys.len() - 1),
        };
        self.selected = Some(keys[next].clone());
        cx.notify();
    }

    fn open(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(place) = self.places.get(key) {
            cx.emit(BoardEvent::Open(place.pane));
        }
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.selected.clone() {
            self.open(&key, cx);
        }
    }

    /// The name of a row: the agent, and where its pane is.
    fn lane_name(&self, lane: &Lane) -> String {
        let agent = lane.agent.map_or("program", |agent| agent.id());
        match self.places.get(&lane.key) {
            Some(place) if place.stalled_since_ms.is_some() => {
                format!("{agent} · {} · stalled", place.name)
            }
            Some(place) => format!("{agent} · {}", place.name),
            None => format!("{agent} · closed"),
        }
    }

    /// The times at the lines of the timeline, as shares of its width
    /// from the left, with what is written under them.
    fn ticks(&self) -> Vec<(f32, String)> {
        let length = self.span.length();
        let tick = self.span.tick();
        let count = (length.as_secs() / tick.as_secs()) as u32;
        (0..=count)
            .map(|n| {
                let before = tick * (count - n);
                let at = self.now_ms - before.as_millis() as u64;
                let share = time_x(at, self.now_ms, length, 1.);
                (share, tick_label(before))
            })
            .collect()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let spans = TimeSpan::ALL.into_iter().map(|span| {
            let active = span == self.span;
            let id = SharedString::from(format!("board-span-{}", span.label()));
            div()
                .id(ElementId::Name(id.clone()))
                .debug_selector(move || id.to_string())
                .px(px(6.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .cursor_pointer()
                .text_color(hsla(theme.label_text))
                .hover(|button| button.bg(hsla(theme.row_hover_background)))
                .when(active, |button| {
                    button
                        .bg(hsla(theme.range_active_background))
                        .text_color(hsla(theme.title_text))
                })
                .child(span.label())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.set_span(span, cx)),
                )
        });
        let modes = BoardMode::ALL.into_iter().map(|mode| {
            let active = mode == self.mode;
            div()
                .id(mode.selector())
                .debug_selector(|| mode.selector().into())
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(hsla(theme.label_text))
                .hover(|button| button.text_color(hsla(theme.title_text)))
                .when(active, |button| button.text_color(hsla(theme.title_text)))
                .child(mode.label())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)),
                )
        });
        // What goes with the mode: how far back the timeline goes, or
        // whether the graph leaves the ended agents out.
        let options = match self.mode {
            BoardMode::Timeline => div().flex().gap(px(2.)).children(spans),
            BoardMode::Relations => div().flex().child(self.ended_button(cx)),
        };
        let legend = BandState::ALL.into_iter().map(|state| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .size(px(10.))
                        .rounded(px(2.))
                        .bg(hsla(state.color(theme))),
                )
                .child(state.label())
        });
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .mb(px(8.))
            .child(div().flex().gap(px(10.)).children(modes))
            .child(options)
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .gap(px(12.))
                    .text_size(px(crate::theme::small_text(theme.font_size)))
                    .text_color(hsla(theme.label_text))
                    .children(legend),
            )
    }

    /// The track of a row: the lines at the ticks, the bands, and the
    /// line of the present.
    fn track(
        &self,
        position: usize,
        lane: &Lane,
        ticks: &[(f32, String)],
    ) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let length = self.span.length();
        let lines = ticks.iter().map(|(share, _)| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(*share))
                .w(px(1.))
                .bg(hsla(theme.grid_line))
        });
        let bands = lane.bands.iter().filter_map(|band| {
            let (left, right) = band_x(band, self.now_ms, length, 1.)?;
            let opacity = match band.state {
                BandState::Done => DONE_OPACITY,
                _ => BAND_OPACITY,
            };
            let element = div()
                .absolute()
                .top(px((ROW_HEIGHT - BAND_HEIGHT) / 2.))
                .h(px(BAND_HEIGHT))
                .left(relative(left))
                .w(relative(right - left))
                .min_w(px(2. + BAND_GAP))
                // A gap after each band, as in the mock.
                .pr(px(BAND_GAP))
                .opacity(opacity)
                .child(
                    div()
                        .size_full()
                        .rounded(px(3.))
                        .bg(hsla(band.state.color(theme))),
                );
            let new = self.shown_ms.is_some_and(|shown| band.start_ms > shown);
            Some(if new {
                let id = SharedString::from(format!("band-{}-{}", lane.key, band.start_ms));
                element
                    .with_animation(
                        ElementId::Name(id),
                        Animation::new(PULSE),
                        move |element, delta| {
                            // Two pulses down and back, ending as drawn.
                            let dip = 0.5 - 0.5 * (TAU * 2. * delta).cos();
                            element.opacity(opacity * (1. - 0.65 * dip))
                        },
                    )
                    .into_any_element()
            } else {
                element.into_any_element()
            })
        });
        // The end of the band of an agent at work that has stalled: an
        // outline in the color of waiting, as the agent waits for the
        // user to look.
        let stalled = self
            .places
            .get(&lane.key)
            .and_then(|place| place.stalled_since_ms)
            .and_then(|since| stalled_stretch(lane, since, self.now_ms))
            .and_then(|stretch| band_x(&stretch, self.now_ms, length, 1.))
            .map(|(left, right)| {
                div()
                    .debug_selector(move || format!("board-stalled-{position}"))
                    .absolute()
                    .top(px((ROW_HEIGHT - BAND_HEIGHT) / 2. - 2.))
                    .h(px(BAND_HEIGHT + 4.))
                    .left(relative(left))
                    .w(relative(right - left))
                    .min_w(px(4.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .border_2()
                    .border_dashed()
                    .border_color(hsla(theme.waiting))
            });
        div()
            .relative()
            .flex_1()
            .h_full()
            .children(lines)
            .children(bands)
            .children(stalled)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(1.))
                    .border_l_1()
                    .border_dashed()
                    .border_color(hsla(theme.now_line)),
            )
    }

    fn rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let ticks = self.ticks();
        self.shown_lanes()
            .into_iter()
            .enumerate()
            .map(|(position, lane)| {
                let key = lane.key.clone();
                let selected = self.selected.as_deref() == Some(key.as_str());
                div()
                    .id(ElementId::Name(format!("board-lane-{}", lane.key).into()))
                    .debug_selector(move || format!("board-lane-{position}"))
                    .flex()
                    .items_center()
                    .h(px(ROW_HEIGHT))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .hover(|row| row.bg(hsla(theme.row_hover_background)))
                    .when(selected, |row| row.bg(hsla(theme.row_hover_background)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(LABEL_WIDTH))
                            .pl(px(4.))
                            .overflow_hidden()
                            .text_color(hsla(theme.label_text))
                            .child(self.lane_name(lane)),
                    )
                    .child(self.track(position, lane, &ticks))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected = Some(key.clone());
                            this.open(&key, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The times under the timeline.
    fn axis(&self) -> impl IntoElement + use<> {
        const LABEL: f32 = 40.;
        let theme = &self.theme;
        let labels = self.ticks().into_iter().map(|(share, label)| {
            div()
                .absolute()
                .top_0()
                .left(relative(share))
                .ml(px(-LABEL / 2.))
                .w(px(LABEL))
                .flex()
                .justify_center()
                .child(label)
        });
        div()
            .flex()
            .h(px(18.))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.axis_text))
            .child(div().flex_none().w(px(LABEL_WIDTH)))
            .child(div().relative().flex_1().h_full().children(labels))
    }

    /// The panel with the graph in it, as high as the board has room.
    fn relations(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        div()
            .relative()
            .overflow_hidden()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(10.))
            .px(px(12.))
            .bg(hsla(theme.panel))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_PANEL))
            .child(crate::design::top_highlight_bar(theme.panel))
            .child(self.header(cx))
            .child(self.graph(cx))
    }

    fn timeline(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let rows = self.rows(cx);
        let body = if rows.is_empty() {
            div()
                .py(px(10.))
                .child(crate::design::empty_state(
                    format!("No agent was at work in the last {}.", self.span.label()),
                    &[("-", "Wider span"), ("r", "Relations"), ("Esc", "Back")],
                    theme.background,
                    theme.border,
                    theme.label_text,
                    theme.axis_text,
                    theme.font_size,
                ))
                .into_any_element()
        } else {
            div()
                .id("board-rows")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .children(rows)
                .into_any_element()
        };
        div()
            .relative()
            .overflow_hidden()
            .flex()
            .flex_col()
            .min_h_0()
            .p(px(10.))
            .px(px(12.))
            .bg(hsla(theme.panel))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_PANEL))
            .child(crate::design::top_highlight_bar(theme.panel))
            .child(self.header(cx))
            .child(body)
            .child(self.axis())
    }
}

impl Render for ProgressBoard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let relations = self.mode == BoardMode::Relations;
        let panel = match self.mode {
            BoardMode::Timeline => self.timeline(cx).into_any_element(),
            BoardMode::Relations => self.relations(cx).into_any_element(),
        };
        div()
            .id("progress-board")
            .debug_selector(|| "progress-board".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &WiderSpan, _, cx| this.set_span(this.span.wider(), cx)),
            )
            .on_action(
                cx.listener(|this, _: &NarrowerSpan, _, cx| {
                    this.set_span(this.span.narrower(), cx)
                }),
            )
            .on_action(
                cx.listener(move |this, _: &SelectNextLane, _, cx| match relations {
                    true => this.select_node_by(1, cx),
                    false => this.select_by(1, cx),
                }),
            )
            .on_action(
                cx.listener(move |this, _: &SelectPreviousLane, _, cx| match relations {
                    true => this.select_node_by(-1, cx),
                    false => this.select_by(-1, cx),
                }),
            )
            .on_action(
                cx.listener(move |this, _: &OpenLane, _, cx| match relations {
                    true => this.open_selected_node(cx),
                    false => this.open_selected(cx),
                }),
            )
            .on_action(
                cx.listener(|this, _: &ShowTimeline, _, cx| this.set_mode(BoardMode::Timeline, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowRelations, _, cx| {
                    this.set_mode(BoardMode::Relations, cx)
                }),
            )
            .on_action(cx.listener(move |this, _: &ToggleEndedAgents, _, cx| {
                if relations {
                    this.toggle_ended(cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &SelectNextMessage, _, cx| {
                if relations {
                    this.select_next_arrow(cx);
                }
            }))
            .on_action(cx.listener(|_, _: &LeaveProgressBoard, _, cx| cx.emit(BoardEvent::Leave)))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(14.))
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .bg(hsla(theme.background))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .child(panel)
            // Under the panel in either mode, as in the mock.
            .child(self.kanban(cx))
    }
}
