//! What the program in front of the pane's terminal adds to the screen
//! and the hooks: only an agent in front is named from the screen, and an
//! agent that leaves the front takes its state with it.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason};
use common::{definitions, screen_of};
use proptest::prelude::*;

const SETTLE: Duration = Detector::DEFAULT_SETTLE;

fn args(line: &[&str]) -> Vec<String> {
    line.iter().map(|arg| (*arg).to_owned()).collect()
}

fn agent(detector: &Detector) -> Option<&'static str> {
    detector.agent().map(|agent| agent.id)
}

/// Shows `lines` for the settle time.
fn show(detector: &mut Detector, lines: &[&str], t: Instant) -> Instant {
    let screen = screen_of(lines);
    detector.observe_screen(&screen, t);
    detector.observe_screen(&screen, t + SETTLE);
    t + SETTLE
}

#[test]
fn an_agent_in_front_is_named_by_how_it_was_started() {
    let cases: &[(&[&str], Option<&str>)] = &[
        // The native build: the file is named after its version, the
        // shell passes the command as typed.
        (&["claude", "--model", "opus"], Some("claude")),
        (&["/opt/tools/bin/claude"], Some("claude")),
        (&["codex", "-s", "workspace-write"], Some("codex")),
        // A script run by its interpreter, as npm installs them.
        (
            &["node", "/opt/homebrew/bin/claude", "--resume"],
            Some("claude"),
        ),
        (
            &[
                "/usr/local/bin/node",
                "--no-warnings",
                "/usr/local/bin/codex",
            ],
            Some("codex"),
        ),
        (&["/bin/sh", "./toy"], Some("toy")),
        // Code given on the command line is not looked into.
        (&["sh", "-c", "claude"], None),
        (&["node", "-e", "require('claude')"], None),
        (&["-zsh"], None),
        (&["vim", "claude"], None),
        (&[], None),
    ];
    for (line, want) in cases {
        let mut detector = Detector::new(definitions());
        detector.observe_front(Some(&args(line)));
        assert_eq!(agent(&detector), *want, "{line:?}");
    }
}

#[test]
fn with_a_shell_in_front_the_screen_names_no_agent() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["-zsh"])));
    // A banner left from before, and a prompt that looks like the
    // agent's own.
    show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    assert_eq!(agent(&detector), None);
    assert_eq!(detector.state(), AgentState::Unknown);
}

#[test]
fn an_agent_that_leaves_the_front_takes_its_state_with_it() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["toy"])));
    let t = show(&mut detector, &["TOY AGENT v1", "allow? [y/n]"], t);
    assert_eq!(
        detector.state(),
        AgentState::Waiting(WaitReason::Permission)
    );

    detector.observe_front(Some(&args(&["-zsh"])));
    assert_eq!(agent(&detector), None);
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.pending_deadline(), None);
    assert_eq!(detector.owner(), None);
    // What the agent left on the screen does not bring it back.
    show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    assert_eq!(agent(&detector), None);
    assert_eq!(detector.state(), AgentState::Unknown);
}

#[test]
fn an_agent_coming_to_the_front_is_named_before_its_screen_shows() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["-zsh"])));
    show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    detector.observe_front(Some(&args(&["toy"])));
    assert_eq!(agent(&detector), Some("toy"));
    // Nothing has shown its state yet.
    assert_eq!(detector.state(), AgentState::Unknown);
    show(&mut detector, &["TOY AGENT v1", "thinking"], t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn the_same_agent_staying_in_front_keeps_its_state() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["toy"])));
    detector.observe_hook(&HookEvent::new("toy", "start"), t);
    detector.observe_front(Some(&args(&["toy"])));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_hook_still_names_the_agent_under_a_program_that_is_not_one() {
    // A wrapper script started the agent, and stays in front.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["/bin/sh", "./run-the-agent"])));
    detector.observe_hook(&HookEvent::new("toy", "start"), t);
    assert_eq!(agent(&detector), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
    // The same program in front again changes nothing.
    detector.observe_front(Some(&args(&["/bin/sh", "./run-the-agent"])));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_reading_that_fails_between_two_of_the_shell_takes_no_agent_away() {
    // Under load the kernel sometimes fails to give a program's
    // arguments, so one reading of the shell in front comes out as none.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let shell = args(&["/bin/sh", "-i"]);
    detector.observe_front(Some(&shell));
    detector.observe_hook(&HookEvent::new("toy", "start").with_session("outer"), t);
    detector.observe_front(None);
    detector.observe_front(Some(&shell));
    assert_eq!(agent(&detector), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("outer"));
}

#[test]
fn a_reading_that_fails_lets_no_banner_name_an_agent_under_the_shell() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(Some(&args(&["/bin/sh", "-i"])));
    detector.observe_front(None);
    show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    assert_eq!(agent(&detector), None);
}

#[test]
fn a_hook_s_agent_stays_when_the_shell_is_first_read_after_it() {
    // The first reading failed, and a hook came before the next one.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front(None);
    detector.observe_hook(&HookEvent::new("toy", "start").with_session("outer"), t);
    detector.observe_front(Some(&args(&["/bin/sh", "-i"])));
    assert_eq!(agent(&detector), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("outer"));
}

#[test]
fn an_agent_the_screen_named_goes_when_the_shell_is_first_read() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    assert_eq!(agent(&detector), Some("toy"));
    detector.observe_front(Some(&args(&["/bin/sh", "-i"])));
    assert_eq!(agent(&detector), None);
}

#[test]
fn a_detector_that_expects_the_front_names_no_agent_from_the_screen_before_it() {
    // A restored pane shows the banner before the program in front could
    // be read once.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.expect_front();
    detector.observe_front(None);
    let t = show(&mut detector, &["TOY AGENT v1", "toy>"], t);
    assert_eq!(agent(&detector), None);
    // A hook still names the agent.
    detector.observe_hook(&HookEvent::new("toy", "start"), t);
    assert_eq!(agent(&detector), Some("toy"));
    // And once read, an agent in front lets the screen tell its state.
    let mut detector = Detector::new(definitions());
    detector.expect_front();
    detector.observe_front(Some(&args(&["toy"])));
    show(&mut detector, &["TOY AGENT v1", "thinking"], t);
    assert_eq!(detector.state(), AgentState::Working);
}

/// One thing a detector is told, for the made-up agent of `common`.
#[derive(Debug, Clone)]
enum Input {
    Screen(Vec<&'static str>),
    Hook(&'static str),
    Wait,
}

fn input() -> impl Strategy<Value = Input> {
    prop_oneof![
        prop::sample::select(vec![
            vec!["TOY AGENT v1", "toy>"],
            vec!["TOY AGENT v1", "thinking"],
            vec!["output", "allow? [y/n]"],
            vec!["output", "thinking"],
            vec!["output", "toy>"],
            vec!["$ ls"],
        ])
        .prop_map(Input::Screen),
        prop::sample::select(vec!["start", "ask", "end", "bye", "other"]).prop_map(Input::Hook),
        Just(Input::Wait),
    ]
}

proptest! {
    /// A front that cannot be read is no evidence: a detector told so
    /// between every other input decides as one never told anything
    /// about the front.
    #[test]
    fn a_front_that_cannot_be_read_changes_nothing(inputs in prop::collection::vec(input(), 0..40)) {
        let mut told = Detector::new(definitions());
        let mut plain = Detector::new(definitions());
        let mut t = Instant::now();
        for input in inputs {
            told.observe_front(None);
            match &input {
                Input::Screen(lines) => {
                    let screen = screen_of(lines);
                    told.observe_screen(&screen, t);
                    plain.observe_screen(&screen, t);
                }
                Input::Hook(name) => {
                    let event = HookEvent::new("toy", *name);
                    told.observe_hook(&event, t);
                    plain.observe_hook(&event, t);
                }
                Input::Wait => t += SETTLE / 2,
            }
            prop_assert_eq!(agent(&told), agent(&plain));
            prop_assert_eq!(told.state(), plain.state());
            prop_assert_eq!(told.pending_deadline(), plain.pending_deadline());
        }
    }
}
