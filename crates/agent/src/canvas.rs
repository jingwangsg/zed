use agent_canvas::CanvasStore;
use anyhow::Result;
use gpui::{App, Task};
use project::Project;
use std::path::{Path, PathBuf};

pub(crate) fn project_key(project: &Project, cx: &App) -> String {
    let mut roots: Vec<_> = project
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect();
    roots.sort();
    serde_json::json!({ "remote": project.remote_connection_options(cx), "roots": roots })
        .to_string()
}

pub(crate) fn directory(project: &Project, cx: &App) -> Option<PathBuf> {
    (cfg!(target_os = "macos") && CanvasStore::global(cx).is_some())
        .then(|| agent_canvas::directory(&project_key(project, cx)))
}

pub(crate) fn resolve_path(
    project: &Project,
    path: &Path,
    cx: &App,
) -> Task<Result<Option<PathBuf>>> {
    match CanvasStore::global(cx).filter(|_| cfg!(target_os = "macos")) {
        Some(store) => store
            .read(cx)
            .resolve_path(&project_key(project, cx), path, cx),
        None => Task::ready(Ok(None)),
    }
}
