//! The rules a report is held against.

use std::path::Path;

use crate::facts::{TurnFacts, absolute};
use crate::files::clip;
use crate::report::{Deliverable, Report};
use crate::rules::Rules;

fn is_code_file(path: &str, rules: &Rules) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            rules
                .code_extensions
                .iter()
                .any(|code| code.eq_ignore_ascii_case(extension))
        })
}

/// The path as the problems name it: from the working directory when
/// the file is in it, so that a problem holds no more of the machine's
/// folders than it has to.
fn shown(path: &str, cwd: &str) -> String {
    let below = format!("{}/", cwd.trim_end_matches('/'));
    clip(path.strip_prefix(&below).unwrap_or(path), 120)
}

/// Whether `text`, in lower case, has `word`. A word of ASCII letters
/// is looked for at the start of a word, so that "latest" does not say
/// "test" while "tests" does.
fn says(text: &str, word: &str) -> bool {
    let word = word.to_lowercase();
    if !word.is_ascii() {
        return text.contains(&word);
    }
    text.match_indices(&word).any(|(at, _)| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_ascii_alphanumeric())
    })
}

/// The problems of `report`: none when it passes.
///
/// `reply` is what the agent said last in the terminal, `facts` what
/// the transcript says the turn did, and `cwd` the working directory
/// the paths of the report are taken from.
pub fn check_report(
    report: &Report,
    reply: &str,
    facts: &TurnFacts,
    cwd: &str,
    rules: &Rules,
) -> Vec<String> {
    let mut problems = Vec::new();

    if report.title.trim().is_empty() {
        problems.push("title is empty. Write the name of the work.".to_owned());
    }
    if report.goal.trim().is_empty() {
        problems.push(
            "goal is empty. Write in one sentence what the user was trying to do with this work."
                .to_owned(),
        );
    }
    if report.status().is_none() {
        problems.push(format!(
            "conclusion is {:?}. Write pass, issues or blocked: pass when the result can be \
             taken as it is, issues when it has problems you name, blocked when you cannot go \
             on without help. What happened goes in summary.",
            clip(&report.conclusion, 40)
        ));
    }
    let summary = report.summary.trim().chars().count();
    if summary == 0 {
        problems.push("summary is empty. Write what happened or what you changed.".to_owned());
    } else if summary > rules.summary_max_chars {
        problems.push(format!(
            "summary has {summary} characters. Keep it within {}, and move the breakdown or \
             the steps to verified or deliverables.",
            rules.summary_max_chars
        ));
    }
    let reply = reply.trim().chars().count();
    if reply > rules.reply_max_chars {
        problems.push(format!(
            "The reply in the terminal has {reply} characters. Keep it within {}, and write \
             only the conclusion and whether there is anything to decide. The details are in \
             the JSON.",
            rules.reply_max_chars
        ));
    }

    for (at, decision) in report.decisions.iter().enumerate() {
        let question = decision.question.trim().chars().count();
        let name = if question == 0 {
            problems.push(format!("decisions[{at}] has an empty question."));
            format!("decisions[{at}]")
        } else {
            format!("Decision \"{}\"", clip(&decision.question, 60))
        };
        if question > rules.question_max_chars {
            problems.push(format!(
                "{name}: question has {question} characters. It is shown in a row of the \
                 session list, so keep it within {}, and move the background to reason or to \
                 the effect of options.",
                rules.question_max_chars
            ));
        }
        if decision.options.len() < 2 {
            problems.push(format!(
                "{name}: options has fewer than 2 items. Write every choice."
            ));
        }
        let empty = |text: &String| text.trim().is_empty();
        if decision
            .options
            .iter()
            .any(|option| empty(&option.label) || empty(&option.effect))
        {
            problems.push(format!(
                "{name}: an option has an empty label or effect. In effect, write what happens \
                 if it is chosen."
            ));
        }
        if !decision
            .options
            .iter()
            .any(|option| option.label == decision.recommend)
        {
            problems.push(format!(
                "{name}: recommend does not match any label in options."
            ));
        }
        if empty(&decision.reason) {
            problems.push(format!(
                "{name}: reason is empty. Write why you recommend it."
            ));
        }
    }

    for (at, deliverable) in report.deliverables.iter().enumerate() {
        if deliverable.path.trim().is_empty() || deliverable.summary.trim().is_empty() {
            problems.push(format!("deliverables[{at}] has an empty path or summary."));
        }
    }
    let listed: Vec<(String, &Deliverable)> = report
        .deliverables
        .iter()
        .filter(|deliverable| !deliverable.path.trim().is_empty())
        .map(|deliverable| {
            let path = deliverable.path.trim().trim_end_matches('/');
            (absolute(path, cwd), deliverable)
        })
        .collect();
    for file in &facts.edited_files {
        let name = shown(file, cwd);
        let exact = listed
            .iter()
            .find(|(path, _)| path == file)
            .map(|(_, deliverable)| *deliverable);
        if is_code_file(file, rules) {
            match exact {
                None => problems.push(format!(
                    "The code file {name} is not in deliverables. Add an item whose path is \
                     this file, and write changes."
                )),
                Some(deliverable) if deliverable.changes.is_empty() => problems.push(format!(
                    "The code file {name} has empty changes. Write line, before and after for \
                     at least one place."
                )),
                Some(deliverable) => {
                    let bad = deliverable.changes.iter().any(|change| {
                        change.line == 0
                            || change.before.trim().is_empty()
                            || change.after.trim().is_empty()
                    });
                    if bad {
                        problems.push(format!(
                            "{name}: a change has line 0, or an empty before or after. For a \
                             new file, write \"none (new file)\" as before."
                        ));
                    }
                }
            }
            continue;
        }
        let in_listed_folder = listed
            .iter()
            .any(|(path, _)| Path::new(path).is_dir() && file.starts_with(&format!("{path}/")));
        if exact.is_none() && !in_listed_folder {
            problems.push(format!(
                "The edited file {name} is not in deliverables. Write the path of the file, or \
                 of a folder that holds it."
            ));
        }
    }

    let verified = report.verified.join("\n").to_lowercase();
    let commands = facts.commands.join("\n").to_lowercase();
    for rule in &rules.verification_rules {
        let Some(word) = rule.words.iter().find(|word| says(&verified, word)) else {
            continue;
        };
        let ran = rule
            .commands
            .iter()
            .any(|command| commands.contains(&command.to_lowercase()));
        if !ran {
            problems.push(format!(
                "verified says \"{word}\", but this turn has no record of running a matching \
                 command ({}). Run it, or move the item to unverified.",
                rule.commands.join(", ")
            ));
        }
    }

    problems
}
