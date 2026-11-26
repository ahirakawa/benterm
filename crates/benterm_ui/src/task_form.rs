//! The form for a new task: the repository, the task's name, its branch,
//! the role of its agent and what the agent is to do.
//!
//! Tab and Shift-Tab move between the fields, Left and Right choose the
//! role while the roles have the keyboard, Enter makes the task and
//! Escape gives up. When the task cannot be made the form stays and says
//! why, so that the name can be put right.

use benterm_config::{Agent, Role, TaskAgent};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, Render, Styled, Subscription, Window, actions, div, px,
};

use crate::BentermWindow;
use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::roles::role_label;
use crate::sidebar;
use crate::tasks::{NewTask, TaskRequest, TaskStarted, typed_directory};
use crate::terminal_view::hsla;
use crate::theme::CommandPaletteTheme;

actions!(
    task_form,
    [
        NextField,
        /// Moves the keyboard to the field before.
        PreviousField,
        NextRole,
        /// Chooses the role before.
        PreviousRole,
        SubmitTask,
        /// Closes the form without making a task.
        CancelTask,
    ]
);

const KEY_CONTEXT: &str = "TaskForm";
/// The key context of the row of roles, which has no text to type.
const ROLES_CONTEXT: &str = "TaskFormRoles";

const WIDTH: f32 = 560.;
/// How far below the top of the window the form floats.
const TOP: f32 = 48.;
/// The least height of the field for what the agent is to do, with its
/// name: room for three lines.
const WORK_MIN_HEIGHT: f32 = 96.;

pub fn bind_keys(cx: &mut App) {
    let (form, roles) = (Some(KEY_CONTEXT), Some(ROLES_CONTEXT));
    cx.bind_keys([
        // Beside Cmd-N, which makes a workspace without a task.
        KeyBinding::new("cmd-shift-n", NewTask, Some(sidebar::KEY_CONTEXT)),
        KeyBinding::new("tab", NextField, form),
        KeyBinding::new("shift-tab", PreviousField, form),
        // From any field; the field of several lines takes it itself.
        KeyBinding::new("cmd-enter", SubmitTask, form),
        KeyBinding::new("right", NextRole, roles),
        KeyBinding::new("left", PreviousRole, roles),
        KeyBinding::new("enter", SubmitTask, roles),
        KeyBinding::new("cmd-enter", SubmitTask, roles),
        KeyBinding::new("escape", CancelTask, roles),
    ]);
}

/// What the form holds when Enter is pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskFormValues {
    pub repository: String,
    pub name: String,
    pub branch: String,
    pub role: Option<String>,
    pub work: String,
}

/// What the form tells the window.
pub enum TaskFormEvent {
    Submitted(TaskFormValues, Option<TaskAgent>),
    Cancelled,
}

/// A place the keyboard can be in, in the order Tab goes through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Work,
    Role,
    Branch,
    Repository,
}

const ORDER: [Field; 5] = [
    Field::Name,
    Field::Work,
    Field::Role,
    Field::Branch,
    Field::Repository,
];

/// The form as a view.
pub struct TaskForm {
    name: Entity<NameEditor>,
    branch: Entity<NameEditor>,
    work: Entity<NameEditor>,
    repository: Entity<NameEditor>,
    roles: Vec<Role>,
    default_agent: Option<TaskAgent>,
    /// Index of a role or the direct agent after the roles. None selects the shell.
    role: Option<usize>,
    role_focus: FocusHandle,
    /// Why the task was not made.
    error: Option<String>,
    /// A task is being made, and Enter makes no second one.
    busy: bool,
    theme: CommandPaletteTheme,
    _subscriptions: Vec<Subscription>,
}

impl TaskForm {
    fn new(
        defaults: TaskFormValues,
        roles: Vec<Role>,
        default_agent: Option<TaskAgent>,
        theme: CommandPaletteTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        // Each field is drawn in a box of its own.
        let field = |text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| NameEditor::new(text, theme.clone(), cx).without_underline())
        };
        let name = field(&defaults.name, cx);
        let branch = cx.new(|cx| {
            NameEditor::new(&defaults.branch, theme.clone(), cx)
                .with_placeholder("named as the task")
                .without_underline()
        });
        // What the agent is to do may take several lines.
        let work =
            cx.new(|cx| NameEditor::lines(&defaults.work, theme.clone(), cx).without_underline());
        let repository = field(&defaults.repository, cx);
        let role = defaults
            .role
            .as_ref()
            .and_then(|name| roles.iter().position(|role| &role.name == name))
            .or_else(|| default_agent.as_ref().map(|_| roles.len()));
        let mut subscriptions = Vec::new();
        for editor in [&name, &branch, &work, &repository] {
            subscriptions.push(cx.subscribe(editor, |this, _, event, cx| match event {
                NameEditorEvent::Confirmed(_) => this.submit(cx),
                NameEditorEvent::Cancelled => cx.emit(TaskFormEvent::Cancelled),
            }));
            subscriptions.push(cx.observe(editor, |_, _, cx| cx.notify()));
        }
        Self {
            name,
            branch,
            work,
            repository,
            roles,
            default_agent,
            role,
            role_focus: cx.focus_handle(),
            error: None,
            busy: false,
            theme,
            _subscriptions: subscriptions,
        }
    }

    /// The repository as typed.
    pub fn repository(&self, cx: &App) -> String {
        self.repository.read(cx).text().to_owned()
    }

    /// The task's name as typed.
    pub fn name(&self, cx: &App) -> String {
        self.name.read(cx).text().to_owned()
    }

    /// The name of the branch as typed.
    pub fn branch(&self, cx: &App) -> String {
        self.branch.read(cx).text().to_owned()
    }

    /// What the agent is to do, as typed.
    pub fn work(&self, cx: &App) -> String {
        self.work.read(cx).text().to_owned()
    }

    /// The name of the chosen role, or none for a shell.
    pub fn role(&self) -> Option<String> {
        self.role
            .and_then(|index| self.roles.get(index))
            .map(|role| role.name.clone())
    }

    /// Why the task was not made, while the form says so.
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        self.busy = false;
        cx.notify();
    }

    fn focus_of(&self, field: Field, cx: &App) -> FocusHandle {
        let editor = match field {
            Field::Name => &self.name,
            Field::Branch => &self.branch,
            Field::Work => &self.work,
            Field::Repository => &self.repository,
            Field::Role => return self.role_focus.clone(),
        };
        editor.read(cx).focus_handle().clone()
    }

    /// The field that has the keyboard.
    fn focused(&self, window: &Window, cx: &App) -> Option<Field> {
        ORDER
            .into_iter()
            .find(|field| self.focus_of(*field, cx).is_focused(window))
    }

    /// Where the keyboard goes when the form opens.
    pub(crate) fn first_focus(&self, cx: &App) -> FocusHandle {
        self.focus_of(Field::Name, cx)
    }

    fn step(&mut self, by: usize, window: &mut Window, cx: &mut Context<Self>) {
        let at = self
            .focused(window, cx)
            .and_then(|field| ORDER.iter().position(|other| *other == field));
        let next = at.map_or(0, |at| (at + by) % ORDER.len());
        window.focus(&self.focus_of(ORDER[next], cx), cx);
        cx.notify();
    }

    /// Chooses the role `by` places on, with no role between the last
    /// and the first.
    fn turn_role(&mut self, by: usize, cx: &mut Context<Self>) {
        let places = self.roles.len() + usize::from(self.default_agent.is_some()) + 1;
        let at = self.role.map_or(0, |role| role + 1);
        self.role = ((at + by) % places).checked_sub(1);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let text = |editor: &Entity<NameEditor>| editor.read(cx).text().trim().to_owned();
        let values = TaskFormValues {
            repository: text(&self.repository),
            name: text(&self.name),
            branch: text(&self.branch),
            role: self.role(),
            work: text(&self.work),
        };
        let agent = self
            .default_agent
            .as_ref()
            .filter(|_| self.role == Some(self.roles.len()))
            .cloned();
        cx.emit(TaskFormEvent::Submitted(values, agent));
        cx.notify();
    }

    /// A field under its name: a box with an edge, the accent while the
    /// field has the keyboard. A press anywhere in it gives the field the
    /// keyboard.
    fn row<E: IntoElement>(
        &self,
        field: Field,
        label: &'static str,
        focused: Option<Field>,
        content: E,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let has_keyboard = focused == Some(field);
        // The roles are chips, which have their own edges.
        let boxed = field != Field::Role;
        div()
            .id(label)
            .debug_selector(move || format!("task-field-{label}"))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_size(px(crate::theme::small_text(theme.font_size)))
                    .text_color(hsla(if has_keyboard {
                        theme.text
                    } else {
                        theme.secondary_text
                    }))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .when(boxed, |field| {
                        field
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(crate::theme::RADIUS_CONTROL))
                            .bg(hsla(theme.input_background))
                            .border_1()
                            .border_color(hsla(if has_keyboard {
                                theme.accent
                            } else {
                                theme.input_border
                            }))
                            .cursor_text()
                    })
                    .child(content),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    window.focus(&this.focus_of(field, cx), cx);
                    cx.notify();
                }),
            )
    }

    /// A button of the bar under the fields; the one that makes the task
    /// is filled with the accent.
    fn button(
        &self,
        name: &'static str,
        label: &'static str,
        filled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        div()
            .id(name)
            .debug_selector(move || name.into())
            .px(px(12.))
            .py(px(3.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .border_1()
            .cursor_pointer()
            .map(|button| {
                if filled {
                    button
                        .bg(hsla(theme.accent))
                        .border_color(hsla(theme.accent))
                        .text_color(hsla(theme.input_background))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                } else {
                    button
                        .bg(hsla(theme.input_background))
                        .border_color(hsla(theme.input_border))
                        .hover(|button| button.bg(hsla(theme.selected_background)))
                }
            })
            .child(label)
    }

    fn role_chips(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let chip = |index: Option<usize>,
                    label: String,
                    agent_tag: Option<(&'static str, benterm_core::Rgb)>,
                    cx: &mut Context<Self>| {
            let chosen = self.role == index;
            div()
                .id(("task-role", index.map_or(0, |index| index + 1)))
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .py(px(3.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .border_1()
                .border_color(hsla(theme.input_border))
                .cursor_pointer()
                .when(chosen, |chip| {
                    chip.bg(crate::design::with_alpha(theme.accent, 0.14))
                        .border_color(hsla(theme.accent))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .when(!chosen, |chip| chip.text_color(hsla(theme.secondary_text)))
                .child(label)
                .when_some(agent_tag, |chip, (tag, color)| {
                    chip.child(crate::design::pill(tag, color, theme.font_size, false))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.role = index;
                        cx.notify();
                    }),
                )
        };
        let mut chips = vec![chip(None, "none (a shell)".to_owned(), None, cx)];
        for (index, role) in self.roles.iter().enumerate() {
            let tag = match role.agent {
                Agent::Claude => Some(("Claude", theme.accent)),
                Agent::Codex => Some(("Codex", theme.secondary_text)),
            };
            chips.push(chip(Some(index), role_label(role), tag, cx));
        }
        if let Some(agent) = &self.default_agent {
            let (name, tag) = match agent.agent {
                Agent::Claude => ("Claude Code", Some(("Claude", theme.accent))),
                Agent::Codex => ("Codex", Some(("Codex", theme.secondary_text))),
            };
            let label = agent
                .model
                .as_ref()
                .map_or_else(|| name.to_owned(), |model| format!("{name} · {model}"));
            chips.push(chip(Some(self.roles.len()), label, tag, cx));
        }
        div()
            .id("task-roles")
            .track_focus(&self.role_focus)
            .key_context(ROLES_CONTEXT)
            .on_action(cx.listener(|this, _: &NextRole, _, cx| this.turn_role(1, cx)))
            .on_action(
                cx.listener(|this, _: &PreviousRole, _, cx| this.turn_role(this.roles.len(), cx)),
            )
            .on_action(cx.listener(|this, _: &SubmitTask, _, cx| this.submit(cx)))
            .on_action(cx.listener(|_, _: &CancelTask, _, cx| cx.emit(TaskFormEvent::Cancelled)))
            .flex()
            .flex_wrap()
            .flex_1()
            .min_w_0()
            .gap_1()
            .children(chips)
    }
}

impl EventEmitter<TaskFormEvent> for TaskForm {}

impl Render for TaskForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focused(window, cx);
        let name = self.row(Field::Name, "Task", focused, self.name.clone(), cx);
        // Room for three lines, and more as they are typed.
        let work = self
            .row(Field::Work, "Work", focused, self.work.clone(), cx)
            .min_h(px(WORK_MIN_HEIGHT));
        let chips = self.role_chips(cx);
        let role = self.row(Field::Role, "Role", focused, chips, cx);
        // What is seldom changed, side by side under the rest.
        let branch = self
            .row(Field::Branch, "Branch", focused, self.branch.clone(), cx)
            .flex_1()
            .min_w_0();
        let repository = self
            .row(
                Field::Repository,
                "Repository",
                focused,
                self.repository.clone(),
                cx,
            )
            .flex_1()
            .min_w_0();
        let theme = &self.theme;
        let hint_bar = if self.busy {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(crate::theme::small_text(theme.font_size)))
                .text_color(hsla(theme.secondary_text))
                .child("Making the worktree…")
        } else {
            let hint_item = |key: &str, label: &'static str| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .children(crate::design::kbd_group(
                        key,
                        theme.background,
                        theme.border,
                        theme.secondary_text,
                        theme.font_size,
                    ))
                    .child(label)
            };
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_size(px(crate::theme::small_text(theme.font_size)))
                .text_color(hsla(theme.secondary_text))
                .child(hint_item("Tab", "Next"))
                .child(hint_item("← →", "Role"))
                .child(hint_item("Esc", "Close"))
        };
        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "task-error".into())
                .text_color(hsla(theme.error))
                .child(error)
        });
        let create = self
            .button("task-create", "Create Task  ⌘↵", true)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit(cx);
                }),
            );
        let cancel = self.button("task-cancel", "Cancel", false).on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(TaskFormEvent::Cancelled);
            }),
        );
        div()
            .id("task-form")
            .debug_selector(|| "task-form".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &NextField, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousField, window, cx| {
                this.step(ORDER.len() - 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SubmitTask, _, cx| this.submit(cx)))
            .occlude()
            .relative()
            .overflow_hidden()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .bg(hsla(theme.background))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_CARD))
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .text_color(hsla(theme.text))
            .child(crate::design::top_highlight_bar(theme.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .pt(px(12.))
                    .pb(px(12.))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("New Task"),
                    )
                    .child(name)
                    .child(work)
                    .child(role)
                    .child(div().flex().gap(px(10.)).child(branch).child(repository))
                    .children(error),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(crate::design::subtle_border(theme.border, theme.background))
                    .child(hint_bar)
                    .child(cancel)
                    .child(create),
            )
    }
}

/// The form while it is open in a window.
pub(crate) struct OpenTaskForm {
    view: Entity<TaskForm>,
    _subscription: Subscription,
}

impl BentermWindow {
    /// The form for a new task, while it is open.
    pub fn task_form(&self) -> Option<Entity<TaskForm>> {
        self.tasks.form.as_ref().map(|form| form.view.clone())
    }

    /// Opens the form for a new task and gives it the keyboard. The
    /// repository is the one a new tab would start in.
    pub fn open_task_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.grid(cx).read(cx).directory_for_new_terminal(cx);
        self.open_task_form_with(
            TaskFormValues {
                repository: directory
                    .as_deref()
                    .and_then(|directory| directory.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                name: String::new(),
                branch: String::new(),
                role: None,
                work: String::new(),
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_issue_task_form(
        &mut self,
        mut values: TaskFormValues,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if values.role.is_none() && self.settings.default_task_agent().is_none() {
            values.role = self.settings.roles().first().map(|role| role.name.clone());
        }
        self.open_task_form_with(values, window, cx);
    }

    fn open_task_form_with(
        &mut self,
        defaults: TaskFormValues,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(form) = &self.tasks.form {
            let focus = form.view.read(cx).first_focus(cx);
            window.focus(&focus, cx);
            return;
        }
        let roles = self.settings.roles().to_vec();
        let theme = self.settings.theme().command_palette.clone();
        let agent = self.settings.default_task_agent().cloned();
        let view = cx.new(|cx| TaskForm::new(defaults, roles, agent, theme, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            TaskFormEvent::Submitted(values, agent) => {
                this.submit_task_form(values.clone(), agent.clone(), window, cx)
            }
            TaskFormEvent::Cancelled => this.close_task_form(window, cx),
        });
        let focus = view.read(cx).first_focus(cx);
        window.focus(&focus, cx);
        self.tasks.form = Some(OpenTaskForm {
            view,
            _subscription: events,
        });
        cx.notify();
    }

    fn close_task_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tasks.form.take().is_some() {
            if let Some(issues) = self.issue_graph_view() {
                let focus = issues.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                cx.notify();
            } else {
                self.focus_selected(window, cx);
            }
        }
    }

    fn submit_task_form(
        &mut self,
        values: TaskFormValues,
        agent: Option<TaskAgent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shells) = self.shells.clone() else {
            return self.task_form_failed("this window cannot start tasks".into(), cx);
        };
        // git and the agent are looked up as the user's shell finds
        // them, so the task waits for the shell's environment.
        if shells.login_env.is_pending() {
            let taken = shells.login_env.taken();
            cx.spawn_in(window, async move |this, cx| {
                taken.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.submit_task_form(values, agent, window, cx)
                });
            })
            .detach();
            return;
        }
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let request = TaskRequest {
            directory: typed_directory(&values.repository, home.as_deref()),
            name: values.name,
            branch: Some(values.branch).filter(|branch| !branch.is_empty()),
            role: values.role,
            agent,
            work: values.work,
            select: true,
        };
        // The form may be closed, and another opened, before the task
        // is made: only the form that asked is told.
        let asked = self.task_form().map(|form| form.entity_id());
        let done = Box::new(
            move |this: &mut Self,
                  started: Result<TaskStarted, String>,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                let same = asked.is_some() && this.task_form().map(|f| f.entity_id()) == asked;
                match started {
                    // The task's workspace has the keyboard already.
                    Ok(_) if same => {
                        this.tasks.form = None;
                        this.focus_selected(window, cx);
                    }
                    Ok(_) => {}
                    Err(error) if same => this.task_form_failed(error, cx),
                    Err(error) => this.task_failed(format!("The task was not made: {error}."), cx),
                }
            },
        );
        self.create_task(request, &shells, window, cx, done);
    }

    fn task_form_failed(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(form) = &self.tasks.form {
            form.view.update(cx, |form, cx| form.failed(error, cx));
        }
    }

    /// The form floating over the top of the window, while it is open.
    pub(crate) fn task_form_overlay(&self) -> Option<impl IntoElement> {
        let terminal_bg = self.settings.theme().terminal.background;
        self.tasks.form.as_ref().map(|form| {
            div()
                .absolute()
                .inset_0()
                .child(crate::design::modal_scrim(terminal_bg))
                .child(
                    div()
                        .absolute()
                        .top(px(TOP))
                        .left_0()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(crate::browser::covers())
                        .child(form.view.clone()),
                )
        })
    }
}
