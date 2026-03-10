use gpui::{AnyElement, Context, Window};
use ui::{
    ButtonCommon, ButtonStyle, IconButton, IconButtonShape, IconName, IconSize, Label, Tooltip,
    h_flex, prelude::*,
};

use crate::PullRequestPanel;

pub(super) fn render_top_bar(
    panel: &PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let focus_handle = panel.focus_handle.clone();
    let refresh_disabled = !panel.view_state.refresh_ready;
    let collapse_all_disabled = !panel.can_collapse_all_sections();

    h_flex()
        .id("pull-request-panel-top-bar")
        .w_full()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .py_1()
        .bg(cx.theme().colors().tab_bar_background)
        .border_b_1()
        .border_color(cx.theme().colors().border)
        .child(Label::new("Pull requests"))
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .child(
                    IconButton::new("create-pull-request", IconName::Plus)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .on_click(|_, window, cx| {
                            window
                                .dispatch_action(Box::new(zed_actions::git::CreatePullRequest), cx)
                        })
                        .tooltip({
                            let focus_handle = focus_handle.clone();
                            move |_window, cx| {
                                Tooltip::for_action_in(
                                    "Create pull request",
                                    &zed_actions::git::CreatePullRequest,
                                    &focus_handle,
                                    cx,
                                )
                            }
                        }),
                )
                .child(
                    IconButton::new("refresh-pull-request-list", IconName::RotateCcw)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .disabled(refresh_disabled)
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                        .tooltip(move |_window, cx| {
                            Tooltip::with_meta(
                                "Refresh pull requests",
                                None,
                                "Reload live GitHub pull requests",
                                cx,
                            )
                        }),
                )
                .child(
                    IconButton::new("collapse-all-pull-request-lists", IconName::ListCollapse)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .style(ButtonStyle::Subtle)
                        .disabled(collapse_all_disabled)
                        .on_click(cx.listener(|this, _, _, cx| this.collapse_all_sections(cx)))
                        .tooltip(move |_window, cx| {
                            Tooltip::with_meta(
                                "Collapse all lists",
                                None,
                                "Collapse every pull request section",
                                cx,
                            )
                        }),
                ),
        )
        .into_any_element()
}
