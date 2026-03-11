use std::rc::Rc;

use gpui::{App, Entity, FocusHandle, Window};
use ui::{ContextMenu, ContextMenuEntry, IconName};

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

    fn icon(self) -> IconName {
        match self {
            Self::OpenInGitHub => IconName::Github,
            Self::CheckoutBranch => IconName::GitBranch,
            Self::OpenChanges => IconName::Diff,
            Self::RefreshPullRequest => IconName::RefreshTitle,
        }
    }

    fn disabled(self, checkout_branch_disabled: bool) -> bool {
        matches!(self, Self::CheckoutBranch) && checkout_branch_disabled
    }
}

pub(super) fn build_context_menu(
    window: &mut Window,
    cx: &mut App,
    focus_handle: FocusHandle,
    checkout_branch_disabled: bool,
    on_action: impl Fn(PullRequestContextMenuAction, &mut Window, &mut App) + 'static,
) -> Entity<ContextMenu> {
    let on_action = Rc::new(on_action);

    ContextMenu::build(window, cx, move |menu, _, _| {
        PullRequestContextMenuAction::ordered().into_iter().fold(
            menu.context(focus_handle.clone()),
            |menu, action| {
                let on_action = on_action.clone();
                menu.item(
                    ContextMenuEntry::new(action.label())
                        .icon(action.icon())
                        .disabled(action.disabled(checkout_branch_disabled))
                        .handler(move |window, cx| on_action(action, window, cx)),
                )
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::PullRequestContextMenuAction;
    use ui::IconName;

    #[test]
    fn ordered_actions_match_requested_menu_metadata() {
        let entries = PullRequestContextMenuAction::ordered()
            .into_iter()
            .map(|action| (action.label(), action.icon()))
            .collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![
                ("Open in GitHub", IconName::Github),
                ("Checkout branch", IconName::GitBranch),
                ("Open changes", IconName::Diff),
                ("Refresh PR", IconName::RefreshTitle),
            ]
        );
    }

    #[test]
    fn checkout_branch_is_the_only_conditionally_disabled_action() {
        assert!(!PullRequestContextMenuAction::OpenInGitHub.disabled(true));
        assert!(PullRequestContextMenuAction::CheckoutBranch.disabled(true));
        assert!(!PullRequestContextMenuAction::OpenChanges.disabled(true));
        assert!(!PullRequestContextMenuAction::RefreshPullRequest.disabled(true));
        assert!(!PullRequestContextMenuAction::CheckoutBranch.disabled(false));
    }
}
