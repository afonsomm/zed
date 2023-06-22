use crate::{
    contact_notification::ContactNotification, contacts_popover, face_pile::FacePile, toggle_mute,
    toggle_screen_sharing, ToggleMute, ToggleScreenSharing,
};
use call::{ActiveCall, ParticipantLocation, Room};
use client::{proto::PeerId, Client, ContactEventKind, SignIn, SignOut, User, UserStore};
use clock::ReplicaId;
use contacts_popover::ContactsPopover;
use context_menu::{ContextMenu, ContextMenuItem};
use gpui::{
    actions,
    color::Color,
    elements::*,
    geometry::{rect::RectF, vector::vec2f, PathBuilder},
    json::{self, ToJson},
    platform::{CursorStyle, MouseButton},
    AppContext, Entity, ImageData, LayoutContext, ModelHandle, SceneBuilder, Subscription, View,
    ViewContext, ViewHandle, WeakViewHandle,
};
use project::{Project, RepositoryEntry};
use std::{ops::Range, sync::Arc};
use theme::{AvatarStyle, Theme};
use util::ResultExt;
use workspace::{FollowNextCollaborator, Workspace};

// const MAX_TITLE_LENGTH: usize = 75;

actions!(
    collab,
    [
        ToggleContactsMenu,
        ToggleUserMenu,
        ShareProject,
        UnshareProject,
    ]
);

pub fn init(cx: &mut AppContext) {
    cx.add_action(CollabTitlebarItem::toggle_contacts_popover);
    cx.add_action(CollabTitlebarItem::share_project);
    cx.add_action(CollabTitlebarItem::unshare_project);
    cx.add_action(CollabTitlebarItem::toggle_user_menu);
}

pub struct CollabTitlebarItem {
    project: ModelHandle<Project>,
    user_store: ModelHandle<UserStore>,
    client: Arc<Client>,
    workspace: WeakViewHandle<Workspace>,
    contacts_popover: Option<ViewHandle<ContactsPopover>>,
    user_menu: ViewHandle<ContextMenu>,
    _subscriptions: Vec<Subscription>,
}

impl Entity for CollabTitlebarItem {
    type Event = ();
}

impl View for CollabTitlebarItem {
    fn ui_name() -> &'static str {
        "CollabTitlebarItem"
    }

    fn render(&mut self, cx: &mut ViewContext<Self>) -> AnyElement<Self> {
        let workspace = if let Some(workspace) = self.workspace.upgrade(cx) {
            workspace
        } else {
            return Empty::new().into_any();
        };

        let project = self.project.read(cx);
        let theme = theme::current(cx).clone();
        let mut left_container = Flex::row();
        let mut right_container = Flex::row().align_children_center();

        left_container.add_child(self.collect_title_root_names(&project, theme.clone(), cx));

        let user = self.user_store.read(cx).current_user();
        let peer_id = self.client.peer_id();
        if let Some(((user, peer_id), room)) = user
            .as_ref()
            .zip(peer_id)
            .zip(ActiveCall::global(cx).read(cx).room().cloned())
        {
            right_container
                .add_children(self.render_in_call_share_unshare_button(&workspace, &theme, cx));

            left_container
                .add_child(self.render_current_user(&workspace, &theme, &user, peer_id, cx));
            left_container.add_children(self.render_collaborators(&workspace, &theme, &room, cx));
            right_container.add_child(self.render_toggle_microphone(&theme, &room, cx));
            right_container.add_child(self.render_toggle_screen_sharing_button(&theme, &room, cx));
        }

        let status = workspace.read(cx).client().status();
        let status = &*status.borrow();
        if matches!(status, client::Status::Connected { .. }) {
            right_container.add_child(self.render_toggle_contacts_button(&theme, cx));
            let avatar = ActiveCall::global(cx)
                .read(cx)
                .room()
                .is_none()
                .then(|| user.as_ref())
                .flatten()
                .and_then(|user| user.avatar.clone());
            right_container.add_child(self.render_user_menu_button(&theme, avatar, cx));
        } else {
            right_container.add_children(self.render_connection_status(status, cx));
            right_container.add_child(self.render_sign_in_button(&theme, cx));
            right_container.add_child(self.render_user_menu_button(&theme, None, cx));
        }

        Stack::new()
            .with_child(left_container)
            .with_child(
                Flex::row()
                    .with_child(
                        right_container.contained().with_background_color(
                            theme
                                .workspace
                                .titlebar
                                .container
                                .background_color
                                .unwrap_or_else(|| Color::transparent_black()),
                        ),
                    )
                    .aligned()
                    .right(),
            )
            .into_any()
    }
}

impl CollabTitlebarItem {
    pub fn new(
        workspace: &Workspace,
        workspace_handle: &ViewHandle<Workspace>,
        cx: &mut ViewContext<Self>,
    ) -> Self {
        let project = workspace.project().clone();
        let user_store = workspace.app_state().user_store.clone();
        let client = workspace.app_state().client.clone();
        let active_call = ActiveCall::global(cx);
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.observe(workspace_handle, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&project, |_, _, cx| cx.notify()));
        subscriptions.push(cx.observe(&active_call, |this, _, cx| this.active_call_changed(cx)));
        subscriptions.push(cx.observe_window_activation(|this, active, cx| {
            this.window_activation_changed(active, cx)
        }));
        subscriptions.push(cx.observe(&user_store, |_, _, cx| cx.notify()));
        subscriptions.push(
            cx.subscribe(&user_store, move |this, user_store, event, cx| {
                if let Some(workspace) = this.workspace.upgrade(cx) {
                    workspace.update(cx, |workspace, cx| {
                        if let client::Event::Contact { user, kind } = event {
                            if let ContactEventKind::Requested | ContactEventKind::Accepted = kind {
                                workspace.show_notification(user.id as usize, cx, |cx| {
                                    cx.add_view(|cx| {
                                        ContactNotification::new(
                                            user.clone(),
                                            *kind,
                                            user_store,
                                            cx,
                                        )
                                    })
                                })
                            }
                        }
                    });
                }
            }),
        );

        let view_id = cx.view_id();
        Self {
            workspace: workspace.weak_handle(),
            project,
            user_store,
            client,
            contacts_popover: None,
            user_menu: cx.add_view(|cx| {
                let mut menu = ContextMenu::new(view_id, cx);
                menu.set_position_mode(OverlayPositionMode::Local);
                menu
            }),
            _subscriptions: subscriptions,
        }
    }

    fn collect_title_root_names(
        &self,
        project: &Project,
        theme: Arc<Theme>,
        cx: &ViewContext<Self>,
    ) -> AnyElement<Self> {
        let mut names_and_branches = project.visible_worktrees(cx).map(|worktree| {
            let worktree = worktree.read(cx);
            (worktree.root_name(), worktree.root_git_entry())
        });

        let (name, entry) = names_and_branches.next().unwrap_or(("", None));
        let branch_prepended = entry
            .as_ref()
            .and_then(RepositoryEntry::branch)
            .map(|branch| format!("/{branch}"));
        let text_style = theme.workspace.titlebar.title.clone();
        let item_spacing = theme.workspace.titlebar.item_spacing;

        let mut highlight = text_style.clone();
        highlight.color = theme.workspace.titlebar.highlight_color;

        let style = LabelStyle {
            text: text_style,
            highlight_text: Some(highlight),
        };
        let mut ret = Flex::row().with_child(
            Label::new(name.to_owned(), style.clone())
                .with_highlights((0..name.len()).into_iter().collect())
                .contained()
                .aligned()
                .left()
                .into_any_named("title-project-name"),
        );
        if let Some(git_branch) = branch_prepended {
            ret = ret.with_child(
                Label::new(git_branch, style)
                    .contained()
                    .with_margin_right(item_spacing)
                    .aligned()
                    .left()
                    .into_any_named("title-project-branch"),
            )
        }
        ret.into_any()
    }

    fn window_activation_changed(&mut self, active: bool, cx: &mut ViewContext<Self>) {
        let project = if active {
            Some(self.project.clone())
        } else {
            None
        };
        ActiveCall::global(cx)
            .update(cx, |call, cx| call.set_location(project.as_ref(), cx))
            .detach_and_log_err(cx);
    }

    fn active_call_changed(&mut self, cx: &mut ViewContext<Self>) {
        if ActiveCall::global(cx).read(cx).room().is_none() {
            self.contacts_popover = None;
        }
        cx.notify();
    }

    fn share_project(&mut self, _: &ShareProject, cx: &mut ViewContext<Self>) {
        let active_call = ActiveCall::global(cx);
        let project = self.project.clone();
        active_call
            .update(cx, |call, cx| call.share_project(project, cx))
            .detach_and_log_err(cx);
    }

    fn unshare_project(&mut self, _: &UnshareProject, cx: &mut ViewContext<Self>) {
        let active_call = ActiveCall::global(cx);
        let project = self.project.clone();
        active_call
            .update(cx, |call, cx| call.unshare_project(project, cx))
            .log_err();
    }

    pub fn toggle_contacts_popover(&mut self, _: &ToggleContactsMenu, cx: &mut ViewContext<Self>) {
        if self.contacts_popover.take().is_none() {
            let view = cx.add_view(|cx| {
                ContactsPopover::new(
                    self.project.clone(),
                    self.user_store.clone(),
                    self.workspace.clone(),
                    cx,
                )
            });
            cx.subscribe(&view, |this, _, event, cx| {
                match event {
                    contacts_popover::Event::Dismissed => {
                        this.contacts_popover = None;
                    }
                }

                cx.notify();
            })
            .detach();
            self.contacts_popover = Some(view);
        }

        cx.notify();
    }

    pub fn toggle_user_menu(&mut self, _: &ToggleUserMenu, cx: &mut ViewContext<Self>) {
        self.user_menu.update(cx, |user_menu, cx| {
            let items = if let Some(_) = self.user_store.read(cx).current_user() {
                vec![
                    ContextMenuItem::action("Settings", zed_actions::OpenSettings),
                    ContextMenuItem::action("Theme", theme_selector::Toggle),
                    ContextMenuItem::separator(),
                    ContextMenuItem::action(
                        "Share Feedback",
                        feedback::feedback_editor::GiveFeedback,
                    ),
                    ContextMenuItem::action("Sign out", SignOut),
                ]
            } else {
                vec![
                    ContextMenuItem::action("Settings", zed_actions::OpenSettings),
                    ContextMenuItem::action("Theme", theme_selector::Toggle),
                    ContextMenuItem::separator(),
                    ContextMenuItem::action(
                        "Share Feedback",
                        feedback::feedback_editor::GiveFeedback,
                    ),
                ]
            };

            user_menu.show(Default::default(), AnchorCorner::TopRight, items, cx);
        });
    }

    fn render_toggle_contacts_button(
        &self,
        theme: &Theme,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let titlebar = &theme.workspace.titlebar;

        let badge = if self
            .user_store
            .read(cx)
            .incoming_contact_requests()
            .is_empty()
        {
            None
        } else {
            Some(
                Empty::new()
                    .collapsed()
                    .contained()
                    .with_style(titlebar.toggle_contacts_badge)
                    .contained()
                    .with_margin_left(
                        titlebar
                            .toggle_contacts_button
                            .inactive_state()
                            .default
                            .icon_width,
                    )
                    .with_margin_top(
                        titlebar
                            .toggle_contacts_button
                            .inactive_state()
                            .default
                            .icon_width,
                    )
                    .aligned(),
            )
        };

        Stack::new()
            .with_child(
                MouseEventHandler::<ToggleContactsMenu, Self>::new(0, cx, |state, _| {
                    let style = titlebar
                        .toggle_contacts_button
                        .in_state(self.contacts_popover.is_some())
                        .style_for(state);
                    Svg::new("icons/user_plus_16.svg")
                        .with_color(style.color)
                        .constrained()
                        .with_width(style.icon_width)
                        .aligned()
                        .constrained()
                        .with_width(style.button_width)
                        .with_height(style.button_width)
                        .contained()
                        .with_style(style.container)
                })
                .with_cursor_style(CursorStyle::PointingHand)
                .on_click(MouseButton::Left, move |_, this, cx| {
                    this.toggle_contacts_popover(&Default::default(), cx)
                })
                .with_tooltip::<ToggleContactsMenu>(
                    0,
                    "Show contacts menu".into(),
                    Some(Box::new(ToggleContactsMenu)),
                    theme.tooltip.clone(),
                    cx,
                ),
            )
            .with_children(badge)
            .with_children(self.render_contacts_popover_host(titlebar, cx))
            .into_any()
    }
    fn render_toggle_screen_sharing_button(
        &self,
        theme: &Theme,
        room: &ModelHandle<Room>,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let icon;
        let tooltip;
        if room.read(cx).is_screen_sharing() {
            icon = "icons/enable_screen_sharing_12.svg";
            tooltip = "Stop Sharing Screen"
        } else {
            icon = "icons/disable_screen_sharing_12.svg";
            tooltip = "Share Screen";
        }

        let titlebar = &theme.workspace.titlebar;
        MouseEventHandler::<ToggleScreenSharing, Self>::new(0, cx, |state, _| {
            let style = titlebar.call_control.style_for(state);
            Svg::new(icon)
                .with_color(style.color)
                .constrained()
                .with_width(style.icon_width)
                .aligned()
                .constrained()
                .with_width(style.button_width)
                .with_height(style.button_width)
                .contained()
                .with_style(style.container)
        })
        .with_cursor_style(CursorStyle::PointingHand)
        .on_click(MouseButton::Left, move |_, _, cx| {
            toggle_screen_sharing(&Default::default(), cx)
        })
        .with_tooltip::<ToggleScreenSharing>(
            0,
            tooltip.into(),
            Some(Box::new(ToggleScreenSharing)),
            theme.tooltip.clone(),
            cx,
        )
        .aligned()
        .into_any()
    }
    fn render_toggle_microphone(
        &self,
        theme: &Theme,
        room: &ModelHandle<Room>,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let icon;
        let tooltip;
        let background;
        if room.read(cx).is_muted().unwrap_or(false) {
            icon = "icons/microphone_inactive_12.svg";
            tooltip = "Unmute microphone\nRight click for options";
            background = Color::red();
        } else {
            icon = "icons/microphone_active_12.svg";
            tooltip = "Mute microphone\nRight click for options";
            background = Color::transparent_black();
        }

        let titlebar = &theme.workspace.titlebar;
        MouseEventHandler::<ToggleMute, Self>::new(0, cx, |state, _| {
            let style = titlebar.call_control.style_for(state);
            Svg::new(icon)
                .with_color(style.color)
                .constrained()
                .with_width(style.icon_width)
                .aligned()
                .constrained()
                .with_width(style.button_width)
                .with_height(style.button_width)
                .contained()
                .with_style(style.container)
                .with_background_color(background)
        })
        .with_cursor_style(CursorStyle::PointingHand)
        .on_click(MouseButton::Left, move |_, _, cx| {
            toggle_mute(&Default::default(), cx)
        })
        .with_tooltip::<ToggleMute>(
            0,
            tooltip.into(),
            Some(Box::new(ToggleMute)),
            theme.tooltip.clone(),
            cx,
        )
        .aligned()
        .into_any()
    }

    fn render_in_call_share_unshare_button(
        &self,
        workspace: &ViewHandle<Workspace>,
        theme: &Theme,
        cx: &mut ViewContext<Self>,
    ) -> Option<AnyElement<Self>> {
        let project = workspace.read(cx).project();
        if project.read(cx).is_remote() {
            return None;
        }

        let is_shared = project.read(cx).is_shared();
        let label = if is_shared { "Unshare" } else { "Share" };
        let tooltip = if is_shared {
            "Unshare project from call participants"
        } else {
            "Share project with call participants"
        };

        let titlebar = &theme.workspace.titlebar;

        enum ShareUnshare {}
        Some(
            Stack::new()
                .with_child(
                    MouseEventHandler::<ShareUnshare, Self>::new(0, cx, |state, _| {
                        //TODO: Ensure this button has consistent width for both text variations
                        let style = titlebar.share_button.inactive_state().style_for(state);
                        Label::new(label, style.text.clone())
                            .contained()
                            .with_style(style.container)
                    })
                    .with_cursor_style(CursorStyle::PointingHand)
                    .on_click(MouseButton::Left, move |_, this, cx| {
                        if is_shared {
                            this.unshare_project(&Default::default(), cx);
                        } else {
                            this.share_project(&Default::default(), cx);
                        }
                    })
                    .with_tooltip::<ShareUnshare>(
                        0,
                        tooltip.to_owned(),
                        None,
                        theme.tooltip.clone(),
                        cx,
                    ),
                )
                .aligned()
                .contained()
                .with_margin_left(theme.workspace.titlebar.item_spacing)
                .into_any(),
        )
    }

    fn render_user_menu_button(
        &self,
        theme: &Theme,
        avatar: Option<Arc<ImageData>>,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let titlebar = &theme.workspace.titlebar;
        let avatar_style = &theme.workspace.titlebar.follower_avatar;
        Stack::new()
            .with_child(
                MouseEventHandler::<ToggleUserMenu, Self>::new(0, cx, |state, _| {
                    let style = titlebar.call_control.style_for(state);

                    let mut dropdown = Flex::row().align_children_center();

                    if let Some(avatar_img) = avatar {
                        dropdown = dropdown.with_child(Self::render_face(
                            avatar_img,
                            *avatar_style,
                            Color::transparent_black(),
                        ));
                    };
                    dropdown
                        .with_child(
                            Svg::new("icons/caret_down_8.svg")
                                .with_color(style.color)
                                .constrained()
                                .with_width(style.icon_width)
                                .contained()
                                .into_any(),
                        )
                        .aligned()
                        .constrained()
                        .with_height(style.button_width)
                        .contained()
                        .with_style(style.container)
                        .into_any()
                })
                .with_cursor_style(CursorStyle::PointingHand)
                .on_click(MouseButton::Left, move |_, this, cx| {
                    this.toggle_user_menu(&Default::default(), cx)
                })
                .with_tooltip::<ToggleUserMenu>(
                    0,
                    "Toggle user menu".to_owned(),
                    Some(Box::new(ToggleUserMenu)),
                    theme.tooltip.clone(),
                    cx,
                )
                .contained()
                .with_margin_left(theme.workspace.titlebar.item_spacing),
            )
            .with_child(
                ChildView::new(&self.user_menu, cx)
                    .aligned()
                    .bottom()
                    .right(),
            )
            .into_any()
    }

    fn render_sign_in_button(&self, theme: &Theme, cx: &mut ViewContext<Self>) -> AnyElement<Self> {
        let titlebar = &theme.workspace.titlebar;
        MouseEventHandler::<SignIn, Self>::new(0, cx, |state, _| {
            let style = titlebar.sign_in_prompt.inactive_state().style_for(state);
            Label::new("Sign In", style.text.clone())
                .contained()
                .with_style(style.container)
        })
        .with_cursor_style(CursorStyle::PointingHand)
        .on_click(MouseButton::Left, move |_, this, cx| {
            let client = this.client.clone();
            cx.app_context()
                .spawn(|cx| async move { client.authenticate_and_connect(true, &cx).await })
                .detach_and_log_err(cx);
        })
        .into_any()
    }

    fn render_contacts_popover_host<'a>(
        &'a self,
        _theme: &'a theme::Titlebar,
        cx: &'a ViewContext<Self>,
    ) -> Option<AnyElement<Self>> {
        self.contacts_popover.as_ref().map(|popover| {
            Overlay::new(ChildView::new(popover, cx))
                .with_fit_mode(OverlayFitMode::SwitchAnchor)
                .with_anchor_corner(AnchorCorner::TopRight)
                .with_z_index(999)
                .aligned()
                .bottom()
                .right()
                .into_any()
        })
    }

    fn render_collaborators(
        &self,
        workspace: &ViewHandle<Workspace>,
        theme: &Theme,
        room: &ModelHandle<Room>,
        cx: &mut ViewContext<Self>,
    ) -> Vec<Container<Self>> {
        let mut participants = room
            .read(cx)
            .remote_participants()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        participants.sort_by_cached_key(|p| p.user.github_login.clone());

        participants
            .into_iter()
            .filter_map(|participant| {
                let project = workspace.read(cx).project().read(cx);
                let replica_id = project
                    .collaborators()
                    .get(&participant.peer_id)
                    .map(|collaborator| collaborator.replica_id);
                let user = participant.user.clone();
                Some(
                    Container::new(self.render_face_pile(
                        &user,
                        replica_id,
                        participant.peer_id,
                        Some(participant.location),
                        workspace,
                        theme,
                        cx,
                    ))
                    .with_margin_right(theme.workspace.titlebar.face_pile_spacing),
                )
            })
            .collect()
    }

    fn render_current_user(
        &self,
        workspace: &ViewHandle<Workspace>,
        theme: &Theme,
        user: &Arc<User>,
        peer_id: PeerId,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let replica_id = workspace.read(cx).project().read(cx).replica_id();
        Container::new(self.render_face_pile(
            user,
            Some(replica_id),
            peer_id,
            None,
            workspace,
            theme,
            cx,
        ))
        .with_margin_right(theme.workspace.titlebar.item_spacing)
        .into_any()
    }

    fn render_face_pile(
        &self,
        user: &User,
        replica_id: Option<ReplicaId>,
        peer_id: PeerId,
        location: Option<ParticipantLocation>,
        workspace: &ViewHandle<Workspace>,
        theme: &Theme,
        cx: &mut ViewContext<Self>,
    ) -> AnyElement<Self> {
        let project_id = workspace.read(cx).project().read(cx).remote_id();
        let room = ActiveCall::global(cx).read(cx).room();
        let is_being_followed = workspace.read(cx).is_being_followed(peer_id);
        let followed_by_self = room
            .and_then(|room| {
                Some(
                    is_being_followed
                        && room
                            .read(cx)
                            .followers_for(peer_id, project_id?)
                            .iter()
                            .any(|&follower| {
                                Some(follower) == workspace.read(cx).client().peer_id()
                            }),
                )
            })
            .unwrap_or(false);

        let leader_style = theme.workspace.titlebar.leader_avatar;
        let follower_style = theme.workspace.titlebar.follower_avatar;

        let mut background_color = theme
            .workspace
            .titlebar
            .container
            .background_color
            .unwrap_or_default();
        if let Some(replica_id) = replica_id {
            if followed_by_self {
                let selection = theme.editor.replica_selection_style(replica_id).selection;
                background_color = Color::blend(selection, background_color);
                background_color.a = 255;
            }
        }

        let mut content = Stack::new()
            .with_children(user.avatar.as_ref().map(|avatar| {
                let face_pile = FacePile::new(theme.workspace.titlebar.follower_avatar_overlap)
                    .with_child(Self::render_face(
                        avatar.clone(),
                        Self::location_style(workspace, location, leader_style, cx),
                        background_color,
                    ))
                    .with_children(
                        (|| {
                            let project_id = project_id?;
                            let room = room?.read(cx);
                            let followers = room.followers_for(peer_id, project_id);

                            Some(followers.into_iter().flat_map(|&follower| {
                                let remote_participant =
                                    room.remote_participant_for_peer_id(follower);

                                let avatar = remote_participant
                                    .and_then(|p| p.user.avatar.clone())
                                    .or_else(|| {
                                        if follower == workspace.read(cx).client().peer_id()? {
                                            workspace
                                                .read(cx)
                                                .user_store()
                                                .read(cx)
                                                .current_user()?
                                                .avatar
                                                .clone()
                                        } else {
                                            None
                                        }
                                    })?;

                                Some(Self::render_face(
                                    avatar.clone(),
                                    follower_style,
                                    background_color,
                                ))
                            }))
                        })()
                        .into_iter()
                        .flatten(),
                    );

                let mut container = face_pile
                    .contained()
                    .with_style(theme.workspace.titlebar.leader_selection);

                if let Some(replica_id) = replica_id {
                    if followed_by_self {
                        let color = theme.editor.replica_selection_style(replica_id).selection;
                        container = container.with_background_color(color);
                    }
                }

                container
            }))
            .with_children((|| {
                let replica_id = replica_id?;
                let color = theme.editor.replica_selection_style(replica_id).cursor;
                Some(
                    AvatarRibbon::new(color)
                        .constrained()
                        .with_width(theme.workspace.titlebar.avatar_ribbon.width)
                        .with_height(theme.workspace.titlebar.avatar_ribbon.height)
                        .aligned()
                        .bottom(),
                )
            })())
            .into_any();

        if let Some(location) = location {
            if let Some(replica_id) = replica_id {
                enum ToggleFollow {}

                content = MouseEventHandler::<ToggleFollow, Self>::new(
                    replica_id.into(),
                    cx,
                    move |_, _| content,
                )
                .with_cursor_style(CursorStyle::PointingHand)
                .on_click(MouseButton::Left, move |_, item, cx| {
                    if let Some(workspace) = item.workspace.upgrade(cx) {
                        if let Some(task) = workspace
                            .update(cx, |workspace, cx| workspace.toggle_follow(peer_id, cx))
                        {
                            task.detach_and_log_err(cx);
                        }
                    }
                })
                .with_tooltip::<ToggleFollow>(
                    peer_id.as_u64() as usize,
                    if is_being_followed {
                        format!("Unfollow {}", user.github_login)
                    } else {
                        format!("Follow {}", user.github_login)
                    },
                    Some(Box::new(FollowNextCollaborator)),
                    theme.tooltip.clone(),
                    cx,
                )
                .into_any();
            } else if let ParticipantLocation::SharedProject { project_id } = location {
                enum JoinProject {}

                let user_id = user.id;
                content = MouseEventHandler::<JoinProject, Self>::new(
                    peer_id.as_u64() as usize,
                    cx,
                    move |_, _| content,
                )
                .with_cursor_style(CursorStyle::PointingHand)
                .on_click(MouseButton::Left, move |_, this, cx| {
                    if let Some(workspace) = this.workspace.upgrade(cx) {
                        let app_state = workspace.read(cx).app_state().clone();
                        workspace::join_remote_project(project_id, user_id, app_state, cx)
                            .detach_and_log_err(cx);
                    }
                })
                .with_tooltip::<JoinProject>(
                    peer_id.as_u64() as usize,
                    format!("Follow {} into external project", user.github_login),
                    Some(Box::new(FollowNextCollaborator)),
                    theme.tooltip.clone(),
                    cx,
                )
                .into_any();
            }
        }
        content
    }

    fn location_style(
        workspace: &ViewHandle<Workspace>,
        location: Option<ParticipantLocation>,
        mut style: AvatarStyle,
        cx: &ViewContext<Self>,
    ) -> AvatarStyle {
        if let Some(location) = location {
            if let ParticipantLocation::SharedProject { project_id } = location {
                if Some(project_id) != workspace.read(cx).project().read(cx).remote_id() {
                    style.image.grayscale = true;
                }
            } else {
                style.image.grayscale = true;
            }
        }

        style
    }

    fn render_face<V: View>(
        avatar: Arc<ImageData>,
        avatar_style: AvatarStyle,
        background_color: Color,
    ) -> AnyElement<V> {
        Image::from_data(avatar)
            .with_style(avatar_style.image)
            .aligned()
            .contained()
            .with_background_color(background_color)
            .with_corner_radius(avatar_style.outer_corner_radius)
            .constrained()
            .with_width(avatar_style.outer_width)
            .with_height(avatar_style.outer_width)
            .aligned()
            .into_any()
    }

    fn render_connection_status(
        &self,
        status: &client::Status,
        cx: &mut ViewContext<Self>,
    ) -> Option<AnyElement<Self>> {
        enum ConnectionStatusButton {}

        let theme = &theme::current(cx).clone();
        match status {
            client::Status::ConnectionError
            | client::Status::ConnectionLost
            | client::Status::Reauthenticating { .. }
            | client::Status::Reconnecting { .. }
            | client::Status::ReconnectionError { .. } => Some(
                Svg::new("icons/cloud_slash_12.svg")
                    .with_color(theme.workspace.titlebar.offline_icon.color)
                    .constrained()
                    .with_width(theme.workspace.titlebar.offline_icon.width)
                    .aligned()
                    .contained()
                    .with_style(theme.workspace.titlebar.offline_icon.container)
                    .into_any(),
            ),
            client::Status::UpgradeRequired => Some(
                MouseEventHandler::<ConnectionStatusButton, Self>::new(0, cx, |_, _| {
                    Label::new(
                        "Please update Zed to collaborate",
                        theme.workspace.titlebar.outdated_warning.text.clone(),
                    )
                    .contained()
                    .with_style(theme.workspace.titlebar.outdated_warning.container)
                    .aligned()
                })
                .with_cursor_style(CursorStyle::PointingHand)
                .on_click(MouseButton::Left, |_, _, cx| {
                    auto_update::check(&Default::default(), cx);
                })
                .into_any(),
            ),
            _ => None,
        }
    }
}

pub struct AvatarRibbon {
    color: Color,
}

impl AvatarRibbon {
    pub fn new(color: Color) -> AvatarRibbon {
        AvatarRibbon { color }
    }
}

impl Element<CollabTitlebarItem> for AvatarRibbon {
    type LayoutState = ();

    type PaintState = ();

    fn layout(
        &mut self,
        constraint: gpui::SizeConstraint,
        _: &mut CollabTitlebarItem,
        _: &mut LayoutContext<CollabTitlebarItem>,
    ) -> (gpui::geometry::vector::Vector2F, Self::LayoutState) {
        (constraint.max, ())
    }

    fn paint(
        &mut self,
        scene: &mut SceneBuilder,
        bounds: RectF,
        _: RectF,
        _: &mut Self::LayoutState,
        _: &mut CollabTitlebarItem,
        _: &mut ViewContext<CollabTitlebarItem>,
    ) -> Self::PaintState {
        let mut path = PathBuilder::new();
        path.reset(bounds.lower_left());
        path.curve_to(
            bounds.origin() + vec2f(bounds.height(), 0.),
            bounds.origin(),
        );
        path.line_to(bounds.upper_right() - vec2f(bounds.height(), 0.));
        path.curve_to(bounds.lower_right(), bounds.upper_right());
        path.line_to(bounds.lower_left());
        scene.push_path(path.build(self.color, None));
    }

    fn rect_for_text_range(
        &self,
        _: Range<usize>,
        _: RectF,
        _: RectF,
        _: &Self::LayoutState,
        _: &Self::PaintState,
        _: &CollabTitlebarItem,
        _: &ViewContext<CollabTitlebarItem>,
    ) -> Option<RectF> {
        None
    }

    fn debug(
        &self,
        bounds: RectF,
        _: &Self::LayoutState,
        _: &Self::PaintState,
        _: &CollabTitlebarItem,
        _: &ViewContext<CollabTitlebarItem>,
    ) -> gpui::json::Value {
        json::json!({
            "type": "AvatarRibbon",
            "bounds": bounds.to_json(),
            "color": self.color.to_json(),
        })
    }
}
