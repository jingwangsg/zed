use anyhow::{Context as _, Result, ensure};
use db::{
    query,
    sqlez::{domain::Domain, statement::Statement, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use fs::Fs;
use futures::{AsyncWriteExt, FutureExt as _, StreamExt as _, future::Shared};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use node_runtime::NodeRuntime;
use project::{LocalProjectFlags, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use util::ResultExt as _;
use uuid::Uuid;

util::fs_embed! {
    struct RuntimeAssets,
    crate_relative = "runtime/dist",
    root_relative = "crates/agent_canvas/runtime/dist",
    include = ["**/*"],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanvasRecord {
    pub id: String,
    pub path: PathBuf,
    pub project_key: String,
    pub session_id: String,
    pub title: String,
    /// Last checked snapshot; the managed TSX file is authoritative.
    pub source: String,
    pub diagnostics: Option<String>,
    pub runtime_version: String,
    pub revision: i64,
    #[serde(skip)]
    pub javascript: String,
}

impl CanvasRecord {
    pub fn uri(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", content = "error", rename_all = "snake_case")]
pub enum PreviewStatus {
    NotLoaded,
    Loaded,
    Error(String),
    Unavailable(String),
}

pub enum CanvasEvent {
    Created(CanvasRecord),
    Updated(CanvasRecord),
    StateUpdated { id: String, state: Value },
    Unavailable { id: String, message: String },
}

struct GlobalCanvasStore(Entity<CanvasStore>);
impl Global for GlobalCanvasStore {}

pub struct CanvasStore {
    node_runtime: NodeRuntime,
    fs: Arc<dyn Fs>,
    local_project: Option<Entity<Project>>,
    preview_status: HashMap<String, (i64, PreviewStatus)>,
    watches: HashMap<String, Task<()>>,
    builds: HashMap<PathBuf, Shared<Task<Result<CanvasRecord, gpui::SharedString>>>>,
}

pub fn directory(project_key: &str) -> PathBuf {
    paths::data_dir()
        .join("canvases")
        .join(format!("{:x}", Sha256::digest(project_key.as_bytes())))
}

impl EventEmitter<CanvasEvent> for CanvasStore {}

impl CanvasStore {
    pub fn init(node_runtime: NodeRuntime, fs: Arc<dyn Fs>, cx: &mut App) {
        if Self::global(cx).is_none() {
            let store = cx.new(|_| Self {
                node_runtime,
                fs,
                local_project: None,
                preview_status: HashMap::new(),
                watches: HashMap::new(),
                builds: HashMap::new(),
            });
            cx.set_global(GlobalCanvasStore(store));
        }
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalCanvasStore>()
            .map(|global| global.0.clone())
    }

    pub fn local_project(
        &mut self,
        workspace_project: &Entity<Project>,
        cx: &mut Context<Self>,
    ) -> Entity<Project> {
        self.local_project
            .get_or_insert_with(|| {
                let project = workspace_project.read(cx);
                let client = project.client();
                let user_store = project.user_store();
                let languages = project.languages().clone();
                // Canvas files belong to this machine even when the workspace is remote.
                Project::local(
                    client,
                    self.node_runtime.clone(),
                    user_store,
                    languages,
                    self.fs.clone(),
                    None,
                    LocalProjectFlags {
                        init_worktree_trust: false,
                        watch_global_configs: false,
                    },
                    cx,
                )
            })
            .clone()
    }

    pub fn register_project(&mut self, project_key: String, cx: &mut Context<Self>) {
        if self.watches.contains_key(&project_key) {
            return;
        }
        let fs = self.fs.clone();
        let root = directory(&project_key);
        let database = CanvasDb::global(cx);
        let key = project_key.clone();
        let task = cx.spawn(async move |this, cx| {
            if let Err(error) = fs.create_dir(&root).await {
                log::error!("Provisioning Canvas directory: {error:#}");
                return;
            }
            let (mut events, _watcher) = fs.watch(&root, Duration::from_millis(300)).await;
            while let Some(events) = events.next().await {
                for event in events {
                    if let Some(stem) = event
                        .path
                        .to_string_lossy()
                        .strip_suffix(".canvas.data.json")
                    {
                        match database.by_path(format!("{stem}.canvas.tsx")) {
                            Ok(Some(id)) => match database.state(&id) {
                                Ok(state) => {
                                    this.update(cx, |_, cx| {
                                        cx.emit(CanvasEvent::StateUpdated { id, state })
                                    })
                                    .log_err();
                                }
                                Err(error) => log::error!("Reading Canvas state: {error:#}"),
                            },
                            Ok(None) => {}
                            Err(error) => log::error!("Looking up Canvas state: {error:#}"),
                        }
                        continue;
                    }
                    if !event.path.to_string_lossy().ends_with(".canvas.tsx") {
                        continue;
                    }
                    let record = match database.by_path(event.path.to_string_lossy().into_owned()) {
                        Ok(Some(id)) => database.get(&id),
                        Ok(None) => continue,
                        Err(error) => {
                            log::error!("Reading Canvas index: {error:#}");
                            continue;
                        }
                    };
                    if let Ok(record) = record {
                        let id = record.id.clone();
                        let revision = record.revision;
                        let task = this.update(cx, |store, cx| {
                            store.refresh(key.clone(), record.session_id, event.path, cx)
                        });
                        match task {
                            Ok(task) => match task.await {
                                Ok(record) => {
                                    this.update(cx, |store, cx| {
                                        if store.preview_status.get(&id).is_some_and(
                                            |(_, status)| {
                                                matches!(status, PreviewStatus::Unavailable(_))
                                            },
                                        ) {
                                            store.set_preview_status(
                                                &id,
                                                record.revision,
                                                PreviewStatus::NotLoaded,
                                            );
                                            cx.emit(CanvasEvent::Updated(record));
                                        }
                                    })
                                    .log_err();
                                }
                                Err(error) => {
                                    this.update(cx, |store, cx| {
                                        store.set_preview_status(
                                            &id,
                                            revision,
                                            PreviewStatus::Unavailable(error.to_string()),
                                        );
                                        cx.emit(CanvasEvent::Unavailable {
                                            id,
                                            message: error.to_string(),
                                        });
                                    })
                                    .log_err();
                                }
                            },
                            Err(_) => return,
                        }
                    }
                }
            }
        });
        self.watches.insert(project_key, task);
    }

    pub fn resolve_path(
        &self,
        project_key: &str,
        path: &Path,
        cx: &App,
    ) -> Task<Result<Option<PathBuf>>> {
        let root = directory(project_key);
        if !path.starts_with(&root) {
            return Task::ready(Ok(None));
        }
        let path = path.to_path_buf();
        let fs = self.fs.clone();
        cx.background_spawn(async move {
            ensure!(
                path.parent() == Some(root.as_path())
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().ends_with(".canvas.tsx")),
                "Canvas files must be directly inside the managed directory and end in .canvas.tsx"
            );
            fs.create_dir(&root).await?;
            let canonical_root = fs.canonicalize(&root).await?;
            let canonical_data = fs.canonicalize(paths::data_dir()).await?;
            ensure!(
                canonical_root
                    == canonical_data.join("canvases").join(
                        root.file_name()
                            .context("Canvas directory name is missing")?
                    ),
                "Managed Canvas directory must not redirect through a symlink"
            );
            if let Some(metadata) = fs.metadata(&path).await? {
                ensure!(
                    !metadata.is_dir && !metadata.is_symlink,
                    "Canvas must be a regular file, not a directory or symlink"
                );
                let canonical = fs.canonicalize(&path).await?;
                ensure!(
                    canonical.parent() == Some(canonical_root.as_path()),
                    "Canvas path escapes its managed directory"
                );
                Ok(Some(canonical))
            } else {
                Ok(Some(canonical_root.join(
                    path.file_name().context("Canvas file name is missing")?,
                )))
            }
        })
    }

    pub fn set_preview_status(&mut self, id: &str, revision: i64, status: PreviewStatus) {
        self.preview_status
            .insert(id.to_owned(), (revision, status));
    }

    pub fn preview_status(&self, record: &CanvasRecord) -> PreviewStatus {
        if let Some(error) = &record.diagnostics {
            return PreviewStatus::Error(error.clone());
        }
        self.preview_status
            .get(&record.id)
            .filter(|(revision, _)| *revision == record.revision)
            .map(|(_, status)| status.clone())
            .unwrap_or(PreviewStatus::NotLoaded)
    }

    pub fn refresh(
        &mut self,
        project_key: String,
        session_id: String,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Shared<Task<Result<CanvasRecord, gpui::SharedString>>> {
        if let Some(task) = self.builds.get(&path) {
            return task.clone();
        }
        let fs = self.fs.clone();
        let node_runtime = self.node_runtime.clone();
        let database = CanvasDb::global(cx);
        let build_path = path.clone();
        let task = cx
            .spawn(async move |this, cx| {
                let result: Result<CanvasRecord> = async {
                    loop {
                        let source = fs.load(&build_path).await?;
                        let version = runtime_version()?;
                        let runtime = paths::data_dir().join("canvases/runtime").join(&version);
                        let config_path = build_path
                            .parent()
                            .context("Canvas directory is missing")?
                            .join("tsconfig.json");
                        let types = runtime.join("types");
                        let config = serde_json::to_string_pretty(&serde_json::json!({
                            "compilerOptions": {
                                "target": "ES2022",
                                "module": "ESNext",
                                "moduleResolution": "bundler",
                                "jsx": "react-jsx",
                                "strict": true,
                                "noEmit": true,
                                "skipLibCheck": true,
                                "lib": ["ES2022", "DOM"],
                                "types": [],
                                "paths": {
                                    "@zed/canvas": [runtime.join("sdk.d.ts")],
                                    "react": [types.join("react/index.d.ts")],
                                    "react/jsx-runtime": [types.join("react/jsx-runtime.d.ts")],
                                    "react/jsx-dev-runtime": [
                                        types.join("react/jsx-dev-runtime.d.ts")
                                    ],
                                    "csstype": [types.join("csstype/index.d.ts")]
                                }
                            },
                            "include": ["*.canvas.tsx"]
                        }))?;
                        if fs.load(&config_path).await.ok().as_deref() != Some(&config) {
                            fs.atomic_write(config_path, config).await?;
                        }
                        let existing = database
                            .by_path(build_path.to_string_lossy().into_owned())?
                            .map(|id| database.get(&id))
                            .transpose()?;
                        if let Some(record) = &existing
                            && record.source == source
                            && record.runtime_version == version
                            && record.diagnostics.is_none()
                        {
                            database
                                .link_session(session_id.clone(), record.id.clone())
                                .await?;
                            return Ok(record.clone());
                        }
                        let compiled = compile(&node_runtime, source.clone()).await;
                        // The file can change while TypeScript is running. Publish only the
                        // current contents.
                        if fs.load(&build_path).await? != source {
                            continue;
                        }
                        let was_visible = existing
                            .as_ref()
                            .is_some_and(|record| !record.javascript.is_empty());
                        let mut record = existing.unwrap_or_else(|| CanvasRecord {
                            id: Uuid::new_v4().to_string(),
                            path: build_path.clone(),
                            project_key: project_key.clone(),
                            session_id: session_id.clone(),
                            title: build_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .trim_end_matches(".canvas.tsx")
                                .replace(['-', '_'], " "),
                            source: String::new(),
                            diagnostics: None,
                            runtime_version: String::new(),
                            revision: 0,
                            javascript: String::new(),
                        });
                        record.source = source;
                        record.runtime_version = version;
                        match compiled {
                            Ok(javascript) => {
                                record.javascript = javascript;
                                record.diagnostics = None;
                            }
                            Err(error) => {
                                record.javascript.clear();
                                record.diagnostics = Some(format!("{error:#}"));
                            }
                        }
                        let Some(record) = database.save(record).await? else {
                            continue;
                        };
                        database
                            .link_session(session_id.clone(), record.id.clone())
                            .await?;
                        if fs.load(&build_path).await? != record.source {
                            continue;
                        }
                        this.update(cx, |_, cx| {
                            if !was_visible && record.diagnostics.is_none() {
                                cx.emit(CanvasEvent::Created(record.clone()));
                            } else {
                                cx.emit(CanvasEvent::Updated(record.clone()));
                            }
                        })?;
                        return Ok(record);
                    }
                }
                .await;
                this.update(cx, |store, _| {
                    store.builds.remove(&build_path);
                })
                .log_err();
                result.map_err(|error| gpui::SharedString::from(format!("{error:#}")))
            })
            .shared();
        self.builds.insert(path, task.clone());
        task
    }
}

/// Compile without evaluating generated JavaScript or resolving project packages.
pub async fn compile(node_runtime: &NodeRuntime, source: String) -> Result<String> {
    let node = node_runtime
        .binary_path()
        .await
        .context("Canvas needs the Zed Node runtime")?;
    let directory = paths::data_dir()
        .join("canvases/runtime")
        .join(runtime_version()?);
    for asset in RuntimeAssets::iter() {
        let path = directory.join(asset.as_ref());
        let data = RuntimeAssets::get(asset.as_ref()).context("Missing Canvas runtime asset")?;
        if smol::fs::read(&path).await.ok().as_deref() != Some(data.data.as_ref()) {
            smol::fs::create_dir_all(path.parent().context("Invalid runtime asset path")?).await?;
            let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
            smol::fs::write(&temporary, data.data.as_ref()).await?;
            smol::fs::rename(temporary, path).await?;
        }
    }
    let mut command = smol::process::Command::new(&node);
    let mut executable_paths = vec![
        node.parent()
            .context("Node runtime has no parent directory")?
            .to_path_buf(),
    ];
    executable_paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    command
        .arg(directory.join("compile.cjs"))
        .env("PATH", std::env::join_paths(executable_paths)?)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("Starting the Canvas compiler")?;
    let mut stdin = child
        .stdin
        .take()
        .context("Canvas compiler stdin is unavailable")?;
    stdin.write_all(source.as_bytes()).await?;
    drop(stdin);
    let output = child.output().await?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn runtime_version() -> Result<String> {
    Ok(String::from_utf8(runtime_asset("version")?)?
        .trim()
        .to_owned())
}

pub fn runtime_asset(name: &str) -> Result<Vec<u8>> {
    Ok(RuntimeAssets::get(name)
        .context("Canvas runtime asset is missing")?
        .data
        .to_vec())
}

pub struct CanvasDb(ThreadSafeConnection);

impl Domain for CanvasDb {
    const NAME: &str = "CanvasDb";
    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE agent_canvases (
            id TEXT PRIMARY KEY NOT NULL,
            path TEXT NOT NULL UNIQUE,
            project_key TEXT NOT NULL,
            session_id TEXT NOT NULL,
            title TEXT NOT NULL,
            source TEXT NOT NULL,
            revision INTEGER NOT NULL,
            javascript TEXT NOT NULL,
            diagnostics TEXT,
            runtime_version TEXT NOT NULL
        ) STRICT;
        CREATE TABLE agent_canvas_sessions (
            session_id TEXT NOT NULL,
            canvas_id TEXT NOT NULL REFERENCES agent_canvases(id) ON DELETE CASCADE,
            PRIMARY KEY(session_id, canvas_id)
        ) STRICT;
        CREATE TABLE agent_canvas_items (
            workspace_id INTEGER NOT NULL,
            item_id INTEGER NOT NULL,
            canvas_id TEXT NOT NULL REFERENCES agent_canvases(id) ON DELETE CASCADE,
            PRIMARY KEY(workspace_id, item_id)
        ) STRICT;
    )];
}

db::static_connection!(CanvasDb, []);

impl CanvasDb {
    pub fn get(&self, id: &str) -> Result<CanvasRecord> {
        Uuid::parse_str(id).context("Invalid Canvas ID")?;
        let mut statement = Statement::prepare(
            &self.0,
            "SELECT id, path, project_key, session_id, title, source, revision, javascript, \
             diagnostics, runtime_version FROM agent_canvases WHERE id = ?",
        )?;
        statement.bind(&id, 1)?;
        statement
            .maybe(|row| {
                Ok(CanvasRecord {
                    id: row.column_text(0)?.into(),
                    path: PathBuf::from(row.column_text(1)?),
                    project_key: row.column_text(2)?.into(),
                    session_id: row.column_text(3)?.into(),
                    title: row.column_text(4)?.into(),
                    source: row.column_text(5)?.into(),
                    revision: row.column_int64(6)?,
                    javascript: row.column_text(7)?.into(),
                    diagnostics: if row.column_type(8)? == db::sqlez::statement::SqlType::Null {
                        None
                    } else {
                        Some(row.column_text(8)?.into())
                    },
                    runtime_version: row.column_text(9)?.into(),
                })
            })?
            .context("Canvas no longer exists")
    }

    pub async fn save(&self, mut record: CanvasRecord) -> Result<Option<CanvasRecord>> {
        self.write(move |connection| {
            let changed = if record.revision == 0 {
                let insert = "INSERT INTO agent_canvases VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?) \
                              ON CONFLICT(path) DO NOTHING RETURNING revision";
                Statement::prepare(connection, insert)?
                    .with_bindings(&(
                        record.id.clone(),
                        record.path.to_string_lossy().into_owned(),
                        record.project_key.clone(),
                        record.session_id.clone(),
                        record.title.clone(),
                        record.source.clone(),
                        record.javascript.clone(),
                        record.diagnostics.clone(),
                        record.runtime_version.clone(),
                    ))?
                    .maybe_row::<i64>()?
            } else {
                let update = "UPDATE agent_canvases SET source = ?, javascript = ?, \
                              diagnostics = ?, runtime_version = ?, revision = revision + 1 \
                              WHERE id = ? AND revision = ? RETURNING revision";
                Statement::prepare(connection, update)?
                    .with_bindings(&(
                        record.source.clone(),
                        record.javascript.clone(),
                        record.diagnostics.clone(),
                        record.runtime_version.clone(),
                        record.id.clone(),
                        record.revision,
                    ))?
                    .maybe_row::<i64>()?
            };
            Ok(changed.map(|revision| {
                record.revision = revision;
                record
            }))
        })
        .await
    }

    pub fn state(&self, id: &str) -> Result<Value> {
        let path = self.get(id)?.path.with_extension("data.json");
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "Canvas state must not be a symlink"
            );
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(serde_json::from_str(&contents)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn set_state(
        &self,
        id: String,
        revision: i64,
        key: String,
        value: Value,
    ) -> Result<()> {
        ensure!(
            !key.is_empty() && key.len() <= 256,
            "Invalid Canvas state key"
        );
        self.write(move |connection| {
            let path = Statement::prepare(
                connection,
                "SELECT path FROM agent_canvases WHERE id = ? AND revision = ?",
            )?
            .with_bindings(&(id, revision))?
            .maybe_row::<String>()?;
            let Some(path) = path else {
                return Ok(());
            };
            let path = PathBuf::from(path).with_extension("data.json");
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "Canvas state must not be a symlink"
                );
            }
            let mut state: serde_json::Map<String, Value> = match std::fs::read_to_string(&path) {
                Ok(contents) => serde_json::from_str(&contents)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    serde_json::Map::new()
                }
                Err(error) => return Err(error.into()),
            };
            state.insert(key, value);
            let contents = serde_json::to_vec_pretty(&state)?;
            ensure!(contents.len() <= 1024 * 1024, "Canvas state exceeds 1 MiB");
            let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
            std::fs::write(&temporary, contents)?;
            std::fs::rename(temporary, path)?;
            Ok(())
        })
        .await
    }

    query! { pub fn by_path(path: String) -> Result<Option<String>> { SELECT id FROM agent_canvases WHERE path = ? } }
    query! { pub async fn link_session(session_id: String, canvas_id: String) -> Result<()> { INSERT OR IGNORE INTO agent_canvas_sessions VALUES (?, ?) } }
    query! {
        pub fn for_session(session_id: String) -> Result<Vec<(String, String, i64)>> {
            SELECT path, title, revision FROM agent_canvases JOIN agent_canvas_sessions ON agent_canvases.id = agent_canvas_sessions.canvas_id WHERE agent_canvas_sessions.session_id = ? ORDER BY path
        }
    }
    query! { pub async fn save_item(workspace_id: i64, item_id: i64, canvas_id: String) -> Result<()> { INSERT OR REPLACE INTO agent_canvas_items(workspace_id, item_id, canvas_id) VALUES (?, ?, ?) } }
    query! { pub fn get_item(workspace_id: i64, item_id: i64) -> Result<Option<String>> { SELECT canvas_id FROM agent_canvas_items WHERE workspace_id = ? AND item_id = ? } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    async fn source_conflicts_do_not_overwrite_state(cx: &mut gpui::TestAppContext) {
        let database = cx.update(|cx| CanvasDb::global(cx));
        let directory = tempfile::tempdir().unwrap();
        let record = CanvasRecord {
            id: Uuid::new_v4().to_string(),
            path: directory.path().join("report.canvas.tsx"),
            project_key: "project".into(),
            session_id: "session".into(),
            title: "Report".into(),
            source: "original".into(),
            diagnostics: None,
            runtime_version: "test".into(),
            revision: 0,
            javascript: "original".into(),
        };
        let record = database.save(record).await.unwrap().unwrap();
        database
            .set_state(
                record.id.clone(),
                1,
                "filter".into(),
                serde_json::json!("active"),
            )
            .await
            .unwrap();
        let mut update = record.clone();
        update.source = "updated".into();
        let updated = database.save(update).await.unwrap().unwrap();
        assert_eq!(updated.revision, 2);
        assert!(database.save(record.clone()).await.unwrap().is_none());
        assert_eq!(database.get(&record.id).unwrap().source, "updated");
        assert_eq!(database.state(&record.id).unwrap()["filter"], "active");
        database
            .set_state(
                record.id.clone(),
                1,
                "filter".into(),
                serde_json::json!("stale frame"),
            )
            .await
            .unwrap();
        assert_eq!(database.state(&record.id).unwrap()["filter"], "active");
        assert!(record.path.with_extension("data.json").is_file());
    }
}
