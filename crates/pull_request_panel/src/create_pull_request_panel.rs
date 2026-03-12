use editor::{Editor, EditorElement, EditorMode, EditorStyle, MultiBuffer};
use gpui::{AnyElement, Context, Corner, Entity, SharedString, Window, px};
use language::Buffer;
use settings::Settings;
use ui::{
    ButtonLike, ButtonSize, ButtonStyle, Color, ContextMenu, ContextMenuEntry, ElevationIndex,
    Icon, IconButton, IconButtonShape, IconName, IconSize, Label, LabelSize, PopoverMenu,
    SplitButton, Tooltip, h_flex, prelude::*, v_flex,
};

use crate::{PullRequestPanel, PullRequestPanelData};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedBranch {
    pub(crate) display_name: SharedString,
    pub(crate) api_ref: SharedString,
}

pub(crate) struct CreatePullRequestState {
    pub(crate) base_branch: Option<SelectedBranch>,
    pub(crate) head_branch: Option<SelectedBranch>,

    pub(crate) title_buffer: Entity<Buffer>,
    pub(crate) description_buffer: Entity<Buffer>,

    pub(crate) error_message: Option<SharedString>,
    pub(crate) create_task_in_flight: bool,
}

impl CreatePullRequestState {
    pub(crate) fn new(title_buffer: Entity<Buffer>, description_buffer: Entity<Buffer>) -> Self {
        Self {
            base_branch: None,
            head_branch: None,
            title_buffer,
            description_buffer,
            error_message: None,
            create_task_in_flight: false,
        }
    }

    pub(crate) fn clear_inputs(&mut self, cx: &mut Context<PullRequestPanel>) {
        self.title_buffer
            .update(cx, |buffer, cx| buffer.set_text("", cx));
        self.description_buffer
            .update(cx, |buffer, cx| buffer.set_text("", cx));
    }

    pub(crate) fn title_text(&self, cx: &Context<PullRequestPanel>) -> String {
        self.title_buffer.read(cx).text()
    }

    pub(crate) fn description_text(&self, cx: &Context<PullRequestPanel>) -> String {
        self.description_buffer.read(cx).text()
    }
}

pub(crate) fn render_create_pull_request_panel(
    panel: &PullRequestPanel,
    data: &PullRequestPanelData,
    window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let state = &panel.create_pull_request;

    let title_editor = window.use_keyed_state("create-pr-title-editor", cx, {
        let title_buffer = state.title_buffer.clone();
        move |window, cx| {
            let buffer = cx.new(|cx| MultiBuffer::singleton(title_buffer.clone(), cx));
            let mut editor = Editor::new(EditorMode::SingleLine, buffer, None, window, cx);
            editor.set_use_autoclose(false);
            editor.set_show_gutter(false, cx);
            editor.set_use_modal_editing(true);
            editor.set_show_wrap_guides(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.set_placeholder_text("Title", window, cx);
            editor
        }
    });

    let description_editor = window.use_keyed_state("create-pr-description-editor", cx, {
        let description_buffer = state.description_buffer.clone();
        move |window, cx| {
            let buffer = cx.new(|cx| MultiBuffer::singleton(description_buffer.clone(), cx));
            let mut editor = Editor::new(
                EditorMode::AutoHeight {
                    min_lines: 6,
                    max_lines: Some(6),
                },
                buffer,
                None,
                window,
                cx,
            );
            editor.set_use_autoclose(false);
            editor.set_show_gutter(false, cx);
            editor.set_use_modal_editing(true);
            editor.set_show_wrap_guides(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.set_placeholder_text("Description", window, cx);
            editor
        }
    });

    let base_branch = state
        .base_branch
        .as_ref()
        .map(|branch| branch.display_name.clone())
        .unwrap_or_else(|| "Select base branch".into());
    let head_branch = state
        .head_branch
        .as_ref()
        .map(|branch| branch.display_name.clone())
        .unwrap_or_else(|| "Select branch".into());

    let title = state.title_text(cx);
    let title_required_missing = title.trim().is_empty();
    let base_api_ref = state.base_branch.as_ref().map(|b| b.api_ref.as_ref());
    let head_api_ref = state.head_branch.as_ref().map(|b| b.api_ref.as_ref());
    let same_ref = base_api_ref.is_some_and(|base| Some(base) == head_api_ref);
    let has_required_branches = base_api_ref.is_some() && head_api_ref.is_some();

    let can_create = !state.create_task_in_flight
        && has_required_branches
        && !title_required_missing
        && !same_ref;

    let title_focused = title_editor.read(cx).is_focused(window);
    let description_focused = description_editor.read(cx).is_focused(window);

    v_flex()
        .id("pull-request-panel-create")
        .gap_3()
        .p_3()
        .bg(cx.theme().colors().surface_background)
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .rounded_md()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(Label::new("CREATE").size(LabelSize::Small)),
        )
        .child(render_branch_dropdown_row(
            "BASE",
            "create-pull-request-base-branch",
            base_branch,
            data,
            panel,
            window,
            cx,
            BranchRole::Base,
        ))
        .child(render_branch_dropdown_row(
            "MERGE",
            "create-pull-request-head-branch",
            head_branch,
            data,
            panel,
            window,
            cx,
            BranchRole::Head,
        ))
        .child(
            v_flex()
                .gap_1()
                .child(
                    Label::new("TITLE")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(render_editor_field(
                    "create-pr-title",
                    &title_editor,
                    title_focused,
                    window,
                    cx,
                )),
        )
        .child(
            v_flex()
                .gap_1()
                .child(
                    Label::new("DESCRIPTION")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(render_editor_field(
                    "create-pr-description",
                    &description_editor,
                    description_focused,
                    window,
                    cx,
                )),
        )
        .when_some(state.error_message.as_ref(), |this, message| {
            this.child(
                Label::new(message.clone())
                    .size(LabelSize::Small)
                    .color(Color::Error),
            )
        })
        .when(same_ref, |this| {
            this.child(
                Label::new("Base and head branches must be different.")
                    .size(LabelSize::Small)
                    .color(Color::Error),
            )
        })
        .child(render_action_row(can_create, window, cx))
        .into_any_element()
}

fn render_action_row(
    can_create: bool,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_end()
        .gap_2()
        .child(
            ButtonLike::new("create-pull-request-cancel")
                .size(ButtonSize::Compact)
                .style(ButtonStyle::Transparent)
                .child(Label::new("Cancel").size(LabelSize::Small))
                .on_click(cx.listener(|this, _, _window, cx| {
                    this.hide_create_pull_request_panel(cx);
                })),
        )
        .child({
            let create_pr = ButtonLike::new_rounded_left("create-pull-request-submit")
                .layer(ElevationIndex::ModalSurface)
                .size(ButtonSize::Compact)
                .style(ButtonStyle::Filled)
                .child(
                    div()
                        .child(Label::new("Create").size(LabelSize::Small))
                        .mr_0p5(),
                )
                .disabled(!can_create)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.create_pull_request(false, window, cx);
                }))
                .tooltip(move |_window, cx| {
                    if can_create {
                        Tooltip::simple("Create PR", cx)
                    } else {
                        Tooltip::simple("Fill in required fields to create a PR", cx)
                    }
                });

            let create_menu = PopoverMenu::new("create-pull-request-submit-menu")
                .anchor(Corner::TopRight)
                .offset(gpui::Point {
                    x: px(0.0),
                    y: px(2.0),
                })
                .trigger(
                    IconButton::new(
                        "create-pull-request-submit-menu-trigger",
                        IconName::ChevronDown,
                    )
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .style(ButtonStyle::Filled)
                    .disabled(!can_create),
                )
                .menu({
                    let panel_handle = cx.entity().downgrade();
                    move |window, cx| {
                        let panel_handle = panel_handle.clone();
                        Some(ContextMenu::build(window, cx, move |menu, _, _| {
                            let menu = menu
                                .item(ContextMenuEntry::new("Create PR").handler({
                                    let panel_handle = panel_handle.clone();
                                    move |window, cx| {
                                        panel_handle
                                            .update(cx, |panel, cx| {
                                                panel.create_pull_request(false, window, cx);
                                            })
                                            .ok();
                                    }
                                }))
                                .item(ContextMenuEntry::new("Create Draft PR").handler({
                                    let panel_handle = panel_handle.clone();
                                    move |window, cx| {
                                        panel_handle
                                            .update(cx, |panel, cx| {
                                                panel.create_pull_request(true, window, cx);
                                            })
                                            .ok();
                                    }
                                }));

                            menu
                        }))
                    }
                });

            SplitButton::new(create_pr, create_menu.into_any_element())
        })
}

fn render_editor_field(
    id: &'static str,
    editor: &Entity<Editor>,
    focused: bool,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let settings = theme::ThemeSettings::get_global(cx);
    let text_style = gpui::TextStyle {
        color: cx.theme().colors().text,
        font_family: settings.buffer_font.family.clone(),
        font_features: settings.buffer_font.features.clone(),
        font_fallbacks: settings.buffer_font.fallbacks.clone(),
        font_size: ui::rems(0.875).into(),
        font_weight: settings.buffer_font.weight,
        line_height: ui::relative(1.3),
        ..gpui::TextStyle::default()
    };

    let mut editor_style = EditorStyle {
        background: cx.theme().colors().surface_background,
        local_player: cx.theme().players().local(),
        text: text_style,
        ..EditorStyle::default()
    };
    editor_style.syntax = cx.theme().syntax().clone();

    let border_color = if focused {
        cx.theme().colors().text_accent
    } else {
        cx.theme().colors().border
    };

    div()
        .id(id)
        .w_full()
        .min_w_32()
        .px_2()
        .py_1()
        .border_1()
        .border_color(border_color)
        .rounded_md()
        .child(EditorElement::new(editor, editor_style))
        .into_any_element()
}

#[derive(Clone, Copy)]
pub(crate) enum BranchRole {
    Base,
    Head,
}

fn render_branch_dropdown_row(
    label: &'static str,
    id: &'static str,
    current: SharedString,
    data: &PullRequestPanelData,
    _panel: &PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
    role: BranchRole,
) -> AnyElement {
    let panel_handle = cx.entity().downgrade();
    let current_string = current.to_string();
    let branches = data.branches.clone();

    let trigger = ButtonLike::new(id)
        .size(ButtonSize::Compact)
        .style(ButtonStyle::OutlinedGhost)
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .child(Icon::new(IconName::Folder).size(IconSize::Small))
                .child(Label::new(current).size(LabelSize::Small))
                .child(div().flex_1())
                .child(Icon::new(IconName::ChevronDown).size(IconSize::Small)),
        );

    v_flex()
        .gap_1()
        .child(
            Label::new(label)
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
        .child(
            PopoverMenu::new(format!("{id}-menu"))
                .anchor(Corner::TopLeft)
                .offset(gpui::Point {
                    x: px(0.0),
                    y: px(2.0),
                })
                .trigger(trigger)
                .menu(move |window, cx| {
                    let panel_handle = panel_handle.clone();
                    let branches = branches.clone();
                    let current_string = current_string.clone();
                    Some(ContextMenu::build(window, cx, move |menu, _, _| {
                        branches.iter().fold(menu, |menu, branch| {
                            let api_ref = crate::branch_api_ref(branch);
                            let display = branch.name().to_string();
                            let selected = display == current_string;
                            let panel_handle = panel_handle.clone();

                            menu.item(
                                ContextMenuEntry::new(display.clone())
                                    .icon(IconName::Folder)
                                    .toggleable(ui::IconPosition::End, selected)
                                    .handler(move |window, cx| {
                                        let selected = SelectedBranch {
                                            display_name: display.clone().into(),
                                            api_ref: api_ref.clone().into(),
                                        };
                                        panel_handle
                                            .update(cx, |panel, cx| {
                                                panel.set_selected_branch(
                                                    role, selected, window, cx,
                                                );
                                            })
                                            .ok();
                                    }),
                            )
                        })
                    }))
                }),
        )
        .into_any_element()
}

pub(crate) fn initialize_create_panel_defaults(
    state: &mut CreatePullRequestState,
    data: &PullRequestPanelData,
    cx: &mut Context<PullRequestPanel>,
) {
    state.clear_inputs(cx);

    let head_default = data
        .branches
        .iter()
        .find(|branch| branch.is_head)
        .and_then(|branch| {
            branch
                .upstream
                .as_ref()
                .and_then(|upstream| upstream.branch_name())
                .map(|name| SelectedBranch {
                    display_name: name.to_string().into(),
                    api_ref: name.to_string().into(),
                })
        })
        .or_else(|| {
            data.branches
                .iter()
                .find(|branch| branch.is_head && !branch.is_remote())
                .map(|branch| SelectedBranch {
                    display_name: branch.name().to_string().into(),
                    api_ref: crate::branch_api_ref(branch).into(),
                })
        });

    let base_default = find_preferred_base_branch(&data.branches).or_else(|| {
        data.branches
            .iter()
            .find(|branch| !branch.is_head)
            .map(|branch| SelectedBranch {
                display_name: branch.name().to_string().into(),
                api_ref: crate::branch_api_ref(branch).into(),
            })
    });

    state.head_branch = head_default;
    state.base_branch = base_default;
    state.error_message.take();
    state.create_task_in_flight = false;
    cx.notify();
}

fn find_preferred_base_branch(branches: &[git::repository::Branch]) -> Option<SelectedBranch> {
    for preferred in ["main", "master"] {
        if let Some(branch) = branches.iter().find(|branch| {
            let api_ref = crate::branch_api_ref(branch);
            api_ref == preferred
        }) {
            return Some(SelectedBranch {
                display_name: branch.name().to_string().into(),
                api_ref: preferred.into(),
            });
        }
    }
    None
}
