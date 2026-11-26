//! The graph of the progress board: where the agents are put, which
//! lines join them, and what the lines of their messages say.

use benterm_core::Rgb;
use benterm_ui::graph::{
    Curve, Layout, MessageKind, Messages, Node, arrows, clock, first_line, layout, message_curve,
    prominence, shown, shrink,
};
use benterm_ui::theme::Theme;
use benterm_ui::timeline::BandState;

const CONDUCTOR: u64 = 1;
const A: u64 = 2;
const B: u64 = 3;
const C: u64 = 4;

/// The conductor started a and b, and a started c.
fn family() -> Vec<(u64, Option<u64>)> {
    vec![
        (CONDUCTOR, None),
        (A, Some(CONDUCTOR)),
        (B, Some(CONDUCTOR)),
        (C, Some(A)),
    ]
}

fn row(layout: &Layout, id: u64) -> usize {
    layout.spot(id).expect("the node has a place").row
}

fn column(layout: &Layout, id: u64) -> f32 {
    layout.spot(id).expect("the node has a place").column
}

fn node(id: u64, parent: Option<u64>, state: Option<BandState>) -> Node {
    Node {
        id,
        parent,
        name: format!("n{id}"),
        agent: Some("claude"),
        state,
        closed: false,
        background: false,
    }
}

#[test]
fn a_family_of_four_has_four_nodes_and_three_lines_from_parent_to_child() {
    let layout = layout(&family());

    assert_eq!(layout.spots.len(), 4);
    assert_eq!(layout.links, [(CONDUCTOR, A), (CONDUCTOR, B), (A, C)]);
}

#[test]
fn a_parent_is_above_its_children() {
    let layout = layout(&family());

    assert_eq!(row(&layout, CONDUCTOR), 0);
    assert_eq!((row(&layout, A), row(&layout, B)), (1, 1));
    assert_eq!(row(&layout, C), 2);
    assert_eq!(layout.rows(), 3);
}

#[test]
fn children_are_side_by_side_in_their_order_and_their_parent_is_over_their_middle() {
    let layout = layout(&family());

    assert!(column(&layout, A) < column(&layout, B));
    assert_eq!(
        column(&layout, CONDUCTOR),
        (column(&layout, A) + column(&layout, B)) / 2.
    );
    // An only child is right under its parent.
    assert_eq!(column(&layout, C), column(&layout, A));
    assert_eq!(layout.columns(), 2.);
}

#[test]
fn no_two_nodes_share_a_place() {
    let mut nodes = family();
    nodes.extend([(5, Some(B)), (6, Some(B)), (7, None), (8, Some(7))]);
    let layout = layout(&nodes);

    let mut places: Vec<(usize, i32)> = layout
        .spots
        .iter()
        .map(|spot| (spot.row, (spot.column * 2.) as i32))
        .collect();
    places.sort();
    places.dedup();
    assert_eq!(places.len(), nodes.len());
}

#[test]
fn agents_nobody_started_stand_side_by_side_in_the_top_row() {
    let layout = layout(&[(1, None), (2, None), (3, Some(2))]);

    assert_eq!((row(&layout, 1), row(&layout, 2)), (0, 0));
    assert!(column(&layout, 1) < column(&layout, 2));
    assert!(layout.links == [(2, 3)]);
}

#[test]
fn a_child_whose_parent_is_not_in_the_graph_stands_in_the_top_row_with_its_own_children() {
    let layout = layout(&[(2, Some(99)), (3, Some(2))]);

    assert_eq!((row(&layout, 2), row(&layout, 3)), (0, 1));
    assert_eq!(layout.links, [(2, 3)]);
}

#[test]
fn nodes_that_name_each_other_as_parents_are_all_placed_once() {
    let layout = layout(&[(1, Some(2)), (2, Some(1)), (3, Some(3))]);

    assert_eq!(layout.spots.len(), 3);
    assert_eq!(layout.links.len(), 1);
}

#[test]
fn the_places_do_not_depend_on_the_states_of_the_agents() {
    // Only ids and parents are given, so a change of state moves nothing.
    assert_eq!(layout(&family()), layout(&family()));
}

#[test]
fn the_four_states_of_a_node_have_the_four_colors_of_the_timeline() {
    let theme = Theme::default().board;
    let colors: Vec<Rgb> = BandState::ALL
        .into_iter()
        .map(|state| node(1, None, Some(state)).color(&theme))
        .collect();

    assert_eq!(
        colors,
        [theme.working, theme.waiting, theme.done, theme.error]
    );
    // Four colors, none of them twice.
    for (position, color) in colors.iter().enumerate() {
        assert!(!colors[position + 1..].contains(color));
    }
    // A terminal with no agent in a state has none of the four.
    assert!(!colors.contains(&node(1, None, None).color(&theme)));
}

#[test]
fn ended_agents_are_left_out_when_asked_unless_a_shown_agent_hangs_under_them() {
    let done = Some(BandState::Done);
    let working = Some(BandState::Working);
    let mut closed = node(6, Some(1), Some(BandState::Error));
    closed.closed = true;
    let nodes = [
        node(1, None, done),
        node(2, Some(1), working),
        node(3, Some(1), done),
        node(4, None, done),
        node(5, Some(4), done),
        closed,
    ];
    let ids = |hide| -> Vec<u64> { shown(&nodes, hide).iter().map(|node| node.id).collect() };

    assert_eq!(ids(false), [1, 2, 3, 4, 5, 6]);
    // 1 is done, and stays as the parent of 2, which is at work.
    assert_eq!(ids(true), [1, 2]);
}

#[test]
fn an_agent_is_ended_when_it_is_done_or_its_terminal_has_closed() {
    assert!(node(1, None, Some(BandState::Done)).ended());
    assert!(!node(1, None, Some(BandState::Working)).ended());
    assert!(!node(1, None, Some(BandState::Waiting)).ended());
    assert!(!node(1, None, Some(BandState::Error)).ended());
    assert!(!node(1, None, None).ended());
    let mut closed = node(1, None, Some(BandState::Working));
    closed.closed = true;
    assert!(closed.ended());
}

#[test]
fn the_first_line_of_a_message_is_its_first_line_with_something_in_it() {
    assert_eq!(first_line("done\nthe rest"), "done");
    assert_eq!(first_line("\n\n  PR 12 is up  \nmore"), "PR 12 is up");
    assert_eq!(first_line(""), "");
    assert_eq!(first_line("a\u{1b}[31mb\tc"), "a[31mb c");
}

#[test]
fn a_long_first_line_is_cut_between_characters() {
    let line = first_line(&"あ".repeat(500));

    assert_eq!(line.chars().count(), 201);
    assert!(line.ends_with('…'));
}

#[test]
fn messages_are_kept_in_order_with_their_sender_and_their_time() {
    let mut messages = Messages::default();
    messages.push(MessageKind::Request, CONDUCTOR, A, 1_000, "do this\nplease");
    messages.push(MessageKind::Report, A, CONDUCTOR, 2_000, "done");

    let kept: Vec<_> = messages
        .iter()
        .map(|m| (m.kind, m.from, m.to, m.at_ms, m.line.as_str()))
        .collect();
    assert_eq!(
        kept,
        [
            (MessageKind::Request, CONDUCTOR, A, 1_000, "do this"),
            (MessageKind::Report, A, CONDUCTOR, 2_000, "done"),
        ]
    );
}

#[test]
fn only_the_latest_messages_are_kept() {
    let mut messages = Messages::default();
    for n in 0..Messages::CAPACITY as u64 + 10 {
        messages.push(MessageKind::Report, A, CONDUCTOR, n, "x");
    }

    assert_eq!(messages.iter().count(), Messages::CAPACITY);
    assert_eq!(messages.iter().next().unwrap().at_ms, 10);
}

#[test]
fn each_direction_between_two_agents_is_one_arrow_with_its_latest_message() {
    let mut messages = Messages::default();
    messages.push(MessageKind::Request, CONDUCTOR, A, 1_000, "first");
    messages.push(MessageKind::Report, A, CONDUCTOR, 2_000, "done");
    messages.push(MessageKind::Request, CONDUCTOR, A, 3_000, "second");
    messages.push(MessageKind::Report, B, CONDUCTOR, 4_000, "from b");

    let arrows = arrows(messages.iter(), |_| true);

    let told: Vec<_> = arrows
        .iter()
        .map(|a| (a.from, a.to, a.count, a.latest.line.as_str()))
        .collect();
    assert_eq!(
        told,
        [
            (CONDUCTOR, A, 2, "second"),
            (A, CONDUCTOR, 1, "done"),
            (B, CONDUCTOR, 1, "from b"),
        ]
    );
}

#[test]
fn a_message_of_an_agent_the_graph_does_not_show_has_no_arrow() {
    let mut messages = Messages::default();
    messages.push(MessageKind::Report, A, CONDUCTOR, 2_000, "done");
    messages.push(MessageKind::Report, B, CONDUCTOR, 3_000, "done");

    let arrows = arrows(messages.iter(), |id| id != B);

    assert_eq!(arrows.len(), 1);
    assert_eq!(arrows[0].from, A);
}

#[test]
fn a_message_stands_out_less_the_longer_ago_it_was_sent() {
    const MINUTE: u64 = 60_000;

    assert_eq!(prominence(0), 1.);
    assert!(prominence(MINUTE) < prominence(0));
    assert!(prominence(10 * MINUTE) < prominence(MINUTE));
    // Never so faint that the line cannot be seen.
    assert!(prominence(24 * 60 * MINUTE) >= 0.25);
    // In steps, so that a line is not drawn again for every second.
    assert_eq!(prominence(60 * MINUTE), prominence(61 * MINUTE));
}

#[test]
fn the_time_of_a_message_is_told_as_the_clock_of_the_day() {
    // 2026-11-22 10:20:30 UTC.
    let at_ms = 1_795_342_830_000;

    assert_eq!(clock(at_ms, 0), "10:20:30");
    assert_eq!(clock(at_ms, 9 * 3600), "19:20:30");
    assert_eq!(clock(at_ms, -11 * 3600), "23:20:30");
}

fn close(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
}

#[test]
fn the_line_of_a_message_goes_from_the_edge_of_its_sender_to_the_edge_of_its_receiver() {
    // Boxes 100 wide and 40 high, one right under the other.
    let half = (50., 20.);
    let curve = message_curve((100., 100.), (100., 300.), half);

    // Out of the bottom of the sender and into the top of the receiver.
    assert!((curve.start.1 - 120.).abs() < 0.01, "{curve:?}");
    assert!((curve.end.1 - 280.).abs() < 0.01, "{curve:?}");
    assert!(close(curve.at(0.), curve.start));
    assert!(close(curve.at(1.), curve.end));
}

#[test]
fn the_lines_of_the_two_directions_between_two_agents_bend_apart() {
    let half = (50., 20.);
    let down = message_curve((100., 100.), (100., 300.), half);
    let up = message_curve((100., 300.), (100., 100.), half);

    let (down_x, _) = down.at(0.5);
    let (up_x, _) = up.at(0.5);
    // Neither lies on the straight line between the two, where the line
    // from parent to child is.
    assert!((down_x - 100.).abs() > 10.);
    assert!((up_x - 100.).abs() > 10.);
    assert!((down_x - 100.) * (up_x - 100.) < 0.);
}

#[test]
fn a_point_is_near_a_line_only_close_to_where_the_line_runs() {
    let curve = Curve {
        start: (0., 0.),
        control: (50., 0.),
        end: (100., 0.),
    };

    assert!(curve.distance((50., 3.)) < 3.5);
    assert!(curve.distance((50., 30.)) > 29.);
    assert!(curve.distance((130., 0.)) > 29.);
}

#[test]
fn the_line_between_two_nodes_of_a_row_goes_around_a_node_between_them() {
    // Three boxes in a row, 192 apart; the line goes from the first to
    // the third.
    let half = (80., 24.);
    let curve = message_curve((100., 100.), (484., 100.), half);

    let (x, y) = curve.at(0.5);
    assert!((x - 292.).abs() < 1.);
    // The mark in the middle, 18 across, is clear of the box.
    assert!((y - 100.).abs() > 24. + 9., "{y}");
}

#[test]
fn a_graph_wider_than_its_place_is_shrunk_to_fit_and_no_further_than_told() {
    // Six boxes of 160 with 32 between them are 1120 wide.
    assert_eq!(shrink(1120., 1200., 0.5), 1.);
    assert_eq!(shrink(1120., 1120., 0.5), 1.);
    assert_eq!(shrink(1120., 840., 0.5), 0.75);
    // Boxes too narrow to read are of no use: the rest is scrolled to.
    assert_eq!(shrink(1120., 280., 0.5), 0.5);
    // A place that has not been measured yet shrinks nothing.
    assert_eq!(shrink(1120., 0., 0.5), 1.);
}
