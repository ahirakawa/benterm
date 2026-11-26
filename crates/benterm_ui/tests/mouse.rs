//! The bytes of mouse reports in each format.

use benterm_core::{Modes, MouseEncoding, MouseTracking};
use benterm_ui::input::mouse::{
    Action, Button, CellPoint, Mods, MouseEvent, Wheel, encode, mouse_bytes, reports,
};

fn event(action: Action, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        action,
        cell: CellPoint { col, row },
        mods: Mods::default(),
    }
}

fn modes(tracking: MouseTracking, encoding: MouseEncoding) -> Modes {
    Modes {
        mouse_tracking: tracking,
        mouse_encoding: encoding,
        ..Modes::default()
    }
}

const SGR: MouseEncoding = MouseEncoding::Sgr;
const UTF8: MouseEncoding = MouseEncoding::Utf8;
const X10: MouseEncoding = MouseEncoding::Default;

#[test]
fn sgr_press_and_release_differ_only_in_the_final_byte() {
    let press = event(Action::Press(Button::Left), 0, 0);
    let release = event(Action::Release(Button::Left), 0, 0);
    assert_eq!(encode(&press, SGR).unwrap(), b"\x1b[<0;1;1M");
    assert_eq!(encode(&release, SGR).unwrap(), b"\x1b[<0;1;1m");
}

#[test]
fn sgr_numbers_buttons_and_counts_cells_from_one() {
    let middle = event(Action::Press(Button::Middle), 4, 9);
    let right = event(Action::Release(Button::Right), 299, 120);
    assert_eq!(encode(&middle, SGR).unwrap(), b"\x1b[<1;5;10M");
    assert_eq!(encode(&right, SGR).unwrap(), b"\x1b[<2;300;121m");
}

#[test]
fn sgr_has_no_upper_limit_on_cells() {
    let far = event(Action::Press(Button::Left), u16::MAX, u16::MAX);
    assert_eq!(encode(&far, SGR).unwrap(), b"\x1b[<0;65536;65536M");
}

#[test]
fn modifiers_add_to_the_button_code() {
    let mut e = event(Action::Press(Button::Left), 0, 0);
    e.mods.shift = true;
    assert_eq!(encode(&e, SGR).unwrap(), b"\x1b[<4;1;1M");
    e.mods = Mods {
        alt: true,
        ..Mods::default()
    };
    assert_eq!(encode(&e, SGR).unwrap(), b"\x1b[<8;1;1M");
    e.mods = Mods {
        shift: true,
        alt: true,
        control: true,
    };
    assert_eq!(encode(&e, SGR).unwrap(), b"\x1b[<28;1;1M");
}

#[test]
fn motion_adds_32_and_a_bare_move_reports_button_3() {
    let drag = event(Action::Motion(Some(Button::Left)), 2, 3);
    let bare = event(Action::Motion(None), 2, 3);
    assert_eq!(encode(&drag, SGR).unwrap(), b"\x1b[<32;3;4M");
    assert_eq!(encode(&bare, SGR).unwrap(), b"\x1b[<35;3;4M");
}

#[test]
fn wheel_steps_are_buttons_64_to_67_and_always_presses() {
    let cases = [
        (Wheel::Up, &b"\x1b[<64;1;1M"[..]),
        (Wheel::Down, b"\x1b[<65;1;1M"),
        (Wheel::Left, b"\x1b[<66;1;1M"),
        (Wheel::Right, b"\x1b[<67;1;1M"),
    ];
    for (wheel, bytes) in cases {
        assert_eq!(
            encode(&event(Action::Wheel(wheel), 0, 0), SGR).unwrap(),
            bytes
        );
    }
}

#[test]
fn x10_offsets_everything_by_32_and_releases_as_button_3() {
    let press = event(Action::Press(Button::Right), 0, 0);
    let release = event(Action::Release(Button::Right), 9, 19);
    assert_eq!(encode(&press, X10).unwrap(), b"\x1b[M\x22\x21\x21");
    assert_eq!(encode(&release, X10).unwrap(), b"\x1b[M\x23\x2a\x34");
}

#[test]
fn x10_release_keeps_the_modifiers() {
    let mut release = event(Action::Release(Button::Left), 0, 0);
    release.mods.control = true;
    assert_eq!(encode(&release, X10).unwrap(), b"\x1b[M\x33\x21\x21");
}

#[test]
fn x10_carries_cells_up_to_223_and_no_further() {
    let last = event(Action::Press(Button::Left), 222, 222);
    assert_eq!(encode(&last, X10).unwrap(), b"\x1b[M\x20\xff\xff");
    assert_eq!(
        encode(&event(Action::Press(Button::Left), 223, 0), X10),
        None
    );
    assert_eq!(
        encode(&event(Action::Press(Button::Left), 0, 223), X10),
        None
    );
}

#[test]
fn utf8_writes_large_cells_as_two_byte_characters() {
    let near = event(Action::Press(Button::Left), 0, 0);
    assert_eq!(encode(&near, UTF8).unwrap(), b"\x1b[M\x20\x21\x21");
    // Column 95 is 95 + 33 = 128, the first value that needs two bytes.
    let far = event(Action::Press(Button::Left), 95, 0);
    let mut expected = b"\x1b[M\x20".to_vec();
    expected.extend("\u{80}!".as_bytes());
    assert_eq!(encode(&far, UTF8).unwrap(), expected);
}

#[test]
fn utf8_carries_cells_up_to_2015_and_no_further() {
    let last = event(Action::Press(Button::Left), 2014, 2014);
    let mut expected = b"\x1b[M\x20".to_vec();
    expected.extend("\u{7ff}\u{7ff}".as_bytes());
    assert_eq!(encode(&last, UTF8).unwrap(), expected);
    assert_eq!(
        encode(&event(Action::Press(Button::Left), 2015, 0), UTF8),
        None
    );
}

#[test]
fn utf8_releases_as_button_3_like_x10() {
    let release = event(Action::Release(Button::Middle), 0, 0);
    assert_eq!(encode(&release, UTF8).unwrap(), b"\x1b[M\x23\x21\x21");
}

#[test]
fn each_tracking_mode_reports_its_own_actions() {
    use MouseTracking::{Click, Drag, Motion, Off};
    let left = Some(Button::Left);
    let cases = [
        (Action::Press(Button::Left), [false, true, true, true]),
        (Action::Release(Button::Left), [false, true, true, true]),
        (Action::Wheel(Wheel::Up), [false, true, true, true]),
        (Action::Motion(left), [false, false, true, true]),
        (Action::Motion(None), [false, false, false, true]),
    ];
    for (action, expected) in cases {
        let got = [Off, Click, Drag, Motion].map(|tracking| reports(action, tracking));
        assert_eq!(got, expected, "{action:?}");
    }
}

#[test]
fn nothing_is_sent_while_the_program_has_not_asked() {
    let press = event(Action::Press(Button::Left), 0, 0);
    assert_eq!(mouse_bytes(&press, Modes::default()), None);
    assert_eq!(
        mouse_bytes(&press, modes(MouseTracking::Click, SGR)).unwrap(),
        b"\x1b[<0;1;1M"
    );
}
