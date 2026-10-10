use super::*;
#[test]
fn cancelled_selection_and_late_refresh_cannot_overwrite_current_intent() {
    let mut state = State::default();
    let Some(Effect::Refresh(first)) = state.reduce(Action::Refresh) else {
        panic!()
    };
    let Some(Effect::Refresh(second)) = state.reduce(Action::Refresh) else {
        panic!()
    };
    assert!(!state.refreshed(first, vec![]));
    assert!(state.refreshed(
        second,
        vec![TargetRow {
            key: "a".into(),
            label: "viewer a".into()
        }]
    ));
    let Some(Effect::Select { attempt, .. }) =
        state.reduce(Action::Select("a".into(), ShareKind::Window))
    else {
        panic!()
    };
    assert!(
        state
            .reduce(Action::Select("a".into(), ShareKind::Display))
            .is_none()
    );
    assert!(state.reduce(Action::Refresh).is_none());
    assert_eq!(state.reduce(Action::Cancel), Some(Effect::Cancel(attempt)));
    assert!(!state.completed(attempt, Ok(())));
    let Some(Effect::Select { attempt: next, .. }) =
        state.reduce(Action::Select("a".into(), ShareKind::Display))
    else {
        panic!()
    };
    assert!(next > attempt);
    assert!(!state.completed(attempt, Err("late failure".into())));
    assert!(state.completed(next, Ok(())));
    assert!(state.message.is_none());
}
#[test]
fn separate_window_models_keep_selection_and_failures_independent() {
    let rows = vec![
        TargetRow {
            key: "a".into(),
            label: "a".into(),
        },
        TargetRow {
            key: "b".into(),
            label: "b".into(),
        },
    ];
    let mut a = State {
        rows: rows.clone(),
        ..State::default()
    };
    let mut b = State {
        rows,
        ..State::default()
    };
    assert!(
        a.reduce(Action::Select("missing".into(), ShareKind::Window))
            .is_none()
    );
    let Some(Effect::Select { attempt, .. }) =
        a.reduce(Action::Select("a".into(), ShareKind::Window))
    else {
        panic!()
    };
    b.reduce(Action::Select("b".into(), ShareKind::Display))
        .unwrap();
    assert!(a.completed(attempt, Err("Selection ended".into())));
    assert!(!a.pending());
    assert!(b.pending());
    assert!(b.message.is_none());
    assert_eq!(
        a.reduce(Action::Stop("a".into())),
        Some(Effect::Stop("a".into()))
    );
}

use ely_gpui_component::primitives::FocusScope;
use gpui::{Context, FocusHandle, Modifiers, Render, TestAppContext, WindowAppearance};
use std::cell::RefCell;
struct Fixture {
    root: FocusHandle,
    state: State,
    output: Rc<RefCell<Vec<Action>>>,
}
impl Render for Fixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let output = self.output.clone();
        FocusScope::new(&self.root)
            .root()
            .child(div().w(px(390.0)).child(Controls::new(
                self.state.clone(),
                vec![],
                move |action, _| output.borrow_mut().push(action),
            )))
    }
}
#[gpui::test]
fn actual_ely_share_buttons_dispatch_selection_disable_repeat_and_cancel(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::product_components::component::init(cx).unwrap();
        crate::product_components::theme::install(
            WindowAppearance::Dark,
            crate::design_system::remote_play_themes(),
            cx,
        );
    });
    let output = Rc::new(RefCell::new(vec![]));
    let state = State {
        rows: vec![TargetRow {
            key: "a".into(),
            label: "viewer a".into(),
        }],
        ..State::default()
    };
    let (view, cx) = cx.add_window_view({
        let output = output.clone();
        let state = state.clone();
        move |_, cx| Fixture {
            root: cx.focus_handle(),
            state,
            output,
        }
    });
    cx.run_until_parked();
    let center = cx.debug_bounds("share-window-a").unwrap().center();
    cx.simulate_click(center, Modifiers::none());
    assert_eq!(
        *output.borrow(),
        [Action::Select("a".into(), ShareKind::Window)]
    );
    view.update(cx, |view, cx| {
        view.state
            .reduce(Action::Select("a".into(), ShareKind::Window));
        cx.notify();
    });
    cx.run_until_parked();
    output.borrow_mut().clear();
    let center = cx.debug_bounds("share-window-a").unwrap().center();
    cx.simulate_click(center, Modifiers::none());
    let center = cx.debug_bounds("share-refresh").unwrap().center();
    cx.simulate_click(center, Modifiers::none());
    assert!(output.borrow().is_empty());
    let center = cx.debug_bounds("share-cancel").unwrap().center();
    cx.simulate_click(center, Modifiers::none());
    assert_eq!(*output.borrow(), [Action::Cancel]);
}
