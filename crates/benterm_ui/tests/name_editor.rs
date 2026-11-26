//! GPUI tests for NameEditor: click-to-place caret, multiline Up/Down
//! navigation, and line wrapping in narrow containers.

use benterm_ui::name_editor::NameEditor;
use benterm_ui::theme::Theme;
use gpui::{
    AppContext, Context, Entity, Font, FontWeight, IntoElement, Modifiers, ParentElement, Render,
    Styled, TestAppContext, TextRun, VisualTestContext, Window, div, point, px, size,
};

struct Host {
    editor: Entity<NameEditor>,
    width: f32,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p(px(16.))
            .font_family("Menlo")
            .text_size(px(14.))
            .child(div().w(px(self.width)).flex().child(self.editor.clone()))
    }
}

fn open_editor(
    cx: &mut TestAppContext,
    multiline: bool,
    text: &str,
    width: f32,
) -> (Entity<NameEditor>, VisualTestContext) {
    cx.update(benterm_ui::bind_keys);
    let theme = Theme::default().command_palette;
    let text = text.to_owned();
    let window = cx.add_window(|window, cx| {
        let editor = cx.new(|cx| {
            if multiline {
                NameEditor::lines(&text, theme, cx).without_underline()
            } else {
                NameEditor::new(&text, theme, cx).without_underline()
            }
        });
        let focus = editor.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        Host { editor, width }
    });
    let editor = window.read_with(cx, |host, _| host.editor.clone()).unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(600.), px(400.)));
    vcx.run_until_parked();
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
    (editor, vcx)
}

#[gpui::test]
fn clicking_between_c_and_d_places_caret_there(cx: &mut TestAppContext) {
    let (editor, mut vcx) = open_editor(cx, false, "abcdef", 300.);
    let bounds = vcx
        .debug_bounds("name-editor")
        .expect("name-editor is drawn");

    // Measure the exact x offset between 'c' and 'd' in the same font.
    let x_offset = vcx.update(|window, _| {
        let mut font = Font {
            family: "Menlo".into(),
            weight: FontWeight::NORMAL,
            ..Default::default()
        };
        font.family = "Menlo".into();
        let run = TextRun {
            len: 6,
            font,
            color: gpui::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line("abcdef".into(), px(14.), &[run], None);
        shaped.x_for_index(3)
    });

    let click_at = point(bounds.origin.x + x_offset, bounds.center().y);
    vcx.simulate_click(click_at, Modifiers::none());
    vcx.run_until_parked();
    vcx.simulate_input("x");
    vcx.run_until_parked();

    let text = editor.read_with(&vcx, |editor, _| editor.text().to_owned());
    assert_eq!(text, "abcxdef");
}

#[gpui::test]
fn down_and_up_arrows_move_caret_between_lines_in_multiline_editor(cx: &mut TestAppContext) {
    let (editor, mut vcx) = open_editor(cx, true, "first\nsecond\nthird", 300.);

    vcx.simulate_keystrokes("home right right");
    vcx.run_until_parked();

    // Down arrow moves to line 2 ("se|cond").
    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();
    vcx.simulate_input("X");
    vcx.run_until_parked();
    assert_eq!(
        editor.read_with(&vcx, |editor, _| editor.text().to_owned()),
        "first\nseXcond\nthird"
    );

    // Up arrow moves back to line 1 ("fir|st").
    vcx.simulate_keystrokes("up");
    vcx.run_until_parked();
    vcx.simulate_input("Y");
    vcx.run_until_parked();
    assert_eq!(
        editor.read_with(&vcx, |editor, _| editor.text().to_owned()),
        "firYst\nseXcond\nthird"
    );
}

#[gpui::test]
fn long_line_in_multiline_editor_wraps_into_multiple_visual_lines(cx: &mut TestAppContext) {
    let (_editor, mut vcx) = open_editor(cx, true, "abcdefghijklmnopqrstuvwxyz0123456789", 120.);

    let line0 = vcx
        .debug_bounds("name-editor-line-0")
        .expect("first wrapped visual line is drawn");
    let line1 = vcx
        .debug_bounds("name-editor-line-1")
        .expect("second wrapped visual line is drawn");
    assert!(
        line1.origin.y > line0.origin.y,
        "wrapped second line should be drawn below the first line"
    );
}
