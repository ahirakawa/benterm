//! Turning pointer actions in pixels-turned-cells into reports.

use benterm_core::{GridSize, Modes, MouseEncoding, MouseTracking};
use benterm_ui::input::mouse::{Button, Mods, MouseReporter, Pointer};
use proptest::prelude::*;

const SIZE: GridSize = GridSize::new(24, 80);

fn sgr(tracking: MouseTracking) -> Modes {
    Modes {
        mouse_tracking: tracking,
        mouse_encoding: MouseEncoding::Sgr,
        ..Modes::default()
    }
}

fn alt_screen(app_cursor_keys: bool) -> Modes {
    Modes {
        alternate_screen: true,
        app_cursor_keys,
        ..Modes::default()
    }
}

fn at(x: f32, y: f32) -> Pointer {
    Pointer { x, y }
}

fn text(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes.map(|b| String::from_utf8(b).unwrap())
}

const NONE: Mods = Mods {
    shift: false,
    alt: false,
    control: false,
};

#[test]
fn a_press_reports_the_cell_under_the_pointer() {
    let mut r = MouseReporter::default();
    let bytes = r.press(
        Button::Left,
        at(3.9, 1.2),
        NONE,
        sgr(MouseTracking::Click),
        SIZE,
    );
    assert_eq!(text(bytes).unwrap(), "\x1b[<0;4;2M");
}

#[test]
fn nothing_is_reported_while_tracking_is_off() {
    let mut r = MouseReporter::default();
    let off = Modes::default();
    assert_eq!(r.press(Button::Left, at(1., 1.), NONE, off, SIZE), None);
    assert_eq!(r.release(Button::Left, at(1., 1.), NONE, off, SIZE), None);
    assert_eq!(
        r.motion(Some(Button::Left), at(2., 1.), NONE, off, SIZE),
        None
    );
    assert!(r.wheel((0., 3.), at(1., 1.), NONE, off, SIZE).is_empty());
}

#[test]
fn a_press_outside_the_screen_is_not_reported() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    assert_eq!(r.press(Button::Left, at(-0.5, 1.), NONE, modes, SIZE), None);
    assert_eq!(r.press(Button::Left, at(1., 24.), NONE, modes, SIZE), None);
}

#[test]
fn a_release_outside_the_screen_is_reported_at_the_nearest_cell() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    let bytes = r.release(Button::Left, at(100., -3.), NONE, modes, SIZE);
    assert_eq!(text(bytes).unwrap(), "\x1b[<0;80;1m");
}

#[test]
fn a_move_within_the_same_cell_is_reported_once() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Motion);
    let first = r.motion(None, at(5.1, 2.1), NONE, modes, SIZE);
    assert_eq!(text(first).unwrap(), "\x1b[<35;6;3M");
    assert_eq!(r.motion(None, at(5.9, 2.9), NONE, modes, SIZE), None);
    let next = r.motion(None, at(6.0, 2.9), NONE, modes, SIZE);
    assert_eq!(text(next).unwrap(), "\x1b[<35;7;3M");
}

#[test]
fn a_move_right_after_a_press_in_the_same_cell_is_not_reported() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Drag);
    r.press(Button::Left, at(5.1, 2.1), NONE, modes, SIZE)
        .unwrap();
    assert_eq!(
        r.motion(Some(Button::Left), at(5.5, 2.5), NONE, modes, SIZE),
        None
    );
}

#[test]
fn a_drag_out_of_the_screen_is_reported_at_the_edge() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Drag);
    let bytes = r.motion(Some(Button::Left), at(-4., 30.), NONE, modes, SIZE);
    assert_eq!(text(bytes).unwrap(), "\x1b[<32;1;24M");
}

#[test]
fn a_bare_move_out_of_the_screen_is_not_reported() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Motion);
    assert_eq!(r.motion(None, at(-1., 3.), NONE, modes, SIZE), None);
}

#[test]
fn moves_are_not_reported_to_a_program_that_wants_only_clicks() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    assert_eq!(
        r.motion(Some(Button::Left), at(1., 1.), NONE, modes, SIZE),
        None
    );
}

#[test]
fn each_whole_line_of_scrolling_is_one_wheel_step() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    let up = r.wheel((0., 2.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(up, b"\x1b[<64;1;1M\x1b[<64;1;1M");
    let down = r.wheel((0., -1.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(down, b"\x1b[<65;1;1M");
}

#[test]
fn parts_of_a_line_add_up_to_a_step() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    assert!(r.wheel((0., 0.6), at(0., 0.), NONE, modes, SIZE).is_empty());
    let step = r.wheel((0., 0.6), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(step, b"\x1b[<64;1;1M");
}

#[test]
fn sideways_scrolling_is_the_left_and_right_wheel() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    let left = r.wheel((1., 0.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(left, b"\x1b[<66;1;1M");
    let right = r.wheel((-1., 0.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(right, b"\x1b[<67;1;1M");
}

#[test]
fn the_wheel_carries_the_modifiers() {
    let mut r = MouseReporter::default();
    let modes = sgr(MouseTracking::Click);
    let shift = Mods {
        shift: true,
        ..NONE
    };
    let bytes = r.wheel((0., 1.), at(0., 0.), shift, modes, SIZE);
    assert_eq!(bytes, b"\x1b[<68;1;1M");
}

#[test]
fn alternate_screen_wheel_sends_arrow_keys_without_mouse_tracking() {
    let mut r = MouseReporter::default();
    let modes = alt_screen(false);
    let up = r.wheel((0., 3.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(up, b"\x1b[A\x1b[A\x1b[A");
    let down = r.wheel((0., -3.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(down, b"\x1b[B\x1b[B\x1b[B");
}

#[test]
fn alternate_screen_wheel_follows_application_cursor_mode() {
    let mut r = MouseReporter::default();
    let modes = alt_screen(true);
    let up = r.wheel((0., 3.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(up, b"\x1bOA\x1bOA\x1bOA");
    let down = r.wheel((0., -3.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(down, b"\x1bOB\x1bOB\x1bOB");
}

#[test]
fn alternate_screen_wheel_carries_fractional_lines() {
    let modes = alt_screen(false);
    let mut up = MouseReporter::default();
    assert!(
        up.wheel((0., 0.4), at(0., 0.), NONE, modes, SIZE)
            .is_empty()
    );
    assert!(
        up.wheel((0., 0.4), at(0., 0.), NONE, modes, SIZE)
            .is_empty()
    );
    assert_eq!(
        up.wheel((0., 0.4), at(0., 0.), NONE, modes, SIZE),
        b"\x1b[A"
    );

    let mut down = MouseReporter::default();
    assert!(
        down.wheel((0., -0.4), at(0., 0.), NONE, modes, SIZE)
            .is_empty()
    );
    assert!(
        down.wheel((0., -0.4), at(0., 0.), NONE, modes, SIZE)
            .is_empty()
    );
    assert_eq!(
        down.wheel((0., -0.4), at(0., 0.), NONE, modes, SIZE),
        b"\x1b[B"
    );
}

#[test]
fn alternate_screen_wheel_ignores_horizontal_scrolling() {
    let mut r = MouseReporter::default();
    let modes = alt_screen(false);
    assert!(r.wheel((3., 0.), at(0., 0.), NONE, modes, SIZE).is_empty());
    assert!(r.wheel((-3., 0.), at(0., 0.), NONE, modes, SIZE).is_empty());
}

#[test]
fn alternate_screen_wheel_keeps_mouse_reports_when_tracking_is_on() {
    let mut r = MouseReporter::default();
    let modes = Modes {
        alternate_screen: true,
        ..sgr(MouseTracking::Click)
    };
    let up = r.wheel((0., 2.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(up, b"\x1b[<64;1;1M\x1b[<64;1;1M");
    let down = r.wheel((0., -1.), at(0., 0.), NONE, modes, SIZE);
    assert_eq!(down, b"\x1b[<65;1;1M");
}

#[test]
fn leaving_alternate_screen_resets_fractional_wheel_remainder() {
    let mut r = MouseReporter::default();
    let alt = alt_screen(false);
    let primary = Modes::default();
    assert!(r.wheel((0., 0.5), at(0., 0.), NONE, alt, SIZE).is_empty());
    assert!(
        r.wheel((0., 0.5), at(0., 0.), NONE, primary, SIZE)
            .is_empty()
    );
    assert!(r.wheel((0., 0.6), at(0., 0.), NONE, alt, SIZE).is_empty());
}

/// One thing the pointer did, for the properties below.
#[derive(Debug, Clone)]
enum Step {
    Press(Button, Pointer),
    Release(Button, Pointer),
    Motion(Option<Button>, Pointer),
    /// Rows scrolled, in quarters of a line so that sums are exact.
    Wheel(i8, Pointer),
}

fn pointer() -> impl Strategy<Value = Pointer> {
    (-10.0f32..100.0, -10.0f32..40.0).prop_map(|(x, y)| Pointer { x, y })
}

fn step() -> impl Strategy<Value = Step> {
    let button = prop::sample::select(vec![Button::Left, Button::Middle, Button::Right]);
    prop_oneof![
        (button.clone(), pointer()).prop_map(|(b, p)| Step::Press(b, p)),
        (button.clone(), pointer()).prop_map(|(b, p)| Step::Release(b, p)),
        (prop::option::of(button), pointer()).prop_map(|(b, p)| Step::Motion(b, p)),
        (-12i8..=12, pointer()).prop_map(|(q, p)| Step::Wheel(q, p)),
    ]
}

fn tracking() -> impl Strategy<Value = MouseTracking> {
    prop::sample::select(vec![
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ])
}

/// Splits SGR output into reports of (code, col, row).
fn reports(bytes: &[u8]) -> Vec<(u32, u32, u32)> {
    let text = std::str::from_utf8(bytes).unwrap();
    text.split("\x1b[<")
        .filter(|part| !part.is_empty())
        .map(|part| {
            let body = part.strip_suffix(['M', 'm']).unwrap();
            let n: Vec<u32> = body.split(';').map(|n| n.parse().unwrap()).collect();
            (n[0], n[1], n[2])
        })
        .collect()
}

proptest! {
    #[test]
    fn every_reported_cell_is_on_the_screen(
        tracking in tracking(),
        steps in prop::collection::vec(step(), 0..40),
    ) {
        let modes = sgr(tracking);
        let mut r = MouseReporter::default();
        for step in steps {
            let bytes = match step {
                Step::Press(b, p) => r.press(b, p, NONE, modes, SIZE).unwrap_or_default(),
                Step::Release(b, p) => r.release(b, p, NONE, modes, SIZE).unwrap_or_default(),
                Step::Motion(b, p) => r.motion(b, p, NONE, modes, SIZE).unwrap_or_default(),
                Step::Wheel(q, p) => r.wheel((0., f32::from(q) / 4.), p, NONE, modes, SIZE),
            };
            for (_, col, row) in reports(&bytes) {
                prop_assert!((1..=u32::from(SIZE.cols)).contains(&col));
                prop_assert!((1..=u32::from(SIZE.rows)).contains(&row));
            }
        }
    }

    #[test]
    fn two_reports_in_a_row_from_moves_never_name_the_same_cell(
        moves in prop::collection::vec((prop::option::of(Just(Button::Left)), pointer()), 0..40),
    ) {
        let modes = sgr(MouseTracking::Motion);
        let mut r = MouseReporter::default();
        let mut last = None;
        for (held, p) in moves {
            if let Some(bytes) = r.motion(held, p, NONE, modes, SIZE) {
                let (_, col, row) = reports(&bytes)[0];
                prop_assert_ne!(last, Some((col, row)));
                last = Some((col, row));
            }
        }
    }

    /// The wheel steps sent so far are always less than a line away from
    /// the lines scrolled so far: no part of a line is lost or doubled.
    #[test]
    fn wheel_steps_stay_within_a_line_of_the_lines_scrolled(
        quarters in prop::collection::vec(-12i8..=12, 0..40),
    ) {
        let modes = sgr(MouseTracking::Click);
        let mut r = MouseReporter::default();
        let (mut total, mut sent) = (0i32, 0i32);
        for q in quarters {
            total += i32::from(q);
            let bytes = r.wheel((0., f32::from(q) / 4.), at(0., 0.), NONE, modes, SIZE);
            for (code, ..) in reports(&bytes) {
                sent += if code == 64 { 1 } else { -1 };
            }
            prop_assert!((4 * sent - total).abs() < 4, "sent {} for {} quarters", sent, total);
        }
    }

    /// On the alternate screen without mouse tracking, arrow keys stay
    /// within a line of the vertical lines scrolled, matching mouse reports.
    #[test]
    fn alternate_screen_wheel_steps_stay_within_a_line_of_the_lines_scrolled(
        app_cursor_keys in any::<bool>(),
        quarters in prop::collection::vec(-12i8..=12, 0..40),
    ) {
        let modes = alt_screen(app_cursor_keys);
        let up = if app_cursor_keys { b"\x1bOA" } else { b"\x1b[A" };
        let down = if app_cursor_keys { b"\x1bOB" } else { b"\x1b[B" };
        let mut r = MouseReporter::default();
        let (mut total, mut sent) = (0i32, 0i32);
        for q in quarters {
            total += i32::from(q);
            let bytes = r.wheel((0., f32::from(q) / 4.), at(0., 0.), NONE, modes, SIZE);
            let (chunks, rest) = bytes.as_chunks::<3>();
            prop_assert!(rest.is_empty());
            for chunk in chunks {
                if chunk == up {
                    sent += 1;
                } else if chunk == down {
                    sent -= 1;
                } else {
                    prop_assert!(false, "unexpected bytes {:?}", chunk);
                }
            }
            prop_assert!((4 * sent - total).abs() < 4, "sent {} for {} quarters", sent, total);
        }
    }
}
