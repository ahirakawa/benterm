//! Properties that hold for any input.
//!
//! The checks are written against `TerminalCore`, not `AlacrittyCore`, so a
//! second implementation can be put through the same properties.

use benterm_core::{
    AlacrittyCore, CellWidth, Damage, GridSize, Row, RowEnd, TerminalCore, TerminalEvent,
};
use proptest::collection::vec;
use proptest::prelude::*;

const SCROLLBACK_LIMIT: usize = 40;

#[derive(Debug, Clone)]
enum Op {
    Feed(Vec<u8>),
    Resize(GridSize),
}

fn new_core(size: GridSize) -> AlacrittyCore {
    AlacrittyCore::new(size, SCROLLBACK_LIMIT)
}

/// Sizes include ones below the minimum of 1x2, which the core must raise.
fn grid_size() -> impl Strategy<Value = GridSize> {
    (0u16..=40, 0u16..=100).prop_map(|(rows, cols)| GridSize::new(rows, cols))
}

/// A CSI sequence with arbitrary parameters, so that cursor movement, erasing,
/// scrolling, modes and SGR are reached far more often than by random bytes.
fn csi() -> impl Strategy<Value = Vec<u8>> {
    let params = vec(prop_oneof![0u16..=10, any::<u16>()], 0..4);
    let prefix = prop_oneof![3 => Just(""), 1 => Just("?"), 1 => Just(">")];
    let intermediate = prop_oneof![4 => Just(""), 1 => Just(" "), 1 => Just("$")];
    (prefix, params, intermediate, 0x40u8..=0x7e).prop_map(
        |(prefix, params, intermediate, last)| {
            let params: Vec<String> = params.iter().map(u16::to_string).collect();
            let mut bytes = format!("\x1b[{prefix}{}{intermediate}", params.join(";")).into_bytes();
            bytes.push(last);
            bytes
        },
    )
}

fn osc() -> impl Strategy<Value = Vec<u8>> {
    let number = prop_oneof![Just(0u16), Just(2), Just(9), Just(777), 0u16..=120];
    let terminator = prop_oneof![Just("\x07"), Just("\x1b\\"), Just("")];
    (number, "[a-z0-9;あ ]{0,12}", terminator).prop_map(|(number, text, terminator)| {
        format!("\x1b]{number};{text}{terminator}").into_bytes()
    })
}

fn fragment() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => vec(any::<u8>(), 0..24),
        3 => "[ -~]{0,24}".prop_map(String::into_bytes),
        2 => "[a-zあ漢🙂\u{301}\u{200d}ｱ]{0,8}".prop_map(String::into_bytes),
        2 => prop_oneof![Just("\r\n"), Just("\n"), Just("\r"), Just("\t"), Just("\x08")]
            .prop_map(|text| text.as_bytes().to_vec()),
        4 => csi(),
        1 => osc(),
        1 => (any::<u8>()).prop_map(|byte| vec![0x1b, byte]),
        1 => prop_oneof![
            Just("\x1b[?1049h"), Just("\x1b[?1049l"), Just("\x1b[?2026h"), Just("\x1b[?2026l"),
            Just("\x1bc"), Just("\x1b7"), Just("\x1b8"), Just("\x1bM"), Just("\x1b[?6h"),
        ]
        .prop_map(|text| text.as_bytes().to_vec()),
    ]
}

fn bytes() -> impl Strategy<Value = Vec<u8>> {
    vec(fragment(), 0..12).prop_map(|fragments| fragments.concat())
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    vec(
        prop_oneof![3 => bytes().prop_map(Op::Feed), 1 => grid_size().prop_map(Op::Resize)],
        0..12,
    )
}

fn apply(core: &mut impl TerminalCore, op: &Op) {
    match op {
        Op::Feed(bytes) => core.feed(bytes),
        Op::Resize(size) => core.resize(*size),
    }
}

/// The size the core must report after being asked for `requested`.
fn effective(requested: GridSize) -> GridSize {
    GridSize::new(requested.rows.max(1), requested.cols.max(2))
}

fn check_row(row: &Row, cols: u16) -> Result<(), TestCaseError> {
    prop_assert_eq!(row.cells.len(), usize::from(cols));
    // The two halves of a wide character always come as a pair.
    for (index, cell) in row.cells.iter().enumerate() {
        let width_of = |index: usize| row.cells.get(index).map(|cell| cell.width);
        match cell.width {
            CellWidth::Narrow => {}
            CellWidth::Wide => prop_assert_eq!(
                width_of(index + 1),
                Some(CellWidth::WideSpacer),
                "wide character without its second half at column {}",
                index
            ),
            CellWidth::WideSpacer => prop_assert_eq!(
                index.checked_sub(1).and_then(width_of),
                Some(CellWidth::Wide),
                "second half without its wide character at column {}",
                index
            ),
        }
    }
    // The blank before a wide character that went to the next row is a
    // plain narrow blank, so a renderer needs no case for it.
    if row.end == RowEnd::WrappedBeforeWide {
        let last = row.cells.last();
        prop_assert_eq!(
            last.map(|cell| (cell.ch, cell.width)),
            Some((' ', CellWidth::Narrow))
        );
    }
    Ok(())
}

/// Everything a caller may rely on at any moment.
fn check_invariants(core: &impl TerminalCore, expected: GridSize) -> Result<(), TestCaseError> {
    let size = core.size();
    prop_assert_eq!(size, expected);

    let snapshot = core.snapshot();
    prop_assert_eq!(snapshot.size, size);
    prop_assert_eq!(snapshot.rows.len(), usize::from(size.rows));
    for row in &snapshot.rows {
        check_row(row, size.cols)?;
    }

    prop_assert!(
        snapshot.cursor.row < size.rows,
        "cursor row {} of {}",
        snapshot.cursor.row,
        size.rows
    );
    prop_assert!(
        snapshot.cursor.col < size.cols,
        "cursor col {} of {}",
        snapshot.cursor.col,
        size.cols
    );

    prop_assert_eq!(snapshot.modes, core.modes());

    let scrollback_len = core.scrollback_len();
    prop_assert_eq!(snapshot.scrollback_len, scrollback_len);
    prop_assert!(scrollback_len <= SCROLLBACK_LIMIT);
    let scrollback = core.scrollback_rows(0..usize::MAX);
    prop_assert_eq!(scrollback.len(), scrollback_len);
    for row in &scrollback {
        check_row(row, size.cols)?;
    }
    if snapshot.modes.alternate_screen {
        prop_assert_eq!(scrollback_len, 0);
    }
    Ok(())
}

/// Everything observable about a core, for comparing two of them.
fn observe(core: &mut impl TerminalCore) -> impl PartialEq + std::fmt::Debug {
    (
        core.snapshot(),
        core.scrollback_evicted(),
        core.scrollback_rows(0..usize::MAX),
        core.take_events(),
        core.take_pty_replies(),
    )
}

/// Brings `mirror` up to date using only the rows the core reports as
/// changed.
fn refresh_mirror(core: &mut impl TerminalCore, mirror: &mut Vec<Row>) {
    match core.take_damage() {
        Damage::Full => *mirror = core.screen_rows(0..core.size().rows),
        Damage::Rows(rows) => {
            for row in rows {
                mirror[usize::from(row)] = core.screen_rows(row..row + 1).remove(0);
            }
        }
    }
}

proptest! {
    /// A renderer that re-reads only damaged rows sees the same screen as
    /// one that takes a full snapshot.
    #[test]
    fn damage_covers_every_changed_row(size in grid_size(), ops in ops()) {
        let mut core = new_core(size);
        let mut mirror = Vec::new();
        refresh_mirror(&mut core, &mut mirror);
        for op in &ops {
            apply(&mut core, op);
            refresh_mirror(&mut core, &mut mirror);
            let snapshot = core.snapshot();
            prop_assert_eq!(&mirror, &snapshot.rows);
            prop_assert_eq!(core.cursor(), snapshot.cursor);
        }
    }
}

proptest! {
    #[test]
    fn arbitrary_bytes_keep_the_invariants(size in grid_size(), input in vec(any::<u8>(), 0..2048)) {
        let mut core = new_core(size);
        check_invariants(&core, effective(size))?;
        core.feed(&input);
        check_invariants(&core, effective(size))?;
    }

    #[test]
    fn feeds_and_resizes_keep_the_invariants(size in grid_size(), ops in ops()) {
        let mut core = new_core(size);
        let mut expected = effective(size);
        for op in &ops {
            apply(&mut core, op);
            if let Op::Resize(size) = op {
                expected = effective(*size);
            }
            check_invariants(&core, expected)?;
        }
    }

    #[test]
    fn same_input_gives_the_same_state(size in grid_size(), ops in ops()) {
        let mut a = new_core(size);
        let mut b = new_core(size);
        for op in &ops {
            apply(&mut a, op);
            apply(&mut b, op);
        }
        prop_assert_eq!(observe(&mut a), observe(&mut b));
    }

    #[test]
    fn splitting_the_input_does_not_change_the_result(
        size in grid_size(),
        input in bytes(),
        cuts in vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let mut whole = new_core(size);
        whole.feed(&input);

        let mut cuts: Vec<usize> = cuts.iter().map(|cut| cut.index(input.len() + 1)).collect();
        cuts.sort_unstable();
        let mut pieces = new_core(size);
        let mut start = 0;
        for cut in cuts {
            pieces.feed(&input[start..cut]);
            start = cut;
        }
        pieces.feed(&input[start..]);

        prop_assert_eq!(observe(&mut whole), observe(&mut pieces));
    }

    #[test]
    fn taken_events_and_replies_are_not_returned_again(size in grid_size(), input in bytes()) {
        let mut core = new_core(size);
        core.feed(&input);
        core.take_events();
        core.take_pty_replies();
        prop_assert_eq!(core.take_events(), Vec::<TerminalEvent>::new());
        prop_assert_eq!(core.take_pty_replies(), Vec::<u8>::new());
    }
}
