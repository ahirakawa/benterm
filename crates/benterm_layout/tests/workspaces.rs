//! The order, selection, names and colors of workspaces, by example.

use benterm_layout::{WorkspaceColor, WorkspaceError, WorkspaceId, WorkspaceList};

/// A list of `n` workspaces holding 0, 1, ... with the first one selected.
fn list(n: u32) -> WorkspaceList<u32> {
    let mut workspaces = WorkspaceList::new(None, 0);
    for i in 1..n {
        workspaces.add(None, i);
    }
    workspaces
}

fn contents(workspaces: &WorkspaceList<u32>) -> Vec<u32> {
    workspaces.iter().map(|w| *w.content()).collect()
}

fn names(workspaces: &WorkspaceList<u32>) -> Vec<String> {
    workspaces.iter().map(|w| w.name().to_owned()).collect()
}

fn colors(workspaces: &WorkspaceList<u32>) -> Vec<usize> {
    workspaces.iter().map(|w| w.color().index()).collect()
}

fn selected(workspaces: &WorkspaceList<u32>) -> u32 {
    *workspaces.selected_workspace().content()
}

fn id_at(workspaces: &WorkspaceList<u32>, position: usize) -> WorkspaceId {
    workspaces.ids()[position]
}

#[test]
fn a_new_list_has_one_selected_workspace() {
    let workspaces = WorkspaceList::new(None, "first");
    assert_eq!(workspaces.count(), 1);
    assert_eq!(workspaces.ids(), vec![workspaces.selected()]);
    assert_eq!(*workspaces.selected_workspace().content(), "first");
    assert_eq!(workspaces.position(workspaces.selected()), Some(0));
    let first = workspaces.get(workspaces.selected()).unwrap();
    assert_eq!(first.id(), workspaces.selected());
    assert_eq!(first.name(), "Workspace 1");
    assert_eq!(first.color().index(), 0);
}

#[test]
fn a_new_workspace_goes_to_the_end_and_leaves_the_selection() {
    let mut workspaces = list(2);
    let id = workspaces.add(None, 9);
    assert_eq!(contents(&workspaces), vec![0, 1, 9]);
    assert_eq!(workspaces.position(id), Some(2));
    assert_eq!(selected(&workspaces), 0);
}

#[test]
fn every_workspace_gets_a_new_id() {
    let mut a = WorkspaceList::new(None, 0);
    let b = WorkspaceList::new(None, 0);
    let first = a.selected();
    let second = a.add(None, 1);
    a.close(second).unwrap();
    let third = a.add(None, 2);
    let mut ids = vec![first, second, third, b.selected()];
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    assert!(ids.iter().all(|id| id.get() > 0));
}

#[test]
fn workspaces_without_a_name_are_numbered_as_they_are_made() {
    let mut workspaces = list(3);
    assert_eq!(
        names(&workspaces),
        vec!["Workspace 1", "Workspace 2", "Workspace 3"]
    );
    // A number is not given twice, so a name does not come back for
    // something else.
    workspaces.close(id_at(&workspaces, 2)).unwrap();
    workspaces.add(None, 9);
    assert_eq!(
        names(&workspaces),
        vec!["Workspace 1", "Workspace 2", "Workspace 4"]
    );
}

#[test]
fn a_workspace_can_be_made_with_a_name() {
    let mut workspaces = WorkspaceList::new(Some("  api  "), 0);
    workspaces.add(Some("fix-auth-timeout"), 1);
    workspaces.add(Some("日本語"), 2);
    assert_eq!(
        names(&workspaces),
        vec!["api", "fix-auth-timeout", "日本語"]
    );
}

#[test]
fn a_name_that_cannot_be_shown_gives_way_to_the_numbered_one() {
    let mut workspaces = WorkspaceList::new(Some("   "), 0);
    workspaces.add(Some(""), 1);
    workspaces.add(Some("a\nb"), 2);
    assert_eq!(
        names(&workspaces),
        vec!["Workspace 1", "Workspace 2", "Workspace 3"]
    );
}

#[test]
fn renaming_changes_only_the_name() {
    let mut workspaces = list(3);
    let id = id_at(&workspaces, 1);
    let before_colors = colors(&workspaces);
    assert_eq!(workspaces.rename(id, " web "), Ok(()));
    assert_eq!(
        names(&workspaces),
        vec!["Workspace 1", "web", "Workspace 3"]
    );
    assert_eq!(colors(&workspaces), before_colors);
    assert_eq!(workspaces.ids()[1], id);
    assert_eq!(selected(&workspaces), 0);
}

#[test]
fn two_workspaces_may_have_the_same_name() {
    let mut workspaces = list(2);
    assert_eq!(
        workspaces.rename(id_at(&workspaces, 1), "Workspace 1"),
        Ok(())
    );
    assert_eq!(names(&workspaces), vec!["Workspace 1", "Workspace 1"]);
}

#[test]
fn an_empty_name_or_one_with_control_characters_is_refused() {
    let mut workspaces = list(1);
    let id = workspaces.selected();
    for name in [
        "",
        "  \t ",
        "a\nb",
        "a\u{1b}[31m",
        "a\u{7f}",
        "a\u{2028}b",
        "a\u{2029}b",
    ] {
        assert_eq!(
            workspaces.rename(id, name),
            Err(WorkspaceError::InvalidName),
            "{name:?}"
        );
    }
    assert_eq!(names(&workspaces), vec!["Workspace 1"]);
}

#[test]
fn the_first_workspaces_all_get_different_colors() {
    let workspaces = list(WorkspaceColor::COUNT as u32);
    assert_eq!(
        colors(&workspaces),
        (0..WorkspaceColor::COUNT).collect::<Vec<_>>()
    );
}

#[test]
fn a_new_workspace_takes_the_color_fewest_others_have() {
    let mut workspaces = list(4);
    // Color 1 is free again, and a color in use is not doubled before it
    // is taken.
    workspaces.close(id_at(&workspaces, 1)).unwrap();
    let id = workspaces.add(None, 9);
    assert_eq!(workspaces.get(id).unwrap().color().index(), 1);
}

#[test]
fn with_every_color_in_use_the_first_one_is_doubled_first() {
    let mut workspaces = list(WorkspaceColor::COUNT as u32);
    let seventh = workspaces.add(None, 7);
    let eighth = workspaces.add(None, 8);
    assert_eq!(workspaces.get(seventh).unwrap().color().index(), 0);
    assert_eq!(workspaces.get(eighth).unwrap().color().index(), 1);
}

#[test]
fn a_color_stays_when_other_workspaces_close() {
    let mut workspaces = list(4);
    let last = id_at(&workspaces, 3);
    workspaces.close(id_at(&workspaces, 0)).unwrap();
    workspaces.close(id_at(&workspaces, 0)).unwrap();
    assert_eq!(workspaces.get(last).unwrap().color().index(), 3);
    assert_eq!(colors(&workspaces), vec![2, 3]);
}

#[test]
fn every_color_has_an_index_below_the_count() {
    let all = WorkspaceColor::all();
    assert_eq!(all.len(), WorkspaceColor::COUNT);
    for (index, color) in all.into_iter().enumerate() {
        assert_eq!(color.index(), index);
    }
}

#[test]
fn selecting_by_id() {
    let mut workspaces = list(3);
    let id = id_at(&workspaces, 2);
    assert_eq!(workspaces.select(id), Ok(()));
    assert_eq!(workspaces.selected(), id);
    assert_eq!(selected(&workspaces), 2);
}

#[test]
fn next_and_previous_wrap_around() {
    let mut workspaces = list(3);
    workspaces.select_previous();
    assert_eq!(selected(&workspaces), 2);
    workspaces.select_next();
    assert_eq!(selected(&workspaces), 0);
    workspaces.select_next();
    assert_eq!(selected(&workspaces), 1);
}

#[test]
fn closing_the_selected_workspace_selects_the_one_below() {
    let mut workspaces = list(3);
    workspaces.select(id_at(&workspaces, 1)).unwrap();
    assert_eq!(workspaces.close(workspaces.selected()), Ok(1));
    assert_eq!(contents(&workspaces), vec![0, 2]);
    assert_eq!(selected(&workspaces), 2);
}

#[test]
fn closing_the_selected_last_workspace_selects_the_one_above() {
    let mut workspaces = list(3);
    workspaces.select(id_at(&workspaces, 2)).unwrap();
    assert_eq!(workspaces.close(workspaces.selected()), Ok(2));
    assert_eq!(selected(&workspaces), 1);
}

#[test]
fn closing_another_workspace_leaves_the_selection() {
    let mut workspaces = list(3);
    workspaces.select(id_at(&workspaces, 1)).unwrap();
    assert_eq!(workspaces.close(id_at(&workspaces, 0)), Ok(0));
    assert_eq!(selected(&workspaces), 1);
}

#[test]
fn the_last_workspace_cannot_be_closed() {
    let mut workspaces = list(1);
    let id = workspaces.selected();
    assert_eq!(workspaces.close(id), Err(WorkspaceError::LastWorkspace));
    assert_eq!(workspaces.count(), 1);
    assert_eq!(workspaces.selected(), id);
}

#[test]
fn a_closed_workspace_is_unknown() {
    let mut workspaces = list(2);
    let id = id_at(&workspaces, 1);
    workspaces.close(id).unwrap();
    let unknown = Err(WorkspaceError::UnknownWorkspace(id));
    assert_eq!(workspaces.select(id), unknown);
    assert_eq!(workspaces.rename(id, "x"), unknown);
    assert_eq!(
        workspaces.close(id),
        Err(WorkspaceError::UnknownWorkspace(id))
    );
    assert_eq!(workspaces.get(id).map(|w| w.id()), None);
    assert_eq!(workspaces.position(id), None);
    assert_eq!(contents(&workspaces), vec![0]);
}

#[test]
fn errors_say_what_was_refused() {
    let workspaces = list(1);
    let id = workspaces.selected();
    assert_eq!(
        WorkspaceError::UnknownWorkspace(id).to_string(),
        format!("no workspace has the id {id}")
    );
    assert_eq!(
        WorkspaceError::LastWorkspace.to_string(),
        "the last workspace cannot be closed"
    );
    assert_eq!(
        WorkspaceError::InvalidName.to_string(),
        "a workspace name needs a character and no control characters"
    );
}

#[test]
fn a_workspace_knows_whether_it_was_given_its_name() {
    let mut workspaces = WorkspaceList::new(None, 0);
    let first = workspaces.selected();
    let named = workspaces.add(Some("api"), 1);
    let numbered_by_hand = workspaces.add(Some("Workspace 1"), 2);
    assert!(!workspaces.get(first).unwrap().is_named());
    assert!(workspaces.get(named).unwrap().is_named());
    assert!(workspaces.get(numbered_by_hand).unwrap().is_named());

    workspaces.rename(first, "Workspace 7").unwrap();
    assert!(workspaces.get(first).unwrap().is_named());
}

/// A saved session keeps only the names, so the numbered name is known
/// by its form.
#[test]
fn the_numbered_name_is_known_by_its_form() {
    use benterm_layout::is_numbered_name;
    assert!(is_numbered_name("Workspace 1"));
    assert!(is_numbered_name("Workspace 12"));
    for name in [
        "Workspace",
        "Workspace ",
        "Workspace x",
        "Workspace 1a",
        "workspace 1",
        "api",
    ] {
        assert!(!is_numbered_name(name), "{name}");
    }
}

/// A session keeps a workspace nobody named under its numbered name: it
/// comes back with that name, as one nobody named.
#[test]
fn a_saved_numbered_name_comes_back_as_one_nobody_gave() {
    let saved = vec![(Some("Workspace 3"), None, 0), (Some("api"), None, 1)];
    let workspaces = WorkspaceList::from_saved(saved, 0, 3).unwrap();
    let all: Vec<_> = workspaces.iter().collect();
    assert_eq!(all[0].name(), "Workspace 3");
    assert!(!all[0].is_named());
    assert!(all[1].is_named());
}
