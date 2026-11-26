//! The form for a local issue: its title, its description, the issue it
//! is a part of and the issues to be done before it. It makes a new
//! issue of the local issues file of a repository, or says an issue of
//! the file anew.
//!
//! Tab and Shift-Tab move between the fields, Enter saves and Escape
//! gives up; in the description Enter starts a line and Cmd-Enter
//! saves. When the issue cannot be saved the form stays and says why.

use benterm_task::NewLocalIssue;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, Render, Styled, Subscription, Window, actions, div, px,
};

use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::terminal_view::hsla;
use crate::theme::CommandPaletteTheme;

actions!(
    local_issue_form,
    [
        NextIssueField,
        /// Moves the keyboard to the field before.
        PreviousIssueField,
        SubmitIssue,
    ]
);

const KEY_CONTEXT: &str = "LocalIssueForm";
const WIDTH: f32 = 480.;
/// The least height of the field for the description, with its name.
const DESCRIPTION_MIN_HEIGHT: f32 = 84.;

pub fn bind_keys(cx: &mut App) {
    let form = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", NextIssueField, form),
        KeyBinding::new("shift-tab", PreviousIssueField, form),
        // From any field; the field of several lines takes it itself.
        KeyBinding::new("cmd-enter", SubmitIssue, form),
    ]);
}

/// What the form is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueFormKind {
    /// A new issue that is a part of no other: an Epic to be.
    NewEpic,
    /// A new issue that is a part of this one.
    NewChild(u64),
    /// The issue of this number, said anew.
    Edit(u64),
}

/// What the form tells the view it is in.
pub enum IssueFormEvent {
    Submitted(NewLocalIssue),
    Cancelled,
}

/// A place the keyboard can be in, in the order Tab goes through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Title,
    Description,
    Parent,
    BlockedBy,
}

const ORDER: [Field; 4] = [
    Field::Title,
    Field::Description,
    Field::Parent,
    Field::BlockedBy,
];

/// The numbers `text` names: `2, #3` and `2 3` are #2 and #3.
pub fn parse_issue_numbers(text: &str) -> Result<Vec<u64>, String> {
    text.split(|ch: char| ch == ',' || ch == '、' || ch.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let digits = part.strip_prefix('#').unwrap_or(part);
            match digits.parse::<u64>() {
                Ok(number) if number > 0 => Ok(number),
                _ => Err(format!(
                    "「{part}」は Issue の番号ではありません。正の整数をカンマで区切って入力してください。"
                )),
            }
        })
        .collect()
}

fn numbers_text(numbers: &[u64]) -> String {
    let texts: Vec<String> = numbers.iter().map(u64::to_string).collect();
    texts.join(", ")
}

/// The form as a view.
pub struct LocalIssueForm {
    kind: IssueFormKind,
    title: Entity<NameEditor>,
    description: Entity<NameEditor>,
    parent: Entity<NameEditor>,
    blocked_by: Entity<NameEditor>,
    /// Why the issue was not saved.
    error: Option<String>,
    /// The issue is being saved, and Enter saves no second one.
    busy: bool,
    theme: CommandPaletteTheme,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<IssueFormEvent> for LocalIssueForm {}

impl LocalIssueForm {
    pub(crate) fn new(
        kind: IssueFormKind,
        issue: &NewLocalIssue,
        theme: CommandPaletteTheme,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |text: &str, placeholder: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                NameEditor::new(text, theme.clone(), cx)
                    .with_placeholder(placeholder)
                    .without_underline()
            })
        };
        let title = field(&issue.title, "", cx);
        let description = cx.new(|cx| {
            NameEditor::lines(
                issue.description.as_deref().unwrap_or(""),
                theme.clone(),
                cx,
            )
            .without_underline()
        });
        let parent = field(
            &issue.parent.map(|n| n.to_string()).unwrap_or_default(),
            "なし（番号を入れると、その Issue の子になります）",
            cx,
        );
        let blocked_by = field(&numbers_text(&issue.blocked_by), "なし（例: 2, 3）", cx);
        let mut subscriptions = Vec::new();
        for editor in [&title, &description, &parent, &blocked_by] {
            subscriptions.push(cx.subscribe(editor, |this, _, event, cx| match event {
                NameEditorEvent::Confirmed(_) => this.submit(cx),
                NameEditorEvent::Cancelled => cx.emit(IssueFormEvent::Cancelled),
            }));
            subscriptions.push(cx.observe(editor, |_, _, cx| cx.notify()));
        }
        Self {
            kind,
            title,
            description,
            parent,
            blocked_by,
            error: None,
            busy: false,
            theme,
            _subscriptions: subscriptions,
        }
    }

    pub fn kind(&self) -> IssueFormKind {
        self.kind
    }

    /// The title as typed.
    pub fn title(&self, cx: &App) -> String {
        self.title.read(cx).text().to_owned()
    }

    /// The description as typed.
    pub fn description(&self, cx: &App) -> String {
        self.description.read(cx).text().to_owned()
    }

    /// The parent as typed.
    pub fn parent(&self, cx: &App) -> String {
        self.parent.read(cx).text().to_owned()
    }

    /// The blockers as typed.
    pub fn blocked_by(&self, cx: &App) -> String {
        self.blocked_by.read(cx).text().to_owned()
    }

    /// Why the issue was not saved, while the form says so.
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
            Field::Title => &self.title,
            Field::Description => &self.description,
            Field::Parent => &self.parent,
            Field::BlockedBy => &self.blocked_by,
        };
        editor.read(cx).focus_handle().clone()
    }

    fn focused(&self, window: &Window, cx: &App) -> Option<Field> {
        ORDER
            .into_iter()
            .find(|field| self.focus_of(*field, cx).is_focused(window))
    }

    /// Where the keyboard goes when the form opens.
    pub(crate) fn first_focus(&self, cx: &App) -> FocusHandle {
        self.focus_of(Field::Title, cx)
    }

    fn step(&mut self, by: usize, window: &mut Window, cx: &mut Context<Self>) {
        let at = self
            .focused(window, cx)
            .and_then(|field| ORDER.iter().position(|other| *other == field));
        let next = at.map_or(0, |at| (at + by) % ORDER.len());
        window.focus(&self.focus_of(ORDER[next], cx), cx);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let title = self.title(cx).trim().to_owned();
        if title.is_empty() {
            return self.failed("タイトルを入力してください。".to_owned(), cx);
        }
        let parent = match parse_issue_numbers(&self.parent(cx)) {
            Ok(numbers) if numbers.len() <= 1 => numbers.first().copied(),
            Ok(_) => return self.failed("親 Issue は 1 件だけ指定できます。".to_owned(), cx),
            Err(error) => return self.failed(error, cx),
        };
        let blocked_by = match parse_issue_numbers(&self.blocked_by(cx)) {
            Ok(numbers) => numbers,
            Err(error) => return self.failed(error, cx),
        };
        let description = Some(self.description(cx).trim().to_owned()).filter(|d| !d.is_empty());
        self.busy = true;
        self.error = None;
        cx.emit(IssueFormEvent::Submitted(NewLocalIssue {
            title,
            description,
            parent,
            blocked_by,
        }));
        cx.notify();
    }

    /// A field under its name, in a box whose edge is the accent while
    /// the field has the keyboard. A press anywhere in it gives the
    /// field the keyboard.
    fn row(
        &self,
        field: Field,
        name: &'static str,
        label: &'static str,
        focused: Option<Field>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let has_keyboard = focused == Some(field);
        let editor = match field {
            Field::Title => &self.title,
            Field::Description => &self.description,
            Field::Parent => &self.parent,
            Field::BlockedBy => &self.blocked_by,
        };
        div()
            .id(name)
            .debug_selector(move || name.into())
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
                    .child(editor.clone()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    window.focus(&this.focus_of(field, cx), cx);
                    cx.notify();
                }),
            )
    }

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
}

impl Render for LocalIssueForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focused(window, cx);
        let title = self.row(Field::Title, "issue-field-title", "タイトル", focused, cx);
        let description = self
            .row(
                Field::Description,
                "issue-field-description",
                "説明",
                focused,
                cx,
            )
            .min_h(px(DESCRIPTION_MIN_HEIGHT));
        let parent = self
            .row(
                Field::Parent,
                "issue-field-parent",
                "親 Issue の番号",
                focused,
                cx,
            )
            .flex_1()
            .min_w_0();
        let blocked_by = self
            .row(
                Field::BlockedBy,
                "issue-field-blocked-by",
                "先に終わる必要がある Issue の番号",
                focused,
                cx,
            )
            .flex_1()
            .min_w_0();
        let theme = &self.theme;
        let (heading, save) = match self.kind {
            IssueFormKind::NewEpic => ("新規 Epic".to_owned(), "作成  ⌘↵"),
            IssueFormKind::NewChild(parent) => (format!("#{parent} の子 Issue を追加"), "作成  ⌘↵"),
            IssueFormKind::Edit(number) => (format!("#{number} を編集"), "保存  ⌘↵"),
        };
        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "issue-form-error".into())
                .text_color(hsla(theme.error))
                .child(error)
        });
        let submit = self.button("issue-form-submit", save, true).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.submit(cx);
            }),
        );
        let cancel = self
            .button("issue-form-cancel", "キャンセル", false)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(IssueFormEvent::Cancelled);
                }),
            );
        div()
            .id("issue-form")
            .debug_selector(|| "issue-form".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &NextIssueField, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousIssueField, window, cx| {
                this.step(ORDER.len() - 1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SubmitIssue, _, cx| this.submit(cx)))
            .occlude()
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
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(12.))
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(heading))
                    .child(title)
                    .child(description)
                    .child(div().flex().gap(px(10.)).child(parent).child(blocked_by))
                    .children(error),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(crate::design::subtle_border(theme.border, theme.background))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(crate::theme::small_text(theme.font_size)))
                            .text_color(hsla(theme.secondary_text))
                            .child(if self.busy {
                                "保存しています…"
                            } else {
                                "Tab で次の欄 · Esc で閉じる"
                            }),
                    )
                    .child(cancel)
                    .child(submit),
            )
    }
}
