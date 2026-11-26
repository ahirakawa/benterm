//! The rules of the in-app notifications, without a window: how long one
//! is shown, and which are shown when many come at once.

use std::time::{Duration, Instant};

use benterm_layout::{PaneId, PaneTree, Size};
use benterm_ui::toast::{
    COMMAND_NOT_FOUND_EXIT_CODE, FAILURE_OUTPUT_LINES, MAX_SHOWN, SHOWN_FOR,
    STARTUP_FAILURE_WINDOW, StartupFailure, Toast, ToastKind, Toasts, is_startup_failure,
    is_startup_failure_within, last_non_empty_lines,
};

/// `count` ids no other pane has.
fn panes(count: usize) -> Vec<PaneId> {
    (0..count)
        .map(|_| PaneTree::new(Size::new(1, 1)).focused())
        .collect()
}

fn toast(pane: PaneId, title: &str) -> Toast {
    Toast {
        pane,
        kind: ToastKind::Done,
        source: "Claude Code · project".into(),
        title: title.into(),
        body: None,
    }
}

fn shown_titles(toasts: &Toasts, now: Instant) -> Vec<String> {
    toasts
        .shown(now)
        .into_iter()
        .map(|toast| toast.title.clone())
        .collect()
}

#[test]
fn a_notification_is_shown_until_its_time_is_up() {
    let start = Instant::now();
    let pane = panes(1)[0];
    let mut toasts = Toasts::new(3, Duration::from_secs(5));
    toasts.push(toast(pane, "Finished"), start);

    assert_eq!(toasts.shown(start).len(), 1);
    let just_before = start + Duration::from_secs(5) - Duration::from_millis(1);
    assert_eq!(toasts.shown(just_before).len(), 1);
    assert_eq!(toasts.shown(start + Duration::from_secs(5)).len(), 0);
}

#[test]
fn past_the_limit_the_newest_are_shown() {
    let start = Instant::now();
    let mut toasts = Toasts::new(3, Duration::from_secs(5));
    for (index, pane) in panes(5).into_iter().enumerate() {
        let at = start + Duration::from_millis(index as u64);
        toasts.push(toast(pane, &format!("{index}")), at);
    }
    // From the oldest to the newest, which is nearest the corner.
    assert_eq!(shown_titles(&toasts, start), ["2", "3", "4"]);
}

#[test]
fn a_newer_notification_of_a_pane_takes_the_place_of_the_older() {
    let start = Instant::now();
    let [first, second] = panes(2).try_into().unwrap();
    let mut toasts = Toasts::new(3, Duration::from_secs(5));
    toasts.push(toast(first, "old"), start);
    toasts.push(toast(second, "other"), start);
    let later = start + Duration::from_secs(4);
    toasts.push(toast(first, "new"), later);

    assert_eq!(shown_titles(&toasts, later), ["other", "new"]);
    // The newer one has its own time.
    let after_the_old_time = start + Duration::from_secs(6);
    assert_eq!(shown_titles(&toasts, after_the_old_time), ["new"]);
}

#[test]
fn a_dismissed_notification_is_not_shown() {
    let start = Instant::now();
    let [first, second] = panes(2).try_into().unwrap();
    let mut toasts = Toasts::new(3, Duration::from_secs(5));
    toasts.push(toast(first, "first"), start);
    toasts.push(toast(second, "second"), start);
    assert!(toasts.dismiss(first));
    assert!(!toasts.dismiss(first));
    assert_eq!(shown_titles(&toasts, start), ["second"]);
}

#[test]
fn expiring_drops_what_is_past_its_time_and_says_when_the_next_one_goes() {
    let start = Instant::now();
    let [first, second] = panes(2).try_into().unwrap();
    let mut toasts = Toasts::new(3, Duration::from_secs(5));
    assert_eq!(toasts.next_expiry(), None);
    toasts.push(toast(first, "first"), start);
    toasts.push(toast(second, "second"), start + Duration::from_secs(2));
    assert_eq!(toasts.next_expiry(), Some(start + Duration::from_secs(5)));

    assert!(!toasts.expire(start + Duration::from_secs(4)));
    assert!(toasts.expire(start + Duration::from_secs(5)));
    assert_eq!(toasts.next_expiry(), Some(start + Duration::from_secs(7)));
    assert!(toasts.expire(start + Duration::from_secs(7)));
    assert_eq!(toasts.next_expiry(), None);
}

#[test]
fn the_app_shows_a_few_for_some_seconds() {
    let toasts = Toasts::default();
    let start = Instant::now();
    assert_eq!(toasts.limit(), MAX_SHOWN);
    assert_eq!(toasts.duration(), SHOWN_FOR);
    assert!((2..=5).contains(&MAX_SHOWN));
    assert!(SHOWN_FOR >= Duration::from_secs(3) && SHOWN_FOR <= Duration::from_secs(10));
    assert!(toasts.shown(start).is_empty());
}

#[test]
fn startup_failure_reports_nonzero_exit_within_window() {
    assert_eq!(STARTUP_FAILURE_WINDOW, Duration::from_secs(2));
    assert_eq!(COMMAND_NOT_FOUND_EXIT_CODE, 127);
    assert_eq!(FAILURE_OUTPUT_LINES, 5);

    assert!(is_startup_failure(Some(3), Duration::from_millis(500)));
    assert!(is_startup_failure(Some(1), STARTUP_FAILURE_WINDOW));
    assert!(is_startup_failure_within(
        Some(2),
        Duration::from_millis(100),
        Duration::from_millis(100),
    ));

    let failure = StartupFailure::from_exit(Some(3), Duration::from_millis(500), "boom\n")
        .expect("nonzero exit within window");
    assert_eq!(failure.exit_code, 3);
    assert_eq!(failure.output, vec!["boom"]);
}

#[test]
fn startup_failure_ignores_zero_exit_and_signals() {
    assert!(!is_startup_failure(Some(0), Duration::ZERO));
    assert!(!is_startup_failure(None, Duration::ZERO));
    assert_eq!(
        StartupFailure::from_exit(Some(0), Duration::from_millis(100), "ok\n"),
        None
    );
    assert_eq!(
        StartupFailure::from_exit(None, Duration::from_millis(100), "killed\n"),
        None
    );
}

#[test]
fn startup_failure_ignores_late_exit_except_127() {
    let late = STARTUP_FAILURE_WINDOW + Duration::from_millis(1);
    assert!(!is_startup_failure(Some(1), late));
    assert_eq!(
        StartupFailure::from_exit(Some(1), late, "late error\n"),
        None
    );

    // 127 (command not found) is always reported, even after the window.
    assert!(is_startup_failure(Some(127), Duration::from_secs(60)));
    let failure = StartupFailure::from_exit(
        Some(127),
        Duration::from_secs(60),
        "sh: missing: command not found\n",
    )
    .expect("127 is always reported");
    assert_eq!(failure.exit_code, 127);
    assert_eq!(failure.output, vec!["sh: missing: command not found"]);
}

#[test]
fn startup_failure_keeps_at_most_five_non_empty_lines() {
    let text = "\
line 1
line 2

line 3   
   
line 4
line 5
line 6
line 7

";
    assert_eq!(
        last_non_empty_lines(text, 5),
        vec!["line 3", "line 4", "line 5", "line 6", "line 7"]
    );
    assert_eq!(last_non_empty_lines("   \n\n\t\n", 5), Vec::<String>::new());

    let failure = StartupFailure::from_exit(Some(2), Duration::from_millis(50), text)
        .expect("nonzero exit within window");
    assert_eq!(
        failure.output,
        vec!["line 3", "line 4", "line 5", "line 6", "line 7"]
    );
}

#[test]
fn error_toast_kind_is_distinct() {
    assert_ne!(ToastKind::Error, ToastKind::Done);
    assert_ne!(ToastKind::Error, ToastKind::Waiting);
    assert_ne!(ToastKind::Error, ToastKind::Osc);
}
