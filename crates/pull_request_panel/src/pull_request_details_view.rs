use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, Render,
    SharedString, Task, Window,
};
use gpui::StatefulInteractiveElement as _;
use gpui::Styled as _;
use http_client::{AsyncBody, HttpClient, HttpRequestExt, RedirectPolicy, Request, StatusCode};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use ui::{Color, Label, LabelSize, prelude::*, v_flex};
use workspace::item::Item;

use crate::{GITHUB_ACCEPT_HEADER, GITHUB_API_VERSION, GitHubRepositoryContext, PullRequestSummary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewEvent {
    UpdateTab,
}

#[derive(Clone, Debug)]
struct PullRequestDetails {
    number: u64,
    title: String,
    body: String,
    html_url: String,
    author_login: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    head_ref: String,
    base_ref: String,
    requested_reviewer_logins: Vec<String>,
}

#[derive(Clone, Debug)]
enum PullRequestDetailsViewContent {
    Loading,
    Error { message: String },
    Ready(PullRequestDetails),
}

pub(super) struct PullRequestDetailsView {
    focus_handle: FocusHandle,
    github_repository: GitHubRepositoryContext,
    pull_request: PullRequestSummary,
    content: PullRequestDetailsViewContent,
    markdown: Option<Entity<Markdown>>,
    load_task: Option<Task<()>>,
    load_generation: usize,
}

impl PullRequestDetailsView {
    pub(super) fn new(
        github_repository: GitHubRepositoryContext,
        pull_request: PullRequestSummary,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            github_repository,
            pull_request,
            content: PullRequestDetailsViewContent::Loading,
            markdown: None,
            load_task: None,
            load_generation: 0,
        };
        this.reload(cx);
        this
    }

    pub(super) fn set_pull_request(
        &mut self,
        github_repository: GitHubRepositoryContext,
        pull_request: PullRequestSummary,
        cx: &mut Context<Self>,
    ) {
        if self.github_repository.full_name == github_repository.full_name
            && self.pull_request == pull_request
        {
            return;
        }

        self.github_repository = github_repository;
        self.pull_request = pull_request;
        self.reload(cx);
        cx.emit(ViewEvent::UpdateTab);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        let repository = self.github_repository.clone();
        let pull_request = self.pull_request.clone();

        self.content = PullRequestDetailsViewContent::Loading;
        self.markdown = None;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let http_client = cx.update(|cx| cx.http_client());
            let result = fetch_pull_request_details(&repository, pull_request.number, http_client)
                .await
                .and_then(|details| {
                    Ok(PullRequestDetails {
                        number: details.number,
                        title: details.title,
                        body: details.body.unwrap_or_default(),
                        html_url: details.html_url,
                        author_login: details.user.login,
                        created_at: details.created_at,
                        updated_at: details.updated_at,
                        head_ref: details.head.reference,
                        base_ref: details.base.reference,
                        requested_reviewer_logins: details
                            .requested_reviewers
                            .into_iter()
                            .map(|reviewer| reviewer.login)
                            .collect(),
                    })
                });

            this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }

                match result {
                    Ok(details) => {
                        let markdown_source: SharedString = if details.body.trim().is_empty() {
                            "No description provided.".into()
                        } else {
                            details.body.clone().into()
                        };
                        this.markdown = Some(cx.new(|cx| Markdown::new(markdown_source, None, None, cx)));
                        this.content = PullRequestDetailsViewContent::Ready(details);
                    }
                    Err(error) => {
                        this.content =
                            PullRequestDetailsViewContent::Error { message: error.to_string() };
                    }
                }

                this.load_task.take();
                cx.notify();
            })
            .ok();
        }));

        cx.notify();
    }
}

impl EventEmitter<ViewEvent> for PullRequestDetailsView {}

impl Focusable for PullRequestDetailsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PullRequestDetailsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self
            .render_header(self.pull_request.number, &self.pull_request.title)
            .into_any_element();

        let element: AnyElement = match &self.content {
            PullRequestDetailsViewContent::Loading => render_center_message(
                header,
                "Loading pull request details…".into(),
                cx.theme().colors().editor_background,
            ),
            PullRequestDetailsViewContent::Error { message } => render_center_message(
                header,
                message.clone().into(),
                cx.theme().colors().editor_background,
            ),
            PullRequestDetailsViewContent::Ready(details) => {
                let markdown_style = MarkdownStyle::themed(MarkdownFont::Editor, window, cx);
                let requested_reviewers = if details.requested_reviewer_logins.is_empty() {
                    "Requested reviewers: none".to_string()
                } else {
                    format!(
                        "Requested reviewers: {}",
                        details.requested_reviewer_logins.join(", ")
                    )
                };

                let metadata = v_flex()
                    .gap_1()
                    .child(
                        Label::new(format!("Author: {}", details.author_login))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(details.html_url.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(
                        Label::new(format!("Created: {}", details.created_at.to_rfc3339()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("Updated: {}", details.updated_at.to_rfc3339()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("Base: {}", details.base_ref))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("Head: {}", details.head_ref))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(requested_reviewers)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    );

                v_flex()
                    .size_full()
                    .overflow_hidden()
                    .bg(cx.theme().colors().editor_background)
                    .track_focus(&self.focus_handle)
                    .child(
                        div()
                            .id("pull-request-details-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_4()
                            .gap_3()
                            .child(self.render_header(details.number, &details.title))
                            .child(metadata)
                            .children(self.markdown.clone().map(|markdown| {
                                MarkdownElement::new(markdown, markdown_style.clone())
                                    .text_size(TextSize::Small.rems(cx))
                                    .on_url_click(|link, _, cx| cx.open_url(&link))
                            })),
                    )
                    .into_any_element()
            }
        };

        element
    }
}

impl Item for PullRequestDetailsView {
    type Event = ViewEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        SharedString::from(format!("PR #{}", self.pull_request.number))
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(SharedString::from(self.github_repository.full_name.clone()))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Pull Request Details Opened")
    }
}

fn render_center_message(header: AnyElement, message: SharedString, background: Hsla) -> AnyElement {
    v_flex()
        .size_full()
        .overflow_hidden()
        .bg(background)
        .child(
            h_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .gap_2()
                        .p_4()
                        .child(header)
                        .child(
                            Label::new(message)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                ),
        )
        .into_any_element()
}

impl PullRequestDetailsView {
    fn render_header(&self, number: u64, title: &str) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(
                Label::new(format!("#{number} {title}"))
                    .size(LabelSize::Large),
            )
            .child(
                Label::new(self.github_repository.full_name.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }
}

#[derive(Debug, serde::Deserialize)]
struct GitHubPullRequestDetailsResponse {
    number: u64,
    title: String,
    body: Option<String>,
    html_url: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    user: GitHubUserResponse,
    head: GitHubPullRequestHeadResponse,
    base: GitHubPullRequestBaseResponse,
    #[serde(default)]
    requested_reviewers: Vec<GitHubUserResponse>,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubUserResponse {
    login: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubPullRequestHeadResponse {
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubPullRequestBaseResponse {
    #[serde(rename = "ref")]
    reference: String,
}

async fn fetch_pull_request_details(
    repository: &GitHubRepositoryContext,
    pull_request_number: u64,
    http_client: std::sync::Arc<dyn HttpClient>,
) -> Result<GitHubPullRequestDetailsResponse> {
    let request = github_pull_request_details_request(repository, pull_request_number, &http_client)?;
    let mut response = http_client.send(request).await?;
    let status = response.status();
    let body = super::read_response_body(response.body_mut()).await?;

    if status != StatusCode::OK {
        let body_text = String::from_utf8_lossy(&body);
        bail!("GitHub pull request details query failed with status {status}: {body_text}");
    }

    serde_json::from_slice(&body)
        .map_err(|error| anyhow!("Failed to parse GitHub pull request details response: {error}"))
}

fn github_pull_request_details_request(
    repository: &GitHubRepositoryContext,
    pull_request_number: u64,
    http_client: &std::sync::Arc<dyn HttpClient>,
) -> Result<Request<AsyncBody>> {
    if pull_request_number == 0 {
        bail!("Pull request number must be non-zero.");
    }

    Request::builder()
        .method("GET")
        .uri(format!(
            "{}/repos/{}/{}/pulls/{pull_request_number}",
            repository.api_base_url, repository.owner, repository.repo
        ))
        .header("Accept", GITHUB_ACCEPT_HEADER)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .when_some(http_client.user_agent().cloned(), |request, user_agent| {
            request.header("User-Agent", user_agent)
        })
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(AsyncBody::default())
        .context("Failed to build GitHub pull request details request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use http_client::{Url, http::HeaderValue};

    struct NoopHttpClient;

    impl HttpClient for NoopHttpClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _req: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, Result<http_client::Response<AsyncBody>>> {
            Box::pin(async { Err(anyhow!("send not implemented for NoopHttpClient")) })
        }
    }

    #[test]
    fn pr_details_request_builder_sets_url_and_headers() {
        let repository = GitHubRepositoryContext {
            owner: "zed-industries".to_string(),
            repo: "zed".to_string(),
            full_name: "zed-industries/zed".to_string(),
            api_base_url: "https://api.github.com".to_string(),
        };
        let http_client: std::sync::Arc<dyn HttpClient> = std::sync::Arc::new(NoopHttpClient);

        let request = github_pull_request_details_request(&repository, 42, &http_client)
            .expect("request should build");

        assert_eq!(
            request.uri().to_string(),
            "https://api.github.com/repos/zed-industries/zed/pulls/42"
        );
        assert_eq!(
            request.headers().get("Accept").and_then(|v| v.to_str().ok()),
            Some(GITHUB_ACCEPT_HEADER)
        );
        assert_eq!(
            request
                .headers()
                .get("X-GitHub-Api-Version")
                .and_then(|v| v.to_str().ok()),
            Some(GITHUB_API_VERSION)
        );
    }
}
