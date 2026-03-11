use std::rc::Rc;

use gpui::{App, Entity, FocusHandle, Window};
use ui::ContextMenu;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PullRequestContextMenuAction {
    OpenInGitHub,
    CheckoutBranch,
    OpenChanges,
    RefreshPullRequest,
}

impl PullRequestContextMenuAction {
    pub(super) fn ordered() -> [Self; 4] {
        [
            Self::OpenInGitHub,
            Self::CheckoutBranch,
            Self::OpenChanges,
            Self::RefreshPullRequest,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenInGitHub => "Open in GitHub",
            Self::CheckoutBranch => "Checkout branch",
            Self::OpenChanges => "Open changes",
            Self::RefreshPullRequest => "Refresh PR",
        }
    }
}

pub(super) fn build_context_menu(
    window: &mut Window,
    cx: &mut App,
    focus_handle: FocusHandle,
    on_action: impl Fn(PullRequestContextMenuAction, &mut Window, &mut App) + 'static,
) -> Entity<ContextMenu> {
    let on_action = Rc::new(on_action);

    ContextMenu::build(window, cx, move |menu, _, _| {
        PullRequestContextMenuAction::ordered()
            .into_iter()
            .fold(menu.context(focus_handle.clone()), |menu, action| {
                let on_action = on_action.clone();
                menu.entry(action.label(), None, move |window, cx| {
                    on_action(action, window, cx);
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::PullRequestContextMenuAction;

    #[test]
    fn ordered_actions_match_requested_menu_order() {
        let labels = PullRequestContextMenuAction::ordered()
            .into_iter()
            .map(|action| action.label())
            .collect::<Vec<_>>();

        assert_eq!(
            labels,
            vec![
                "Open in GitHub",
                "Checkout branch",
                "Open changes",
                "Refresh PR",
            ]
        );
    }
}