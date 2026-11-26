//! Properties of the mouse report formats that hold for every action,
//! cell and modifier.

use benterm_core::MouseEncoding;
use benterm_ui::input::mouse::{Action, Button, CellPoint, Mods, MouseEvent, Wheel, encode};
use proptest::prelude::*;

fn button() -> impl Strategy<Value = Button> {
    prop::sample::select(vec![Button::Left, Button::Middle, Button::Right])
}

fn action() -> impl Strategy<Value = Action> {
    let wheel = prop::sample::select(vec![Wheel::Up, Wheel::Down, Wheel::Left, Wheel::Right]);
    prop_oneof![
        button().prop_map(Action::Press),
        button().prop_map(Action::Release),
        prop::option::of(button()).prop_map(Action::Motion),
        wheel.prop_map(Action::Wheel),
    ]
}

fn mods() -> impl Strategy<Value = Mods> {
    any::<[bool; 3]>().prop_map(|[shift, alt, control]| Mods {
        shift,
        alt,
        control,
    })
}

fn event(max_cell: u16) -> impl Strategy<Value = MouseEvent> {
    (action(), 0..=max_cell, 0..=max_cell, mods()).prop_map(|(action, col, row, mods)| MouseEvent {
        action,
        cell: CellPoint { col, row },
        mods,
    })
}

/// The button code, column and row of an SGR report, and whether it is a
/// release.
fn parse_sgr(bytes: &[u8]) -> (u32, u32, u32, bool) {
    let text = std::str::from_utf8(bytes).expect("SGR reports are ASCII");
    let body = text.strip_prefix("\x1b[<").expect("the SGR prefix");
    let release = body.ends_with('m');
    let body = body.strip_suffix(['M', 'm']).expect("the SGR final byte");
    let numbers: Vec<u32> = body.split(';').map(|n| n.parse().unwrap()).collect();
    assert_eq!(numbers.len(), 3, "{text:?}");
    (numbers[0], numbers[1], numbers[2], release)
}

/// The button code, column and row of a legacy report, each offset by
/// 32: single bytes in X10, characters in UTF-8.
fn parse_legacy(bytes: &[u8], encoding: MouseEncoding) -> (u32, u32, u32) {
    let body = bytes.strip_prefix(b"\x1b[M").expect("the legacy prefix");
    let cells: Vec<u32> = if encoding == MouseEncoding::Utf8 {
        let text = std::str::from_utf8(&body[1..]).expect("the cells decode as UTF-8");
        text.chars().map(|c| c as u32 - 32).collect()
    } else {
        body[1..].iter().map(|b| u32::from(*b) - 32).collect()
    };
    assert_eq!(cells.len(), 2);
    (u32::from(body[0]) - 32, cells[0], cells[1])
}

fn mod_bits(mods: Mods) -> u32 {
    4 * u32::from(mods.shift) + 8 * u32::from(mods.alt) + 16 * u32::from(mods.control)
}

proptest! {
    #[test]
    fn sgr_carries_every_cell_counted_from_one(e in event(u16::MAX)) {
        let bytes = encode(&e, MouseEncoding::Sgr).expect("SGR carries every cell");
        let (_, col, row, release) = parse_sgr(&bytes);
        prop_assert_eq!(col, u32::from(e.cell.col) + 1);
        prop_assert_eq!(row, u32::from(e.cell.row) + 1);
        prop_assert_eq!(release, matches!(e.action, Action::Release(_)));
    }

    #[test]
    fn x10_carries_a_cell_exactly_when_it_fits_in_a_byte(e in event(400)) {
        let fits = e.cell.col <= 222 && e.cell.row <= 222;
        let bytes = encode(&e, MouseEncoding::Default);
        prop_assert_eq!(bytes.is_some(), fits);
        if let Some(bytes) = bytes {
            prop_assert_eq!(bytes.len(), 6);
            prop_assert!(bytes[3..].iter().all(|b| *b >= 32));
            prop_assert_eq!(u32::from(bytes[4]) - 32, u32::from(e.cell.col) + 1);
            prop_assert_eq!(u32::from(bytes[5]) - 32, u32::from(e.cell.row) + 1);
        }
    }

    #[test]
    fn utf8_carries_a_cell_exactly_when_it_fits_in_two_bytes(e in event(3000)) {
        let fits = e.cell.col <= 2014 && e.cell.row <= 2014;
        let bytes = encode(&e, MouseEncoding::Utf8);
        prop_assert_eq!(bytes.is_some(), fits);
        if let Some(bytes) = bytes {
            let (_, col, row) = parse_legacy(&bytes, MouseEncoding::Utf8);
            prop_assert_eq!(col, u32::from(e.cell.col) + 1);
            prop_assert_eq!(row, u32::from(e.cell.row) + 1);
        }
    }

    #[test]
    fn every_format_agrees_on_the_modifiers(e in event(200)) {
        let (sgr, ..) = parse_sgr(&encode(&e, MouseEncoding::Sgr).unwrap());
        let (x10, ..) = parse_legacy(&encode(&e, MouseEncoding::Default).unwrap(), MouseEncoding::Default);
        let (utf8, ..) = parse_legacy(&encode(&e, MouseEncoding::Utf8).unwrap(), MouseEncoding::Utf8);
        prop_assert_eq!(sgr & 28, mod_bits(e.mods));
        prop_assert_eq!(x10 & 28, mod_bits(e.mods));
        prop_assert_eq!(utf8, x10);
    }

    /// A release names its button in SGR and is button 3 in the legacy
    /// formats; either way it matches its press in all other bits.
    #[test]
    fn a_release_pairs_with_its_press(
        b in button(),
        col in 0..=200u16,
        row in 0..=200u16,
        m in mods(),
    ) {
        let at = CellPoint { col, row };
        let press = MouseEvent { action: Action::Press(b), cell: at, mods: m };
        let release = MouseEvent { action: Action::Release(b), cell: at, mods: m };
        let (p, pc, pr, p_up) = parse_sgr(&encode(&press, MouseEncoding::Sgr).unwrap());
        let (r, rc, rr, r_up) = parse_sgr(&encode(&release, MouseEncoding::Sgr).unwrap());
        prop_assert_eq!((p, pc, pr, p_up), (r, rc, rr, !r_up));
        for encoding in [MouseEncoding::Default, MouseEncoding::Utf8] {
            let (p, pc, pr) = parse_legacy(&encode(&press, encoding).unwrap(), encoding);
            let (r, rc, rr) = parse_legacy(&encode(&release, encoding).unwrap(), encoding);
            prop_assert_eq!(p & 3, b as u32);
            prop_assert_eq!(r & 3, 3);
            prop_assert_eq!((p & !3, pc, pr), (r & !3, rc, rr));
        }
    }
}
