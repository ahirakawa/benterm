//! The kitty keyboard flag stacks, against a model of two stacks.
//!
//! Programs can push as often as they like, so the stacks are capped. A
//! push onto a full stack is ignored.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};
use proptest::prelude::*;

/// The most flags one screen keeps.
const LIMIT: usize = 1024;

#[derive(Debug, Clone)]
enum Op {
    /// Pushes `flags` this many times.
    Push(u8, usize),
    Pop(u16),
    AlternateScreen(bool),
    Reset,
}

impl Op {
    fn bytes(&self) -> Vec<u8> {
        match *self {
            Op::Push(flags, times) => format!("\x1b[>{flags}u").repeat(times).into_bytes(),
            Op::Pop(count) => format!("\x1b[<{count}u").into_bytes(),
            Op::AlternateScreen(true) => b"\x1b[?1049h".to_vec(),
            Op::AlternateScreen(false) => b"\x1b[?1049l".to_vec(),
            Op::Reset => b"\x1bc".to_vec(),
        }
    }
}

#[derive(Default)]
struct Model {
    stacks: [Vec<u8>; 2],
    alternate: bool,
}

impl Model {
    fn apply(&mut self, op: &Op) {
        let stack = &mut self.stacks[usize::from(self.alternate)];
        match *op {
            Op::Push(flags, times) => {
                let room = LIMIT - stack.len();
                stack.extend(std::iter::repeat_n(flags, times.min(room)));
            }
            Op::Pop(count) => {
                let count = if count == 0 { 1 } else { usize::from(count) };
                stack.truncate(stack.len().saturating_sub(count));
            }
            Op::AlternateScreen(on) => self.alternate = on,
            Op::Reset => *self = Model::default(),
        }
    }

    fn flags(&self) -> u8 {
        let stack = &self.stacks[usize::from(self.alternate)];
        stack.last().copied().unwrap_or(0)
    }
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0u8..32, prop_oneof![1usize..4, 1000usize..1100]).prop_map(|(f, n)| Op::Push(f, n)),
        3 => (0u16..1100).prop_map(Op::Pop),
        2 => any::<bool>().prop_map(Op::AlternateScreen),
        1 => Just(Op::Reset),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn the_flags_in_use_are_the_top_of_the_screens_stack(ops in prop::collection::vec(op(), 0..12)) {
        let mut core = AlacrittyCore::new(GridSize::new(4, 20), 10);
        let mut model = Model::default();
        for op in &ops {
            core.feed(&op.bytes());
            model.apply(op);
            core.feed(b"\x1b[?u");
            let reply = core.take_pty_replies();
            prop_assert_eq!(reply, format!("\x1b[?{}u", model.flags()).into_bytes());
        }
    }
}

#[test]
fn a_push_onto_a_full_stack_is_ignored() {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 10);
    core.feed(&b"\x1b[>1u".repeat(LIMIT));
    core.feed(b"\x1b[>4u\x1b[?u");
    assert_eq!(core.take_pty_replies(), b"\x1b[?1u");
    // The cancelled push leaves no stray text on the screen.
    assert!(
        core.snapshot().rows[0]
            .cells
            .iter()
            .all(|cell| cell.ch == ' ')
    );
}
