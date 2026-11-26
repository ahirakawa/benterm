//! The plan of an agent, read from the calls of the tools it keeps it
//! with. The inputs are made up after what Claude Code 2.1.330 and Codex
//! 0.169.0 give a PostToolUse hook as `tool_input` and `tool_response`.

use benterm_agent::{Plan, PlanCounts, PlanItem, PlanStatus, PlanTracker, PlanUpdate};
use serde_json::{Value, json};

use PlanStatus::{Completed, InProgress, Pending};

fn item(text: &str, status: PlanStatus) -> PlanItem {
    PlanItem {
        text: text.into(),
        status,
    }
}

fn counts(done: usize, in_progress: usize, remaining: usize) -> PlanCounts {
    PlanCounts {
        done,
        in_progress,
        remaining,
    }
}

/// The plan of the conversation `s1` after the calls, each a tool, its
/// input and its answer.
fn plan_after(agent: &str, calls: &[(&str, Value, Value)]) -> Option<Plan> {
    let mut tracker = PlanTracker::default();
    for (tool, input, response) in calls {
        if let Some(update) = PlanUpdate::from_tool(agent, tool, input, Some(response)) {
            tracker.apply("s1", update);
        }
    }
    tracker.plan_of("s1").cloned()
}

fn create(id: &str, subject: &str) -> (&'static str, Value, Value) {
    (
        "TaskCreate",
        json!({"subject": subject, "description": "", "activeForm": "Doing it"}),
        json!({"task": {"id": id, "subject": subject}}),
    )
}

fn update(id: &str, status: &str) -> (&'static str, Value, Value) {
    (
        "TaskUpdate",
        json!({"taskId": id, "status": status}),
        json!({"success": true, "taskId": id, "updatedFields": ["status"]}),
    )
}

#[test]
fn claudes_todo_list_is_the_plan() {
    let todos = json!({"todos": [
        {"content": "Read the code", "status": "in_progress", "activeForm": "Reading the code"},
        {"content": "Write the test", "status": "pending", "activeForm": "Writing the test"},
    ]});
    let answer = json!({"oldTodos": [], "newTodos": todos["todos"]});

    let plan = plan_after("claude", &[("TodoWrite", todos, answer)]).unwrap();

    assert_eq!(
        plan.items,
        [
            item("Read the code", InProgress),
            item("Write the test", Pending)
        ]
    );
    assert_eq!(plan.counts(), counts(0, 1, 1));
}

#[test]
fn claudes_tasks_make_the_same_plan_as_its_todo_list() {
    let plan = plan_after(
        "claude",
        &[
            create("1", "Read the code"),
            create("2", "Write the test"),
            update("1", "in_progress"),
        ],
    )
    .unwrap();

    assert_eq!(
        plan.items,
        [
            item("Read the code", InProgress),
            item("Write the test", Pending)
        ]
    );
    assert_eq!(plan.counts(), counts(0, 1, 1));
}

#[test]
fn a_task_can_be_renamed_completed_and_deleted() {
    let rename = (
        "TaskUpdate",
        json!({"taskId": "2", "subject": "Write two tests", "status": "completed"}),
        json!({"success": true, "taskId": "2", "updatedFields": ["subject", "status"]}),
    );
    let plan = plan_after(
        "claude",
        &[
            create("1", "Read the code"),
            create("2", "Write the test"),
            create("3", "Tidy up"),
            rename,
            update("3", "deleted"),
        ],
    )
    .unwrap();

    assert_eq!(
        plan.items,
        [
            item("Read the code", Pending),
            item("Write two tests", Completed)
        ]
    );
}

#[test]
fn a_task_update_that_failed_changes_nothing() {
    let failed = (
        "TaskUpdate",
        json!({"taskId": "1", "status": "completed"}),
        json!({"success": false, "taskId": "1", "updatedFields": [], "error": "Task not found"}),
    );

    let plan = plan_after("claude", &[create("1", "Read the code"), failed]).unwrap();

    assert_eq!(plan.items, [item("Read the code", Pending)]);
}

#[test]
fn a_task_is_numbered_by_the_text_of_the_answer_too() {
    // The answer as the model reads it, which is all some hooks get.
    let made = (
        "TaskCreate",
        json!({"subject": "Read the code"}),
        json!("Task #7 created successfully: Read the code"),
    );

    let plan = plan_after("claude", &[made, update("7", "completed")]).unwrap();

    assert_eq!(plan.items, [item("Read the code", Completed)]);
}

#[test]
fn a_task_update_answered_with_a_text_counts_only_if_the_text_says_so() {
    let said = |answer: &str| {
        let gone = (
            "TaskUpdate",
            json!({"taskId": "1", "status": "deleted"}),
            json!(answer),
        );
        plan_after("claude", &[create("1", "Read the code"), gone])
    };

    assert!(said("Task #1 not found").is_some());
    assert!(said("Updated task #1 deleted").is_none());
}

#[test]
fn a_task_told_twice_keeps_how_far_it_is() {
    let plan = plan_after(
        "claude",
        &[
            create("1", "Read the code"),
            update("1", "completed"),
            create("1", "Read the code"),
        ],
    )
    .unwrap();

    assert_eq!(plan.items, [item("Read the code", Completed)]);
}

#[test]
fn a_list_of_the_tasks_changes_nothing() {
    // TaskList may run beside other tools, so its hook may come after
    // that of a later change, with the plan as it was before.
    let list = (
        "TaskList",
        json!({}),
        json!({"tasks": [{"id": "1", "subject": "Read the code", "status": "pending"}]}),
    );

    let plan = plan_after(
        "claude",
        &[create("1", "Read the code"), update("1", "completed"), list],
    )
    .unwrap();

    assert_eq!(plan.items, [item("Read the code", Completed)]);
}

#[test]
fn codexs_plan_is_the_plan() {
    for in_progress in ["inProgress", "in_progress"] {
        let input = json!({"explanation": "Starting", "plan": [
            {"step": "Look", "status": "completed"},
            {"step": "Think", "status": in_progress},
            {"step": "Do", "status": "pending"},
        ]});

        let plan = plan_after("codex", &[("update_plan", input, json!("Plan updated"))]).unwrap();

        assert_eq!(
            plan.items,
            [
                item("Look", Completed),
                item("Think", InProgress),
                item("Do", Pending)
            ]
        );
        assert_eq!(plan.counts(), counts(1, 1, 1));
    }
}

#[test]
fn codexs_plan_is_read_from_arguments_left_as_text_too() {
    let input = json!(r#"{"plan":[{"step":"Look","status":"pending"}]}"#);

    let plan = plan_after("codex", &[("update_plan", input, json!("Plan updated"))]).unwrap();

    assert_eq!(plan.items, [item("Look", Pending)]);
}

#[test]
fn a_second_update_changes_the_plan() {
    let first = json!({"todos": [{"content": "Look", "status": "in_progress"}]});
    let second = json!({"todos": [{"content": "Look", "status": "completed"}]});
    let mut tracker = PlanTracker::default();
    let read = |input: &Value| PlanUpdate::from_tool("claude", "TodoWrite", input, None).unwrap();

    assert!(tracker.apply("s1", read(&first)));
    assert!(!tracker.apply("s1", read(&first)), "the same plan again");
    assert!(tracker.apply("s1", read(&second)));

    assert_eq!(tracker.plan_of("s1").unwrap().counts(), counts(1, 0, 0));
}

#[test]
fn the_plan_of_one_conversation_is_not_that_of_another() {
    let mut tracker = PlanTracker::default();
    let (tool, input, answer) = create("1", "Read the code");
    let made = PlanUpdate::from_tool("claude", tool, &input, Some(&answer)).unwrap();
    tracker.apply("s1", made);

    assert!(tracker.plan_of("s2").is_none());

    // The next conversation numbers its tasks from 1 again.
    let (tool, input, answer) = update("1", "completed");
    let done = PlanUpdate::from_tool("claude", tool, &input, Some(&answer)).unwrap();
    tracker.apply("s2", done);

    assert!(tracker.plan_of("s1").is_none(), "{tracker:?}");
    assert!(tracker.plan_of("s2").is_none(), "{tracker:?}");
}

#[test]
fn other_tools_and_broken_inputs_keep_no_plan() {
    let todos = json!({"todos": [{"content": "Read", "status": "pending"}]});
    let read = PlanUpdate::from_tool;

    assert!(read("claude", "Bash", &json!({"command": "ls"}), None).is_none());
    // Each agent has its own tools.
    assert!(read("codex", "TodoWrite", &todos, None).is_none());
    assert!(
        read(
            "claude",
            "TodoWrite",
            &json!({"todos": "all of them"}),
            None
        )
        .is_none()
    );
    assert!(
        read(
            "claude",
            "TaskCreate",
            &json!({"subject": "No number"}),
            None
        )
        .is_none()
    );
    assert!(plan_after("claude", &[]).is_none());
}

#[test]
fn an_item_in_a_state_unheard_of_is_left_to_do() {
    let todos = json!({"todos": [{"content": "Read", "status": "blocked"}]});

    let plan = plan_after("claude", &[("TodoWrite", todos, json!({}))]).unwrap();

    assert_eq!(plan.items, [item("Read", Pending)]);
}

#[test]
fn a_plan_is_read_only_once_its_tool_has_run() {
    let todos = json!({"todos": [{"content": "Read", "status": "pending"}]});
    let read =
        |event| PlanUpdate::from_hook("claude", event, Some("TodoWrite"), Some(&todos), None);

    assert!(read("PostToolUse").is_some());
    assert!(read("PreToolUse").is_none());
    assert!(PlanUpdate::from_hook("claude", "PostToolUse", None, Some(&todos), None).is_none());
}
