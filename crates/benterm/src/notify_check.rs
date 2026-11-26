//! `benterm --notify-check` posts one macOS notification, to check that
//! Benterm.app can ask for permission and send notifications. macOS sends
//! notifications only for an app bundle, so a run outside Benterm.app, such as
//! `cargo run`, posts nothing.

use gpui::{App, SystemNotification};

/// The argument that asks for the check.
pub const FLAG: &str = "--notify-check";

/// Whether the arguments, without the program name, ask for the check.
pub fn requested(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().any(|arg| arg == FLAG)
}

/// The notification the check posts.
pub fn notification() -> SystemNotification {
    SystemNotification {
        tag: "benterm-notify-check".into(),
        title: "Benterm can send notifications".into(),
        body: "This is the notification check started with --notify-check.".into(),
        actions: Vec::new(),
    }
}

/// Posts the check's notification when the arguments ask for it.
/// GPUI asks for permission before the first notification and does nothing
/// outside an app bundle.
pub fn post_if_requested(args: impl IntoIterator<Item = String>, cx: &App) {
    if requested(args) {
        cx.show_system_notification(notification());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn only_the_flag_asks_for_the_check() {
        assert!(requested(args(&[FLAG])));
        assert!(!requested(args(&[])));
        assert!(!requested(args(&["--notify"])));
    }

    #[test]
    fn the_notification_says_what_it_checks() {
        let notification = notification();
        assert!(!notification.tag.is_empty());
        assert!(notification.title.contains("Benterm"));
        assert!(!notification.body.is_empty());
        assert!(notification.actions.is_empty());
    }

    #[gpui::test]
    fn the_flag_posts_the_notification(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_app_identity("benterm.test", "Benterm"));
        cx.update(|cx| post_if_requested(args(&[FLAG]), cx));
        assert_eq!(cx.shown_system_notifications(), vec![notification()]);
    }

    #[gpui::test]
    fn without_the_flag_nothing_is_posted(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_app_identity("benterm.test", "Benterm"));
        cx.update(|cx| post_if_requested(args(&[]), cx));
        assert!(cx.shown_system_notifications().is_empty());
    }
}
