//! Renders a terminal to a PNG without showing a window, to check by eye
//! what the terminal view draws.
//!
//!     cargo run --example screenshot -- OUT.png [--config FILE] [--save FILE]
//!         [--split right|down]... [--type TEXT] [--text TEXT] [--mark TEXT]
//!         [--keys KEYS] [--peek] [--graph] [--kanban]
//!         [--wait-for TEXT]... [--resize WIDTHxHEIGHT] [--sleep MS]
//!         [--scroll PIXELS] [--right-click X,Y] [--shot FILE] [--redact TEXT]...
//!         [--history SESSION] [--diff WORKTREE BASE] [--browse URL]
//!         [--report NAME STATUS] [--checked NAME passed|stopped]
//!         [--open-report] [--tasks] [PROGRAM [ARGS...]]
//!
//! Without PROGRAM the login shell runs. `--config` uses FILE as the config
//! file instead of the defaults, and watches it as the app does; every pane
//! records a PTY log if its `[pty_log]` section enables it. `--save`
//! then copies FILE over the config file once the program has started, to
//! check that a save is applied without a restart. `--split` splits the
//! focused pane with the app's keys, Cmd-D or Cmd-Shift-D, and may be
//! given more than once; every new pane runs PROGRAM too and takes the
//! focus. Once the program has had time to start, `--type` types TEXT into
//! the focused pane of the selected tab and presses enter, `--text` types TEXT without enter
//! (into the field for a workspace's name while one is being renamed,
//! into the command palette while it is open, into the field of the
//! task form that `cmd-shift-n` opens, or into the field for a comment
//! while the diff is shown),
//! `--mark` shows TEXT as the input method's text being converted,
//! `--keys` presses keys named as in GPUI's bindings, separated by spaces
//! (such as `shift-enter`, `cmd-t` for a new tab, `cmd-n` for a new
//! workspace, `ctrl-cmd-]` and `ctrl-cmd-[` for the next and the previous
//! workspace, `cmd-shift-r` to rename the selected one, `cmd-shift-w`
//! to close it, `cmd-k` for the command palette, or `cmd-shift-e` for the
//! file browser, whose tree then takes the arrows and enter), and `--wait-for` waits until TEXT is on the
//! screen. `--resize` changes the window's size in points, `--sleep` keeps
//! the app running for MS milliseconds, `--scroll` scrolls up by PIXELS,
//! `--right-click` presses the right button at X,Y in window points, which
//! opens the menu of what is there, and `--shot` writes the window to FILE
//! as a PNG. These run in the order
//! given, and OUT.png is written at the end. `--redact` paints
//! over TEXT wherever it is on a pane's live screen in the PNG, to keep
//! things such as a plan name out of the image; it does not look at rows
//! scrolled back into view. `--history` reads the agents' history kept
//! beside the session file SESSION, as a second app does, for the
//! progress board that `cmd-shift-b` opens. `--peek` lays the terminal of
//! the first tab's first pane over the pane area, as a row of the session
//! list does for its agent; give it after `--keys cmd-t`, as a pane on
//! view is not peeked at. What is typed then goes to the peek, and
//! `cmd-.` closes it. `--redact` cannot be given with it.
//! `--diff` opens the diff tab on WORKTREE against the commit BASE, as
//! for a task made there, and waits until the diff is read. `--graph`
//! hands the progress board a made-up family of agents and their
//! messages, for the graph that `r` shows in the board; give it after
//! `--keys "cmd-shift-b r"`. `--kanban` hands the progress board made-up
//! tasks and the records of their agents, for the kanban under the
//! timeline; give it after `--keys cmd-shift-b`. `--browse` opens a
//! browser pane for URL at the right of the focused pane and leaves the
//! keyboard where it is. No page is fetched or drawn: the PNG is of what
//! GPUI draws, and WebKit draws a page in a view of its own over that.
//! The pane shows what it shows while its page is hidden.
//! `--report` makes the program of the focused pane the agent NAME, whose
//! last report had STATUS (pass, issues or blocked), for the badge of its
//! row in the session list; no agent is started and no report is sent.
//! `--checked` makes the program of the focused pane the agent NAME and
//! gives it a made-up checked report, for the question in its row: one
//! that `passed` the check, or one that `stopped` with problems left.
//! `--open-report` then opens the panel of the focused pane's report.
//! `--tasks` opens the window as the app does, without a socket and
//! without a session file, so that the form `cmd-shift-n` opens can make
//! tasks; the panes then start in the directory the example is run in.
//! Only the list of the tasks is kept in the window alone: making a task
//! makes its branch and its worktree, under the directory the config
//! names. No pane records a PTY log then, and `--history` cannot be
//! given with it.

use std::time::{Duration, Instant, SystemTime};

use std::path::Path;

use benterm_core::CellWidth;
use benterm_ipc::ReportStatus;
use benterm_report::{Choice, Decision, Deliverable, Report};
use benterm_session::history::Record;
use benterm_task::Worktree;
use benterm_terminal::PtyConfig;
use benterm_ui::checked_reports::CheckedReport;
use benterm_ui::diff_view::DiffSource;
use benterm_ui::graph::{Message, MessageKind, Messages, Node};
use benterm_ui::kanban::{BoardTask, Sent};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::timeline::BandState;
use benterm_ui::{
    AgentFeed, BentermWindow, FirstPane, MainView, PaneGrid, SpawnTerminal, StartedTerminal,
    TerminalView,
};

/// The terminal input goes to: the one in the peek while it is open,
/// or else the focused pane of the selected tab.
fn focused(root: &Entity<BentermWindow>, cx: &App) -> Entity<TerminalView> {
    let root = root.read(cx);
    root.peeked()
        .and_then(|pane| root.terminal_with_id(pane, cx))
        .unwrap_or_else(|| root.terminal(cx))
}
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, EntityInputHandler, HeadlessAppContext,
    InputEvent, KeyDownEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    ScrollDelta, ScrollEvent, TouchPhase, point, px, size,
};

/// A page that is not fetched: it has the address it is given and a
/// made-up title, and has been somewhere before.
#[derive(Default)]
struct MadeUpPage(benterm_ui::browser::PageState);

impl benterm_ui::browser::Page for MadeUpPage {
    fn load(&mut self, url: &str) {
        self.0.url = Some(url.to_owned());
        self.0.title = Some("A made-up page".to_owned());
        self.0.can_go_back = true;
    }

    fn go_back(&mut self) {}

    fn go_forward(&mut self) {}

    fn reload(&mut self) {}

    fn state(&self) -> benterm_ui::browser::PageState {
        self.0.clone()
    }

    fn place(&mut self, _: Option<Bounds<gpui::Pixels>>) {}

    fn set_keyboard(&mut self, _: bool) {}

    fn has_keyboard(&self) -> bool {
        false
    }

    fn edit(&mut self, _: benterm_ui::browser::PageEdit) {}
}

/// Makes the browser panes show made-up pages instead of WebKit's.
fn made_up_pages(cx: &mut App) {
    let open = std::rc::Rc::new(|_: &gpui::Window, _| {
        Box::new(MadeUpPage::default()) as Box<dyn benterm_ui::browser::Page>
    });
    benterm_ui::browser::set_page_opener(cx, open);
}

/// Where `texts` are on the live screens of the panes, in window points.
fn hidden_cells(grid: &PaneGrid, texts: &[String], cx: &App) -> Vec<Bounds<f32>> {
    let mut found = Vec::new();
    for (pane, bounds) in grid.pane_bounds() {
        // A browser pane has no screen to look on.
        let Some(view) = grid.terminal(pane) else {
            continue;
        };
        let view = view.read(cx);
        let cell = view.cell_size();
        let (cell_width, line_height) = (f32::from(cell.width), f32::from(cell.height));
        let padding = view.theme().terminal.padding;
        let origin = (
            f32::from(bounds.origin.x) + padding,
            f32::from(bounds.origin.y) + padding,
        );
        for (line, row) in view.screen().rows.iter().enumerate() {
            // The column each character starts at, so that wide
            // characters before the text move it right.
            let (chars, columns): (Vec<char>, Vec<usize>) = row
                .cells
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell.width != CellWidth::WideSpacer)
                .map(|(column, cell)| (cell.ch, column))
                .unzip();
            for text in texts {
                let wanted: Vec<char> = text.chars().collect();
                for start in 0..chars.len() {
                    if chars[start..].starts_with(&wanted) {
                        let first = columns[start];
                        let end = columns
                            .get(start + wanted.len())
                            .copied()
                            .unwrap_or(row.cells.len());
                        found.push(Bounds::new(
                            point(
                                origin.0 + cell_width * first as f32,
                                origin.1 + line_height * line as f32,
                            ),
                            size(cell_width * (end - first) as f32, line_height),
                        ));
                    }
                }
            }
        }
    }
    found
}

fn send(cx: &mut HeadlessAppContext, window: AnyWindowHandle, event: impl InputEvent) {
    let _ = cx.update_window(window, |_, window, cx| {
        window.dispatch_event(event.to_platform_input(), cx);
    });
    cx.run_until_parked();
}

fn press(cx: &mut HeadlessAppContext, window: AnyWindowHandle, keystroke: Keystroke) {
    let event = KeyDownEvent {
        keystroke,
        is_held: false,
        prefer_character_input: false,
    };
    send(cx, window, event);
}

/// Input to give the focused pane, in order.
enum Step {
    Text { text: String, enter: bool },
    Mark(String),
    Keys(String),
    Peek,
    Graph,
    Issues,
    Browse(String),
    Kanban,
    WaitFor(String),
    Resize(f32, f32),
    Sleep(Duration),
    Scroll(f32),
    RightClick(f32, f32),
    Shot(String),
    Diff { worktree: String, base: String },
    Report { name: String, status: ReportStatus },
    Checked { name: String, stopped: bool },
    OpenReport,
}

const TIMEOUT: Duration = Duration::from_secs(15);
const STARTUP: Duration = Duration::from_millis(1500);

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .expect("usage: screenshot OUT.png [options] [PROGRAM [ARGS...]]");
    let mut steps = Vec::new();
    let (mut config, mut save) = (None, None);
    let mut splits = Vec::new();
    let mut redact = Vec::new();
    let mut history: Option<String> = None;
    let mut tasks = false;
    let mut command = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next(),
            "--save" => save = args.next(),
            "--split" => splits.push(match args.next().as_deref() {
                Some("right") => "cmd-d",
                Some("down") => "cmd-shift-d",
                other => panic!("--split takes right or down, not {other:?}"),
            }),
            "--type" | "--text" => steps.push(Step::Text {
                text: args.next().expect("--type and --text take TEXT"),
                enter: arg == "--type",
            }),
            "--mark" => steps.push(Step::Mark(args.next().expect("--mark takes TEXT"))),
            "--keys" => steps.push(Step::Keys(args.next().expect("--keys takes KEYS"))),
            "--peek" => steps.push(Step::Peek),
            "--graph" => steps.push(Step::Graph),
            "--issues" => steps.push(Step::Issues),
            "--browse" => steps.push(Step::Browse(args.next().expect("--browse takes URL"))),
            "--kanban" => steps.push(Step::Kanban),
            "--wait-for" => steps.push(Step::WaitFor(args.next().expect("--wait-for takes TEXT"))),
            "--resize" => {
                let value = args.next().expect("--resize takes WIDTHxHEIGHT");
                let (width, height) = value.split_once('x').expect("--resize takes WIDTHxHEIGHT");
                steps.push(Step::Resize(
                    width.parse().unwrap(),
                    height.parse().unwrap(),
                ));
            }
            "--sleep" => steps.push(Step::Sleep(Duration::from_millis(
                args.next().expect("--sleep takes MS").parse().unwrap(),
            ))),
            "--redact" => redact.push(args.next().expect("--redact takes TEXT")),
            "--history" => history = args.next(),
            "--tasks" => tasks = true,
            "--scroll" => steps.push(Step::Scroll(
                args.next().expect("--scroll takes PIXELS").parse().unwrap(),
            )),
            "--right-click" => {
                let value = args.next().expect("--right-click takes X,Y");
                let (x, y) = value.split_once(',').expect("--right-click takes X,Y");
                steps.push(Step::RightClick(x.parse().unwrap(), y.parse().unwrap()));
            }
            "--shot" => steps.push(Step::Shot(args.next().expect("--shot takes FILE"))),
            "--report" => steps.push(Step::Report {
                name: args.next().expect("--report takes NAME STATUS"),
                status: args
                    .next()
                    .expect("--report takes NAME STATUS")
                    .parse()
                    .expect("--report takes pass, issues or blocked"),
            }),
            "--checked" => steps.push(Step::Checked {
                name: args.next().expect("--checked takes NAME passed|stopped"),
                stopped: match args.next().as_deref() {
                    Some("passed") => false,
                    Some("stopped") => true,
                    other => panic!("--checked takes passed or stopped, not {other:?}"),
                },
            }),
            "--open-report" => steps.push(Step::OpenReport),
            "--diff" => steps.push(Step::Diff {
                worktree: args.next().expect("--diff takes WORKTREE BASE"),
                base: args.next().expect("--diff takes WORKTREE BASE"),
            }),
            _ => {
                command.push(arg);
                command.extend(&mut args);
            }
        }
    }
    // The texts are looked for in the panes of the selected tab, which
    // the peek covers.
    let peeks = steps.iter().any(|step| matches!(step, Step::Peek));
    assert!(
        !peeks || redact.is_empty(),
        "--redact does not find the text in a peek; leave out --peek or --redact"
    );
    let pty = match command.split_first() {
        Some((program, args)) => PtyConfig::command(program, args),
        None => PtyConfig::login_shell(),
    };

    let pty_of_tasks = pty.clone();

    // Real text shaping and a real GPU renderer, but no window server.
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::current_platform(false).text_system(),
        std::sync::Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    // The terminal's worker thread wakes the app from outside the
    // scheduler.
    cx.allow_parking();
    cx.update(benterm_ui::bind_keys);
    cx.update(made_up_pages);
    let feed = match &config {
        Some(path) => ConfigFeed::watch(Path::new(path)),
        None => ConfigFeed::defaults(),
    };
    // Every pane records a PTY log if the config enables it, as in the app.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let pty_log = feed.settings().pty_log().clone();
    let spawn: SpawnTerminal = std::rc::Rc::new(move |_, theme, _, size| {
        let pty = PtyConfig {
            size,
            ..pty.clone()
        };
        StartedTerminal::spawn_recorded(pty, theme, &pty_log, home.as_deref())
    });
    let (root, window): (Entity<BentermWindow>, AnyWindowHandle) = if tasks {
        assert!(history.is_none(), "--history cannot be given with --tasks");
        let mut pty = pty_of_tasks;
        pty.cwd = std::env::current_dir().ok();
        let start: benterm_ui::ipc::StartTerminal = std::rc::Rc::new(StartedTerminal::spawn);
        let window = cx
            .update(|cx| benterm_ui::ipc::open_session_window(pty, start, None, None, feed, cx))
            .expect("failed to open the window");
        let root = cx
            .update(|cx| window.entity(cx))
            .expect("the window is open");
        (root, window.into())
    } else {
        let first = FirstPane::start(&spawn, &feed.settings().theme().terminal, None, None)
            .expect("failed to start");
        let mut root = None;
        let window = cx
            .open_window(size(px(960.), px(640.)), |window, cx| {
                let view = cx.new(|cx| BentermWindow::new(first, spawn, feed, window, cx));
                if let Some(session) = &history {
                    let agents = AgentFeed::default();
                    view.update(cx, |root, cx| {
                        root.record_agents(&agents, Some(Path::new(session)), window, cx)
                    });
                }
                let terminal = view.read(cx).terminal(cx);
                window.focus(&terminal.read(cx).focus_handle().clone(), cx);
                root = Some(view.clone());
                view
            })
            .expect("failed to open the window");
        (root.unwrap(), window.into())
    };

    let pump = |cx: &mut HeadlessAppContext, done: &dyn Fn(&TerminalView) -> bool| {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let _ = cx.update_window(window, |_, window, _| window.refresh());
            cx.run_until_parked();
            if cx.update(|cx| done(focused(&root, cx).read(cx))) || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
            // The app's timers follow this clock, such as the one that
            // lets the state of an agent settle.
            cx.advance_clock(Duration::from_millis(10));
        }
    };

    let started_at = Instant::now();
    pump(&mut cx, &|_| started_at.elapsed() > STARTUP);
    if let Some(source) = save {
        let target = config.as_ref().expect("--save needs --config");
        std::fs::copy(&source, target).expect("failed to save the config file");
        let saved_at = Instant::now();
        // The watcher waits for the file to settle before reading it.
        pump(&mut cx, &|_| {
            saved_at.elapsed() > Duration::from_millis(1000)
        });
    }
    if !splits.is_empty() {
        for keys in splits {
            press(&mut cx, window, Keystroke::parse(keys).unwrap());
        }
        let split_at = Instant::now();
        pump(&mut cx, &|_| split_at.elapsed() > STARTUP);
    }
    for step in steps {
        match step {
            Step::Text { text, enter } => {
                // A window without a screen has no input method, so the
                // text is committed as the input method would commit it.
                let _ = cx.update_window(window, |_, window, cx| {
                    let root_view = root.read(cx);
                    if root_view.renaming().is_some()
                        || root_view.command_palette().is_some()
                        || root_view.epic_prompt().is_some()
                        || root_view.task_form().is_some()
                        || root_view.diff_view().is_some()
                    {
                        // The field for a workspace's name, the
                        // palette's, or one of the task form's has the
                        // keyboard.
                        for ch in text.chars() {
                            let key = Keystroke {
                                modifiers: Modifiers::default(),
                                key: ch.to_string(),
                                key_char: Some(ch.to_string()),
                            };
                            window.dispatch_keystroke(key, cx);
                        }
                    } else {
                        focused(&root, cx).update(cx, |view, cx| {
                            view.replace_text_in_range(None, &text, window, cx)
                        });
                    }
                });
                cx.run_until_parked();
                if enter {
                    press(&mut cx, window, Keystroke::parse("enter").unwrap());
                }
            }
            Step::Diff { worktree, base } => {
                // WORKTREE is a worktree git made, which knows its repository.
                let repository = benterm_task::Git::default()
                    .main_checkout(Path::new(&worktree))
                    .expect("WORKTREE is in a git repository");
                let source = DiffSource {
                    task: 0,
                    repository,
                    worktree: worktree.into(),
                    base_commit: base,
                    base_branch: "main".into(),
                    branch: "task".into(),
                };
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        root.show_view(MainView::Diff, window, cx);
                        let diff = root.diff_view().expect("the diff is shown");
                        diff.update(cx, |diff, cx| diff.set_source(Some(source), cx));
                    })
                });
                let started = Instant::now();
                loop {
                    cx.run_until_parked();
                    let read = cx.update(|cx| {
                        let diff = root.read(cx).diff_view().unwrap();
                        diff.read(cx).counts().is_some()
                    });
                    if read || started.elapsed() > TIMEOUT {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            Step::Mark(text) => {
                let _ = cx.update_window(window, |_, window, cx| {
                    focused(&root, cx).update(cx, |view, cx| {
                        view.replace_and_mark_text_in_range(None, &text, None, window, cx)
                    })
                });
                cx.run_until_parked();
            }
            Step::Keys(keys) => {
                // GPUI's parser wants text that lives as long as the program.
                for name in keys.leak().split_whitespace() {
                    let keystroke = Keystroke::parse(name).expect("a key name");
                    press(&mut cx, window, keystroke);
                }
            }
            Step::Peek => {
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        let tabs = root.tabs().read(cx);
                        let first = tabs.grid(tabs.tabs()[0]).expect("the first tab");
                        let pane = first.read(cx).panes()[0];
                        root.peek(pane, window, cx);
                    })
                });
                cx.run_until_parked();
            }
            Step::Report { name, status } => {
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        root.take_made_up_report(&name, status, window, cx)
                    })
                });
                cx.run_until_parked();
            }
            Step::Checked { name, stopped } => {
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        // A conclusion of the work for the badge too.
                        root.take_made_up_report(&name, ReportStatus::Issues, window, cx);
                        root.take_made_up_checked_report(made_up_checked_report(stopped), cx);
                    })
                });
                cx.run_until_parked();
            }
            Step::OpenReport => {
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        let pane = root.grid(cx).read(cx).focused();
                        root.open_report(pane, window, cx);
                    })
                });
                cx.run_until_parked();
            }
            Step::Graph => {
                let board = cx.update(|cx| root.read(cx).progress_board());
                let board = board.expect("--graph is given while the progress board is shown");
                let (nodes, messages) = made_up_graph();
                cx.update(|cx| board.update(cx, |board, cx| board.take_graph(nodes, messages, cx)));
                cx.run_until_parked();
            }
            Step::Issues => {
                let (epic, states) = benterm_ui::issue_graph::made_up_epic();
                let _ = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        root.show_view(MainView::Issues, window, cx);
                        if let Some(issues) = root.issue_graph_view() {
                            issues.update(cx, |issues, cx| {
                                issues.take_made_up_epic(epic, states, cx);
                            });
                        }
                    })
                });
                cx.run_until_parked();
            }
            Step::Browse(url) => {
                let opened = cx.update_window(window, |_, window, cx| {
                    root.update(cx, |root, cx| {
                        let direction = benterm_ui::browser::SplitDirection::Right;
                        root.open_browser(Some(&url), None, direction, false, window, cx)
                    })
                });
                opened
                    .expect("the window is open")
                    .expect("--browse takes the address of a web page");
                cx.run_until_parked();
            }
            Step::Kanban => {
                let board = cx.update(|cx| root.read(cx).progress_board());
                let board = board.expect("--kanban is given while the progress board is shown");
                let (tasks, records) = made_up_tasks();
                cx.update(|cx| {
                    board.update(cx, |board, cx| board.take_made_up_tasks(tasks, records, cx))
                });
                cx.run_until_parked();
            }
            Step::WaitFor(text) => {
                let on_screen = |view: &TerminalView| {
                    view.screen()
                        .rows
                        .iter()
                        .any(|row| row.text().contains(&text))
                };
                pump(&mut cx, &on_screen);
            }
            Step::Resize(width, height) => {
                let _ = cx.update_window(window, |_, window, _| {
                    window.resize(size(px(width), px(height)))
                });
                // The headless window tells the app about a new size only
                // when its scale factor changes, so give it the same one.
                let scale = cx
                    .update_window(window, |_, window, _| window.scale_factor())
                    .unwrap();
                cx.simulate_window_scale_factor_change(window, scale);
                let resized_at = Instant::now();
                pump(&mut cx, &|_| {
                    resized_at.elapsed() > Duration::from_millis(500)
                });
            }
            Step::Sleep(duration) => {
                let slept_at = Instant::now();
                pump(&mut cx, &|_| slept_at.elapsed() > duration);
            }
            Step::RightClick(x, y) => {
                let position = point(px(x), px(y));
                send(
                    &mut cx,
                    window,
                    MouseDownEvent {
                        position,
                        button: MouseButton::Right,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    },
                );
                send(
                    &mut cx,
                    window,
                    MouseUpEvent {
                        position,
                        button: MouseButton::Right,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    },
                );
            }
            Step::Scroll(pixels) => {
                // The middle of the focused pane, which the sidebar and
                // the tab bar do not cover.
                let position = cx.update(|cx| {
                    let grid = root.read(cx).grid(cx).read(cx);
                    let focused = grid.focused();
                    let (_, bounds) = grid
                        .pane_bounds()
                        .into_iter()
                        .find(|(pane, _)| *pane == focused)
                        .expect("the focused pane has a place");
                    bounds.center()
                });
                send(
                    &mut cx,
                    window,
                    ScrollEvent {
                        position,
                        delta: ScrollDelta::Pixels(point(px(0.), px(pixels))),
                        modifiers: Modifiers::default(),
                        touch_phase: TouchPhase::Moved,
                        ..Default::default()
                    },
                )
            }
            Step::Shot(path) => {
                let settled_at = Instant::now();
                pump(&mut cx, &|_| {
                    settled_at.elapsed() > Duration::from_millis(300)
                });
                shoot(&mut cx, window, &root, &redact, &path);
            }
        }
    }
    let settled_at = Instant::now();
    pump(&mut cx, &|_| {
        settled_at.elapsed() > Duration::from_millis(300)
    });
    shoot(&mut cx, window, &root, &redact, &out);
}

/// A conductor with its workers in every state, one of which has a
/// helper of its own and one of which has closed, and what they told
/// each other in the last half hour.
/// A made-up checked report: one that passed, or one that ran out of
/// rounds with problems left.
fn made_up_checked_report(stopped: bool) -> CheckedReport {
    let text = |text: &str| text.to_owned();
    let decision = Decision {
        question: text("Should the old settings file still be read?"),
        options: vec![
            Choice {
                label: text("Keep reading it"),
                effect: text("Both files work; the old one is read when the new one is missing."),
            },
            Choice {
                label: text("Stop reading it"),
                effect: text("Only the new file works; a warning names the old one once."),
            },
        ],
        recommend: text("Stop reading it"),
        reason: text("Nothing has written the old file since the last release."),
    };
    let report = Report {
        title: text("Move the settings to the new file"),
        goal: text("To read every setting from one file."),
        conclusion: if stopped { text("") } else { text("issues") },
        summary: text(
            "The settings are read from the new file. The old file is still read \
             when the new one is missing, until it is decided whether to keep it.",
        ),
        decisions: vec![decision],
        verified: vec![text(
            "The tests of the settings pass (cargo test -p settings).",
        )],
        unverified: vec![text("A settings file written by hand with comments in it.")],
        deliverables: vec![Deliverable {
            path: text("src/settings.rs"),
            summary: text("Reads the new file first."),
            changes: vec![],
        }],
    };
    let problems = if stopped {
        vec![
            text("conclusion is not one of pass, issues or blocked."),
            text("verified names no command that was run."),
        ]
    } else {
        vec![]
    };
    CheckedReport {
        agent: text("claude"),
        session_id: None,
        report,
        problems,
    }
}

fn made_up_graph() -> (Vec<Node>, Vec<Message>) {
    use BandState::{Done, Error, Waiting, Working};
    let node = |id, parent, name: &str, agent, state| Node {
        id,
        parent,
        name: name.into(),
        agent: Some(agent),
        state: Some(state),
        closed: false,
        background: parent.is_some(),
    };
    let mut nodes = vec![
        node(1, None, "conductor", "claude", Working),
        node(2, Some(1), "graph-worker", "claude", Working),
        node(3, Some(1), "kanban-worker", "claude", Waiting),
        node(4, Some(1), "diff-worker", "codex", Done),
        node(5, Some(2), "graph-reviewer", "codex", Working),
        node(6, Some(1), "browser-worker", "claude", Error),
        node(7, Some(1), "docs-worker", "claude", Done),
    ];
    nodes[6].closed = true;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    let now = now.map_or(0, |since| since.as_millis() as u64);
    let mut messages = Messages::default();
    let minute = 60_000;
    let told = [
        (
            MessageKind::Request,
            1,
            2,
            28 * minute,
            "Draw the graph of the agents in the board.",
        ),
        (
            MessageKind::Report,
            7,
            1,
            25 * minute,
            "The README says how the board is opened.",
        ),
        (
            MessageKind::Request,
            1,
            4,
            20 * minute,
            "Review the diff of the kanban.",
        ),
        (
            MessageKind::Report,
            4,
            1,
            9 * minute,
            "Two findings, both in the pull request.",
        ),
        (
            MessageKind::Request,
            2,
            5,
            4 * minute,
            "Review the layout of the graph.",
        ),
        (
            MessageKind::Report,
            5,
            2,
            90_000,
            "The layout holds; one line is too long.",
        ),
        (
            MessageKind::Request,
            1,
            2,
            60_000,
            "Add the picture to the pull request.",
        ),
        (
            MessageKind::Report,
            2,
            1,
            5_000,
            "The pull request is up, with the picture.",
        ),
    ];
    for (kind, from, to, ago, text) in told {
        messages.push(kind, from, to, now - ago, text);
    }
    (nodes, messages.iter().cloned().collect())
}

/// Tasks in each column of the kanban, as in the mock's progress board,
/// with what their agents did in the last hour.
fn made_up_tasks() -> (Vec<BoardTask>, Vec<Record>) {
    use benterm_session::Agent::{Claude, Codex};
    use benterm_session::history::{State, Waiting};
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    let now = now.map_or(0, |since| since.as_millis() as u64);
    let minute = 60_000;
    let task = |id, name: &str, repository: &str, panes: &[&str]| BoardTask {
        id,
        name: name.into(),
        repository: repository.into(),
        closed: None,
        panes: panes.iter().map(|pane| pane.to_string()).collect(),
        sends: Vec::new(),
    };
    let mut tasks = vec![
        task(1, "rate-limit-headers", "api", &["k1"]),
        task(2, "dark-mode-toggle", "web", &["k2"]),
        task(3, "add-export-csv", "api", &["k3"]),
        task(4, "fix-auth-timeout", "api", &["k4", "k5"]),
        task(5, "refactor-settings", "web", &["k6"]),
        task(6, "fix-login-redirect", "web", &[]),
        task(7, "bump-deps", "api", &[]),
    ];
    tasks[4].sends.push(Sent {
        pane: "k6".into(),
        at_ms: now - 30 * minute,
        comments: 2,
    });
    tasks[5].closed = Some(Worktree::Removed);
    tasks[6].closed = Some(Worktree::Kept);
    let record = |ago, key: &str, agent, state| Record {
        at_ms: now - ago,
        key: key.into(),
        pane: 0,
        agent: Some(agent),
        state,
    };
    let mut records = vec![
        record(32 * minute, "k3", Claude, State::Working),
        record(50 * minute, "k4", Claude, State::Working),
        record(
            14 * minute,
            "k4",
            Claude,
            State::Waiting(Waiting::Permission),
        ),
        record(40 * minute, "k5", Codex, State::Working),
        record(55 * minute, "k6", Codex, State::Working),
        record(35 * minute, "k6", Codex, State::Done),
        record(29 * minute, "k6", Codex, State::Working),
        record(12 * minute, "k6", Codex, State::Done),
    ];
    records.sort_by_key(|record| record.at_ms);
    (tasks, records)
}

/// Writes the window to `out` as a PNG, with `redact` painted over.
fn shoot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    root: &Entity<BentermWindow>,
    redact: &[String],
    out: &str,
) {
    let mut image = cx.capture_screenshot(window).expect("failed to render");
    let scale = cx
        .update_window(window, |_, window, _| window.scale_factor())
        .unwrap();
    let hidden = cx.update(|cx| hidden_cells(root.read(cx).grid(cx).read(cx), redact, cx));
    for cells in hidden {
        let (x0, y0) = ((cells.left() * scale) as u32, (cells.top() * scale) as u32);
        let (x1, y1) = (
            (cells.right() * scale) as u32,
            (cells.bottom() * scale) as u32,
        );
        for y in y0..y1.min(image.height()) {
            for x in x0..x1.min(image.width()) {
                image.get_pixel_mut(x, y).0 = [128, 128, 128, 255];
            }
        }
    }
    image.save(&out).expect("failed to write the image");
    println!("wrote {out} ({}x{})", image.width(), image.height());
}
