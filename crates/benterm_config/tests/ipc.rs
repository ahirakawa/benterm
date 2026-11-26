//! The `[ipc]` section: whether the socket API asks every request for a
//! credential.

use benterm_config::{Config, IpcSettings, parse};

#[test]
fn requests_need_a_credential_by_default() {
    assert!(Config::default().ipc.restricted);
    assert_eq!(parse("").unwrap().config.ipc, IpcSettings::default());
}

#[test]
fn the_section_turns_the_check_off() {
    let loaded = parse("[ipc]\nrestricted = false\n").unwrap();
    assert!(!loaded.config.ipc.restricted);
    assert_eq!(loaded.warnings, vec![]);
}

#[test]
fn an_unknown_key_is_warned_about() {
    let loaded = parse("[ipc]\nopen = true\n").unwrap();
    assert!(loaded.config.ipc.restricted);
    assert_eq!(loaded.warnings.len(), 1);
}
