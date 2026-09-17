use agent_canvas::{CanvasDb, CanvasEvent, CanvasRecord, CanvasStore, PreviewStatus};
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use editor::{Editor, EditorEvent};
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    PlatformNativeView as _, Render, SharedString, Subscription, Task, WeakEntity, Window,
};
use gpui_webview::WebView;
use language::Buffer;
use project::Project;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{rc::Rc, sync::Arc};
use ui::{Button, Label, prelude::*};
use util::ResultExt as _;
use workspace::{
    ItemId, Workspace, WorkspaceId,
    item::{Item, ItemEvent, SaveOptions, SerializableItem},
};

use crate::{AgentPanel, thread_metadata_store::ThreadMetadataStore};

pub fn init(cx: &mut App) {
    workspace::register_serializable_item::<CanvasItem>(cx);
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        CanvasStore::init(
            workspace.app_state().node_runtime.clone(),
            workspace.app_state().fs.clone(),
            cx,
        );
        if let Some(store) = CanvasStore::global(cx)
            && let Some(window) = window
        {
            cx.subscribe_in(&store, window, |workspace, _, event, window, cx| {
                if let CanvasEvent::Created(record) = event {
                    let belongs_here = workspace
                        .panel::<AgentPanel>(cx)
                        .and_then(|panel| panel.read(cx).active_agent_thread(cx))
                        .is_some_and(|thread| {
                            thread.read(cx).session_id().0.as_ref() == record.session_id
                        });
                    if belongs_here {
                        let id = record.id.clone();
                        // Defer: open() updates the workspace this callback is still borrowing.
                        cx.spawn_in(window, async move |workspace, cx| {
                            cx.update(|window, cx| open(&id, &workspace, window, cx))
                                .log_err();
                        })
                        .detach();
                    }
                }
            })
            .detach();
        }
    })
    .detach();
}

pub fn open(id: &str, workspace: &WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let Some(workspace) = workspace.upgrade() else {
        return;
    };
    let record = match CanvasDb::global(cx).get(id) {
        Ok(record) => record,
        Err(error) => {
            workspace.update(cx, |workspace, cx| workspace.show_error(error, cx));
            return;
        }
    };
    let existing = workspace
        .read(cx)
        .items_of_type::<CanvasItem>(cx)
        .find(|item| item.read(cx).record.id == record.id);
    if let Some(existing) = existing {
        workspace.update(cx, |workspace, cx| {
            workspace.activate_item(&existing, false, false, window, cx)
        });
        return;
    }
    let project = workspace.read(cx).project().clone();
    window
        .spawn(cx, async move |cx| {
            let result = async {
                let store = cx
                    .update(|_, cx| CanvasStore::global(cx))?
                    .context("Canvas runtime is unavailable")?;
                let record = store
                    .update(cx, |store, cx| {
                        store.refresh(
                            record.project_key.clone(),
                            record.session_id.clone(),
                            record.path.clone(),
                            cx,
                        )
                    })
                    .await
                    .map_err(anyhow::Error::msg)?;
                let project = store.update(cx, |store, cx| store.local_project(&project, cx));
                let buffer = project
                    .update(cx, |project, cx| {
                        project.open_local_buffer(record.path.clone(), cx)
                    })
                    .await?;
                cx.update(|window, cx| {
                    let item = cx.new(|cx| {
                        CanvasItem::new(record, buffer, project, workspace.downgrade(), window, cx)
                    });
                    workspace.update(cx, |workspace, cx| {
                        workspace.add_item_to_active_pane(Box::new(item), None, false, window, cx)
                    });
                })?;
                anyhow::Ok(())
            }
            .await;
            if let Err(error) = result {
                cx.update(|_, cx| {
                    workspace.update(cx, |workspace, cx| workspace.show_error(error, cx))
                })
                .log_err();
            }
        })
        .detach();
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PageMessage {
    Ready,
    Error {
        error: String,
    },
    State {
        key: String,
        value: Value,
    },
    Action {
        action: CanvasAction,
    },
    Selection {
        elements: Vec<Value>,
        complete: bool,
    },
    Link {
        url: String,
    },
    Shortcut {
        action: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum CanvasAction {
    AskAgent {
        prompt: String,
    },
    OpenFile {
        path: String,
        line: Option<u32>,
    },
    OpenAgent {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

#[derive(Clone)]
struct CanvasSelection {
    id: uuid::Uuid,
    revision: i64,
    elements: Vec<Value>,
    snapshot: Option<Arc<[u8]>>,
}

pub struct CanvasItem {
    record: CanvasRecord,
    workspace: WeakEntity<Workspace>,
    editor: Entity<Editor>,
    feedback: Entity<Editor>,
    focus_handle: FocusHandle,
    source_mode: bool,
    source_dirty: bool,
    selection: Option<CanvasSelection>,
    error: Option<SharedString>,
    saving: bool,
    webview: Option<Rc<WebView>>,
    pending_prompt: Option<String>,
    pending_snapshots: Vec<Arc<[u8]>>,
    _snapshot_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _messages_task: Task<()>,
    _draft_subscription: Option<Subscription>,
}

impl CanvasItem {
    fn new(
        record: CanvasRecord,
        buffer: Entity<Buffer>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| Editor::for_buffer(buffer.clone(), Some(project), window, cx));
        let feedback = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Describe the change to this element…", window, cx);
            editor
        });
        let mut subscriptions = vec![cx.subscribe(&editor, |this, editor, event, cx| {
            if matches!(
                event,
                EditorEvent::Edited { .. } | EditorEvent::BufferEdited
            ) {
                this.source_dirty = editor.read(cx).is_dirty(cx);
                cx.emit(CanvasItemEvent);
                cx.notify();
            }
        })];
        if let Some(store) = CanvasStore::global(cx) {
            subscriptions.push(
                cx.subscribe_in(&store, window, |this, _, event, window, cx| {
                    if let CanvasEvent::Unavailable { id, message } = event
                        && *id == this.record.id
                    {
                        this.webview = None;
                        this.error = Some(message.clone().into());
                        cx.notify();
                    }
                    if let CanvasEvent::StateUpdated { id, state } = event
                        && *id == this.record.id
                    {
                        if let Some(view) = &this.webview {
                            view.evaluate(&format!(
                                "globalThis.__zedCanvasHost?.updateState({state})"
                            ));
                        }
                    }
                    if let CanvasEvent::Created(record) | CanvasEvent::Updated(record) = event
                        && record.id == this.record.id
                    {
                        this.record = record.clone();
                        this.source_dirty = this.editor.read(cx).is_dirty(cx);
                        this.reload(window, cx);
                        cx.emit(CanvasItemEvent);
                    }
                }),
            );
        }
        subscriptions.push(cx.observe_global::<settings::SettingsStore>(|this, cx| {
            if let Some(view) = &this.webview {
                view.evaluate(&format!(
                    "globalThis.__zedCanvasHost?.updateTheme({})",
                    host_theme(cx)
                ));
            }
        }));
        if let Some(workspace) = workspace.upgrade() {
            let languages = workspace.read(cx).project().read(cx).languages().clone();
            cx.spawn(async move |_, cx| {
                if let Ok(language) = languages.language_for_name("TSX").await {
                    buffer.update(cx, |buffer, cx| buffer.set_language(Some(language), cx));
                }
            })
            .detach();
        }
        let mut item = Self {
            record,
            workspace,
            editor,
            feedback,
            focus_handle: cx.focus_handle(),
            source_mode: false,
            source_dirty: false,
            selection: None,
            error: None,
            saving: false,
            webview: None,
            pending_prompt: None,
            pending_snapshots: Vec::new(),
            _snapshot_task: None,
            _subscriptions: subscriptions,
            _messages_task: Task::ready(()),
            _draft_subscription: None,
        };
        item.reload(window, cx);
        item
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.webview.take();
        self.error = self.record.diagnostics.clone().map(Into::into);
        if self.error.is_some() {
            cx.notify();
            return;
        }
        let result = (|| {
            let database = CanvasDb::global(cx);
            let state = database.state(&self.record.id)?;
            let html = canvas_html(&self.record.javascript, &state, &host_theme(cx))?;
            let (sender, mut receiver) = mpsc::unbounded();
            let view = WebView::new(
                window,
                Rc::new(move |message| {
                    if let Err(error) = sender.unbounded_send(message) {
                        log::debug!("Canvas message receiver closed: {error}");
                    }
                }),
            )?;
            view.load_html(&html)?;
            self.webview = Some(view);
            let id = self.record.id.clone();
            let revision = self.record.revision;
            if let Some(store) = CanvasStore::global(cx) {
                store.update(cx, |store, _| {
                    store.set_preview_status(&id, revision, PreviewStatus::NotLoaded)
                });
            }
            self._messages_task = cx.spawn_in(window, async move |this, cx| {
                while let Some(message) = receiver.next().await {
                    if message.len() > 2 * 1024 * 1024 {
                        continue;
                    }
                    let result: Result<()> = async {
                        match serde_json::from_str::<PageMessage>(&message)
                            .context("Invalid Canvas message")?
                        {
                            PageMessage::State { key, value } => {
                                database.set_state(id.clone(), revision, key, value).await?;
                            }
                            message => this.update_in(cx, |this, window, cx| {
                                this.handle_page_message(message, revision, window, cx)
                            })??,
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        this.update(cx, |this, cx| {
                            this.error = Some(format!("{error:#}").into());
                            cx.notify();
                        })
                        .log_err();
                    }
                }
            });
            anyhow::Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("{error:#}").into());
        }
        cx.notify();
    }

    fn handle_page_message(
        &mut self,
        message: PageMessage,
        revision: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.record.revision != revision {
            return Ok(());
        }
        match message {
            PageMessage::Ready => {
                if self.error.is_none()
                    && let Some(store) = CanvasStore::global(cx)
                {
                    store.update(cx, |store, _| {
                        store.set_preview_status(&self.record.id, revision, PreviewStatus::Loaded)
                    });
                }
            }
            PageMessage::Error { error } => {
                self.error = Some(error.clone().into());
                if let Some(store) = CanvasStore::global(cx) {
                    store.update(cx, |store, _| {
                        store.set_preview_status(
                            &self.record.id,
                            revision,
                            PreviewStatus::Error(error),
                        )
                    });
                }
            }
            PageMessage::Action { action } => match action {
                CanvasAction::AskAgent { prompt } => {
                    ensure!(
                        !prompt.trim().is_empty() && prompt.len() <= 8192,
                        "Invalid Canvas prompt"
                    );
                    self.prepare_prompt(prompt, None, window, cx)?;
                }
                CanvasAction::OpenFile { path, line } => {
                    let workspace = self.workspace.upgrade().context("Workspace closed")?;
                    let project = workspace.read(cx).project().read(cx);
                    let project_path = project
                        .find_project_path(&path, cx)
                        .context("Canvas links can open only existing workspace files")?;
                    ensure!(
                        project.entry_for_path(&project_path, cx).is_some(),
                        "Canvas file link was not found"
                    );
                    let absolute = project
                        .absolute_path(&project_path, cx)
                        .context("File path is unavailable")?;
                    let mut target = absolute.to_string_lossy().into_owned();
                    if let Some(line) = line {
                        ensure!(line > 0, "Line numbers start at 1");
                        target.push_str(&format!("#L{line}"));
                    }
                    crate::conversation_view::open_link(target.into(), &self.workspace, window, cx);
                }
                CanvasAction::OpenAgent { session_id } => {
                    if let Some(view) = &self.webview {
                        view.blur();
                    }
                    let session_id = acp::SessionId::new(session_id);
                    ensure!(
                        ThreadMetadataStore::try_global(cx).is_some_and(|store| {
                            store.read(cx).entry_by_session(&session_id).is_some()
                        }),
                        "Conversation was not found"
                    );
                    let workspace = self.workspace.upgrade().context("Workspace closed")?;
                    let panel = workspace
                        .update(cx, |workspace, cx| {
                            workspace.focus_panel::<AgentPanel>(window, cx)
                        })
                        .context("Agent panel is unavailable")?;
                    panel.update(cx, |panel, cx| {
                        panel.open_thread(session_id, None, None, window, cx)
                    });
                }
            },
            PageMessage::Selection { elements, complete } => {
                ensure!(
                    elements.len() <= 16 && serde_json::to_string(&elements)?.len() <= 128 * 1024,
                    "Canvas selection is too large"
                );
                if elements.is_empty() {
                    self.selection = None;
                    self._snapshot_task = None;
                    cx.notify();
                    return Ok(());
                }
                let selection_id = uuid::Uuid::new_v4();
                self.selection = Some(CanvasSelection {
                    id: selection_id,
                    revision,
                    elements,
                    snapshot: None,
                });
                if let Some(view) = &self.webview {
                    let snapshot = view.snapshot(cx);
                    self._snapshot_task = Some(cx.spawn(async move |this, cx| {
                        let result = snapshot.await;
                        this.update(cx, |this, cx| {
                            this.attach_snapshot(selection_id, result, cx)
                        })
                        .log_err();
                    }));
                }
                if complete {
                    if let Some(view) = &self.webview {
                        view.blur();
                    }
                    window.focus(&self.feedback.focus_handle(cx), cx);
                }
            }
            PageMessage::Link { url } => {
                let parsed = url::Url::parse(&url)?;
                if matches!(parsed.scheme(), "https" | "http") {
                    cx.open_url(&url);
                }
            }
            PageMessage::Shortcut { action } => {
                if action == "command_palette" {
                    if let Some(view) = &self.webview {
                        view.blur();
                    }
                    let action = cx.build_action("command_palette::Toggle", None)?;
                    window.dispatch_action(action, cx);
                }
            }
            PageMessage::State { .. } => unreachable!("state is persisted by the receiver task"),
        }
        cx.notify();
        Ok(())
    }

    fn attach_snapshot(
        &mut self,
        selection_id: uuid::Uuid,
        result: Result<Vec<u8>>,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        if selection.id != selection_id {
            return;
        }
        match result {
            Ok(bytes) => selection.snapshot = Some(bytes.into()),
            Err(error) => {
                self.error =
                    Some(format!("Could not capture Canvas feedback image: {error:#}").into())
            }
        }
        self._snapshot_task = None;
        cx.notify();
    }

    fn refresh_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = CanvasStore::global(cx) else {
            return;
        };
        let record = self.record.clone();
        let task = store.update(cx, |store, cx| {
            store.refresh(record.project_key, record.session_id, record.path, cx)
        });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(record) => {
                this.update_in(cx, |this, window, cx| {
                    this.record = record;
                    this.reload(window, cx);
                })
                .log_err();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
    }

    fn save_source(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let Some(store) = CanvasStore::global(cx) else {
            return Task::ready(Err(anyhow::anyhow!("Canvas runtime is unavailable")));
        };
        let Some(project) = self.editor.read(cx).project().cloned() else {
            return Task::ready(Err(anyhow::anyhow!("Canvas source project is unavailable")));
        };
        let Some(buffer) = self.editor.read(cx).buffer().read(cx).as_singleton() else {
            return Task::ready(Err(anyhow::anyhow!("Canvas source buffer is unavailable")));
        };
        if self.saving {
            return Task::ready(Err(anyhow::anyhow!("Canvas save is already in progress")));
        }
        self.saving = true;
        let save = project.update(cx, |project, cx| project.save_buffer(buffer, cx));
        let record = self.record.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = async {
                save.await?;
                store
                    .update(cx, |store, cx| {
                        store.refresh(record.project_key, record.session_id, record.path, cx)
                    })
                    .await
                    .map_err(anyhow::Error::msg)
            }
            .await;
            this.update(cx, |this, cx| {
                this.saving = false;
                match &result {
                    Ok(record) => {
                        this.record = record.clone();
                        this.source_dirty = this.editor.read(cx).is_dirty(cx);
                        this.error = record.diagnostics.clone().map(Into::into);
                        if let Some(view) = this.webview.take() {
                            view.hide();
                        }
                    }
                    Err(error) => this.error = Some(format!("{error:#}").into()),
                }
                cx.emit(CanvasItemEvent);
                cx.notify();
            })?;
            result.map(|_| ())
        })
    }

    fn prepare_prompt(
        &mut self,
        prompt: String,
        snapshot: Option<Arc<[u8]>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if let Some(view) = &self.webview {
            view.blur();
        }
        let workspace = self.workspace.upgrade().context("Workspace closed")?;
        let panel = workspace
            .update(cx, |workspace, cx| {
                workspace.focus_panel::<AgentPanel>(window, cx)
            })
            .context("Agent panel is unavailable")?;
        let session_id = acp::SessionId::new(self.record.session_id.clone());
        let existing = ThreadMetadataStore::try_global(cx)
            .and_then(|store| store.read(cx).entry_by_session(&session_id).map(|_| ()));
        ensure!(
            existing.is_some()
                || panel
                    .read(cx)
                    .active_agent_thread(cx)
                    .is_some_and(|thread| thread.read(cx).session_id() == &session_id),
            "The Canvas conversation was deleted or is unavailable"
        );
        let prompt = format!(
            "\n\nCanvas: {} ({}, revision {})\n{}",
            self.record.title,
            self.record.uri(),
            self.record.revision,
            prompt
        );
        if let Some(snapshot) = snapshot {
            self.pending_snapshots.push(snapshot);
        }
        self.pending_prompt
            .get_or_insert_with(String::new)
            .push_str(&prompt);
        self._draft_subscription =
            Some(cx.observe_in(&panel, window, |this, panel, window, cx| {
                this.fill_draft(&panel, window, cx);
            }));
        panel.update(cx, |panel, cx| {
            panel.open_thread(session_id, None, None, window, cx)
        });
        self.fill_draft(&panel, window, cx);
        Ok(())
    }

    fn fill_draft(
        &mut self,
        panel: &Entity<AgentPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = panel.read(cx).active_agent_thread(cx) else {
            return;
        };
        if thread.read(cx).session_id().0.as_ref() != self.record.session_id {
            return;
        }
        let Some(view) = panel.read(cx).active_thread_view(cx) else {
            return;
        };
        let Some(prompt) = self.pending_prompt.take() else {
            return;
        };
        let editor = view.read(cx).message_editor.clone();
        editor.update(cx, |editor, cx| editor.append_text(&prompt, window, cx));
        if !self.pending_snapshots.is_empty() {
            let snapshots = std::mem::take(&mut self.pending_snapshots);
            match editor.update(cx, |editor, cx| {
                editor.append_canvas_snapshots(snapshots, window, cx)
            }) {
                Ok(task) => task.detach(),
                Err(error) => editor.update(cx, |editor, cx| {
                    editor.append_text(&format!("\n{error}"), window, cx)
                }),
            }
        }
        window.focus(&editor.focus_handle(cx), cx);
        self._draft_subscription.take();
    }
}

fn host_theme(cx: &App) -> Value {
    let colors = cx.theme().colors();
    json!({
        "background": colors.editor_background.to_string(),
        "foreground": colors.text.to_string(),
        "muted": colors.text_muted.to_string(),
        "border": colors.border.to_string(),
        "accent": colors.text_accent.to_string(),
        "kind": if cx.theme().appearance().is_light() { "light" } else { "dark" }
    })
}

fn canvas_html(javascript: &str, state: &Value, theme: &Value) -> Result<String> {
    let vendor = agent_canvas::runtime_asset("vendor.js")?;
    let initialize = format!(
        "__zedCanvasHost.mount({}, {});",
        serde_json::to_string(state)?,
        serde_json::to_string(theme)?
    );
    let scripts = [&vendor[..], javascript.as_bytes(), initialize.as_bytes()]
        .into_iter()
        .map(|source| {
            format!(
                "<script src=\"data:application/javascript;base64,{}\"></script>",
                STANDARD.encode(source)
            )
        })
        .collect::<String>();
    let shell = String::from_utf8(agent_canvas::runtime_asset("shell.html")?)?;
    Ok(shell.replace("<!--CANVAS_SCRIPTS-->", &scripts))
}

pub struct CanvasItemEvent;
impl EventEmitter<CanvasItemEvent> for CanvasItem {}
impl Focusable for CanvasItem {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.source_mode {
            self.editor.focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }
}

impl Item for CanvasItem {
    type Event = CanvasItemEvent;
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        format!("Canvas: {}", self.record.title).into()
    }
    fn is_dirty(&self, _: &App) -> bool {
        self.source_dirty
    }
    fn can_save(&self, _: &App) -> bool {
        true
    }
    fn save(
        &mut self,
        _: SaveOptions,
        _: Entity<Project>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.save_source(cx)
    }
    fn to_item_events(_: &Self::Event, callback: &mut dyn FnMut(ItemEvent)) {
        callback(ItemEvent::UpdateTab);
    }
    fn deactivated(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Some(view) = &self.webview {
            view.hide();
        }
    }
    fn on_removed(&self, cx: &mut Context<Self>) {
        if let Some(view) = &self.webview {
            view.hide();
        }
        if let Some(store) = CanvasStore::global(cx) {
            store.update(cx, |store, _| {
                store.set_preview_status(
                    &self.record.id,
                    self.record.revision,
                    PreviewStatus::NotLoaded,
                )
            });
        }
    }
}

impl SerializableItem for CanvasItem {
    fn serialized_item_kind() -> &'static str {
        "CanvasItem"
    }
    fn cleanup(_: WorkspaceId, _: Vec<ItemId>, _: &mut Window, _: &mut App) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }
    fn deserialize(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>>> {
        let database = CanvasDb::global(cx);
        window.spawn(cx, async move |cx| {
            let id = database
                .get_item(workspace_id.into(), item_id as i64)?
                .context("Canvas tab was not found")?;
            let record = database.get(&id)?;
            let store = cx
                .update(|_, cx| CanvasStore::global(cx))?
                .context("Canvas runtime is unavailable")?;
            let record = store
                .update(cx, |store, cx| {
                    store.refresh(
                        record.project_key.clone(),
                        record.session_id.clone(),
                        record.path.clone(),
                        cx,
                    )
                })
                .await
                .map_err(anyhow::Error::msg)?;
            let project = store.update(cx, |store, cx| store.local_project(&project, cx));
            let buffer = project
                .update(cx, |project, cx| {
                    project.open_local_buffer(record.path.clone(), cx)
                })
                .await?;
            cx.update(|window, cx| {
                cx.new(|cx| Self::new(record, buffer, project, workspace, window, cx))
            })
        })
    }
    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let database = CanvasDb::global(cx);
        let id = self.record.id.clone();
        Some(cx.background_spawn(async move {
            database
                .save_item(workspace_id.into(), item_id as i64, id)
                .await
        }))
    }
    fn should_serialize(&self, _: &Self::Event) -> bool {
        true
    }
}

impl Render for CanvasItem {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.webview.is_none() && self.error.is_none() {
            self.reload(window, cx);
        }
        let toolbar = h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                Button::new("canvas-preview", "Preview")
                    .toggle_state(!self.source_mode)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.source_mode = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("canvas-source", "Source")
                    .toggle_state(self.source_mode)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.source_mode = true;
                        window.focus(&this.editor.focus_handle(cx), cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("canvas-save", "Save")
                    .disabled(!self.source_dirty || self.saving)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_source(cx).detach_and_log_err(cx);
                    })),
            )
            .child(
                Button::new("canvas-reload", "Reload")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh_preview(window, cx))),
            )
            .child(
                Button::new("canvas-select", "Select element")
                    .disabled(self.source_mode)
                    .on_click(cx.listener(|this, _, _, _| {
                        if let Some(view) = &this.webview {
                            view.evaluate("__zedCanvasHost.setSelecting(true)");
                        }
                    })),
            )
            .when(self.source_dirty, |toolbar| {
                toolbar.child(Button::new("canvas-discard", "Discard changes").on_click(
                    cx.listener(|this, _, window, cx| {
                        if let Some(workspace) = this.workspace.upgrade()
                            && let Some(buffer) =
                                this.editor.read(cx).buffer().read(cx).as_singleton()
                        {
                            let project = workspace.read(cx).project().clone();
                            let task = project.update(cx, |project, cx| {
                                project.reload_buffers(
                                    collections::HashSet::from_iter([buffer]),
                                    true,
                                    cx,
                                )
                            });
                            cx.spawn_in(window, async move |this, cx| {
                                task.await?;
                                this.update_in(cx, |this, window, cx| {
                                    this.source_dirty = false;
                                    this.reload(window, cx);
                                })?;
                                anyhow::Ok(())
                            })
                            .detach_and_log_err(cx);
                        }
                        cx.notify();
                    }),
                ))
            });
        let error = self.error.clone().map(|error| {
            v_flex()
                .p_2()
                .gap_2()
                .child(Label::new(error.clone()).color(Color::Error))
                .child(
                    Button::new("canvas-fix", "Fix with Agent").on_click(cx.listener(
                        move |this, _, window, cx| {
                            if let Err(error) = this.prepare_prompt(
                                format!("Please fix this Canvas error:\n{error}"),
                                None,
                                window,
                                cx,
                            ) {
                                this.error = Some(error.to_string().into());
                                cx.notify();
                            }
                        },
                    )),
                )
        });
        let selection = self.selection.clone().map(|selection| {
            h_flex()
                .gap_2()
                .p_2()
                .child(div().flex_1().child(self.feedback.clone()))
                .child(
                    Button::new("canvas-feedback", "Add to chat")
                        .disabled(self._snapshot_task.is_some())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let feedback = this.feedback.read(cx).text(cx);
                            if feedback.trim().is_empty() {
                                return;
                            }
                            let prompt = format!(
                                "Selected Canvas elements from revision {} (current revision {}), \
                                 data:\n{}\n\nRequested change: {feedback}",
                                selection.revision,
                                this.record.revision,
                                serde_json::json!(selection.elements)
                            );
                            match this.prepare_prompt(
                                prompt,
                                selection.snapshot.clone(),
                                window,
                                cx,
                            ) {
                                Ok(()) => {
                                    this.selection = None;
                                    this.feedback
                                        .update(cx, |editor, cx| editor.set_text("", window, cx));
                                }
                                Err(error) => this.error = Some(error.to_string().into()),
                            }
                            cx.notify();
                        })),
                )
        });
        let body = if self.source_mode {
            div()
                .flex_1()
                .min_h_0()
                .child(self.editor.clone())
                .into_any_element()
        } else if let Some(view) = &self.webview {
            div()
                .flex_1()
                .min_h_0()
                .child(gpui::native_view(view.clone()).size_full())
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(toolbar)
            .children(error)
            .children(selection)
            .child(body)
    }
}
