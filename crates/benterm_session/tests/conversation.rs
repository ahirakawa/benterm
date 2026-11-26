//! The conversation of the agent in a pane, which the session file keeps
//! so that the agent can take it up again after a restart.

use benterm_session::{Agent, Conversation, Node, Session, Tab, Terminal, VERSION, Workspace};

fn one_tab(terminal: Terminal) -> Session {
    Session {
        selected_workspace: 0,
        workspaces_made: 1,
        folded_repositories: Vec::new(),
        workspaces: vec![Workspace {
            task: None,
            name: "Workspace 1".into(),
            color: Some(0),
            selected_tab: 0,
            tabs: vec![Tab {
                name: None,
                focused_pane: 0,
                root: Node::Terminal(terminal),
            }],
        }],
    }
}

const KEY: &str = "0123456789abcdef0123456789abcdef";
const ID: &str = "29c3bdbf-8ca0-48bf-8650-74d66bbc19d0";

fn in_conversation(agent: Agent, id: &str) -> Session {
    one_tab(Terminal {
        cwd: Some("/srv/api".into()),
        scrollback: Some(KEY.into()),
        conversation: Some(Conversation::new(agent, id).unwrap()),
        ..Terminal::default()
    })
}

/// The file the third version of the format writes for
/// [`in_conversation`] with Claude Code, kept as text so that a later
/// change to the types cannot change what it means unnoticed.
const THIRD_VERSION: &str = r#"{
  "version": 3,
  "selected_workspace": 0,
  "workspaces_made": 1,
  "workspaces": [
    {
      "name": "Workspace 1",
      "color": 0,
      "selected_tab": 0,
      "tabs": [
        {
          "focused_pane": 0,
          "root": {
            "kind": "terminal",
            "cwd": "/srv/api",
            "scrollback": "0123456789abcdef0123456789abcdef",
            "conversation": {
              "agent": "claude",
              "id": "29c3bdbf-8ca0-48bf-8650-74d66bbc19d0"
            }
          }
        }
      ]
    }
  ]
}
"#;

/// What the second version wrote for the same pane: no conversation.
fn second_version() -> String {
    let text = THIRD_VERSION.replacen("\"version\": 3", "\"version\": 2", 1);
    let conversation = format!(
        ",\n            \"conversation\": {{\n              \"agent\": \"claude\",\n              \"id\": \"{ID}\"\n            }}"
    );
    text.replacen(&conversation, "", 1)
}

fn without_conversation() -> Session {
    one_tab(Terminal {
        cwd: Some("/srv/api".into()),
        scrollback: Some(KEY.into()),
        conversation: None,
        ..Terminal::default()
    })
}

#[test]
fn the_newest_version_is_the_fifth() {
    assert_eq!(VERSION, 5);
}

#[test]
fn the_conversation_of_a_pane_is_written_in_its_leaf() {
    let session = in_conversation(Agent::Claude, ID);
    assert_eq!(session.to_json().unwrap(), THIRD_VERSION);
    assert_eq!(
        Session::from_json(THIRD_VERSION.as_bytes()).unwrap(),
        session
    );
}

#[test]
fn a_codex_conversation_is_read_back_as_one() {
    let session = in_conversation(Agent::Codex, "0199a213-81c0-7800-8aa1-bbab2a035a53");
    let text = session.to_json().unwrap();
    assert!(text.contains(r#""agent": "codex""#), "{text}");
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), session);
}

#[test]
fn a_file_of_the_second_version_is_read_with_no_conversation_and_written_as_the_third() {
    let second = second_version();
    assert!(!second.contains("conversation"), "{second}");
    let read = Session::from_json(second.as_bytes()).unwrap();
    assert_eq!(read, without_conversation());
    let written = read.to_json().unwrap();
    assert_eq!(
        written,
        second.replacen("\"version\": 2", "\"version\": 3", 1)
    );
}

#[test]
fn a_file_of_the_first_version_is_read_with_no_conversation() {
    let first = r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"terminal","cwd":"/tmp"}}]}]}"#;
    let read = Session::from_json(first.as_bytes()).unwrap();
    let leaf = match &read.workspaces[0].tabs[0].root {
        Node::Terminal(terminal) => terminal.clone(),
        _ => panic!("one terminal"),
    };
    assert_eq!(leaf.conversation, None);
    assert_eq!(leaf.cwd.as_deref(), Some("/tmp"));
    assert!(read.to_json().unwrap().starts_with("{\n  \"version\": 3,"));
}

/// Ids that the shell would take for more than one word, or for an
/// option, or that are too long to be an id at all.
const NOT_IDS: [&str; 9] = [
    "",
    "-p",
    "--fork-session",
    "abc def",
    "abc;rm -rf /",
    "$(touch x)",
    "abc\n",
    "'abc'",
    "あ",
];

#[test]
fn an_id_the_shell_could_read_as_something_else_is_no_conversation() {
    let long = "a".repeat(129);
    for id in NOT_IDS.into_iter().chain([long.as_str()]) {
        assert_eq!(Conversation::new(Agent::Claude, id), None, "{id:?}");
        let quoted = serde_json::to_string(id).unwrap();
        let text = THIRD_VERSION.replace(&format!("\"{ID}\""), &quoted);
        let read = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(read, without_conversation(), "{id:?}");
    }
    assert!(Conversation::new(Agent::Claude, &"a".repeat(128)).is_some());
}

#[test]
fn a_conversation_of_an_agent_this_build_does_not_know_is_dropped() {
    for agent in [r#""gemini""#, "7", "null", r#""Claude""#] {
        let text = THIRD_VERSION.replace(r#""claude""#, agent);
        let read = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(read, without_conversation(), "{agent}");
    }
    let text = THIRD_VERSION.replace(&format!(r#""id": "{ID}""#), r#""id": 7"#);
    assert_eq!(
        Session::from_json(text.as_bytes()).unwrap(),
        without_conversation()
    );
}

#[test]
fn an_agent_is_named_by_the_id_of_its_definition() {
    assert_eq!(Agent::from_id("claude"), Some(Agent::Claude));
    assert_eq!(Agent::from_id("codex"), Some(Agent::Codex));
    assert_eq!(Agent::from_id("gemini"), None);
}

#[test]
fn the_command_that_takes_a_conversation_up_again() {
    let claude = Conversation::new(Agent::Claude, "abc").unwrap();
    assert_eq!(claude.resume_command(), "claude --resume abc");
    let id = "0199a213-81c0-7800-8aa1-bbab2a035a53";
    let codex = Conversation::new(Agent::Codex, id).unwrap();
    assert_eq!(codex.resume_command(), format!("codex resume {id}"));
}
