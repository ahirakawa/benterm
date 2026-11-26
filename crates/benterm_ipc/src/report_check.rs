//! `benterm report-check`: the command of the Stop and PostToolUse
//! hooks that checks the report of an agent's turn.
//!
//! Unlike `hook`, which only tells the app, this command may hold the
//! agent, and it takes the place of `hook` at the Stop: at a Stop it prints `{"decision": "block", "reason": ...}` on
//! standard output when a turn that needs a report has none, or one
//! that does not follow the rules. It still never fails, and whatever
//! goes wrong with the check itself lets the agent stop.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use benterm_config::ReportSettings;
use benterm_report::{HookInput, MAX_ROUNDS_LIMIT, Rules, Verdict, check_stop, hint};
use serde_json::{Value, json};

use crate::cli::CliEnv;
use crate::client::Client;
use crate::protocol::{Method, PaneId, ReportChecked, ReportHook};

/// Whether the reports of the agents of a terminal are checked: `on`
/// to check, which the app sets in the panes it opens while `[report]
/// enabled` is true, and anything else not to. A terminal turns the
/// check off with `off`. The pane decides once, when it opens.
pub const REPORT_CHECK_ENV: &str = "BENTERM_REPORT_CHECK";

/// The folder of the counts, in the state directory.
const STATE_FOLDER: &str = "report-check";

/// How long the agent has to finish writing the hook's input.
const INPUT_WAIT: Duration = Duration::from_secs(2);
/// The most that is read of the input. It holds the last reply of the
/// agent, and after a tool call what the tool was given and answered.
const INPUT_LIMIT: u64 = 16 << 20;
/// How long the app has to take a report. The agent waits meanwhile.
const APP_BUDGET: Duration = Duration::from_millis(1000);

/// How long the command takes at most, whatever the agent, the files
/// and the app do. An agent waits for its hooks, and one whose check
/// does not come to an end stops.
const BUDGET: Duration = Duration::from_secs(20);

/// Runs the command and returns 0.
///
/// Outside a pane it does nothing, not even read its input: the same
/// hook settings serve agents in other terminals. What went wrong is
/// written to stderr, which the agent does not show for a hook that
/// exits with 0.
///
/// The command is the hook of the Stop as well: whenever it lets the
/// agent stop, it tells the app of the Stop as `hook` would, and when
/// it holds the agent, it does not. A Stop told before the check has
/// decided would show a blocked agent as done.
pub(crate) fn run(
    args: &[String],
    env: &CliEnv,
    stdin: impl Read + Send + 'static,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let in_pane =
        env.socket.as_ref().is_some_and(|socket| !socket.is_empty()) && env.pane.is_some();
    if !in_pane {
        return 0;
    }
    // Everything that can wait on somebody else, the agent closing the
    // input, a file or the app, happens on a thread this one gives up
    // on.
    let (done, outcome) = mpsc::channel();
    let (args, env) = (args.to_vec(), env.clone());
    let work = move || {
        let _ = done.send(work(&args, &env, stdin));
    };
    let outcome = match thread::Builder::new()
        .name("benterm-report-check".into())
        .spawn(work)
    {
        Ok(_) => outcome.recv_timeout(BUDGET).unwrap_or_else(|_| Outcome {
            print: None,
            notes: vec!["it did not come to an end in time, so the agent stops".into()],
        }),
        Err(error) => Outcome {
            print: None,
            notes: vec![error.to_string()],
        },
    };
    for note in outcome.notes {
        let _ = writeln!(stderr, "benterm: report-check: {note}");
    }
    if let Some(print) = outcome.print {
        let _ = writeln!(stdout, "{print}");
    }
    0
}

/// What a run came to: what to print on standard output for the agent,
/// and what went wrong.
struct Outcome {
    print: Option<Value>,
    notes: Vec<String>,
}

fn work(args: &[String], env: &CliEnv, stdin: impl Read + Send + 'static) -> Outcome {
    let mut notes = Vec::new();
    let call = match Call::read(args, env, stdin) {
        Ok(call) => call,
        Err(message) => {
            return Outcome {
                print: None,
                notes: vec![message],
            };
        }
    };
    let output = call.check(env).unwrap_or_else(|message| {
        notes.push(message);
        Output::default()
    });
    let blocked = output
        .print
        .as_ref()
        .is_some_and(|print| print.get("decision").is_some());
    if let Some(checked) = &output.report
        && let Err(error) = call.send(checked)
    {
        // The check is done; an app that does not take the report does
        // not hold the agent.
        notes.push(format!("the app did not take the report: {error}"));
    }
    if call.input.event == "Stop" && !blocked {
        let stop = ReportHook {
            pane: call.pane,
            agent: call.agent.clone(),
            event: "Stop".into(),
            tool_name: None,
            notification_type: None,
            session_id: Some(call.input.session_id.clone()).filter(|id| !id.is_empty()),
            agent_id: call.input.agent_id.clone(),
            tool_input: None,
            tool_response: None,
        };
        if let Err(error) = call.send(&stop) {
            notes.push(format!("the app was not told of the Stop: {error}"));
        }
    }
    Outcome {
        print: output.print,
        notes,
    }
}

/// What the check came to.
#[derive(Default)]
struct Output {
    /// What to print on standard output for the agent.
    print: Option<Value>,
    /// The report to pass on to the app.
    report: Option<ReportChecked>,
}

/// One run of the command: who calls, and with what input.
struct Call {
    agent: String,
    pane: PaneId,
    socket: OsString,
    token: Option<String>,
    input: HookInput,
}

fn rules_of(settings: &ReportSettings) -> Rules {
    Rules {
        edited_files_threshold: settings.edited_files,
        tool_call_threshold: settings.tool_calls,
        max_rounds: u32::try_from(settings.max_rounds)
            .unwrap_or(MAX_ROUNDS_LIMIT)
            .min(MAX_ROUNDS_LIMIT),
        summary_max_chars: settings.summary_max_chars,
        question_max_chars: settings.question_max_chars,
        reply_max_chars: settings.reply_max_chars,
        ..Rules::default()
    }
}

/// The hook's input, if the agent finished writing it in time.
fn read_input(stdin: impl Read + Send + 'static) -> Result<String, String> {
    let (read, input) = mpsc::channel();
    thread::Builder::new()
        .name("benterm-report-check-input".into())
        .spawn(move || {
            let mut text = String::new();
            let result = stdin.take(INPUT_LIMIT).read_to_string(&mut text);
            let _ = read.send(result.map(|_| text));
        })
        .map_err(|error| error.to_string())?;
    input
        .recv_timeout(INPUT_WAIT)
        .map_err(|_| "the hook's input did not come in time".to_owned())?
        .map_err(|error| format!("cannot read the hook's input: {error}"))
}

impl Call {
    fn read(
        args: &[String],
        env: &CliEnv,
        stdin: impl Read + Send + 'static,
    ) -> Result<Self, String> {
        let (agent, socket) = crate::cli::parse_report_check(args)?;
        let agent = match agent.as_deref() {
            Some(agent @ ("claude" | "codex")) => agent.to_owned(),
            _ => return Err("it needs --agent claude or --agent codex".into()),
        };
        let pane = env.pane.as_deref().unwrap_or_default();
        let pane = pane
            .parse()
            .map_err(|_| format!("{} is not a pane id: {pane:?}", crate::PANE_ID_ENV))?;
        let socket = socket
            .map(OsString::from)
            .or_else(|| env.socket.clone())
            .unwrap_or_default();
        let input = HookInput::from_json(&read_input(stdin)?)?;
        Ok(Self {
            agent,
            pane,
            socket,
            token: env.token().map(str::to_owned),
            input,
        })
    }

    fn send<M: Method>(&self, request: &M) -> Result<M::Output, String> {
        let mut client = Client::connect(&self.socket)
            .map(|client| client.with_token(self.token.clone()))
            .map_err(|error| format!("cannot connect to the app: {error}"))?;
        client
            .call_within(request, APP_BUDGET)
            .map_err(|error| error.to_string())
    }

    /// Checks the turn, if the check is on and the event is one it has
    /// something to do for.
    fn check(&self, env: &CliEnv) -> Result<Output, String> {
        let input = &self.input;
        // The pane was opened while the app did not check reports, or
        // the terminal turned the check off. A subagent's tools are not
        // the turn's, and it writes no report.
        if env.report_check.as_deref() != Some("on") || input.agent_id.is_some() {
            return Ok(Output::default());
        }
        // Only the numbers are read here: whether the check is on was
        // decided when the pane opened. A settings file with mistakes
        // still gives settings; the app shows the warnings.
        let settings = match &env.config_file {
            Some(path) => benterm_config::load_file(path).config.report,
            None => ReportSettings::default(),
        };
        let rules = rules_of(&settings);
        let state_dir = env
            .state_dir
            .as_ref()
            .ok_or("no state directory: neither XDG_STATE_HOME nor HOME is set")?
            .join(STATE_FOLDER);
        match input.event.as_str() {
            "Stop" => Ok(match check_stop(input, &rules, &state_dir)? {
                Verdict::Quiet => Output::default(),
                Verdict::Block { reason } => Output {
                    print: Some(json!({"decision": "block", "reason": reason})),
                    report: None,
                },
                Verdict::Stop { report, problems } => {
                    let message = (!problems.is_empty()).then(|| {
                        let list: Vec<String> = problems
                            .iter()
                            .map(|problem| format!("- {problem}"))
                            .collect();
                        format!(
                            "Benterm: the agent was sent back and stops with these points of its \
                             report left.\n{}",
                            list.join("\n")
                        )
                    });
                    Output {
                        print: message.map(|message| json!({"systemMessage": message})),
                        report: report.map(|report| ReportChecked {
                            pane: self.pane,
                            agent: self.agent.clone(),
                            session_id: Some(input.session_id.clone()).filter(|id| !id.is_empty()),
                            report,
                            problems,
                        }),
                    }
                }
            }),
            "PostToolUse" => {
                let context = hint(input, &rules, &state_dir)?;
                Ok(Output {
                    print: context.map(|context| {
                        json!({
                            "hookSpecificOutput": {
                                "hookEventName": "PostToolUse",
                                "additionalContext": context,
                            }
                        })
                    }),
                    report: None,
                })
            }
            _ => Ok(Output::default()),
        }
    }
}
