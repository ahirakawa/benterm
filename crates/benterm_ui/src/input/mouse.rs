//! What a mouse action sends to a program that asked for mouse reports.
//!
//! Nothing here depends on GPUI, so the encoding is tested without a
//! window. A program turns reports on with one of xterm's tracking modes
//! (1000 for clicks, 1002 for drags, 1003 for every move) and picks the
//! format with another mode (1005 for UTF-8, 1006 for SGR). [`mouse_bytes`]
//! turns one action into bytes; [`MouseReporter`] remembers what it needs
//! between actions, such as the last cell a move was reported in.

use benterm_core::{GridSize, Modes, MouseEncoding, MouseTracking};

/// A mouse button that can be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// Which way one step of the wheel went. `Up` shows older content, as
/// rolling a wheel away from the user does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wheel {
    Up,
    Down,
    Left,
    Right,
}

/// What the mouse did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Press(Button),
    Release(Button),
    /// The pointer moved into another cell, with this button held if any.
    Motion(Option<Button>),
    /// One step of the wheel.
    Wheel(Wheel),
}

/// The modifier keys a report carries. Command is never reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    /// The option key.
    pub alt: bool,
    pub control: bool,
}

/// A cell on the screen, counted from zero at the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPoint {
    pub col: u16,
    pub row: u16,
}

/// One mouse action at one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub action: Action,
    pub cell: CellPoint,
    pub mods: Mods,
}

/// Whether a program in `tracking` wants to hear about `action`.
///
/// Clicks report presses, releases and the wheel. Drags add moves with a
/// button held, and every-move tracking adds moves without one.
pub fn reports(action: Action, tracking: MouseTracking) -> bool {
    match action {
        Action::Press(_) | Action::Release(_) | Action::Wheel(_) => tracking != MouseTracking::Off,
        Action::Motion(Some(_)) => {
            matches!(tracking, MouseTracking::Drag | MouseTracking::Motion)
        }
        Action::Motion(None) => tracking == MouseTracking::Motion,
    }
}

/// The largest zero-based cell the X10 format carries: 223 plus the
/// offset of 32 is the largest byte.
const X10_MAX_CELL: u16 = 222;
/// The largest zero-based cell the UTF-8 format carries. xterm stops at
/// 2015 (one-based), the last value that is two bytes of UTF-8 after the
/// offset of 32.
const UTF8_MAX_CELL: u16 = 2014;

/// The button number of xterm's reports, before modifiers and motion.
fn button_number(button: Button) -> u8 {
    match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
    }
}

/// The first number of a report. The legacy formats cannot say which
/// button came up, so they report every release as button 3; SGR names
/// the button and marks the release with its final byte instead.
fn button_code(event: &MouseEvent, encoding: MouseEncoding) -> u8 {
    let base = match event.action {
        Action::Press(button) => button_number(button),
        Action::Release(button) if encoding == MouseEncoding::Sgr => button_number(button),
        Action::Release(_) => 3,
        Action::Motion(Some(button)) => 32 + button_number(button),
        Action::Motion(None) => 32 + 3,
        Action::Wheel(Wheel::Up) => 64,
        Action::Wheel(Wheel::Down) => 65,
        Action::Wheel(Wheel::Left) => 66,
        Action::Wheel(Wheel::Right) => 67,
    };
    let mods = event.mods;
    base + 4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.control)
}

/// The bytes of `event` in `encoding`, whatever the tracking mode.
///
/// `None` means the format cannot carry the cell.
pub fn encode(event: &MouseEvent, encoding: MouseEncoding) -> Option<Vec<u8>> {
    let code = button_code(event, encoding);
    let CellPoint { col, row } = event.cell;
    match encoding {
        MouseEncoding::Sgr => {
            let last = if matches!(event.action, Action::Release(_)) {
                'm'
            } else {
                'M'
            };
            let (col, row) = (u32::from(col) + 1, u32::from(row) + 1);
            Some(format!("\x1b[<{code};{col};{row}{last}").into_bytes())
        }
        MouseEncoding::Default => {
            if col > X10_MAX_CELL || row > X10_MAX_CELL {
                return None;
            }
            // Both cells are at most 222, so adding 33 stays in a byte.
            let byte = |cell: u16| cell as u8 + 33;
            Some(vec![0x1b, b'[', b'M', 32 + code, byte(col), byte(row)])
        }
        MouseEncoding::Utf8 => {
            if col > UTF8_MAX_CELL || row > UTF8_MAX_CELL {
                return None;
            }
            let mut bytes = vec![0x1b, b'[', b'M', 32 + code];
            for cell in [col, row] {
                let ch = char::from_u32(u32::from(cell) + 33).expect("below the surrogates");
                let mut buf = [0; 4];
                bytes.extend(ch.encode_utf8(&mut buf).as_bytes());
            }
            Some(bytes)
        }
    }
}

/// The bytes to send for `event`, or `None` when the program did not ask
/// for it or its format cannot carry the cell.
pub fn mouse_bytes(event: &MouseEvent, modes: Modes) -> Option<Vec<u8>> {
    if !reports(event.action, modes.mouse_tracking) {
        return None;
    }
    encode(event, modes.mouse_encoding)
}

/// Where the pointer is, in cells from the top left of the screen. It may
/// be outside the screen, at negative values or past the last cell.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
}

impl Pointer {
    /// The cell under the pointer, if it is on a screen of `size`.
    fn cell(self, size: GridSize) -> Option<CellPoint> {
        let inside = |v: f32, len: u16| (0.0..f32::from(len)).contains(&v);
        (inside(self.x, size.cols) && inside(self.y, size.rows)).then(|| self.nearest_cell(size))
    }

    /// The cell under the pointer, or the screen's cell nearest to it.
    fn nearest_cell(self, size: GridSize) -> CellPoint {
        let clamp = |v: f32, len: u16| v.floor().clamp(0.0, f32::from(len.max(1) - 1)) as u16;
        CellPoint {
            col: clamp(self.x, size.cols),
            row: clamp(self.y, size.rows),
        }
    }
}

/// What a scroll event sends while the accumulator holds leftover lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WheelMode {
    #[default]
    Off,
    Mouse,
    AlternateScreen,
}

/// Turns pointer actions into reports, remembering what that takes
/// between them.
///
/// A press is reported only on the screen, since a click beside it is
/// not meant for the program. A release and a drag are reported at the
/// nearest cell wherever the pointer is, so that the program sees the end
/// of a drag that left the screen. A move is reported only when it enters
/// another cell, as xterm and Ghostty do.
#[derive(Debug, Clone, Default)]
pub struct MouseReporter {
    /// The cell of the last report.
    last_cell: Option<CellPoint>,
    /// Scrolling not yet sent, in columns and rows. Each is above -1 and
    /// below 1.
    wheel_rest: (f32, f32),
    /// What kind of wheel output `wheel_rest` belongs to.
    wheel_mode: WheelMode,
}

impl MouseReporter {
    /// Encodes `action` at `cell` if the program wants it, and remembers
    /// the cell.
    fn report(
        &mut self,
        action: Action,
        cell: CellPoint,
        mods: Mods,
        modes: Modes,
    ) -> Option<Vec<u8>> {
        let event = MouseEvent { action, cell, mods };
        let bytes = mouse_bytes(&event, modes)?;
        self.last_cell = Some(cell);
        Some(bytes)
    }

    /// A button went down at `at`.
    pub fn press(
        &mut self,
        button: Button,
        at: Pointer,
        mods: Mods,
        modes: Modes,
        size: GridSize,
    ) -> Option<Vec<u8>> {
        let cell = at.cell(size)?;
        self.report(Action::Press(button), cell, mods, modes)
    }

    /// A button came up at `at`.
    pub fn release(
        &mut self,
        button: Button,
        at: Pointer,
        mods: Mods,
        modes: Modes,
        size: GridSize,
    ) -> Option<Vec<u8>> {
        self.report(Action::Release(button), at.nearest_cell(size), mods, modes)
    }

    /// The pointer moved to `at` with `held` down, if any.
    pub fn motion(
        &mut self,
        held: Option<Button>,
        at: Pointer,
        mods: Mods,
        modes: Modes,
        size: GridSize,
    ) -> Option<Vec<u8>> {
        let cell = match held {
            Some(_) => at.nearest_cell(size),
            None => at.cell(size)?,
        };
        if self.last_cell == Some(cell) {
            return None;
        }
        self.report(Action::Motion(held), cell, mods, modes)
    }

    /// Drops any fractional scrolling not yet sent. Call when leaving mouse
    /// tracking or the alternate screen so leftover scrolling does not carry
    /// into the next program.
    pub fn reset_wheel(&mut self) {
        self.wheel_rest = (0.0, 0.0);
        self.wheel_mode = WheelMode::Off;
    }

    /// The wheel or the trackpad scrolled by `lines` (columns, rows) at
    /// `at`. A positive row count shows older content, as
    /// [`Wheel::Up`] does; a positive column count is [`Wheel::Left`].
    ///
    /// When mouse tracking is on, each whole line is one step of the wheel.
    /// When mouse tracking is off on the alternate screen, each whole
    /// vertical line is an up or down arrow key in the terminal's current
    /// cursor key mode. What is left of a line is kept and added to the next
    /// scroll, and dropped when switching between modes or leaving them.
    pub fn wheel(
        &mut self,
        lines: (f32, f32),
        at: Pointer,
        mods: Mods,
        modes: Modes,
        size: GridSize,
    ) -> Vec<u8> {
        let mode = if modes.mouse_tracking != MouseTracking::Off {
            WheelMode::Mouse
        } else if modes.alternate_screen {
            WheelMode::AlternateScreen
        } else {
            WheelMode::Off
        };
        if mode != self.wheel_mode {
            self.wheel_rest = (0.0, 0.0);
            self.wheel_mode = mode;
        }
        match mode {
            WheelMode::Off => Vec::new(),
            WheelMode::Mouse => {
                let mut bytes = Vec::new();
                let cell = at.nearest_cell(size);
                let (cols, col_rest) = whole_lines(self.wheel_rest.0 + lines.0);
                let (rows, row_rest) = whole_lines(self.wheel_rest.1 + lines.1);
                self.wheel_rest = (col_rest, row_rest);
                let steps = [
                    (rows, Wheel::Up, Wheel::Down),
                    (cols, Wheel::Left, Wheel::Right),
                ];
                for (count, positive, negative) in steps {
                    let wheel = if count > 0 { positive } else { negative };
                    for _ in 0..count.unsigned_abs() {
                        bytes.extend(
                            self.report(Action::Wheel(wheel), cell, mods, modes)
                                .unwrap_or_default(),
                        );
                    }
                }
                bytes
            }
            WheelMode::AlternateScreen => {
                let (rows, row_rest) = whole_lines(self.wheel_rest.1 + lines.1);
                self.wheel_rest = (0.0, row_rest);
                if rows == 0 {
                    return Vec::new();
                }
                let key = super::Key {
                    key: if rows > 0 { "up" } else { "down" },
                    key_char: None,
                    shift: false,
                    alt: false,
                    control: false,
                    platform: false,
                };
                let step =
                    super::key_bytes(&key, modes, super::KeyOptions::default()).unwrap_or_default();
                let mut bytes = Vec::with_capacity(step.len() * rows.unsigned_abs() as usize);
                for _ in 0..rows.unsigned_abs() {
                    bytes.extend_from_slice(&step);
                }
                bytes
            }
        }
    }
}

/// The whole lines in `lines`, rounded toward zero, and the rest.
fn whole_lines(lines: f32) -> (i32, f32) {
    if !lines.is_finite() {
        return (0, 0.0);
    }
    let whole = lines.trunc();
    (whole as i32, lines - whole)
}
