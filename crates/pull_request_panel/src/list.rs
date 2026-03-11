use chrono::{DateTime, Utc};
use gpui::{AnyElement, Context, StatefulInteractiveElement, Window};
use ui::{
    Button, ButtonCommon, ButtonStyle, Color, Label, LabelSize, ListHeader, ListItem, prelude::*,
    v_flex,
};

use crate::{
    CategorizedPullRequests, PullRequestPanel, PullRequestPanelContent, PullRequestPanelData,
    PullRequestSummary,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum PullRequestSection {
    CopilotOnMyBehalf,
    LocalPullRequestBranches,
    WaitingForMyReview,
    CreatedByMe,
    AllOpen,
}

impl PullRequestSection {
    pub(super) fn ordered() -> [Self; 5] {
        [
            Self::CopilotOnMyBehalf,
            Self::LocalPullRequestBranches,
            Self::WaitingForMyReview,
            Self::CreatedByMe,
            Self::AllOpen,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::CopilotOnMyBehalf => "copilot-on-my-behalf",
            Self::LocalPullRequestBranches => "local-pull-request-branches",
            Self::WaitingForMyReview => "waiting-for-my-review",
            Self::CreatedByMe => "created-by-me",
            Self::AllOpen => "all-open",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::CopilotOnMyBehalf => "Copilot on My Behalf",
            Self::LocalPullRequestBranches => "Local Pull Request Branches",
            Self::WaitingForMyReview => "Waiting For My Review",
            Self::CreatedByMe => "Created By Me",
            Self::AllOpen => "All Open",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::CopilotOnMyBehalf => "No Copilot-created pull requests.",
            Self::LocalPullRequestBranches => "No local pull request branches.",
            Self::WaitingForMyReview => "No pull requests waiting for your review.",
            Self::CreatedByMe => "No pull requests created by you.",
            Self::AllOpen => "No open pull requests.",
        }
    }

    fn pull_requests<'a>(self, sections: &'a CategorizedPullRequests) -> &'a [PullRequestSummary] {
        match self {
            Self::CopilotOnMyBehalf => &sections.copilot_on_my_behalf,
            Self::LocalPullRequestBranches => &sections.local_pull_request_branches,
            Self::WaitingForMyReview => &sections.waiting_for_my_review,
            Self::CreatedByMe => &sections.created_by_me,
            Self::AllOpen => &sections.all_open,
        }
    }
}

pub(super) fn render_content(
    panel: &PullRequestPanel,
    _window: &mut Window,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    match &panel.view_state.content {
        PullRequestPanelContent::Loading => render_loading_state(),
        PullRequestPanelContent::Empty { message } => render_message_state(message),
        PullRequestPanelContent::Error { message } => render_message_state(message),
        PullRequestPanelContent::Ready(data) => render_ready_state(panel, data, cx),
    }
}

fn render_loading_state() -> AnyElement {
    v_flex()
        .flex_1()
        .justify_center()
        .items_center()
        .gap_1()
        .p_3()
        .child(
            Label::new("Loading live pull requests…")
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_message_state(message: &str) -> AnyElement {
    v_flex()
        .flex_1()
        .items_start()
        .gap_1()
        .p_3()
        .child(
            Label::new(message.to_string())
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_ready_state(
    panel: &PullRequestPanel,
    data: &PullRequestPanelData,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let visible_sections = data.visible_sections(&panel.visible_pull_request_counts);
    let visible_pull_request_count = visible_sections.all_open.len();
    let total_pull_request_count = data.total_pull_request_count();

    v_flex()
        .flex_1()
        .overflow_hidden()
        .gap_2()
        .p_3()
        .child(Label::new(data.repository.full_name.clone()))
        .child(
            Label::new(format!(
                "{visible_pull_request_count} of {total_pull_request_count} open pull requests shown.",
            ))
            .size(LabelSize::Small)
            .color(Color::Muted),
        )
        .child(
            div()
                .id("pull-request-panel-list-scroll")
                .flex_1()
                .overflow_y_scroll()
                .child(
                v_flex().w_full().gap_1().children(
                    PullRequestSection::ordered()
                        .into_iter()
                        .map(|section| {
                            render_section(panel, section, &data.sections, &visible_sections, cx)
                        }),
                ),
            ),
        )
        .into_any_element()
}

fn render_section(
    panel: &PullRequestPanel,
    section: PullRequestSection,
    all_sections: &CategorizedPullRequests,
    visible_sections: &CategorizedPullRequests,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    let is_collapsed = panel.collapsed_sections.contains(&section);
    let pull_requests = section.pull_requests(visible_sections);
    let should_render_load_more_button = should_render_section_load_more_button(
        section,
        is_collapsed,
        visible_sections,
        all_sections,
    );

    v_flex()
        .id(format!("pull-request-section-{}", section.id()))
        .w_full()
        .gap_1()
        .child(
            ListHeader::new(section.title())
                .toggle(Some(!is_collapsed))
                .on_toggle(cx.listener(move |this, _, _, cx| {
                    this.toggle_section_expanded(section, cx);
                }))
                .end_slot(section_count_label(pull_requests.len()))
                .inset(true),
        )
        .when(!is_collapsed, |section_list| {
            section_list
                .children(if pull_requests.is_empty() {
                    vec![render_empty_row(section)]
                } else {
                    pull_requests
                        .iter()
                        .map(|pull_request| render_pull_request_row(section, pull_request))
                        .collect()
                })
                .when(should_render_load_more_button, |section_list| {
                    section_list.child(render_load_more_button(section, cx))
                })
        })
        .into_any_element()
}

fn should_render_section_load_more_button(
    section: PullRequestSection,
    is_collapsed: bool,
    visible_sections: &CategorizedPullRequests,
    all_sections: &CategorizedPullRequests,
) -> bool {
    !is_collapsed
        && section.pull_requests(visible_sections).len() < section.pull_requests(all_sections).len()
}

fn render_load_more_button(
    section: PullRequestSection,
    cx: &mut Context<PullRequestPanel>,
) -> AnyElement {
    div()
        .child(
            Button::new(
                format!("pull-request-load-more-{}", section.id()),
                "Load more",
            )
            .style(ButtonStyle::Subtle)
            .label_size(LabelSize::Small)
            .full_width()
            .on_click(cx.listener(move |this, _, _, cx| this.load_more_pull_requests(section, cx))),
        )
        .into_any_element()
}

fn render_empty_row(section: PullRequestSection) -> AnyElement {
    ListItem::new(format!("{}-empty", section.id()))
        .selectable(false)
        .inset(true)
        .indent_level(1)
        .child(
            Label::new(section.empty_message())
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
        .into_any_element()
}

fn render_pull_request_row(
    section: PullRequestSection,
    pull_request: &PullRequestSummary,
) -> AnyElement {
    ListItem::new(format!("{}-pr-{}", section.id(), pull_request.number))
        .inset(true)
        .indent_level(1)
        .child(
            v_flex()
                .w_full()
                .overflow_hidden()
                .gap_1()
                .child(
                    Label::new(format!("#{} {}", pull_request.number, pull_request.title))
                        .size(LabelSize::Small)
                        .truncate(),
                )
                .child(
                    Label::new(format!(
                        "{} · {} · updated {}",
                        pull_request.author_login,
                        pull_request.head_ref,
                        format_updated_at(&pull_request.updated_at)
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate(),
                ),
        )
        .into_any_element()
}

fn format_updated_at(updated_at: &DateTime<Utc>) -> String {
    updated_at.format("%Y-%m-%d").to_string()
}

fn section_count_label(count: usize) -> Label {
    Label::new(count.to_string())
        .size(LabelSize::Small)
        .color(Color::Muted)
}

#[cfg(test)]
mod tests {
    use super::{PullRequestSection, should_render_section_load_more_button};
    use crate::{CategorizedPullRequests, PullRequestSummary};

    fn pull_request(number: u64) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: format!("PR {number}"),
            html_url: format!("https://example.com/pull/{number}"),
            author_login: "octocat".to_string(),
            head_ref: format!("branch-{number}"),
            requested_reviewer_logins: Vec::new(),
            updated_at: "2026-03-10T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn ordered_sections_match_requested_screenshot_order() {
        let section_titles = PullRequestSection::ordered()
            .into_iter()
            .map(PullRequestSection::title)
            .collect::<Vec<_>>();

        assert_eq!(
            section_titles,
            vec![
                "Copilot on My Behalf",
                "Local Pull Request Branches",
                "Waiting For My Review",
                "Created By Me",
                "All Open",
            ]
        );
    }

    #[test]
    fn section_load_more_button_renders_when_section_has_hidden_pull_requests() {
        let all_sections = CategorizedPullRequests {
            created_by_me: vec![pull_request(1), pull_request(2)],
            ..Default::default()
        };
        let visible_sections = CategorizedPullRequests {
            created_by_me: vec![pull_request(1)],
            ..Default::default()
        };

        assert!(should_render_section_load_more_button(
            PullRequestSection::CreatedByMe,
            false,
            &visible_sections,
            &all_sections,
        ));
    }

    #[test]
    fn section_load_more_button_is_hidden_when_section_is_collapsed_or_fully_visible() {
        let all_sections = CategorizedPullRequests {
            all_open: vec![pull_request(1), pull_request(2)],
            ..Default::default()
        };
        let visible_sections = CategorizedPullRequests {
            all_open: vec![pull_request(1)],
            ..Default::default()
        };

        assert!(!should_render_section_load_more_button(
            PullRequestSection::AllOpen,
            true,
            &visible_sections,
            &all_sections,
        ));
        assert!(!should_render_section_load_more_button(
            PullRequestSection::AllOpen,
            false,
            &all_sections,
            &all_sections,
        ));
    }
}
