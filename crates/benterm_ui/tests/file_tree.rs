//! The tree of a folder: what its rows are, and that a folder is read
//! when it opens and not before.

use std::fs;
use std::path::{Path, PathBuf};

use benterm_ui::file_tree::{EntryKind, FileTree, read_folder};

/// A folder with `b.txt`, `a.txt`, `src/` with `src/main.rs` in it, and
/// `.git/` with `.git/HEAD` in it.
fn sample() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    fs::write(root.join("b.txt"), "b").unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
    (dir, root)
}

fn names(tree: &FileTree) -> Vec<(usize, String)> {
    tree.rows()
        .into_iter()
        .map(|row| (row.depth, row.name))
        .collect()
}

fn shown(rows: &[(usize, &str)]) -> Vec<(usize, String)> {
    rows.iter()
        .map(|(depth, name)| (*depth, (*name).to_owned()))
        .collect()
}

#[test]
fn the_root_lists_folders_first_then_files_by_name() {
    let (_dir, root) = sample();
    let tree = FileTree::new(&root);
    assert_eq!(
        names(&tree),
        shown(&[(0, ".git"), (0, "src"), (0, "a.txt"), (0, "b.txt")])
    );
    let kinds: Vec<EntryKind> = tree.rows().iter().map(|row| row.kind).collect();
    assert_eq!(
        kinds,
        [
            EntryKind::Folder,
            EntryKind::Folder,
            EntryKind::File,
            EntryKind::File
        ]
    );
    assert!(tree.rows().iter().all(|row| !row.open));
    assert_eq!(tree.error(), None);
}

#[test]
fn a_folder_is_not_read_before_it_opens() {
    let (_dir, root) = sample();
    let tree = FileTree::new(&root);
    assert!(tree.is_read(&root));
    assert!(!tree.is_read(&root.join("src")));
    assert!(!tree.is_read(&root.join(".git")));
}

#[test]
fn an_open_folder_shows_its_entries_below_it() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    tree.open(&root.join("src"));
    assert!(tree.is_read(&root.join("src")));
    assert_eq!(
        names(&tree),
        shown(&[
            (0, ".git"),
            (0, "src"),
            (1, "main.rs"),
            (0, "a.txt"),
            (0, "b.txt")
        ])
    );
    let src = &tree.rows()[1];
    assert!(src.open);
    assert_eq!(tree.rows()[2].path, root.join("src/main.rs"));
}

#[test]
fn a_folder_is_read_when_it_opens_not_when_the_tree_is_made() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    // Made after the tree, before the folder opens.
    fs::write(root.join("src/lib.rs"), "").unwrap();
    tree.open(&root.join("src"));
    assert_eq!(
        names(&tree)[1..4],
        shown(&[(0, "src"), (1, "lib.rs"), (1, "main.rs")])
    );
}

#[test]
fn a_closed_folder_hides_its_entries_and_is_read_again_when_it_opens() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    let src = root.join("src");
    tree.open(&src);
    tree.close(&src);
    assert_eq!(names(&tree).len(), 4);
    fs::write(src.join("lib.rs"), "").unwrap();
    tree.open(&src);
    assert_eq!(names(&tree).len(), 6);
}

#[test]
fn folders_open_inside_open_folders() {
    let (_dir, root) = sample();
    fs::create_dir(root.join("src/deep")).unwrap();
    fs::write(root.join("src/deep/x"), "").unwrap();
    let mut tree = FileTree::new(&root);
    tree.open(&root.join("src"));
    tree.open(&root.join("src/deep"));
    assert_eq!(
        names(&tree)[1..5],
        shown(&[(0, "src"), (1, "deep"), (2, "x"), (1, "main.rs")])
    );
}

#[test]
fn a_root_that_cannot_be_read_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    let tree = FileTree::new(&gone);
    assert!(tree.rows().is_empty());
    assert!(tree.error().is_some());
}

#[test]
fn a_folder_that_cannot_be_read_says_why_when_it_opens() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    fs::remove_dir_all(root.join("src")).unwrap();
    tree.open(&root.join("src"));
    let src = &tree.rows()[1];
    assert!(src.open);
    assert!(src.error.is_some());
}

#[test]
fn a_symbolic_link_to_a_folder_is_a_folder() {
    let (_dir, root) = sample();
    std::os::unix::fs::symlink(root.join("src"), root.join("link")).unwrap();
    let tree = FileTree::new(&root);
    let link = tree
        .rows()
        .into_iter()
        .find(|row| row.name == "link")
        .unwrap();
    assert_eq!(link.kind, EntryKind::Folder);
}

fn selected(tree: &FileTree) -> Option<PathBuf> {
    tree.selected().map(Path::to_owned)
}

#[test]
fn the_arrows_move_the_selection_through_the_rows() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    assert_eq!(selected(&tree), None);
    tree.select_next();
    assert_eq!(selected(&tree), Some(root.join(".git")));
    tree.select_next();
    tree.select_next();
    tree.select_next();
    assert_eq!(selected(&tree), Some(root.join("b.txt")));
    // The last row stays selected.
    tree.select_next();
    assert_eq!(selected(&tree), Some(root.join("b.txt")));
    tree.select_previous();
    assert_eq!(selected(&tree), Some(root.join("a.txt")));
}

#[test]
fn right_opens_the_selected_folder_then_moves_into_it() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    tree.select(&root.join("src"));
    tree.open_selected();
    assert!(tree.rows()[1].open);
    assert_eq!(selected(&tree), Some(root.join("src")));
    tree.open_selected();
    assert_eq!(selected(&tree), Some(root.join("src/main.rs")));
}

#[test]
fn left_moves_to_the_parent_then_closes_it() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    tree.open(&root.join("src"));
    tree.select(&root.join("src/main.rs"));
    tree.close_selected();
    assert_eq!(selected(&tree), Some(root.join("src")));
    tree.close_selected();
    assert!(!tree.rows()[1].open);
    assert_eq!(selected(&tree), Some(root.join("src")));
}

#[test]
fn closing_a_folder_above_the_selection_selects_the_folder() {
    let (_dir, root) = sample();
    let mut tree = FileTree::new(&root);
    tree.open(&root.join("src"));
    tree.select(&root.join("src/main.rs"));
    tree.close(&root.join("src"));
    assert_eq!(selected(&tree), Some(root.join("src")));
}

#[test]
fn applying_refreshed_entries_keeps_open_subfolders() {
    let (_dir, root) = sample();
    let mut tree = FileTree::from_entries(&root, read_folder(&root));
    let src = root.join("src");
    assert!(tree.apply_folder(&src, read_folder(&src)));
    assert!(tree.is_open(&src));

    fs::write(root.join("c.txt"), "c").unwrap();
    assert!(tree.apply_folder(&root, read_folder(&root)));

    assert!(tree.is_open(&src));
    assert_eq!(
        names(&tree),
        shown(&[
            (0, ".git"),
            (0, "src"),
            (1, "main.rs"),
            (0, "a.txt"),
            (0, "b.txt"),
            (0, "c.txt"),
        ])
    );
}

#[test]
fn open_folders_tracks_root_and_open_subfolders_only() {
    let (_dir, root) = sample();
    fs::create_dir(root.join("src/deep")).unwrap();
    let mut tree = FileTree::new(&root);
    let src = root.join("src");
    let deep = root.join("src/deep");
    assert_eq!(tree.open_folders(), vec![root.clone()]);

    tree.open(&src);
    tree.open(&deep);
    assert_eq!(
        tree.open_folders(),
        vec![root.clone(), src.clone(), deep.clone()]
    );

    // Closing a parent removes both it and its open descendants.
    tree.close(&src);
    assert_eq!(tree.open_folders(), vec![root.clone()]);
}
