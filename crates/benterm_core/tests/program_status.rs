//! What a program tells of itself with OSC 7501, the Program Status
//! Protocol, revision 0.4:
//! <https://superlogical.com/rex/docs/build/program-status>.

use benterm_core::{
    AlacrittyCore, BlockKind, GridSize, ProgramState, ProgramStatus, TerminalCore, TerminalEvent,
};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(4, 20), 100)
}

fn events_after(chunks: &[&[u8]]) -> Vec<TerminalEvent> {
    let mut core = core();
    for chunk in chunks {
        core.feed(chunk);
    }
    core.take_events()
}

/// The events of one report with the body `body`, ended by ST.
fn events_of(body: &str) -> Vec<TerminalEvent> {
    events_after(&[format!("\x1b]7501;{body}\x1b\\").as_bytes()])
}

/// The one status the report with the body `body` tells.
fn status_of(body: &str) -> ProgramStatus {
    match events_of(body).as_slice() {
        [TerminalEvent::ProgramStatus(status)] => status.clone(),
        other => panic!("{body}: {other:?}"),
    }
}

fn status(state: ProgramState) -> ProgramStatus {
    ProgramStatus {
        state,
        id: None,
        kind: None,
        progress: None,
        app: None,
        title: None,
        msg: None,
    }
}

fn told(status: ProgramStatus) -> Vec<TerminalEvent> {
    vec![TerminalEvent::ProgramStatus(status)]
}

#[test]
fn a_report_tells_state_kind_and_app() {
    let blocked = ProgramStatus {
        kind: Some(BlockKind::Permission),
        app: Some("terraform".into()),
        ..status(ProgramState::Blocked)
    };
    assert_eq!(
        events_after(&[b"\x1b]7501;state=blocked:kind=permission:app=terraform\x1b\\"]),
        told(blocked.clone())
    );
    // Ended by BEL, it reads the same.
    assert_eq!(
        events_after(&[b"\x1b]7501;state=blocked:kind=permission:app=terraform\x07"]),
        told(blocked)
    );
}

#[test]
fn every_state_of_the_protocol_is_read() {
    for (name, state) in [
        ("idle", ProgramState::Idle),
        ("working", ProgramState::Working),
        ("done", ProgramState::Done),
        ("blocked", ProgramState::Blocked),
        ("error", ProgramState::Error),
    ] {
        assert_eq!(status_of(&format!("state={name}")), status(state), "{name}");
    }
}

#[test]
fn a_malformed_pair_is_skipped_and_the_rest_is_read() {
    let expected = ProgramStatus {
        progress: Some(50),
        ..status(ProgramState::Working)
    };
    assert_eq!(status_of("state=working:BAD:progress=50"), expected);
    // No key, a key that is not of small letters, a value with a byte
    // outside the set.
    assert_eq!(status_of("state=working:=x:progress=50"), expected);
    assert_eq!(status_of("state=working:Kind=x:progress=50"), expected);
    assert_eq!(status_of("state=working:msg=a b:progress=50"), expected);
    assert_eq!(status_of("state=working:msg=a;b:progress=50"), expected);
}

#[test]
fn whitespace_around_keys_and_values_is_removed() {
    assert_eq!(
        status_of(" state = working : progress = 7 "),
        ProgramStatus {
            progress: Some(7),
            ..status(ProgramState::Working)
        }
    );
}

#[test]
fn of_a_key_told_twice_the_last_value_counts() {
    assert_eq!(
        status_of("state=working:state=done"),
        status(ProgramState::Done)
    );
}

#[test]
fn unknown_keys_are_ignored() {
    assert_eq!(
        status_of("state=idle:colour=red"),
        status(ProgramState::Idle)
    );
}

#[test]
fn a_report_with_no_state_or_an_unknown_one_is_no_event() {
    assert!(events_of("state=sleeping").is_empty());
    assert!(events_of("app=cargo").is_empty());
    assert!(events_of("").is_empty());
    assert!(events_of("state=").is_empty());
}

#[test]
fn the_kind_counts_only_when_blocked_and_an_unknown_one_is_none() {
    for (name, kind) in [
        ("permission", BlockKind::Permission),
        ("question", BlockKind::Question),
        ("auth", BlockKind::Auth),
    ] {
        assert_eq!(
            status_of(&format!("state=blocked:kind={name}")).kind,
            Some(kind)
        );
    }
    assert_eq!(status_of("state=blocked:kind=coffee").kind, None);
    assert_eq!(status_of("state=blocked").kind, None);
    assert_eq!(status_of("state=working:kind=question").kind, None);
}

#[test]
fn the_progress_is_a_whole_number_up_to_a_hundred_while_working_or_blocked() {
    assert_eq!(status_of("state=working:progress=0").progress, Some(0));
    assert_eq!(status_of("state=blocked:progress=100").progress, Some(100));
    for body in [
        "state=working:progress=101",
        "state=working:progress=-1",
        "state=working:progress=+5",
        "state=working:progress=5.0",
        "state=working:progress=",
        "state=done:progress=50",
        "state=idle:progress=50",
    ] {
        assert_eq!(status_of(body).progress, None, "{body}");
    }
}

#[test]
fn an_app_outside_its_characters_is_none() {
    assert_eq!(
        status_of("state=idle:app=claude-code").app.as_deref(),
        Some("claude-code")
    );
    assert_eq!(status_of("state=idle:app=a/b").app, None);
    assert_eq!(status_of("state=idle:app=").app, None);
}

#[test]
fn msg_and_title_are_base64_with_or_without_padding() {
    // "Upgraded 12 packages" and "US East".
    let report = status_of("state=done:msg=VXBncmFkZWQgMTIgcGFja2FnZXM=:title=VVMgRWFzdA==");
    assert_eq!(report.msg.as_deref(), Some("Upgraded 12 packages"));
    assert_eq!(report.title.as_deref(), Some("US East"));
    let bare = status_of("state=done:msg=VXBncmFkZWQgMTIgcGFja2FnZXM:title=VVMgRWFzdA");
    assert_eq!(bare, report);
    // "日本語"
    assert_eq!(
        status_of("state=done:msg=5pel5pys6Kqe").msg.as_deref(),
        Some("日本語")
    );
}

#[test]
fn a_report_whose_text_does_not_decode_or_holds_a_control_character_is_no_event() {
    // No base64.
    assert!(events_of("state=done:msg=a").is_empty());
    assert!(events_of("state=done:title=a").is_empty());
    // No UTF-8: the byte 0xff.
    assert!(events_of("state=done:msg=/w==").is_empty());
    // "a\nb", "a\tb", "a\x7fb" and "a\u{85}b".
    for text in ["YQpi", "YQli", "YX9i", "YcKFYg=="] {
        assert!(events_of(&format!("state=done:msg={text}")).is_empty());
        assert!(events_of(&format!("state=done:title={text}")).is_empty());
    }
}

#[test]
fn a_report_tells_the_record_it_is_for() {
    assert_eq!(status_of("state=working").id, None);
    assert_eq!(
        status_of("state=working:id=build/test").id.as_deref(),
        Some("build/test")
    );
    assert_eq!(
        status_of("state=working:id=us-east_1.a+b").id.as_deref(),
        Some("us-east_1.a+b")
    );
}

#[test]
fn a_report_with_a_malformed_id_is_no_event() {
    let long_segment = "a".repeat(33);
    let deep = ["a"; 9].join("/");
    let long = [&"a".repeat(32)[..]; 4].join("/");
    assert_eq!(long.len(), 131);
    for id in [
        "",
        "/a",
        "a/",
        "a//b",
        "a,b",
        // Bytes no value may hold, which spoil the pair.
        "foo@",
        "a b",
        long_segment.as_str(),
        deep.as_str(),
        long.as_str(),
    ] {
        assert!(
            events_of(&format!("state=working:id={id}")).is_empty(),
            "{id}"
        );
        assert!(
            events_of(&format!("state=clear:id={id}")).is_empty(),
            "{id}"
        );
    }
    let deepest = ["a"; 8].join("/");
    assert_eq!(
        status_of(&format!("state=working:id={deepest}")).id,
        Some(deepest)
    );
}

#[test]
fn a_malformed_id_spoils_the_report_even_before_a_good_one() {
    assert!(events_of("state=done:id=foo@:id=ok").is_empty());
    assert!(events_of("state=clear:id=ok:id=foo@").is_empty());
}

#[test]
fn a_clear_with_text_that_does_not_decode_is_no_event() {
    // "\n" as msg, and no base64 as title.
    assert!(events_of("state=clear:msg=Cg==").is_empty());
    assert!(events_of("state=clear:title=a").is_empty());
}

#[test]
fn a_value_beyond_its_limit_spoils_the_report_even_before_a_good_one() {
    let long_title = "YWFh".repeat(65);
    assert!(events_of(&format!("state=idle:title={long_title}:title=YQ==")).is_empty());
    let long_app = "a".repeat(33);
    assert!(events_of(&format!("state=idle:app={long_app}:app=ok")).is_empty());
    let long_id = "a/".repeat(64) + "a";
    assert!(events_of(&format!("state=idle:id={long_id}:id=ok")).is_empty());
}

/// The parser splits an OSC at semicolons and keeps 16 fields: of more,
/// the last ones are not seen, so their length cannot be counted.
#[test]
fn a_report_of_too_many_fields_is_no_event() {
    assert_eq!(
        status_of(&format!("state=working:x=a{}", ";".repeat(13))),
        status(ProgramState::Working)
    );
    for semicolons in [14, 15, 16, 40] {
        let body = format!(
            "state=working:x=a{}{}",
            ";".repeat(semicolons),
            "b".repeat(5000)
        );
        assert!(events_of(&body).is_empty(), "{semicolons}");
    }
}

#[test]
fn clear_is_no_state_and_tells_which_records_go() {
    assert_eq!(
        events_of("state=clear"),
        [TerminalEvent::ProgramStatusCleared { id: None }]
    );
    assert_eq!(
        events_of("state=clear:id=us-east"),
        [TerminalEvent::ProgramStatusCleared {
            id: Some("us-east".into())
        }]
    );
}

#[test]
fn a_report_beyond_a_limit_is_no_event() {
    // The whole sequence, OSC through ST, may be 4096 bytes: "ESC ] 7501 ;"
    // is 7 of them and "ESC \" is 2.
    let fill = |len: usize| format!("state=idle:x={}", "a".repeat(len - "state=idle:x=".len()));
    assert_eq!(status_of(&fill(4096 - 9)), status(ProgramState::Idle));
    assert!(events_of(&fill(4096 - 8)).is_empty());
    // BEL is one byte.
    let with_bel = |body: String| events_after(&[format!("\x1b]7501;{body}\x07").as_bytes()]);
    assert_eq!(with_bel(fill(4096 - 8)).len(), 1);
    assert!(with_bel(fill(4096 - 7)).is_empty());

    // A key of 17 bytes.
    assert_eq!(
        status_of(&format!("state=idle:{}=1", "k".repeat(16))),
        status(ProgramState::Idle)
    );
    assert!(events_of(&format!("state=idle:{}=1", "k".repeat(17))).is_empty());
    // An app of 33 bytes.
    assert!(
        status_of(&format!("state=idle:app={}", "a".repeat(32)))
            .app
            .is_some()
    );
    assert!(events_of(&format!("state=idle:app={}", "a".repeat(33))).is_empty());
    // A title of 192 bytes is 256 encoded; one of 195 is 260.
    let encoded = |len: usize| "YWFh".repeat(len / 3);
    assert_eq!(
        status_of(&format!("state=idle:title={}", encoded(192))).title,
        Some("a".repeat(192))
    );
    assert!(events_of(&format!("state=idle:title={}", encoded(195))).is_empty());
    // A msg of 2046 bytes is 2728 encoded; one of 2049 is 2732 encoded and
    // too long decoded.
    assert_eq!(
        status_of(&format!("state=idle:msg={}", encoded(2046))).msg,
        Some("a".repeat(2046))
    );
    assert!(events_of(&format!("state=idle:msg={}", encoded(2049))).is_empty());
    assert!(events_of(&format!("state=idle:msg={}", encoded(2052))).is_empty());
}

#[test]
fn a_report_split_across_feeds_is_one_event() {
    let whole = b"\x1b]7501;state=working:progress=50\x1b\\";
    for cut in 1..whole.len() {
        let (head, tail) = whole.split_at(cut);
        assert_eq!(
            events_after(&[head, tail]),
            told(ProgramStatus {
                progress: Some(50),
                ..status(ProgramState::Working)
            }),
            "cut at {cut}"
        );
    }
}

#[test]
fn a_report_that_is_cancelled_or_never_ended_is_no_event() {
    // CAN and SUB cancel a sequence.
    assert!(events_after(&[b"\x1b]7501;state=working\x18"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=working\x1a"]).is_empty());
    // ESC that starts another sequence ends no report.
    assert!(events_after(&[b"\x1b]7501;state=working\x1b[0m"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=working\x1b", b"7"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=working"]).is_empty());
    // Only the byte right after the ESC makes an ST of it.
    assert!(events_after(&[b"\x1b]7501;state=working\x1b\x1b\\"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=working\x1b", b"\x1b\\"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=working\x18\\"]).is_empty());
    assert!(events_after(&[b"\x1b]7501;state=clear\x1b\x1b\\"]).is_empty());
}

#[test]
fn what_follows_a_cancelled_report_is_read() {
    let events = events_after(&[
        b"\x1b]7501;state=working\x1b\x1b]7501;state=done\x1b\\\x1b]7501;state=idle\x18\x1b]7501;state=error\x07",
    ]);
    assert_eq!(
        events,
        [
            TerminalEvent::ProgramStatus(status(ProgramState::Done)),
            TerminalEvent::ProgramStatus(status(ProgramState::Error)),
        ]
    );
}

#[test]
fn reports_come_in_order_among_the_other_events() {
    let events = events_after(&[
        b"\x1b]7501;state=working\x07\x1b]9;hello\x07\x1b]7501;state=done\x1b\\\x1b]133;D;0\x07",
    ]);
    assert_eq!(
        events,
        [
            TerminalEvent::ProgramStatus(status(ProgramState::Working)),
            TerminalEvent::Notification {
                title: None,
                body: "hello".into()
            },
            TerminalEvent::ProgramStatus(status(ProgramState::Done)),
            TerminalEvent::CommandFinished { exit_code: Some(0) },
        ]
    );
}

#[test]
fn a_report_shows_nothing_on_the_screen() {
    let mut with = core();
    with.feed(b"a\x1b]7501;state=working:app=x\x1b\\b\x1b]7501;?\x07c");
    let mut without = core();
    without.feed(b"abc");
    assert_eq!(with.snapshot(), without.snapshot());
}

#[test]
fn the_query_is_answered_with_the_states_and_kinds_that_are_read() {
    const ANSWER: &[u8] =
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";
    for query in [&b"\x1b]7501;?\x1b\\"[..], b"\x1b]7501;?\x07"] {
        let mut core = core();
        core.feed(query);
        assert_eq!(core.take_pty_replies(), ANSWER);
        assert!(core.take_events().is_empty());
    }
}

#[test]
fn the_answer_keeps_its_place_among_the_other_replies() {
    // A program asks and sends primary device attributes right after: if
    // that answer came first, it would take the protocol for missing.
    let mut core = core();
    core.feed(b"\x1b[6n\x1b]7501;?\x07\x1b[c");
    let replies = core.take_pty_replies();
    let text = String::from_utf8_lossy(&replies);
    let cursor = text.find("\x1b[1;1R").expect("the cursor's place");
    let status = text.find("\x1b]7501;?").expect("the answer");
    let attributes = text.find("\x1b[?").expect("the attributes");
    assert!(cursor < status && status < attributes, "{text:?}");
}

#[test]
fn an_answer_of_another_terminal_and_a_cancelled_query_get_no_answer() {
    for bytes in [
        &b"\x1b]7501;?:states=idle\x1b\\"[..],
        b"\x1b]7501;??\x07",
        b"\x1b]7501;?\x18",
        b"\x1b]7501;?\x1b[0m",
        b"\x1b]7501;?\x1b\x1b\\",
    ] {
        let mut core = core();
        core.feed(bytes);
        assert!(core.take_pty_replies().is_empty(), "{bytes:?}");
    }
}

#[test]
fn the_start_of_a_prompt_is_told() {
    assert_eq!(
        events_after(&[b"\x1b]133;A\x07"]),
        [TerminalEvent::PromptStarted]
    );
    // kitty and others add options such as `aid=`.
    assert_eq!(
        events_after(&[b"\x1b]133;A;aid=7\x1b\\"]),
        [TerminalEvent::PromptStarted]
    );
    assert_eq!(
        events_after(&[b"\x1b]133;A\x1b", b"\\"]),
        [TerminalEvent::PromptStarted]
    );
}

#[test]
fn a_cancelled_mark_starts_no_prompt() {
    assert!(events_after(&[b"\x1b]133;A\x18"]).is_empty());
    assert!(events_after(&[b"\x1b]133;A\x1b[0m"]).is_empty());
    assert!(events_after(&[b"\x1b]133;A\x1b\x1b\\"]).is_empty());
}

#[test]
fn a_full_reset_removes_every_record_and_a_soft_one_none() {
    assert_eq!(
        events_after(&[b"\x1bc"]),
        [TerminalEvent::ProgramStatusCleared { id: None }]
    );
    assert!(events_after(&[b"\x1b[!p"]).is_empty());
}
