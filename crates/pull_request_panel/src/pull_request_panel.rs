use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use futures::AsyncReadExt;
use git::{GitHostingProviderRegistry, parse_git_remote_url};
use git_ui::resolve_active_repository;
use gpui::{
    Action, App, AppContext, AsyncWindowContext, BorrowAppContext, Context, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, IntoElement, Pixels, Render, Subscription, Task,
    WeakEntity, Window, actions, px,
};
use http_client::{AsyncBody, HttpClient, HttpRequestExt, RedirectPolicy, Request, StatusCode};
use project::git_store::{GitStoreEvent, Repository, RepositoryEvent};
use settings::SettingsStore;
use std::{collections::HashSet, sync::Arc};
use ui::{IconName, prelude::*, v_flex};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

mod list;
mod top_bar;

const PULL_REQUEST_PANEL_KEY: &str = "PullRequestPanel";
const DEFAULT_PULL_REQUEST_PANEL_WIDTH: Pixels = px(320.);
const GITHUB_ACCEPT_HEADER: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";
const COPILOT_AUTHOR_LOGINS: &[&str] = &["copilot-swe-agent", "github-copilot[bot]"];
const INITIAL_VISIBLE_PULL_REQUEST_COUNT: usize = 20;
const LOAD_MORE_PULL_REQUEST_COUNT: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PullRequestSort {
    Created,
    Updated,
    Popularity,
    LongRunning,
}

impl PullRequestSort {
    fn ordered() -> [Self; 4] {
        [
            Self::Created,
            Self::Updated,
            Self::Popularity,
            Self::LongRunning,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Popularity => "popularity",
            Self::LongRunning => "long-running",
        }
    }

    fn github_value(self) -> &'static str {
        self.label()
    }
}

impl Default for PullRequestSort {
    fn default() -> Self {
        Self::Created
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PullRequestSortDirection {
    Asc,
    Desc,
}

impl PullRequestSortDirection {
    fn ordered() -> [Self; 2] {
        [Self::Asc, Self::Desc]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }

    fn github_value(self) -> &'static str {
        self.label()
    }
}

impl Default for PullRequestSortDirection {
    fn default() -> Self {
        Self::Desc
    }
}

actions!(
    pull_request_panel,
    [
        /// Toggles the pull request panel.
        Toggle,
        /// Toggles focus on the pull request panel.
        ToggleFocus,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<PullRequestPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            if !workspace.toggle_panel_focus::<PullRequestPanel>(window, cx) {
                workspace.close_panel::<PullRequestPanel>(window, cx);
            }
        });
    })
    .detach();
}

pub struct PullRequestPanel {
    position: DockPosition,
    width: Pixels,
    zoomed: bool,
    active: bool,
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    _subscriptions: Vec<Subscription>,
    load_task: Option<Task<()>>,
    load_generation: usize,
    active_repository_id: Option<EntityId>,
    visible_pull_request_count: usize,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
    view_state: PullRequestPanelViewState,
    collapsed_sections: HashSet<list::PullRequestSection>,
}

impl PullRequestPanel {
    fn default_collapsed_sections() -> HashSet<list::PullRequestSection> {
        list::PullRequestSection::ordered()
            .into_iter()
            .filter(|section| *section != list::PullRequestSection::AllOpen)
            .collect()
    }

    fn collapse_all_sections_set() -> HashSet<list::PullRequestSection> {
        list::PullRequestSection::ordered().into_iter().collect()
    }

    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let workspace_handle = workspace.weak_handle();
        let workspace_entity = workspace_handle
            .upgrade()
            .expect("workspace should exist while creating pull request panel");
        let git_store = workspace.project().read(cx).git_store().clone();
        let user_store = workspace.user_store();

        let panel = cx.new(|cx| {
            let mut subscriptions = Vec::new();
            subscriptions.push(cx.observe(
                &workspace_entity,
                |this: &mut PullRequestPanel, workspace, cx| {
                    this.reload_if_active_repository_changed(workspace, cx);
                },
            ));
            subscriptions.push(cx.subscribe(
                &git_store,
                |this: &mut PullRequestPanel, _, event: &GitStoreEvent, cx| match event {
                    GitStoreEvent::ActiveRepositoryChanged(_)
                    | GitStoreEvent::RepositoryAdded
                    | GitStoreEvent::RepositoryRemoved(_) => this.reload(cx),
                    GitStoreEvent::RepositoryUpdated(_, RepositoryEvent::BranchChanged, true) => {
                        this.reload(cx)
                    }
                    _ => {}
                },
            ));
            subscriptions.push(cx.subscribe(
                &user_store,
                |this: &mut PullRequestPanel, _, _: &client::user::Event, cx| this.reload(cx),
            ));

            Self {
                position: DockPosition::Left,
                width: DEFAULT_PULL_REQUEST_PANEL_WIDTH,
                zoomed: false,
                active: false,
                focus_handle: cx.focus_handle(),
                workspace: workspace_handle,
                _subscriptions: subscriptions,
                load_task: None,
                load_generation: 0,
                active_repository_id: None,
                visible_pull_request_count: INITIAL_VISIBLE_PULL_REQUEST_COUNT,
                sort: PullRequestSort::default(),
                sort_direction: PullRequestSortDirection::default(),
                view_state: PullRequestPanelViewState::loading(),
                collapsed_sections: Self::default_collapsed_sections(),
            }
        });

        let weak_panel = panel.downgrade();
        window.defer(cx, move |_window, cx| {
            weak_panel.update(cx, |panel, cx| panel.reload(cx)).ok();
        });
        panel
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            Self::new(workspace, window, cx)
        })
    }

    fn reload_if_active_repository_changed(
        &mut self,
        workspace: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) {
        let active_repository_id = active_repository_id_for_workspace(&workspace.read(cx), cx);
        if self.active_repository_id != active_repository_id {
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.visible_pull_request_count = INITIAL_VISIBLE_PULL_REQUEST_COUNT;

        let Some(workspace) = self.workspace.upgrade() else {
            self.active_repository_id = None;
            self.load_task = None;
            self.view_state = PullRequestPanelViewState::empty("Workspace unavailable.", false);
            cx.notify();
            return;
        };

        match prepare_load_context(&workspace.read(cx), self.sort, self.sort_direction, cx) {
            PreparedLoadContext::Unavailable {
                active_repository_id,
                message,
                refresh_ready,
            } => {
                self.active_repository_id = active_repository_id;
                self.load_task = None;
                self.view_state = PullRequestPanelViewState::empty(message, refresh_ready);
                cx.notify();
            }
            PreparedLoadContext::Loadable(load_context) => {
                self.active_repository_id = Some(load_context.repository.entity_id());
                self.load_generation += 1;
                let generation = self.load_generation;
                self.view_state = PullRequestPanelViewState::loading();
                cx.notify();

                self.load_task = Some(cx.spawn(async move |this, cx| {
                    let result = load_pull_request_panel_data(load_context, cx).await;
                    this.update(cx, |this, cx| {
                        if this.load_generation != generation {
                            return;
                        }

                        this.view_state = match result {
                            Ok(data) => PullRequestPanelViewState::ready(data),
                            Err(error) => PullRequestPanelViewState::error(error.to_string(), true),
                        };

                        this.load_task.take();
                        cx.notify();
                    })
                    .ok();
                }));
            }
        }
    }

    fn toggle_section_expanded(
        &mut self,
        section: list::PullRequestSection,
        cx: &mut Context<Self>,
    ) {
        if !self.collapsed_sections.remove(&section) {
            self.collapsed_sections.insert(section);
        }

        cx.notify();
    }

    fn collapse_all_sections(&mut self, cx: &mut Context<Self>) {
        self.collapsed_sections = Self::collapse_all_sections_set();
        cx.notify();
    }

    fn set_sort(&mut self, sort: PullRequestSort, cx: &mut Context<Self>) {
        if self.sort == sort {
            return;
        }

        self.sort = sort;
        self.reload(cx);
    }

    fn set_sort_direction(
        &mut self,
        sort_direction: PullRequestSortDirection,
        cx: &mut Context<Self>,
    ) {
        if self.sort_direction == sort_direction {
            return;
        }

        self.sort_direction = sort_direction;
        self.reload(cx);
    }

    fn can_collapse_all_sections(&self) -> bool {
        matches!(&self.view_state.content, PullRequestPanelContent::Ready(_))
            && self.collapsed_sections.len() < list::PullRequestSection::ordered().len()
    }

    fn load_more_pull_requests(&mut self, cx: &mut Context<Self>) {
        let PullRequestPanelContent::Ready(data) = &self.view_state.content else {
            return;
        };

        self.visible_pull_request_count = self
            .visible_pull_request_count
            .saturating_add(LOAD_MORE_PULL_REQUEST_COUNT)
            .min(data.total_pull_request_count());
        cx.notify();
    }

    fn can_load_more_pull_requests(&self) -> bool {
        match &self.view_state.content {
            PullRequestPanelContent::Ready(data) => has_hidden_pull_requests(
                self.visible_pull_request_count,
                data.total_pull_request_count(),
            ),
            _ => false,
        }
    }
}

fn has_hidden_pull_requests(
    visible_pull_request_count: usize,
    total_pull_request_count: usize,
) -> bool {
    visible_pull_request_count < total_pull_request_count
}

impl EventEmitter<PanelEvent> for PullRequestPanel {}

impl Focusable for PullRequestPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PullRequestPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("pull-request-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .child(top_bar::render_top_bar(self, window, cx))
            .child(list::render_content(self, window, cx))
    }
}

impl Panel for PullRequestPanel {
    fn persistent_name() -> &'static str {
        "Pull request"
    }

    fn panel_key() -> &'static str {
        PULL_REQUEST_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(
        &mut self,
        position: DockPosition,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        cx.notify();
        cx.update_global::<SettingsStore, _>(|_, _| {});
    }

    fn size(&self, _window: &Window, _cx: &App) -> Pixels {
        self.width
    }

    fn set_size(&mut self, size: Option<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        self.width = size.unwrap_or(DEFAULT_PULL_REQUEST_PANEL_WIDTH);
        cx.notify();
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::PullRequest)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Pull request")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        self.active
    }

    fn is_zoomed(&self, _window: &Window, _cx: &App) -> bool {
        self.zoomed
    }

    fn set_zoomed(&mut self, zoomed: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoomed = zoomed;
        cx.notify();
    }

    fn set_active(&mut self, active: bool, _window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

#[derive(Clone, Debug)]
struct PullRequestPanelViewState {
    content: PullRequestPanelContent,
    refresh_ready: bool,
}

impl PullRequestPanelViewState {
    fn loading() -> Self {
        Self {
            content: PullRequestPanelContent::Loading,
            refresh_ready: false,
        }
    }

    fn empty(message: impl Into<String>, refresh_ready: bool) -> Self {
        Self {
            content: PullRequestPanelContent::Empty {
                message: message.into(),
            },
            refresh_ready,
        }
    }

    fn error(message: impl Into<String>, refresh_ready: bool) -> Self {
        Self {
            content: PullRequestPanelContent::Error {
                message: message.into(),
            },
            refresh_ready,
        }
    }

    fn ready(data: PullRequestPanelData) -> Self {
        Self {
            content: PullRequestPanelContent::Ready(data),
            refresh_ready: true,
        }
    }
}

#[derive(Clone, Debug)]
enum PullRequestPanelContent {
    Loading,
    Empty { message: String },
    Error { message: String },
    Ready(PullRequestPanelData),
}

#[derive(Clone, Debug)]
struct PullRequestPanelData {
    repository: GitHubRepositoryContext,
    sections: CategorizedPullRequests,
}

impl PullRequestPanelData {
    fn total_pull_request_count(&self) -> usize {
        self.sections.all_open.len()
    }

    fn visible_sections(&self, visible_pull_request_count: usize) -> CategorizedPullRequests {
        self.sections.limit(visible_pull_request_count)
    }
}

#[derive(Clone, Debug)]
struct GitHubRepositoryContext {
    owner: String,
    repo: String,
    full_name: String,
    api_base_url: String,
}

#[derive(Clone)]
struct PullRequestLoadContext {
    repository: Entity<Repository>,
    github_repository: GitHubRepositoryContext,
    current_user_login: Option<String>,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
}

enum PreparedLoadContext {
    Unavailable {
        active_repository_id: Option<EntityId>,
        message: String,
        refresh_ready: bool,
    },
    Loadable(PullRequestLoadContext),
}

#[derive(Clone, Debug, Default)]
struct CategorizedPullRequests {
    copilot_on_my_behalf: Vec<PullRequestSummary>,
    local_pull_request_branches: Vec<PullRequestSummary>,
    waiting_for_my_review: Vec<PullRequestSummary>,
    created_by_me: Vec<PullRequestSummary>,
    all_open: Vec<PullRequestSummary>,
}

impl CategorizedPullRequests {
    fn limit(&self, visible_pull_request_count: usize) -> Self {
        if visible_pull_request_count >= self.all_open.len() {
            return self.clone();
        }

        let visible_pull_request_numbers = self
            .all_open
            .iter()
            .take(visible_pull_request_count)
            .map(|pull_request| pull_request.number)
            .collect::<HashSet<_>>();

        Self {
            copilot_on_my_behalf: limit_pull_request_summaries(
                &self.copilot_on_my_behalf,
                &visible_pull_request_numbers,
            ),
            local_pull_request_branches: limit_pull_request_summaries(
                &self.local_pull_request_branches,
                &visible_pull_request_numbers,
            ),
            waiting_for_my_review: limit_pull_request_summaries(
                &self.waiting_for_my_review,
                &visible_pull_request_numbers,
            ),
            created_by_me: limit_pull_request_summaries(
                &self.created_by_me,
                &visible_pull_request_numbers,
            ),
            all_open: self
                .all_open
                .iter()
                .take(visible_pull_request_count)
                .cloned()
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PullRequestSummary {
    number: u64,
    title: String,
    html_url: String,
    author_login: String,
    head_ref: String,
    requested_reviewer_logins: Vec<String>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubPullRequestResponse {
    number: u64,
    title: String,
    html_url: String,
    updated_at: DateTime<Utc>,
    user: GitHubUserResponse,
    head: GitHubPullRequestHeadResponse,
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

fn active_repository_id_for_workspace(workspace: &Workspace, cx: &App) -> Option<EntityId> {
    resolve_active_repository(workspace, cx).map(|repository| repository.entity_id())
}

fn prepare_load_context(
    workspace: &Workspace,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
    cx: &App,
) -> PreparedLoadContext {
    let Some(repository) = resolve_active_repository(workspace, cx) else {
        return PreparedLoadContext::Unavailable {
            active_repository_id: None,
            message: "Open a GitHub repository to load pull requests.".to_string(),
            refresh_ready: false,
        };
    };

    let active_repository_id = Some(repository.entity_id());
    let Some(remote_url) = repository.read(cx).default_remote_url() else {
        return PreparedLoadContext::Unavailable {
            active_repository_id,
            message: "No git remote found for the active repository.".to_string(),
            refresh_ready: false,
        };
    };

    let Some(provider_registry) = GitHostingProviderRegistry::try_global(cx) else {
        return PreparedLoadContext::Unavailable {
            active_repository_id,
            message: "Git hosting providers are unavailable.".to_string(),
            refresh_ready: false,
        };
    };

    let Some((provider, parsed_remote)) = parse_git_remote_url(provider_registry, &remote_url)
    else {
        return PreparedLoadContext::Unavailable {
            active_repository_id,
            message: "Unable to resolve the active repository's remote.".to_string(),
            refresh_ready: false,
        };
    };

    if !provider.name().contains("GitHub") {
        return PreparedLoadContext::Unavailable {
            active_repository_id,
            message: "The pull request panel currently supports GitHub remotes only.".to_string(),
            refresh_ready: false,
        };
    }

    let owner = parsed_remote.owner.to_string();
    let repo = parsed_remote.repo.to_string();
    let full_name = format!("{owner}/{repo}");
    let api_base_url = match github_api_base_url(&provider.base_url()) {
        Ok(api_base_url) => api_base_url,
        Err(error) => {
            return PreparedLoadContext::Unavailable {
                active_repository_id,
                message: error.to_string(),
                refresh_ready: false,
            };
        }
    };

    PreparedLoadContext::Loadable(PullRequestLoadContext {
        repository,
        github_repository: GitHubRepositoryContext {
            owner,
            repo,
            full_name,
            api_base_url,
        },
        current_user_login: workspace
            .user_store()
            .read(cx)
            .current_user()
            .map(|user| user.github_login.to_string()),
        sort,
        sort_direction,
    })
}

fn github_api_base_url(base_url: &http_client::Url) -> Result<String> {
    let host = base_url
        .host_str()
        .context("GitHub provider is missing a host.")?;
    let api_host = if host == "github.com" {
        "api.github.com".to_string()
    } else {
        format!("api.{host}")
    };
    Ok(format!("https://{api_host}"))
}

async fn load_pull_request_panel_data(
    load_context: PullRequestLoadContext,
    cx: &mut gpui::AsyncApp,
) -> Result<PullRequestPanelData> {
    let http_client = cx.update(|cx| cx.http_client());
    let pull_requests = fetch_pull_requests(
        &load_context.github_repository,
        load_context.sort,
        load_context.sort_direction,
        http_client,
    )
    .await?;
    let branches: Vec<git::repository::Branch> = load_context
        .repository
        .update(cx, |repository: &mut Repository, _| repository.branches())
        .await??;
    let local_branch_names = branches
        .into_iter()
        .filter(|branch| !branch.is_remote())
        .map(|branch| branch.name().to_string())
        .collect::<HashSet<_>>();

    Ok(PullRequestPanelData {
        repository: load_context.github_repository,
        sections: categorize_pull_requests(
            pull_requests
                .into_iter()
                .map(PullRequestSummary::from)
                .collect(),
            load_context.current_user_login.as_deref(),
            &local_branch_names,
        ),
    })
}

async fn fetch_pull_requests(
    repository: &GitHubRepositoryContext,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
    http_client: Arc<dyn HttpClient>,
) -> Result<Vec<GitHubPullRequestResponse>> {
    let mut page = 1;
    let mut pull_requests = Vec::new();

    loop {
        let request =
            github_pull_request_request(repository, sort, sort_direction, &http_client, page)?;
        let mut response = http_client.send(request).await?;
        let status = response.status();
        let has_next_page = response_has_next_page(&response)?;
        let body = read_response_body(response.body_mut()).await?;

        if status != StatusCode::OK {
            let body_text = String::from_utf8_lossy(&body);
            bail!("GitHub pull request query failed with status {status}: {body_text}");
        }

        let page_pull_requests: Vec<GitHubPullRequestResponse> = serde_json::from_slice(&body)
            .map_err(|error| anyhow!("Failed to parse GitHub pull request response: {error}"))?;
        pull_requests.extend(page_pull_requests);

        if !has_next_page {
            break;
        }

        page += 1;
    }

    Ok(pull_requests)
}

fn github_pull_request_request(
    repository: &GitHubRepositoryContext,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
    http_client: &Arc<dyn HttpClient>,
    page: usize,
) -> Result<Request<AsyncBody>> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "{}/repos/{}/{}/pulls?state=open&sort={}&direction={}&per_page=100&page={page}",
            repository.api_base_url,
            repository.owner,
            repository.repo,
            sort.github_value(),
            sort_direction.github_value(),
        ))
        .header("Accept", GITHUB_ACCEPT_HEADER)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .when_some(http_client.user_agent().cloned(), |request, user_agent| {
            request.header("User-Agent", user_agent)
        })
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(AsyncBody::default())
        .context("Failed to build GitHub pull request request")
}

fn response_has_next_page(response: &http_client::Response<AsyncBody>) -> Result<bool> {
    Ok(response
        .headers()
        .get("link")
        .map(|link| link.to_str())
        .transpose()?
        .is_some_and(|link| link.contains("rel=\"next\"")))
}

async fn read_response_body(body: &mut AsyncBody) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

fn categorize_pull_requests(
    pull_requests: Vec<PullRequestSummary>,
    current_user_login: Option<&str>,
    local_branch_names: &HashSet<String>,
) -> CategorizedPullRequests {
    let mut sections = CategorizedPullRequests {
        all_open: pull_requests.clone(),
        ..Default::default()
    };

    for pull_request in &pull_requests {
        if COPILOT_AUTHOR_LOGINS.contains(&pull_request.author_login.as_str()) {
            sections.copilot_on_my_behalf.push(pull_request.clone());
        }

        if local_branch_names.contains(&pull_request.head_ref) {
            sections
                .local_pull_request_branches
                .push(pull_request.clone());
        }

        if let Some(current_user_login) = current_user_login {
            if pull_request.author_login == current_user_login {
                sections.created_by_me.push(pull_request.clone());
            }

            if pull_request
                .requested_reviewer_logins
                .iter()
                .any(|login| login == current_user_login)
            {
                sections.waiting_for_my_review.push(pull_request.clone());
            }
        }
    }

    sections
}

fn limit_pull_request_summaries(
    pull_requests: &[PullRequestSummary],
    visible_pull_request_numbers: &HashSet<u64>,
) -> Vec<PullRequestSummary> {
    pull_requests
        .iter()
        .filter(|pull_request| visible_pull_request_numbers.contains(&pull_request.number))
        .cloned()
        .collect()
}

impl From<GitHubPullRequestResponse> for PullRequestSummary {
    fn from(value: GitHubPullRequestResponse) -> Self {
        Self {
            number: value.number,
            title: value.title,
            html_url: value.html_url,
            author_login: value.user.login,
            head_ref: value.head.reference,
            requested_reviewer_logins: value
                .requested_reviewers
                .into_iter()
                .map(|reviewer| reviewer.login)
                .collect(),
            updated_at: value.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull_request(
        number: u64,
        author_login: &str,
        head_ref: &str,
        requested_reviewer_logins: &[&str],
        updated_at: &str,
    ) -> PullRequestSummary {
        PullRequestSummary {
            number,
            title: format!("PR {number}"),
            html_url: format!("https://example.com/pull/{number}"),
            author_login: author_login.to_string(),
            head_ref: head_ref.to_string(),
            requested_reviewer_logins: requested_reviewer_logins
                .iter()
                .map(|login| (*login).to_string())
                .collect(),
            updated_at: updated_at.parse().unwrap(),
        }
    }

    #[test]
    fn pull_request_sort_defaults_and_options_match_spec() {
        assert_eq!(PullRequestSort::default(), PullRequestSort::Created);
        assert_eq!(
            PullRequestSortDirection::default(),
            PullRequestSortDirection::Desc
        );
        assert_eq!(
            PullRequestSort::ordered()
                .into_iter()
                .map(PullRequestSort::label)
                .collect::<Vec<_>>(),
            vec!["created", "updated", "popularity", "long-running"]
        );
        assert_eq!(
            PullRequestSortDirection::ordered()
                .into_iter()
                .map(PullRequestSortDirection::label)
                .collect::<Vec<_>>(),
            vec!["asc", "desc"]
        );
    }

    #[test]
    fn github_pull_request_request_uses_selected_sort_and_direction() {
        let repository = GitHubRepositoryContext {
            owner: "zed-industries".to_string(),
            repo: "zed".to_string(),
            full_name: "zed-industries/zed".to_string(),
            api_base_url: "https://api.github.com".to_string(),
        };
        let http_client: Arc<dyn HttpClient> = Arc::new(http_client::BlockedHttpClient::new());

        let request = github_pull_request_request(
            &repository,
            PullRequestSort::Popularity,
            PullRequestSortDirection::Asc,
            &http_client,
            3,
        )
        .unwrap();

        assert_eq!(
            request.uri().path_and_query().unwrap().as_str(),
            "/repos/zed-industries/zed/pulls?state=open&sort=popularity&direction=asc&per_page=100&page=3"
        );
    }

    #[test]
    fn categorize_pull_requests_populates_expected_sections() {
        let pull_requests = vec![
            pull_request(
                2,
                "abeldebruijn",
                "user-branch",
                &[],
                "2026-03-10T12:00:00Z",
            ),
            pull_request(
                1,
                "copilot-swe-agent",
                "local-feature",
                &["abeldebruijn"],
                "2026-03-09T12:00:00Z",
            ),
            pull_request(
                3,
                "someone-else",
                "remote-only",
                &["abeldebruijn"],
                "2026-03-08T12:00:00Z",
            ),
        ];
        let local_branch_names =
            HashSet::from(["local-feature".to_string(), "user-branch".to_string()]);

        let sections =
            categorize_pull_requests(pull_requests, Some("abeldebruijn"), &local_branch_names);

        assert_eq!(
            sections
                .copilot_on_my_behalf
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            sections
                .local_pull_request_branches
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            sections
                .waiting_for_my_review
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            sections
                .created_by_me
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            sections
                .all_open
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
    }

    #[test]
    fn categorize_pull_requests_handles_missing_current_user() {
        let pull_requests = vec![pull_request(
            4,
            "someone-else",
            "local-branch",
            &["abeldebruijn"],
            "2026-03-10T12:00:00Z",
        )];
        let local_branch_names = HashSet::from(["local-branch".to_string()]);

        let sections = categorize_pull_requests(pull_requests, None, &local_branch_names);

        assert!(sections.waiting_for_my_review.is_empty());
        assert!(sections.created_by_me.is_empty());
        assert_eq!(
            sections
                .local_pull_request_branches
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![4]
        );
        assert_eq!(
            sections
                .all_open
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![4]
        );
    }

    #[test]
    fn categorized_pull_requests_limit_preserves_section_order_for_visible_prs() {
        let pull_requests = vec![
            pull_request(
                2,
                "abeldebruijn",
                "user-branch",
                &[],
                "2026-03-10T12:00:00Z",
            ),
            pull_request(
                1,
                "copilot-swe-agent",
                "local-feature",
                &["abeldebruijn"],
                "2026-03-09T12:00:00Z",
            ),
            pull_request(
                3,
                "someone-else",
                "remote-only",
                &["abeldebruijn"],
                "2026-03-08T12:00:00Z",
            ),
        ];
        let local_branch_names =
            HashSet::from(["local-feature".to_string(), "user-branch".to_string()]);

        let sections =
            categorize_pull_requests(pull_requests, Some("abeldebruijn"), &local_branch_names);
        let limited_sections = sections.limit(2);

        assert_eq!(
            limited_sections
                .copilot_on_my_behalf
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            limited_sections
                .local_pull_request_branches
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            limited_sections
                .waiting_for_my_review
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            limited_sections
                .created_by_me
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            limited_sections
                .all_open
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn default_collapsed_sections_leave_all_open_expanded() {
        assert_eq!(
            PullRequestPanel::default_collapsed_sections(),
            list::PullRequestSection::ordered()
                .into_iter()
                .filter(|section| *section != list::PullRequestSection::AllOpen)
                .collect()
        );
    }

    #[test]
    fn collapse_all_sections_set_include_every_pull_request_section() {
        assert_eq!(
            PullRequestPanel::collapse_all_sections_set(),
            list::PullRequestSection::ordered().into_iter().collect()
        );
    }

    #[test]
    fn has_hidden_pull_requests_is_true_only_when_more_open_pull_requests_remain_hidden() {
        assert!(has_hidden_pull_requests(20, 21));
        assert!(!has_hidden_pull_requests(20, 20));
        assert!(!has_hidden_pull_requests(21, 20));
    }
}
