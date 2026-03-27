use gpui::{AnyElement, Context, Corner, Point, Window, px};
use ui::{
    ButtonCommon, ButtonStyle, ContextMenu, IconButton, IconButtonShape, IconName, IconPosition,
    IconSize, Label, PopoverMenu, Tooltip, h_flex, prelude::*,
};

use crate::{PullRequestPanel, PullRequestSort, PullRequestSortDirection};

pub(super) fn render_top_bar(
    panel: &PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let _focus_handle = panel.focus_handle.clone();
    let panel_handle = cx.entity().downgrade();
    let refresh_disabled = !panel.view_state.refresh_ready;
    let collapse_all_disabled = !panel.can_collapse_all_sections();
    let sort = panel.sort;
    let sort_direction = panel.sort_direction;

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
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_create_pull_request_panel(window, cx);
                        }))
                        .tooltip({
                            move |_window, cx| Tooltip::simple("Create pull request", cx)
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
                )
                .child(
                    PopoverMenu::new("pull-request-sort-menu")
                        .anchor(Corner::TopRight)
                        .offset(Point {
                            x: px(0.0),
                            y: px(2.0),
                        })
                        .trigger_with_tooltip(
                            IconButton::new(
                                "pull-request-sort-menu-trigger",
                                IconName::EllipsisVertical,
                            )
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .style(ButtonStyle::Subtle),
                            move |_window, cx| {
                                Tooltip::with_meta(
                                    "Pull request sort options",
                                    None,
                                    "Change the GitHub pull request sort and direction",
                                    cx,
                                )
                            },
                        )
                        .menu(move |window, cx| {
                            let panel_handle = panel_handle.clone();
                            Some(ContextMenu::build(window, cx, move |menu, _, _| {
                                let menu = PullRequestSort::ordered().into_iter().fold(
                                    menu.header("Sort by"),
                                    |menu, option| {
                                        let panel_handle = panel_handle.clone();
                                        menu.toggleable_entry(
                                            option.label(),
                                            sort == option,
                                            IconPosition::Start,
                                            None,
                                            move |_, cx| {
                                                panel_handle
                                                    .update(cx, |panel, cx| {
                                                        panel.set_sort(option, cx)
                                                    })
                                                    .ok();
                                            },
                                        )
                                    },
                                );

                                PullRequestSortDirection::ordered().into_iter().fold(
                                    menu.separator().header("Direction"),
                                    |menu, option| {
                                        let panel_handle = panel_handle.clone();
                                        menu.toggleable_entry(
                                            option.label(),
                                            sort_direction == option,
                                            IconPosition::Start,
                                            None,
                                            move |_, cx| {
                                                panel_handle
                                                    .update(cx, |panel, cx| {
                                                        panel.set_sort_direction(option, cx)
                                                    })
                                                    .ok();
                                            },
                                        )
                                    },
                                )
                            }))
                        }),
                ),
        )
        .into_any_element()
}
