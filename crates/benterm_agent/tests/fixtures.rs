//! The built-in definitions against made-up Claude Code and Codex screens.
//!
//! The logs in `tests/fixtures` are written from the scenes in
//! `tests/scenes`, which say how each screen is put together and which
//! moment of the agent it stands for. Real screens are checked only on the
//! developer's machine; see CONTRIBUTING.md. The `.order` files beside
//! the logs belong to `orders.rs`.

mod common;
mod scenes;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, WaitReason, builtin, claude_code, codex};
use benterm_log::screen_text;
use common::{final_screen, fixture_path, screens};

const CLAUDE: &[(&str, AgentState)] = &[
    ("claude/trust", AgentState::Waiting(WaitReason::Trust)),
    ("claude/idle", AgentState::Done),
    ("claude/working", AgentState::Working),
    ("claude/done", AgentState::Done),
    (
        "claude/permission",
        AgentState::Waiting(WaitReason::Permission),
    ),
    // AskUserQuestion offers its choices, and nothing else.
    ("claude/question", AgentState::Waiting(WaitReason::Choice)),
];

const CODEX: &[(&str, AgentState)] = &[
    ("codex/idle", AgentState::Done),
    // A numbered choice at start-up takes no message.
    ("codex/choice", AgentState::Waiting(WaitReason::Choice)),
    ("codex/chosen", AgentState::Done),
    // A numbered list typed at the prompt is no choice.
    ("codex/draft", AgentState::Done),
    ("codex/working", AgentState::Working),
    ("codex/done", AgentState::Done),
    (
        "codex/permission",
        AgentState::Waiting(WaitReason::Permission),
    ),
    ("codex/trust", AgentState::Waiting(WaitReason::Trust)),
    // request_user_input may take an answer of its own as well as a choice.
    ("codex/question", AgentState::Waiting(WaitReason::Question)),
];

#[test]
fn claude_code_screens_give_their_states() {
    let definition = claude_code();
    for (name, expected) in CLAUDE {
        assert_eq!(
            definition.screen_state(&final_screen(name)),
            Some(*expected),
            "{name}"
        );
    }
}

#[test]
fn codex_screens_give_their_states() {
    let definition = codex();
    for (name, expected) in CODEX {
        assert_eq!(
            definition.screen_state(&final_screen(name)),
            Some(*expected),
            "{name}"
        );
    }
}

#[test]
fn the_other_agent_does_not_claim_a_screen() {
    for (name, _) in CLAUDE {
        assert!(!codex().shows_banner(&final_screen(name)), "{name}");
    }
    for (name, _) in CODEX {
        assert!(!claude_code().shows_banner(&final_screen(name)), "{name}");
    }
}

#[test]
fn each_agent_is_recognised_by_its_start_screen() {
    for (name, id) in [
        ("claude/idle", "claude"),
        ("claude/trust", "claude"),
        ("codex/idle", "codex"),
    ] {
        let mut detector = Detector::new(builtin());
        detector.observe_screen(&final_screen(name), Instant::now());
        assert_eq!(detector.agent().map(|agent| agent.id), Some(id), "{name}");
    }
}

/// Feeds every intermediate screen of a recording to a detector, as the app
/// would while the agent draws, and checks the state it settles on.
#[test]
fn a_detector_following_a_whole_recording_settles_on_the_final_state() {
    for (name, expected) in CLAUDE.iter().chain(CODEX) {
        let mut detector = Detector::new(builtin());
        let start = Instant::now();
        let mut now = start;
        for screen in screens(name) {
            now += Duration::from_millis(20);
            detector.observe_screen(&screen, now);
        }
        let last = final_screen(name);
        detector.observe_screen(&last, now + Detector::DEFAULT_SETTLE);
        assert_eq!(detector.state(), *expected, "{name}");
    }
}

/// Checks that each log in `tests/fixtures` is exactly the log of its
/// scene, and that no other log sits there. With `BENTERM_WRITE_FIXTURES`
/// set, writes the logs from the scenes instead.
#[test]
fn each_fixture_is_the_log_of_its_scene() {
    let scenes = scenes::all();
    if std::env::var_os("BENTERM_WRITE_FIXTURES").is_some() {
        for scene in &scenes {
            let path = fixture_path(scene.name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, scene.log()).unwrap();
        }
    }
    let mut expected: Vec<PathBuf> = scenes
        .iter()
        .map(|scene| PathBuf::from(format!("{}.bentermlog", scene.name)))
        .collect();
    expected.sort();
    // The other files there are the orders of `orders.rs`.
    let logs: Vec<PathBuf> = fixture_files()
        .into_iter()
        .filter(|file| file.extension().is_some_and(|ext| ext == "bentermlog"))
        .collect();
    assert_eq!(logs, expected);
    for scene in &scenes {
        assert!(
            fs::read(fixture_path(scene.name)).unwrap() == scene.log(),
            "{} differs from its scene; rewrite it as tests/scenes/mod.rs says",
            scene.name
        );
    }
}

#[test]
fn every_expected_state_has_a_scene() {
    let mut names: Vec<&str> = scenes::all().iter().map(|scene| scene.name).collect();
    let mut expected: Vec<&str> = CLAUDE.iter().chain(CODEX).map(|(name, _)| *name).collect();
    names.sort();
    expected.sort();
    assert_eq!(names, expected);
}

/// Text that appears in real Claude Code and Codex recordings but never in
/// the made-up screens. The fixtures must stay made up, because a real
/// recording can carry personal details such as the plan name of the
/// account.
const REAL_RECORDING_TRACES: &[&str] = &[
    // Working directories of the recording script and of real users.
    "/private/",
    "/tmp/",
    "/Users/",
    "/home/",
    // The start screens print the version next to the name.
    "Claude Code v",
    "OpenAI Codex (v",
    // Plan names shown on the start screens.
    "Claude Pro",
    "Claude Max",
    "Claude Team",
    "Claude Enterprise",
    "ChatGPT Free",
    "ChatGPT Plus",
    "ChatGPT Pro",
    "ChatGPT Team",
    "ChatGPT Business",
    "ChatGPT Enterprise",
];

/// Whether `c` belongs to an agent's start-up logo: Claude Code draws its
/// logo with quadrant blocks and Codex with braille dots.
fn is_logo_char(c: char) -> bool {
    matches!(c, '\u{2596}'..='\u{259F}' | '\u{2800}'..='\u{28FF}')
}

/// The traces of a real recording found in `text`.
fn traces_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = REAL_RECORDING_TRACES
        .iter()
        .filter(|trace| text.contains(*trace))
        .map(|trace| format!("{trace:?}"))
        .collect();
    if let Some(c) = text.chars().find(|&c| is_logo_char(c)) {
        found.push(format!("logo character {c:?}"));
    }
    found
}

/// Every file under `tests/fixtures`, as paths relative to it.
fn fixture_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                files.push(path.strip_prefix(root).unwrap().to_owned());
            }
        }
    }
    let root = fixture_path("x").parent().unwrap().to_owned();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    files
}

/// Looks for traces of a real recording in the bytes of every fixture and,
/// for logs, in every screen they draw, since cursor moves can split text
/// that the screen shows whole.
#[test]
fn the_fixtures_hold_no_trace_of_a_real_recording() {
    let root = fixture_path("x").parent().unwrap().to_owned();
    let mut problems = Vec::new();
    for file in fixture_files() {
        let bytes = fs::read(root.join(&file)).unwrap();
        let mut found = traces_in(&String::from_utf8_lossy(&bytes));
        if file.extension().is_some_and(|ext| ext == "bentermlog") {
            let name = file.with_extension("");
            for screen in screens(name.to_str().unwrap()) {
                found.extend(traces_in(&screen_text(&screen)));
            }
        }
        found.sort();
        found.dedup();
        if !found.is_empty() {
            problems.push(format!("{}: {}", file.display(), found.join(", ")));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_trace_check_catches_what_it_looks_for() {
    assert!(traces_in("plain made-up text ❯ ? for shortcuts").is_empty());
    for text in [
        " ▐▛███▜▌   Claude",
        "⣿⣿⣿",
        "Opus · Claude Max",
        "/private/tmp/benterm-agent-claude",
        "Claude Code v2.1.0",
    ] {
        assert!(!traces_in(text).is_empty(), "{text}");
    }
}
