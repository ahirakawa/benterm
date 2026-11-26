//! Properties that hold after any sequence of operations on a workspace
//! list. Every operation is also done on a plain model, a list of rows, and
//! the two are compared.

use std::collections::HashSet;

use benterm_layout::{WorkspaceColor, WorkspaceError, WorkspaceId, WorkspaceList};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;

#[derive(Clone, Debug)]
enum Op {
    Add(Option<String>),
    Close(Index),
    CloseSelected,
    Select(Index),
    Next,
    Previous,
    Rename(Index, String),
    /// The same operations on a workspace that has been closed.
    Stale(Index, String),
}

/// Names a user might type: plain, Japanese, padded, blank, and with
/// control characters or line separators.
fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-z]{1,8}",
        2 => "[ぁ-ん一-十]{1,4}",
        2 => " {0,2}[a-z]{1,4} {0,2}",
        1 => "[ \t]{0,3}",
        1 => "[a-z]{0,2}[\n\u{1b}\u{7f}\u{0}\u{2028}\u{2029}][a-z]{0,2}",
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => proptest::option::of(name()).prop_map(Op::Add),
        2 => any::<Index>().prop_map(Op::Close),
        1 => Just(Op::CloseSelected),
        2 => any::<Index>().prop_map(Op::Select),
        1 => Just(Op::Next),
        1 => Just(Op::Previous),
        2 => (any::<Index>(), name()).prop_map(|(i, n)| Op::Rename(i, n)),
        1 => (any::<Index>(), name()).prop_map(|(i, n)| Op::Stale(i, n)),
    ]
}

#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: WorkspaceId,
    name: String,
    color: usize,
    content: u32,
}

/// What the list should be, kept in the plainest way.
#[derive(Clone, Debug)]
struct Model {
    rows: Vec<Row>,
    selected: WorkspaceId,
    /// How many workspaces were ever made, for the numbered names.
    made: usize,
    closed: Vec<WorkspaceId>,
}

fn valid(name: &str) -> Option<&str> {
    let name = name.trim();
    let ends_a_line = |ch: char| ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}';
    (!name.is_empty() && !name.chars().any(ends_a_line)).then_some(name)
}

fn rows(workspaces: &WorkspaceList<u32>) -> Vec<Row> {
    workspaces
        .iter()
        .map(|w| Row {
            id: w.id(),
            name: w.name().to_owned(),
            color: w.color().index(),
            content: *w.content(),
        })
        .collect()
}

/// The color a new workspace should get next to `rows`: the one fewest of
/// them have, and of those the first.
fn expected_color(rows: &[Row]) -> usize {
    let uses = |color: usize| rows.iter().filter(|row| row.color == color).count();
    (0..WorkspaceColor::COUNT)
        .min_by_key(|color| (uses(*color), *color))
        .unwrap()
}

/// The list is never empty, ids are unique, the selected workspace is in
/// it, and it is the model row for row.
fn check(workspaces: &WorkspaceList<u32>, model: &Model) -> Result<(), TestCaseError> {
    let ids = workspaces.ids();
    prop_assert!(workspaces.count() >= 1);
    prop_assert_eq!(ids.len(), workspaces.count());
    prop_assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    prop_assert!(ids.contains(&workspaces.selected()));
    prop_assert_eq!(workspaces.selected(), model.selected);
    prop_assert_eq!(workspaces.selected_workspace().id(), workspaces.selected());
    prop_assert_eq!(rows(workspaces), model.rows.clone());
    for (position, id) in ids.iter().enumerate() {
        prop_assert_eq!(workspaces.position(*id), Some(position));
        prop_assert_eq!(workspaces.get(*id).map(|w| w.id()), Some(*id));
    }
    for row in &model.rows {
        prop_assert!(row.color < WorkspaceColor::COUNT);
        prop_assert!(valid(&row.name) == Some(row.name.as_str()));
    }
    for id in &model.closed {
        prop_assert!(!ids.contains(id));
    }
    Ok(())
}

fn apply(
    workspaces: &mut WorkspaceList<u32>,
    model: &mut Model,
    op: Op,
) -> Result<(), TestCaseError> {
    let count = model.rows.len();
    let at = workspaces.position(model.selected).unwrap();
    match op {
        Op::Add(name) => {
            model.made += 1;
            let content = model.made as u32;
            let id = workspaces.add(name.as_deref(), content);
            prop_assert!(model.rows.iter().all(|row| row.id != id));
            prop_assert!(!model.closed.contains(&id));
            let name = name
                .as_deref()
                .and_then(valid)
                .map_or_else(|| format!("Workspace {}", model.made), str::to_owned);
            let color = expected_color(&model.rows);
            model.rows.push(Row {
                id,
                name,
                color,
                content,
            });
        }
        Op::Close(index) => {
            let id = model.rows[index.index(count)].id;
            close(workspaces, model, id, at)?;
        }
        Op::CloseSelected => close(workspaces, model, model.selected, at)?,
        Op::Select(index) => {
            let id = model.rows[index.index(count)].id;
            prop_assert_eq!(workspaces.select(id), Ok(()));
            model.selected = id;
        }
        Op::Next => {
            workspaces.select_next();
            model.selected = model.rows[(at + 1) % count].id;
            prop_assert_eq!(workspaces.selected(), model.selected);
            // Previous undoes next.
            workspaces.select_previous();
            prop_assert_eq!(workspaces.selected(), model.rows[at].id);
            workspaces.select_next();
        }
        Op::Previous => {
            workspaces.select_previous();
            model.selected = model.rows[(at + count - 1) % count].id;
        }
        Op::Rename(index, name) => {
            let position = index.index(count);
            let id = model.rows[position].id;
            match valid(&name) {
                Some(name) => {
                    prop_assert_eq!(workspaces.rename(id, name), Ok(()));
                    model.rows[position].name = name.to_owned();
                }
                None => {
                    prop_assert_eq!(
                        workspaces.rename(id, &name),
                        Err(WorkspaceError::InvalidName)
                    );
                }
            }
        }
        Op::Stale(index, name) => {
            if model.closed.is_empty() {
                return Ok(());
            }
            let id = model.closed[index.index(model.closed.len())];
            let unknown = WorkspaceError::UnknownWorkspace(id);
            prop_assert_eq!(workspaces.select(id), Err(unknown));
            prop_assert_eq!(workspaces.rename(id, &name), Err(unknown));
            prop_assert_eq!(workspaces.close(id), Err(unknown));
        }
    }
    Ok(())
}

fn close(
    workspaces: &mut WorkspaceList<u32>,
    model: &mut Model,
    id: WorkspaceId,
    at: usize,
) -> Result<(), TestCaseError> {
    if model.rows.len() == 1 {
        prop_assert_eq!(workspaces.close(id), Err(WorkspaceError::LastWorkspace));
        return Ok(());
    }
    let position = model.rows.iter().position(|row| row.id == id).unwrap();
    let row = model.rows.remove(position);
    prop_assert_eq!(workspaces.close(id), Ok(row.content));
    model.closed.push(id);
    if id == model.selected {
        // The one that was below, or the one above the lowest.
        model.selected = model.rows[at.min(model.rows.len() - 1)].id;
    }
    Ok(())
}

fn start() -> (WorkspaceList<u32>, Model) {
    let workspaces = WorkspaceList::new(None, 0);
    let model = Model {
        rows: rows(&workspaces),
        selected: workspaces.selected(),
        made: 1,
        closed: Vec::new(),
    };
    (workspaces, model)
}

proptest! {
    /// After every operation the list is the model. The model keeps the
    /// id, name, color and content of every row an operation does not
    /// name, so this also says that nothing else changes.
    #[test]
    fn every_sequence_matches_the_model(ops in vec(op(), 0..80)) {
        let (mut workspaces, mut model) = start();
        prop_assert_eq!(&model.rows[0].name, "Workspace 1");
        prop_assert_eq!(model.rows[0].color, 0);
        check(&workspaces, &model)?;
        for op in ops {
            apply(&mut workspaces, &mut model, op)?;
            check(&workspaces, &model)?;
        }
    }

    /// As long as there were never more workspaces than colors, no two of
    /// them look the same.
    #[test]
    fn colors_differ_until_there_are_more_workspaces_than_colors(
        ops in vec(op(), 0..80),
    ) {
        let (mut workspaces, mut model) = start();
        for op in ops {
            apply(&mut workspaces, &mut model, op)?;
            if workspaces.count() > WorkspaceColor::COUNT {
                break;
            }
            let colors: HashSet<usize> =
                workspaces.iter().map(|w| w.color().index()).collect();
            prop_assert_eq!(colors.len(), workspaces.count());
        }
    }

    /// However many there are, a new workspace takes one of the colors
    /// that the fewest of the others have.
    #[test]
    fn a_new_workspace_never_takes_a_color_used_more_than_another(
        ops in vec(op(), 0..80),
    ) {
        let (mut workspaces, mut model) = start();
        for op in ops {
            apply(&mut workspaces, &mut model, op)?;
        }
        let before = rows(&workspaces);
        let uses = |color: usize| before.iter().filter(|row| row.color == color).count();
        let id = workspaces.add(None, 0);
        let taken = workspaces.get(id).unwrap().color().index();
        for color in 0..WorkspaceColor::COUNT {
            prop_assert!(uses(taken) <= uses(color));
        }
    }

    /// A workspace keeps its color from when it is made until it closes.
    #[test]
    fn a_color_never_changes(ops in vec(op(), 0..80)) {
        let (mut workspaces, mut model) = start();
        let mut first_seen = std::collections::HashMap::new();
        for op in ops {
            apply(&mut workspaces, &mut model, op)?;
            for w in workspaces.iter() {
                let color = *first_seen.entry(w.id()).or_insert(w.color());
                prop_assert_eq!(w.color(), color);
            }
        }
    }

    #[test]
    fn next_as_many_times_as_there_are_workspaces_comes_back(
        ops in vec(op(), 0..40),
    ) {
        let (mut workspaces, mut model) = start();
        for op in ops {
            apply(&mut workspaces, &mut model, op)?;
        }
        let start = workspaces.selected();
        for _ in 0..workspaces.count() {
            workspaces.select_next();
        }
        prop_assert_eq!(workspaces.selected(), start);
    }
}
