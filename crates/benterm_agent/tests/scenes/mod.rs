//! Made-up Claude Code and Codex screens, the material of `tests/fixtures`.
//!
//! No byte of a real screen is copied here. Each screen holds only what the
//! built-in definitions look at: the banner that names the agent, and the
//! footer or dialog lines at the bottom that the screen rules key on (see
//! `src/agents.rs`). The rest is short made-up text that gives the rules
//! something above the footer to ignore.
//!
//! A scene is a list of frames. Each frame becomes one output record of the
//! log: it clears the screen, moves the cursor home and prints its lines.
//! Every scene starts with the agent's start screen, so that a detector
//! following the whole log recognises the agent by its banner before the
//! frames that show the state; the later frames no longer show the banner.
//!
//! The logs in `tests/fixtures` are written from these scenes, and the test
//! `each_fixture_is_the_log_of_its_scene` checks that they still match. After
//! changing a scene, rewrite the logs with the command below. It runs that
//! test alone, because the other tests read the logs while it writes them.
//!
//! ```text
//! BENTERM_WRITE_FIXTURES=1 cargo test -p benterm_agent --test fixtures \
//!     each_fixture_is_the_log_of_its_scene
//! ```

use benterm_core::GridSize;
use benterm_log::{LogWriter, Record};

pub const SIZE: GridSize = GridSize::new(20, 80);

/// One made-up recording: its fixture name and its frames, in order.
pub struct Scene {
    pub name: &'static str,
    pub frames: Vec<Vec<String>>,
}

impl Scene {
    fn new(name: &'static str, frames: Vec<Vec<String>>) -> Self {
        Self { name, frames }
    }

    /// The scene as a benterm log.
    pub fn log(&self) -> Vec<u8> {
        let mut writer = LogWriter::new(Vec::new(), SIZE).unwrap();
        for frame in &self.frames {
            let mut bytes = b"\x1b[H\x1b[2J".to_vec();
            bytes.extend_from_slice(frame.join("\r\n").as_bytes());
            writer.write_record(&Record::Output(bytes)).unwrap();
        }
        writer.into_inner()
    }
}

/// Every scene, Claude Code's first.
pub fn all() -> Vec<Scene> {
    let mut scenes = claude();
    scenes.extend(codex());
    scenes
}

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

/// A frame of `above` followed by `below`.
fn frame(above: &[&str], below: &[String]) -> Vec<String> {
    let mut frame = lines(above);
    frame.extend_from_slice(below);
    frame
}

/// A horizontal line across the screen, like the edges of Claude Code's
/// prompt box.
fn rule() -> String {
    "─".repeat(usize::from(SIZE.cols))
}

/// Claude Code's empty prompt box with `footer` under it. The prompt line
/// starts with "❯".
fn claude_prompt(footer: &str) -> Vec<String> {
    vec![rule(), "❯ ".to_owned(), rule(), footer.to_owned()]
}

const CLAUDE_IDLE_FOOTER: &str = "  ? for shortcuts";
const CLAUDE_WORKING_FOOTER: &str = "  esc to interrupt";

/// Claude Code scenes. The banner is "Claude Code".
fn claude() -> Vec<Scene> {
    // The start screen: the name, then the empty prompt box with the idle
    // footer ("? for shortcuts").
    let start = frame(
        &[" * Claude Code", "   made-up start screen", ""],
        &claude_prompt(CLAUDE_IDLE_FOOTER),
    );
    // A prompt has been sent and the turn runs: the footer under the prompt
    // box says "esc to interrupt".
    let working = frame(
        &["❯ Write a short poem about tea.", "", "✻ Thinking…", ""],
        &claude_prompt(CLAUDE_WORKING_FOOTER),
    );
    vec![
        // Asking at start-up whether to trust the working directory. The
        // dialog names the agent, so this one frame also shows the banner.
        // As in Claude Code 2.1.330, the choices have no numbers and the
        // refusal comes first, picked. The bottom line is
        // "Enter to confirm · Esc to cancel".
        Scene::new(
            "claude/trust",
            vec![lines(&[
                " Accessing workspace:",
                "",
                " Claude Code will be able to read, edit, and run files here.",
                "",
                " ❯ No, exit",
                "   Yes, I trust this folder",
                "",
                " Enter to confirm · Esc to cancel",
            ])],
        ),
        // Waiting for the first prompt, right after start-up.
        Scene::new("claude/idle", vec![start.clone()]),
        // A turn in progress.
        Scene::new("claude/working", vec![start.clone(), working.clone()]),
        // The reply is printed and the prompt box is back with the idle
        // footer.
        Scene::new(
            "claude/done",
            vec![
                start.clone(),
                working.clone(),
                frame(
                    &[
                        "❯ Write a short poem about tea.",
                        "",
                        "⏺ Steam over the cup,",
                        "  a quiet afternoon.",
                        "",
                    ],
                    &claude_prompt(CLAUDE_IDLE_FOOTER),
                ),
            ],
        ),
        // A tool asks for approval: "Do you want to proceed?" above the
        // options and "Esc to cancel · Tab to amend" at the bottom.
        Scene::new(
            "claude/permission",
            vec![
                start.clone(),
                working.clone(),
                lines(&[
                    "❯ Create an empty file named note.txt.",
                    "",
                    "⏺ Bash(touch note.txt)",
                    "",
                    &rule(),
                    " Bash command",
                    "",
                    "   touch note.txt",
                    "",
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. No",
                    "",
                    " Esc to cancel · Tab to amend",
                ]),
            ],
        ),
        // AskUserQuestion: the options and, at the bottom,
        // "Enter to select · ↑/↓ to navigate · Esc to cancel".
        Scene::new(
            "claude/question",
            vec![
                start,
                working,
                lines(&[
                    "❯ Ask me which tea I like, then reply.",
                    "",
                    &rule(),
                    " Which tea do you like?",
                    "",
                    " ❯ 1. Green",
                    "   2. Black",
                    "",
                    " Enter to select · ↑/↓ to navigate · Esc to cancel",
                ]),
            ],
        ),
    ]
}

/// Codex's prompt line, which starts with "›", and the idle footer under it.
fn codex_prompt() -> Vec<String> {
    lines(&["› ", "", "  ? for shortcuts"])
}

/// Codex scenes. The banner is ">_ OpenAI Codex".
fn codex() -> Vec<Scene> {
    // The start screen: the name in a box, then the prompt and its footer.
    let start = frame(
        &[
            "╭──────────────────────────╮",
            "│ >_ OpenAI Codex          │",
            "│ made-up start screen     │",
            "╰──────────────────────────╯",
            "",
        ],
        &codex_prompt(),
    );
    // A prompt has been sent and the turn runs: the status line above the
    // prompt says "• Working (3s • esc to interrupt)".
    let working = frame(
        &[
            "› Write a short poem about tea.",
            "",
            "• Working (3s • esc to interrupt)",
            "",
        ],
        &codex_prompt(),
    );
    // A numbered choice at start-up, such as the one Codex shows when its
    // background server runs with other feature settings. The options are
    // the last lines drawn, with no footer under them, and the selected one
    // starts with "›", as the prompt line does.
    let choice = |selected: usize| {
        let mut frame = lines(&[
            "  The background server runs with other settings",
            "  Made-up words about what each option does.",
            "",
        ]);
        for (i, option) in ["Go on without it this time", "Restart it", "Cancel"]
            .iter()
            .enumerate()
        {
            let mark = if i + 1 == selected { "›" } else { " " };
            frame.push(format!("{mark} {}. {option}", i + 1));
        }
        frame
    };
    vec![
        // Waiting for the first prompt, right after start-up.
        Scene::new("codex/idle", vec![start.clone()]),
        // The choice at start-up, with its last option selected.
        Scene::new("codex/choice", vec![start.clone(), choice(3)]),
        // The user moved to the first option and chose it, and Codex went
        // on to its prompt.
        Scene::new(
            "codex/chosen",
            vec![start.clone(), choice(3), choice(1), start.clone()],
        ),
        // A numbered list typed at the prompt. Its lines look like the
        // options of a choice, but the footer is drawn under them.
        Scene::new(
            "codex/draft",
            vec![
                start.clone(),
                lines(&[
                    "› 1. first item",
                    "  2. second item",
                    "",
                    "  made-up model · ~/made-up-folder",
                ]),
            ],
        ),
        // A turn in progress.
        Scene::new("codex/working", vec![start.clone(), working.clone()]),
        // The reply is printed and the status line is gone.
        Scene::new(
            "codex/done",
            vec![
                start.clone(),
                working.clone(),
                frame(
                    &[
                        "› Write a short poem about tea.",
                        "",
                        "• Steam over the cup,",
                        "  a quiet afternoon.",
                        "",
                        "─ Worked for 3s ─",
                        "",
                    ],
                    &codex_prompt(),
                ),
            ],
        ),
        // A command asks for approval. The bottom line is
        // "Press enter to confirm or esc to cancel".
        Scene::new(
            "codex/permission",
            vec![
                start.clone(),
                working.clone(),
                lines(&[
                    "› Create an empty file named note.txt.",
                    "",
                    "• Running touch note.txt",
                    "",
                    "  Would you like to run the following command?",
                    "",
                    "  $ touch note.txt",
                    "",
                    "› 1. Yes, proceed (y)",
                    "  2. No, and tell Codex what to do differently (esc)",
                    "",
                    "  Press enter to confirm or esc to cancel",
                ]),
            ],
        ),
        // Asking at start-up whether to trust the working directory, over
        // the start screen. The bottom line is "enter continue · esc back".
        Scene::new(
            "codex/trust",
            vec![
                start.clone(),
                lines(&[
                    "  Folder access",
                    "  ~/made-up-folder",
                    "",
                    "  Trust this folder?",
                    "  Codex can read, edit, and run files here.",
                    "",
                    "  › 1. Trust and continue",
                    "    2. Back to Agent Command Center",
                    "",
                    "  enter continue · esc back",
                ]),
            ],
        ),
        // request_user_input. The status line still says
        // "esc to interrupt)", so this scene also checks that the question
        // rule comes before the working one. The bottom line is
        // "enter to submit answer · esc to interrupt".
        Scene::new(
            "codex/question",
            vec![
                start,
                working,
                lines(&[
                    "› Ask me which tea I like, then reply.",
                    "",
                    "• Working (5s • esc to interrupt)",
                    "",
                    "  Question 1/1",
                    "  Which tea do you like?",
                    "  › 1. Green",
                    "    2. Black",
                    "",
                    "  enter to submit answer · esc to interrupt",
                ]),
            ],
        ),
    ]
}
