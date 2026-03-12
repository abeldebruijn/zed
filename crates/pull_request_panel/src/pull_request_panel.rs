use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use futures::AsyncReadExt;
use git::repository::Branch;
use git::{GitHostingProviderRegistry, parse_git_remote_url};
use git_ui::{project_diff::ProjectDiff, resolve_active_repository};
use gpui::{
    Action, App, AppContext, AsyncWindowContext, BorrowAppContext, Context, Corner, DismissEvent,
    Entity, EntityId, EventEmitter, FocusHandle, Focusable, IntoElement, Pixels, Point, Render,
    Subscription, Task, WeakEntity, Window, actions, anchored, deferred, px,
};
use http_client::{AsyncBody, HttpClient, HttpRequestExt, RedirectPolicy, Request, StatusCode};
use language::Buffer;
use project::git_store::{GitStoreEvent, Repository, RepositoryEvent};
use settings::{RegisterSetting, Settings, SettingsStore};
use std::{collections::HashSet, sync::Arc};
use ui::{ContextMenu, IconName, prelude::*, v_flex};
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::DetachAndPromptErr,
};

mod create_pull_request_panel;
mod list;
mod pull_request_details_view;
mod pull_request_context_panel;
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
    context_menu_pull_request: Option<PullRequestSummary>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    sort: PullRequestSort,
    sort_direction: PullRequestSortDirection,
    view_state: PullRequestPanelViewState,
    collapsed_sections: HashSet<list::PullRequestSection>,

    show_create_panel: bool,
    create_pull_request: create_pull_request_panel::CreatePullRequestState,
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
            let create_title_buffer = cx.new(|cx| Buffer::local("", cx));
            let create_description_buffer = cx.new(|cx| Buffer::local("", cx));

            let mut subscriptions = Vec::new();
            subscriptions.push(cx.observe(&create_title_buffer, |_, _, cx| cx.notify()));
            subscriptions.push(cx.observe(&create_description_buffer, |_, _, cx| cx.notify()));
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
                context_menu_pull_request: None,
                context_menu: None,
                sort: PullRequestSort::default(),
                sort_direction: PullRequestSortDirection::default(),
                view_state: PullRequestPanelViewState::loading(),
                collapsed_sections: Self::default_collapsed_sections(),

                show_create_panel: false,
                create_pull_request: create_pull_request_panel::CreatePullRequestState::new(
                    create_title_buffer,
                    create_description_buffer,
                ),
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
        self.clear_context_menu();

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

    fn toggle_create_pull_request_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.show_create_panel {
            self.hide_create_pull_request_panel(cx);
            return;
        }

        let PullRequestPanelContent::Ready(data) = &self.view_state.content else {
            return;
        };

        self.show_create_panel = true;
        create_pull_request_panel::initialize_create_panel_defaults(
            &mut self.create_pull_request,
            data,
            cx,
        );
        cx.notify();
    }

    fn hide_create_pull_request_panel(&mut self, cx: &mut Context<Self>) {
        self.show_create_panel = false;
        self.create_pull_request.error_message.take();
        self.create_pull_request.create_task_in_flight = false;
        cx.notify();
    }

    fn set_selected_branch(
        &mut self,
        role: create_pull_request_panel::BranchRole,
        branch: create_pull_request_panel::SelectedBranch,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match role {
            create_pull_request_panel::BranchRole::Base => {
                self.create_pull_request.base_branch = Some(branch);
            }
            create_pull_request_panel::BranchRole::Head => {
                self.create_pull_request.head_branch = Some(branch);
            }
        }
        self.create_pull_request.error_message.take();
        cx.notify();
    }

    fn create_pull_request(&mut self, draft: bool, _window: &mut Window, cx: &mut Context<Self>) {
        let PullRequestPanelContent::Ready(data) = &self.view_state.content else {
            return;
        };

        let Some(token) = PullRequestPanelGitHubSettings::get_global(cx)
            .github_token
            .clone()
        else {
            self.create_pull_request.error_message =
                Some("Missing GitHub token. Set `git.github_token` in your Zed settings.".into());
            cx.notify();
            return;
        };

        let title = self.create_pull_request.title_text(cx);
        if title.trim().is_empty() {
            self.create_pull_request.error_message = Some("Title is required.".into());
            cx.notify();
            return;
        }

        let Some(base) = self.create_pull_request.base_branch.as_ref() else {
            self.create_pull_request.error_message = Some("Select a base branch.".into());
            cx.notify();
            return;
        };
        let Some(head) = self.create_pull_request.head_branch.as_ref() else {
            self.create_pull_request.error_message = Some("Select a branch to merge.".into());
            cx.notify();
            return;
        };

        if base.api_ref == head.api_ref {
            self.create_pull_request.error_message =
                Some("Base and head branches must be different.".into());
            cx.notify();
            return;
        }

        if self.create_pull_request.create_task_in_flight {
            return;
        }

        let description = self.create_pull_request.description_text(cx);
        let body = if description.trim().is_empty() {
            None
        } else {
            Some(description)
        };

        let github_repository = data.repository.clone();
        let token = token;
        let base = base.api_ref.to_string();
        let head = head.api_ref.to_string();
        let title = title;

        self.create_pull_request.create_task_in_flight = true;
        self.create_pull_request.error_message.take();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = async {
                let http_client = cx.update(|cx| cx.http_client());
                let request = github_create_pull_request_request(
                    &github_repository,
                    &token,
                    GitHubCreatePullRequestRequestBody {
                        title,
                        head,
                        base,
                        body,
                        draft,
                    },
                    &http_client,
                )?;

                let mut response = http_client.send(request).await?;
                let status = response.status();
                let body_bytes = read_response_body(response.body_mut()).await?;

                if status == StatusCode::UNPROCESSABLE_ENTITY {
                    if let Some(message) = github_first_validation_error_message(&body_bytes) {
                        bail!("{message}");
                    }

                    let body_text = String::from_utf8_lossy(&body_bytes);
                    bail!("GitHub pull request creation failed with status {status}: {body_text}");
                } else if status != StatusCode::CREATED {
                    let body_text = String::from_utf8_lossy(&body_bytes);
                    bail!("GitHub pull request creation failed with status {status}: {body_text}");
                }

                anyhow::Ok(())
            }
            .await;

            this.update(cx, |panel, cx| {
                panel.create_pull_request.create_task_in_flight = false;
                match result {
                    Ok(()) => {
                        panel.create_pull_request.clear_inputs(cx);
                        panel.show_create_panel = false;
                        panel.reload(cx);
                    }
                    Err(error) => {
                        panel.create_pull_request.error_message = Some(error.to_string().into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
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

    fn deploy_pull_request_context_menu(
        &mut self,
        pull_request: PullRequestSummary,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity().downgrade();
        let checkout_branch_disabled = self.checkout_branch_action_disabled(&pull_request, cx);
        let context_menu = pull_request_context_panel::build_context_menu(
            window,
            cx,
            self.focus_handle.clone(),
            checkout_branch_disabled,
            move |action, window, cx| {
                let Some(panel) = panel.upgrade() else {
                    return;
                };

                panel.update(cx, |panel, cx| {
                    panel.handle_pull_request_context_menu_action(action, window, cx);
                });
            },
        );

        self.context_menu_pull_request = Some(pull_request);
        self.set_context_menu(context_menu, position, window, cx);
    }

    fn handle_pull_request_context_menu_action(
        &mut self,
        action: pull_request_context_panel::PullRequestContextMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected_pull_request = self.context_menu_pull_request.clone();
        let effect = pull_request_context_menu_effect(action, selected_pull_request.as_ref());
        self.clear_context_menu();
        cx.focus_self(window);
        cx.notify();

        let Some(effect) = effect else {
            return;
        };

        match effect {
            PullRequestContextMenuEffect::OpenInGitHub(html_url) => {
                cx.open_url(&html_url);
            }
            PullRequestContextMenuEffect::CheckoutBranch(head_ref) => {
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };
                let Some(repository) = resolve_active_repository(&workspace.read(cx), cx) else {
                    return;
                };

                window
                    .spawn(cx, async move |cx| {
                        let branches = repository
                            .update(cx, |repository, _| repository.branches())
                            .await??;
                        let branch_name = branch_name_for_pull_request_head(&head_ref, &branches);

                        repository
                            .update(cx, |repository, _| repository.change_branch(branch_name))
                            .await??;

                        anyhow::Ok(())
                    })
                    .detach_and_prompt_err("Failed to change branch", window, cx, |_, _, _| None);
            }
            PullRequestContextMenuEffect::OpenChanges => {
                let Some(pull_request) = selected_pull_request else {
                    return;
                };
                let Some(workspace) = self.workspace.upgrade() else {
                    return;
                };
                let Some(repository) = resolve_active_repository(&workspace.read(cx), cx) else {
                    return;
                };

                window
                    .spawn(cx, async move |cx| {
                        let branches = repository
                            .update(cx, |repository, _| repository.branches())
                            .await??;
                        let head_branch_name =
                            branch_name_for_pull_request_head(&pull_request.head_ref, &branches);
                        let base_branch_name =
                            branch_name_for_pull_request_base(&pull_request.base_ref, &branches);

                        repository
                            .update(cx, |repository, _| {
                                repository.change_branch(head_branch_name)
                            })
                            .await??;

                        workspace.update_in(cx, |workspace, window, cx| {
                            ProjectDiff::deploy_branch_diff_at(
                                workspace,
                                base_branch_name.into(),
                                window,
                                cx,
                            );
                        })?;

                        anyhow::Ok(())
                    })
                    .detach_and_prompt_err(
                        "Failed to open pull request changes",
                        window,
                        cx,
                        |_, _, _| None,
                    );
            }
            PullRequestContextMenuEffect::RefreshPullRequest => {
                self.reload(cx);
            }
        }
    }

    fn set_context_menu(
        &mut self,
        context_menu: Entity<ContextMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe_in(
            &context_menu,
            window,
            |this, _, _: &DismissEvent, window, cx| {
                if this.context_menu.as_ref().is_some_and(|context_menu| {
                    context_menu.0.focus_handle(cx).contains_focused(window, cx)
                }) {
                    cx.focus_self(window);
                }

                this.clear_context_menu();
                cx.notify();
            },
        );
        self.context_menu = Some((context_menu, position, subscription));
        cx.notify();
    }

    fn checkout_branch_action_disabled(&self, pull_request: &PullRequestSummary, cx: &App) -> bool {
        let PullRequestPanelContent::Ready(data) = &self.view_state.content else {
            return false;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return false;
        };
        let Some(repository) = resolve_active_repository(&workspace.read(cx), cx) else {
            return false;
        };
        let repository = repository.read(cx);

        current_branch_matches_pull_request_head(
            &pull_request.head_ref,
            repository.branch.as_ref(),
            &data.branches,
        )
    }

    fn clear_context_menu(&mut self) {
        self.context_menu_pull_request.take();
        self.context_menu.take();
    }

    fn open_pull_request_details(
        &mut self,
        pull_request: PullRequestSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_context_menu();

        let PullRequestPanelContent::Ready(data) = &self.view_state.content else {
            return;
        };
        let github_repository = data.repository.clone();

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        workspace.update(cx, |workspace, cx| {
            let existing = workspace
                .active_pane()
                .read(cx)
                .items()
                .find_map(|item| item.downcast::<pull_request_details_view::PullRequestDetailsView>());

            if let Some(existing) = existing {
                workspace.activate_item(&existing, true, true, window, cx);
                existing.update(cx, |details_view, cx| {
                    details_view.set_pull_request(github_repository, pull_request, cx);
                });
            } else {
                let details_view = cx.new(|cx| {
                    pull_request_details_view::PullRequestDetailsView::new(
                        github_repository,
                        pull_request,
                        cx,
                    )
                });
                workspace.add_item_to_active_pane(Box::new(details_view), None, true, window, cx);
            }
        });

        cx.notify();
    }
}

#[derive(Clone, Debug, Default, RegisterSetting)]
struct PullRequestPanelGitHubSettings {
    github_token: Option<String>,
}

impl Settings for PullRequestPanelGitHubSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            github_token: content
                .git
                .as_ref()
                .and_then(|git| git.github_token.clone()),
        }
    }
}

fn has_hidden_pull_requests(
    visible_pull_request_count: usize,
    total_pull_request_count: usize,
) -> bool {
    visible_pull_request_count < total_pull_request_count
}

#[derive(Debug, PartialEq, Eq)]
enum PullRequestContextMenuEffect {
    OpenInGitHub(String),
    CheckoutBranch(String),
    OpenChanges,
    RefreshPullRequest,
}

fn pull_request_context_menu_effect(
    action: pull_request_context_panel::PullRequestContextMenuAction,
    selected_pull_request: Option<&PullRequestSummary>,
) -> Option<PullRequestContextMenuEffect> {
    match action {
        pull_request_context_panel::PullRequestContextMenuAction::OpenInGitHub => {
            selected_pull_request.map(|pull_request| {
                PullRequestContextMenuEffect::OpenInGitHub(pull_request.html_url.clone())
            })
        }
        pull_request_context_panel::PullRequestContextMenuAction::CheckoutBranch => {
            selected_pull_request.map(|pull_request| {
                PullRequestContextMenuEffect::CheckoutBranch(pull_request.head_ref.clone())
            })
        }
        pull_request_context_panel::PullRequestContextMenuAction::OpenChanges => {
            Some(PullRequestContextMenuEffect::OpenChanges)
        }
        pull_request_context_panel::PullRequestContextMenuAction::RefreshPullRequest => {
            Some(PullRequestContextMenuEffect::RefreshPullRequest)
        }
    }
}

fn branch_name_for_pull_request_head(head_ref: &str, branches: &[Branch]) -> String {
    branch_name_for_pull_request_ref(head_ref, branches)
}

fn branch_name_for_pull_request_base(base_ref: &str, branches: &[Branch]) -> String {
    branch_name_for_pull_request_ref(base_ref, branches)
}

fn current_branch_matches_pull_request_head(
    head_ref: &str,
    current_branch: Option<&Branch>,
    branches: &[Branch],
) -> bool {
    let Some(current_branch) = current_branch else {
        return false;
    };

    current_branch.name() == branch_name_for_pull_request_head(head_ref, branches)
}

fn branch_name_for_pull_request_ref(reference: &str, branches: &[Branch]) -> String {
    if branches
        .iter()
        .any(|branch| !branch.is_remote() && branch.name() == reference)
    {
        return reference.to_string();
    }

    for preferred_remote in ["upstream", "origin"] {
        let preferred_branch = format!("{preferred_remote}/{reference}");
        if branches
            .iter()
            .any(|branch| branch.is_remote() && branch.name() == preferred_branch)
        {
            return preferred_branch;
        }
    }

    if let Some(remote_branch_name) = branches.iter().find_map(|branch| {
        branch.is_remote().then_some(branch).and_then(|branch| {
            branch
                .name()
                .split_once('/')
                .filter(|(_, branch_name)| *branch_name == reference)
                .map(|_| branch.name().to_string())
        })
    }) {
        return remote_branch_name;
    }

    reference.to_string()
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
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Corner::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
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
        if !active {
            self.clear_context_menu();
        }
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
    branches: Vec<Branch>,
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

#[derive(Debug, serde::Serialize)]
struct GitHubCreatePullRequestRequestBody {
    title: String,
    head: String,
    base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    draft: bool,
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
    base_ref: String,
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

#[derive(Debug, serde::Deserialize)]
struct GitHubValidationFailedResponse {
    message: String,
    #[serde(default)]
    errors: Vec<GitHubValidationFailedItem>,
}

#[derive(Debug, serde::Deserialize)]
struct GitHubValidationFailedItem {
    #[serde(default)]
    message: Option<String>,
}

fn github_first_validation_error_message(body: &[u8]) -> Option<String> {
    let response: GitHubValidationFailedResponse = serde_json::from_slice(body).ok()?;
    if let Some(first) = response
        .errors
        .into_iter()
        .next()
        .and_then(|error| error.message)
    {
        return Some(first);
    }
    (!response.message.trim().is_empty()).then_some(response.message)
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
        .iter()
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
        branches,
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

fn github_create_pull_request_request_body(
    body: GitHubCreatePullRequestRequestBody,
) -> Result<Vec<u8>> {
    serde_json::to_vec(&body)
        .map_err(|error| anyhow!("Failed to serialize create PR request: {error}"))
}

fn github_create_pull_request_request(
    repository: &GitHubRepositoryContext,
    token: &str,
    body: GitHubCreatePullRequestRequestBody,
    http_client: &Arc<dyn HttpClient>,
) -> Result<Request<AsyncBody>> {
    let body_bytes = github_create_pull_request_request_body(body)?;
    let body = AsyncBody::from(body_bytes);

    Request::builder()
        .method("POST")
        .uri(format!(
            "{}/repos/{}/{}/pulls",
            repository.api_base_url, repository.owner, repository.repo
        ))
        .header("Accept", GITHUB_ACCEPT_HEADER)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", token))
        .when_some(http_client.user_agent().cloned(), |request, user_agent| {
            request.header("User-Agent", user_agent)
        })
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(body)
        .context("Failed to build GitHub create pull request request")
}

pub(crate) fn branch_api_ref(branch: &Branch) -> String {
    let name = branch.name();
    if branch.is_remote() {
        name.split_once('/')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| name.to_string())
    } else {
        name.to_string()
    }
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
            base_ref: value.base.reference,
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

    fn branch(ref_name: &str) -> Branch {
        Branch {
            is_head: false,
            ref_name: ref_name.to_string().into(),
            upstream: None,
            most_recent_commit: None,
        }
    }

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
            base_ref: "main".to_string(),
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

    #[test]
    fn branch_name_for_pull_request_head_prefers_local_then_preferred_remotes() {
        assert_eq!(
            branch_name_for_pull_request_head(
                "feature/topic",
                &[
                    branch("refs/remotes/origin/feature/topic"),
                    branch("refs/heads/feature/topic"),
                    branch("refs/remotes/upstream/feature/topic"),
                ]
            ),
            "feature/topic"
        );

        assert_eq!(
            branch_name_for_pull_request_head(
                "feature/topic",
                &[
                    branch("refs/remotes/origin/feature/topic"),
                    branch("refs/remotes/upstream/feature/topic"),
                ]
            ),
            "upstream/feature/topic"
        );
    }

    #[test]
    fn branch_name_for_pull_request_head_falls_back_to_any_matching_remote_or_head_ref() {
        assert_eq!(
            branch_name_for_pull_request_head(
                "feature/topic",
                &[branch("refs/remotes/fork/feature/topic")]
            ),
            "fork/feature/topic"
        );

        assert_eq!(
            branch_name_for_pull_request_head("missing-branch", &[]),
            "missing-branch"
        );
    }

    #[test]
    fn branch_name_for_pull_request_base_prefers_local_then_preferred_remotes() {
        assert_eq!(
            branch_name_for_pull_request_base(
                "main",
                &[
                    branch("refs/remotes/origin/main"),
                    branch("refs/heads/main"),
                    branch("refs/remotes/upstream/main"),
                ]
            ),
            "main"
        );

        assert_eq!(
            branch_name_for_pull_request_base(
                "main",
                &[
                    branch("refs/remotes/origin/main"),
                    branch("refs/remotes/upstream/main"),
                ]
            ),
            "upstream/main"
        );
    }

    #[test]
    fn current_branch_matches_pull_request_head_when_current_branch_already_matches() {
        assert!(current_branch_matches_pull_request_head(
            "feature/topic",
            Some(&branch("refs/heads/feature/topic")),
            &[branch("refs/heads/feature/topic")]
        ));

        assert!(current_branch_matches_pull_request_head(
            "feature/topic",
            Some(&branch("refs/remotes/upstream/feature/topic")),
            &[branch("refs/remotes/upstream/feature/topic")]
        ));
    }

    #[test]
    fn current_branch_matches_pull_request_head_leaves_checkout_enabled_when_branch_is_unknown_or_different()
     {
        assert!(!current_branch_matches_pull_request_head(
            "feature/topic",
            Some(&branch("refs/heads/main")),
            &[branch("refs/heads/main")]
        ));
        assert!(!current_branch_matches_pull_request_head(
            "feature/topic",
            None,
            &[branch("refs/heads/feature/topic")]
        ));
    }

    #[test]
    fn current_branch_matches_pull_request_head_uses_full_branch_list_for_resolution() {
        assert!(!current_branch_matches_pull_request_head(
            "feature/topic",
            Some(&branch("refs/remotes/origin/feature/topic")),
            &[
                branch("refs/remotes/origin/feature/topic"),
                branch("refs/heads/feature/topic"),
            ]
        ));
    }

    #[test]
    fn github_pull_request_response_conversion_includes_base_ref() {
        let summary = PullRequestSummary::from(GitHubPullRequestResponse {
            number: 42,
            title: "PR 42".to_string(),
            html_url: "https://example.com/pull/42".to_string(),
            updated_at: "2026-03-10T12:00:00Z".parse().unwrap(),
            user: GitHubUserResponse {
                login: "author".to_string(),
            },
            head: GitHubPullRequestHeadResponse {
                reference: "feature/topic".to_string(),
            },
            base: GitHubPullRequestBaseResponse {
                reference: "main".to_string(),
            },
            requested_reviewers: vec![GitHubUserResponse {
                login: "reviewer".to_string(),
            }],
        });

        assert_eq!(summary.head_ref, "feature/topic");
        assert_eq!(summary.base_ref, "main");
    }

    #[test]
    fn pull_request_context_menu_effect_uses_selected_pull_request_fields() {
        let pull_request = pull_request(42, "author", "feature/topic", &[], "2026-03-10T12:00:00Z");

        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::OpenInGitHub,
                Some(&pull_request),
            ),
            Some(PullRequestContextMenuEffect::OpenInGitHub(
                pull_request.html_url.clone(),
            ))
        );
        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::CheckoutBranch,
                Some(&pull_request),
            ),
            Some(PullRequestContextMenuEffect::CheckoutBranch(
                pull_request.head_ref.clone(),
            ))
        );
    }

    #[test]
    fn pull_request_context_menu_effect_preserves_non_pull_request_actions_without_selection() {
        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::OpenInGitHub,
                None,
            ),
            None
        );
        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::CheckoutBranch,
                None,
            ),
            None
        );
        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::OpenChanges,
                None,
            ),
            Some(PullRequestContextMenuEffect::OpenChanges)
        );
        assert_eq!(
            pull_request_context_menu_effect(
                pull_request_context_panel::PullRequestContextMenuAction::RefreshPullRequest,
                None,
            ),
            Some(PullRequestContextMenuEffect::RefreshPullRequest)
        );
    }

    #[test]
    fn remote_branch_api_ref_strips_remote_prefix_only_for_remote_refs() {
        let remote = Branch {
            is_head: false,
            ref_name: "refs/remotes/origin/feature/x".to_string().into(),
            upstream: None,
            most_recent_commit: None,
        };
        let local = Branch {
            is_head: false,
            ref_name: "refs/heads/feature/x".to_string().into(),
            upstream: None,
            most_recent_commit: None,
        };

        assert_eq!(branch_api_ref(&remote), "feature/x");
        assert_eq!(branch_api_ref(&local), "feature/x");
    }

    #[test]
    fn request_body_sets_draft_and_omits_empty_body() {
        let body_bytes =
            github_create_pull_request_request_body(GitHubCreatePullRequestRequestBody {
                title: "Title".to_string(),
                head: "feature".to_string(),
                base: "main".to_string(),
                body: None,
                draft: true,
            })
            .unwrap();

        let value: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(value.get("draft").and_then(|v| v.as_bool()), Some(true));
        assert!(value.get("body").is_none());
    }

    #[test]
    fn github_create_pull_request_request_builds_expected_request() {
        let repository = GitHubRepositoryContext {
            owner: "zed-industries".to_string(),
            repo: "zed".to_string(),
            full_name: "zed-industries/zed".to_string(),
            api_base_url: "https://api.github.com".to_string(),
        };
        let http_client: Arc<dyn HttpClient> = Arc::new(http_client::BlockedHttpClient::new());

        let request = github_create_pull_request_request(
            &repository,
            "token",
            GitHubCreatePullRequestRequestBody {
                title: "Title".to_string(),
                head: "feature".to_string(),
                base: "main".to_string(),
                body: Some("Body".to_string()),
                draft: false,
            },
            &http_client,
        )
        .unwrap();

        assert_eq!(*request.method(), http_client::Method::POST);
        assert_eq!(request.uri().path(), "/repos/zed-industries/zed/pulls");
        assert_eq!(
            request
                .headers()
                .get("Accept")
                .and_then(|header| header.to_str().ok()),
            Some(GITHUB_ACCEPT_HEADER)
        );
        assert_eq!(
            request
                .headers()
                .get("X-GitHub-Api-Version")
                .and_then(|header| header.to_str().ok()),
            Some(GITHUB_API_VERSION)
        );
        assert_eq!(
            request
                .headers()
                .get("Content-Type")
                .and_then(|header| header.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            request
                .headers()
                .get("Authorization")
                .and_then(|header| header.to_str().ok()),
            Some("Bearer token")
        );
    }

    #[test]
    fn github_first_validation_error_message_prefers_first_error_message() {
        let body = br#"{
            "message": "Validation Failed",
            "errors": [
                { "message": "No commits between main and feature" },
                { "message": "Some other error" }
            ]
        }"#;

        assert_eq!(
            github_first_validation_error_message(body),
            Some("No commits between main and feature".to_string())
        );
    }
}
