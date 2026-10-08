// Derived from Yororen UI 0.2.0, commit 31005a92286067b56dcc45e7833b91e428533274.
// Copyright 2026 MeowLynxSea; Apache-2.0. See LICENSE-APACHE and NOTICE.
// Modified by RemotePlay: local action-handler import/export paths.
//! Action handler macro for input components.
//!
//! Provides a macro to generate action handlers with a disabled check,
//! reducing boilerplate for repeated on_action patterns.

/// Macro to generate action handlers with disabled check.
///
/// # Example
/// ```ignore
/// .on_action(action_handler!(state, disabled, Backspace, backspace))
/// ```
#[macro_export]
macro_rules! action_handler {
    ($state:expr, $disabled:expr, $action:ty, $method:ident) => {{
        let state = $state.clone();
        let disabled = $disabled;
        move |action: &$action, window, cx| {
            if disabled {
                return;
            }
            state.update(cx, |state, cx| state.$method(action, window, cx))
        }
    }};
}
