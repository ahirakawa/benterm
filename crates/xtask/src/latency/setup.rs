//! The files both apps are started with, so that they draw the same
//! font at the same size and run the same shell, and the keys that put
//! the busy panes beside the one that is typed in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The color of the cells the prompt starts with. The probe finds the
/// prompt on the screen by it; nothing else the shells print has it.
pub const MARKER: [u8; 3] = [255, 0, 255];

/// How many cells of [`MARKER`] the prompt starts with.
pub const MARKER_CELLS: u32 = 4;

/// The colors of the terminal in both apps: Benterm's defaults.
const BACKGROUND: &str = "1a1b1e";
const FOREGROUND: &str = "d4d6db";

/// The space around the grid in both apps, in points.
const PADDING: u32 = 10;

/// Benterm's settings file.
pub fn benterm_config(font: &str, font_size: f64) -> String {
    let font = toml::Value::String(font.to_string());
    format!(
        "\
[terminal]
font_family = {font}
font_size = {font_size:?}
# Close to the height Ghostty gives a row of the same font.
line_height = 1.2
padding = {PADDING}

[colors.terminal]
foreground = \"#{FOREGROUND}\"
background = \"#{BACKGROUND}\"

# Ghostty writes neither of these, so Benterm does not while it is measured.
[pty_log]
enabled = false

[session]
scrollback_lines = 0
"
    )
}

/// Ghostty's config file. It is the only one Ghostty reads when it is
/// started with `--config-default-files=false`.
pub fn ghostty_config(font: &str, font_size: f64) -> String {
    // Benterm opens at 960 by 640 points. A cell of a monospaced font is
    // about 0.6 by 1.2 of its size, which gives Ghostty a window of about
    // that size without measuring the font.
    let columns = (960.0 / (font_size * 0.6)).round();
    let rows = (640.0 / (font_size * 1.2)).round();
    format!(
        "\
font-family = \"{font}\"
font-size = {font_size}
window-padding-x = {PADDING}
window-padding-y = {PADDING}
window-width = {columns}
window-height = {rows}
background = {BACKGROUND}
foreground = {FOREGROUND}
# Benterm's cursor does not blink, and a blink would look like a key.
cursor-style = block
cursor-style-blink = false
# The same shell, told the same about its terminal, as in Benterm.
shell-integration = none
term = xterm-256color
# Nothing of an earlier run, and no questions when the run ends.
window-save-state = never
confirm-close-surface = false
quit-after-last-window-closed = true
auto-update = off
# The keys Benterm splits and moves with.
keybind = super+d=new_split:right
keybind = super+shift+d=new_split:down
keybind = super+alt+left=goto_split:left
"
    )
}

/// The bundle identifier of the Ghostty that is measured.
pub const GHOSTTY_COPY_ID: &str = "io.github.ahirakawa.benterm.latency.ghostty";

/// The binary inside a Ghostty.app.
pub fn ghostty_binary(app: &Path) -> PathBuf {
    app.join("Contents/MacOS/ghostty")
}

/// Copies the Ghostty.app at `installed` into `dir` under the bundle
/// identifier [`GHOSTTY_COPY_ID`], signs the copy ad hoc, and returns it.
///
/// Ghostty writes some of its config to the preferences macOS keeps for
/// the app: whether its windows come back at the next start, and whether
/// it looks for updates. Started with the config of a run, the installed
/// Ghostty would change them for the user. macOS keeps the preferences by
/// bundle identifier, so the copy has its own. The binary is the same.
pub fn ghostty_copy(installed: &Path, dir: &Path) -> Result<PathBuf> {
    if !ghostty_binary(installed).exists() {
        bail!(
            "{} is not a Ghostty.app: name it with --ghostty",
            installed.display()
        );
    }
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let copy = dir.join("Ghostty.app");
    if copy.exists() {
        fs::remove_dir_all(&copy).with_context(|| format!("remove {}", copy.display()))?;
    }
    // cp keeps the symbolic links of the frameworks inside the app.
    crate::run(Command::new("cp").arg("-R").arg(installed).arg(&copy))?;
    crate::run(
        Command::new("plutil")
            .args(["-replace", "CFBundleIdentifier", "-string", GHOSTTY_COPY_ID])
            .arg(copy.join("Contents/Info.plist")),
    )?;
    // The change broke the signature the app came with.
    crate::run(
        Command::new("codesign")
            .args([
                "--force",
                "--deep",
                "--timestamp=none",
                "--sign",
                crate::AD_HOC,
            ])
            .arg(&copy),
    )?;
    Ok(copy)
}

/// The `.zshrc` of the shells in both apps. The prompt is the marker and
/// a space, and `f` turns the shell into the writer `writer flood`, which
/// leaves a file in `started_dir`.
pub fn zshrc(writer: &Path, rate: u32, started_dir: &Path) -> String {
    let [red, green, blue] = MARKER;
    let cells = " ".repeat(MARKER_CELLS as usize);
    let writer = shell_word(&writer.to_string_lossy());
    let started_dir = shell_word(&started_dir.to_string_lossy());
    format!(
        "\
# Made by cargo xtask latency.
unsetopt beep
bindkey -e
PROMPT_EOL_MARK=''
RPROMPT=''
PROMPT=$'%{{\\e[48;2;{red};{green};{blue}m%}}{cells}%{{\\e[0m%}} '
f() {{ exec {writer} flood --rate {rate} --started-dir {started_dir} }}
"
    )
}

/// `text` as one word of a shell command.
fn shell_word(text: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_/.+-:=@%".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// A key that changes the panes. Benterm and Ghostty are given the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// ⌘D: a pane on the right of the focused one, which takes the focus.
    SplitRight,
    /// ⌘⇧D: a pane below the focused one, which takes the focus.
    SplitDown,
    /// ⌘⌥←: focus the pane on the left.
    FocusLeft,
}

/// The keys that open `panes` panes beside the first one and come back
/// to it. A writer is started in every pane right after its split.
///
/// The first pane keeps the left half of the window. The others are two
/// columns on the right, each split downward. Every step to the left
/// starts at the bottom of a column whose left neighbor has the full
/// height, so both apps can only land in the same pane.
pub fn split_plan(panes: u32) -> Vec<Step> {
    let second = panes / 2;
    let first = panes - second;
    let mut plan = vec![Step::SplitRight];
    if second > 0 {
        plan.push(Step::SplitRight);
        plan.extend((1..second).map(|_| Step::SplitDown));
        plan.push(Step::FocusLeft);
    }
    plan.extend((1..first).map(|_| Step::SplitDown));
    plan.push(Step::FocusLeft);
    plan
}

/// The plan as the probe takes it: a letter for each step.
pub fn plan_keys(plan: &[Step]) -> String {
    let letters: Vec<&str> = plan
        .iter()
        .map(|step| match step {
            Step::SplitRight => "R",
            Step::SplitDown => "D",
            Step::FocusLeft => "L",
        })
        .collect();
    letters.join(",")
}
