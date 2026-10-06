use crate::branch_diff::BranchDiff;
use crate::commit_view::CommitView;
use crate::picker_prompt::text_prompt;
use crate::reference_picker::{self, Comparison, PickedReference, ReferenceKind};
use crate::report_git_operation;
use anyhow::{Context as _, Result};
use futures::channel::oneshot;
use git::Oid;
use git::repository::{MergeKind, RepoPath, ResetMode};
use gpui::{
    Action, App, ClipboardItem, Context, Entity, FocusHandle, Global, PromptLevel, SharedString,
    Task, WeakEntity, Window, actions,
};
use project::{
    GIT_COMMAND_TASK_TAG,
    git_store::{Repository, RepositoryId, diff_buffer_list::DiffBase},
};

use task::{TaskContext, TaskVariables, VariableName};
use ui::{Color, ContextMenu, ContextMenuEntry, IconName, IconPosition, prelude::*};
use workspace::Workspace;
use workspace::notifications::NotifyTaskExt;

actions!(
    git_graph,
    [
        /// Copies the SHA of the selected commit to the clipboard.
        CopyCommitSha,
        /// Copies a tag from the selected commit to the clipboard.
        CopyCommitTag,
        /// Opens the commit view for the selected commit.
        OpenCommitView,
    ]
);

const COMMIT_TAG_LIST_WIDTH_IN_REMS: Rems = rems(10.);
const CUSTOM_GIT_COMMANDS_DOCS_SLUG: &str = "tasks#custom-git-commands";

pub(crate) struct CommitContextMenuData {
    pub(crate) sha: Oid,
    pub(crate) tag_names: Vec<SharedString>,
    /// Set when the commit was listed in a file's history, which makes opening that file
    /// at the commit meaningful.
    pub(crate) history_path: Option<RepoPath>,
}

/// The commit or ref chosen through "Select for Compare", offered as the base of the next
/// comparison from any commit or ref menu of the same repository.
struct CompareSelection {
    repository_id: RepositoryId,
    reference: PickedReference,
}

impl Global for CompareSelection {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitContextMenuSource {
    GitGraph,
    GitPanel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefKind {
    LocalBranch { is_head: bool },
    RemoteBranch,
    Tag,
    Unknown,
}

/// Classifies a ref chip's name, since the decoration string alone cannot tell a
/// local branch named `origin/main` apart from a remote-tracking ref.
fn ref_kind(
    ref_name: &str,
    tag_names: &[SharedString],
    repository: Option<&Entity<Repository>>,
    cx: &App,
) -> RefKind {
    if let Some(repository) = repository {
        let local_ref = format!("refs/heads/{ref_name}");
        let remote_ref = format!("refs/remotes/{ref_name}");
        for branch in repository.read(cx).branch_list.iter() {
            if branch.ref_name.as_ref() == local_ref {
                return RefKind::LocalBranch {
                    is_head: branch.is_head,
                };
            }
            if branch.ref_name.as_ref() == remote_ref {
                return RefKind::RemoteBranch;
            }
        }
    }
    if tag_names
        .iter()
        .any(|tag_name| tag_name.as_ref() == ref_name)
    {
        RefKind::Tag
    } else {
        RefKind::Unknown
    }
}

fn run_repository_job(
    repository: &WeakEntity<Repository>,
    workspace: &WeakEntity<Workspace>,
    action: impl Into<SharedString>,
    job: impl FnOnce(&mut Repository, &mut Context<Repository>) -> oneshot::Receiver<Result<()>>,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    let receiver = repository.update(cx, |repository, cx| job(repository, cx));
    report_git_operation(
        workspace.clone(),
        action,
        async move { receiver.await? },
        cx,
    );
}

pub(crate) fn commit_context_menu(
    commit: CommitContextMenuData,
    source: CommitContextMenuSource,
    ref_name: Option<SharedString>,
    focus_handle: FocusHandle,
    repository: Option<WeakEntity<Repository>>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    let sha = commit.sha;
    let sha_short = sha.display_short();
    let git_tasks = git_context_menu_tasks(
        git_task_context(&repository, sha, ref_name.as_deref(), cx),
        &workspace,
        cx,
    );
    let header = match &ref_name {
        Some(ref_name) => format!("Ref {ref_name}"),
        None => format!("Commit {sha_short}"),
    };
    let strong_repository = repository
        .as_ref()
        .and_then(|repository| repository.upgrade());
    let is_head_commit = strong_repository.as_ref().is_some_and(|repository| {
        repository
            .read(cx)
            .head_commit
            .as_ref()
            .is_some_and(|head| head.sha.as_ref() == sha.to_string())
    });
    let ref_target = ref_name.clone().map(|ref_name| {
        let kind = ref_kind(&ref_name, &commit.tag_names, strong_repository.as_ref(), cx);
        (ref_name, kind)
    });
    let repository_id = strong_repository
        .as_ref()
        .map(|repository| repository.read(cx).id);
    let compare_selection = cx
        .try_global::<CompareSelection>()
        .filter(|selection| Some(selection.repository_id) == repository_id)
        .map(|selection| selection.reference.clone());

    ContextMenu::build(window, cx, move |context_menu, _, _| {
        context_menu
            .context(focus_handle)
            .header(header)
            .entry("View Diff", Some(OpenCommitView.boxed_clone()), {
                let repository = repository.clone();
                let workspace = workspace.clone();
                move |window, cx| {
                    let Some(repository) = repository.clone() else {
                        return;
                    };
                    CommitView::open(
                        sha.to_string(),
                        repository,
                        workspace.clone(),
                        None,
                        None,
                        window,
                        cx,
                    );
                }
            })
            .entry(
                "Copy SHA",
                Some(CopyCommitSha.boxed_clone()),
                move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(sha.to_string()));
                },
            )
            .when_some(ref_name.clone(), |menu, ref_name| {
                menu.entry("Copy Ref Name", None, move |_window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(ref_name.to_string()));
                })
            })
            .when(ref_name.is_none(), |menu| {
                menu.map(|menu| {
                    let tag_names = commit.tag_names.clone();
                    let copy_tag_label = "Copy Tag";

                    match tag_names.as_slice() {
                        [] => menu.item(
                            ContextMenuEntry::new(copy_tag_label)
                                .action(CopyCommitTag.boxed_clone())
                                .disabled(true),
                        ),
                        [tag_name] => {
                            let tag_name = tag_name.clone();
                            let label = format!("{copy_tag_label}: {tag_name}");
                            menu.entry(
                                label,
                                Some(CopyCommitTag.boxed_clone()),
                                move |_window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        tag_name.to_string(),
                                    ));
                                },
                            )
                        }
                        _ => menu.submenu(copy_tag_label, move |menu, _window, _cx| {
                            let mut menu = menu.fixed_width(COMMIT_TAG_LIST_WIDTH_IN_REMS.into());

                            for tag_name in tag_names.clone() {
                                let tag_name_to_copy = tag_name.clone();
                                menu = menu.entry(tag_name, None, move |_window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        tag_name_to_copy.to_string(),
                                    ));
                                });
                            }
                            menu
                        }),
                    }
                })
            })
            .when(source == CommitContextMenuSource::GitPanel, |menu| {
                menu.entry("Show in Git Graph", None, move |window, cx| {
                    window.dispatch_action(
                        Box::new(crate::git_graph::OpenAtCommit {
                            sha: sha.to_string(),
                        }),
                        cx,
                    );
                })
            })
            .when_some(repository.clone(), |menu, repository| match &ref_target {
                None => commit_operation_entries(
                    menu,
                    sha,
                    is_head_commit,
                    commit.history_path.clone(),
                    repository,
                    workspace.clone(),
                ),
                Some((ref_name, ref_kind)) => ref_operation_entries(
                    menu,
                    ref_name.clone(),
                    *ref_kind,
                    repository,
                    workspace.clone(),
                ),
            })
            .when_some(
                repository.clone().zip(repository_id),
                |menu, (repository, repository_id)| {
                    let (reference, is_head) = match &ref_target {
                        None => (PickedReference::revision(sha.to_string()), is_head_commit),
                        Some((ref_name, ref_kind)) => {
                            let kind = match ref_kind {
                                RefKind::LocalBranch { .. } => ReferenceKind::LocalBranch,
                                RefKind::RemoteBranch => ReferenceKind::RemoteBranch,
                                RefKind::Tag => ReferenceKind::Tag,
                                RefKind::Unknown => ReferenceKind::Revision,
                            };
                            (
                                PickedReference {
                                    kind,
                                    name: ref_name.clone(),
                                },
                                matches!(ref_kind, RefKind::LocalBranch { is_head: true }),
                            )
                        }
                    };
                    compare_entries(
                        menu,
                        reference,
                        is_head,
                        compare_selection.clone(),
                        repository_id,
                        repository,
                        workspace.clone(),
                    )
                },
            )
            .map(|mut menu| {
                menu = menu.separator().header("Custom Commands");

                if git_tasks.is_empty() {
                    return menu.item(
                        ContextMenuEntry::new("Learn More")
                            .icon(IconName::ArrowUpRight)
                            .icon_color(Color::Muted)
                            .icon_position(IconPosition::End)
                            .handler(|_window, cx| {
                                let docs_url =
                                    release_channel::docs_url(CUSTOM_GIT_COMMANDS_DOCS_SLUG, cx);
                                cx.open_url(&docs_url);
                            }),
                    );
                }

                for (task_source_kind, resolved_task) in git_tasks {
                    let label = resolved_task.display_label().to_string();
                    let workspace = workspace.clone();
                    menu = menu.entry(label, None, move |window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.schedule_resolved_task(
                                    task_source_kind.clone(),
                                    resolved_task.clone(),
                                    false,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    });
                }

                menu
            })
    })
}

fn open_comparison_or_notify(
    comparison: Comparison,
    repository: &WeakEntity<Repository>,
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(repository) = repository.upgrade() else {
        return;
    };
    Task::ready(reference_picker::open_comparison(
        comparison,
        repository,
        workspace.clone(),
        window,
        cx,
    ))
    .detach_and_notify_err(workspace.clone(), window, cx);
}

/// The newer commit becomes the side being compared so the diff reads forward in time.
/// GitLens orders the two by ancestry and then by date; the dates alone are what this
/// repository can tell cheaply, and they agree whenever the commits are related.
fn compare_with_head(
    reference: PickedReference,
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<()>> {
    let (reference_details, head_details) = repository.update(cx, |repository, _| {
        (
            repository.show(reference.name.to_string()),
            repository.show("HEAD".to_string()),
        )
    });
    window.spawn(cx, async move |cx| {
        let (reference_details, head_details) = futures::join!(reference_details, head_details);
        let reference_details = reference_details.context("the commit load was cancelled")??;
        let head_details = head_details.context("the commit load was cancelled")??;
        let comparison = if reference_details.commit_timestamp > head_details.commit_timestamp {
            Comparison {
                head: reference,
                base: PickedReference::head(),
            }
        } else {
            Comparison {
                head: PickedReference::head(),
                base: reference,
            }
        };
        cx.update(|window, cx| {
            reference_picker::open_comparison(comparison, repository, workspace, window, cx)
        })?
    })
}

fn compare_entries(
    menu: ContextMenu,
    reference: PickedReference,
    is_head: bool,
    compare_selection: Option<PickedReference>,
    repository_id: RepositoryId,
    repository: WeakEntity<Repository>,
    workspace: WeakEntity<Workspace>,
) -> ContextMenu {
    let is_branch = matches!(
        reference.kind,
        ReferenceKind::LocalBranch | ReferenceKind::RemoteBranch
    );
    menu.separator()
        .entry("Compare with Working Tree", None, {
            let repository = repository.clone();
            let workspace = workspace.clone();
            let reference = reference.clone();
            move |window, cx| {
                open_comparison_or_notify(
                    Comparison {
                        head: PickedReference::working_tree(),
                        base: reference.clone(),
                    },
                    &repository,
                    &workspace,
                    window,
                    cx,
                );
            }
        })
        .when(is_branch, |menu| {
            menu.entry("Compare Ancestry with Working Tree", None, {
                let repository = repository.clone();
                let workspace = workspace.clone();
                let reference = reference.clone();
                move |window, cx| {
                    let Some(repository) = repository.upgrade() else {
                        return;
                    };
                    workspace
                        .update(cx, |workspace, cx| {
                            let project = workspace.project().clone();
                            BranchDiff::deploy_branch_diff_with_base(
                                workspace,
                                project,
                                repository,
                                DiffBase::Merge {
                                    base_ref: reference.name.clone(),
                                },
                                None,
                                window,
                                cx,
                            );
                        })
                        .ok();
                }
            })
        })
        .item(
            ContextMenuEntry::new("Compare with HEAD")
                .disabled(is_head)
                .handler({
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    let reference = reference.clone();
                    move |window, cx| {
                        let Some(repository) = repository.upgrade() else {
                            return;
                        };
                        compare_with_head(
                            reference.clone(),
                            repository,
                            workspace.clone(),
                            window,
                            cx,
                        )
                        .detach_and_notify_err(
                            workspace.clone(),
                            window,
                            cx,
                        );
                    }
                }),
        )
        .when_some(
            compare_selection.filter(|selected| selected.name != reference.name),
            |menu, selected| {
                let repository = repository.clone();
                let workspace = workspace.clone();
                let reference = reference.clone();
                menu.entry(
                    format!("Compare with {}", selected.label()),
                    None,
                    move |window, cx| {
                        // GitLens compares the selection with the clicked ref, selection
                        // first, so the diff runs from here to the selection.
                        open_comparison_or_notify(
                            Comparison {
                                head: selected.clone(),
                                base: reference.clone(),
                            },
                            &repository,
                            &workspace,
                            window,
                            cx,
                        );
                    },
                )
            },
        )
        .entry("Select for Compare", None, move |_window, cx| {
            cx.set_global(CompareSelection {
                repository_id,
                reference: reference.clone(),
            });
        })
}

fn commit_operation_entries(
    menu: ContextMenu,
    sha: Oid,
    is_head_commit: bool,
    history_path: Option<RepoPath>,
    repository: WeakEntity<Repository>,
    workspace: WeakEntity<Workspace>,
) -> ContextMenu {
    let sha_short = SharedString::from(sha.display_short());
    menu.separator()
        .when_some(history_path, |menu, path| {
            let repository = repository.clone();
            let workspace = workspace.clone();
            menu.entry("Open File at This Commit", None, move |window, cx| {
                let Some(repository) = repository.upgrade() else {
                    return;
                };
                crate::blame_ui::open_buffer_blame_at_revision(
                    repository,
                    workspace.clone(),
                    path.clone(),
                    sha,
                    window,
                    cx,
                );
            })
        })
        .item(
            ContextMenuEntry::new("Checkout Commit (Detached HEAD)")
                .disabled(is_head_commit)
                .handler({
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "checkout",
                            |repository, cx| repository.checkout_commit(sha.to_string(), cx),
                            cx,
                        );
                    }
                }),
        )
        .item(
            ContextMenuEntry::new("Cherry-Pick Commit")
                .disabled(is_head_commit)
                .handler({
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "cherry-pick",
                            |repository, cx| repository.cherry_pick(vec![sha.to_string()], cx),
                            cx,
                        );
                    }
                }),
        )
        .entry("Revert Commit", None, {
            let repository = repository.clone();
            let workspace = workspace.clone();
            move |_window, cx| {
                run_repository_job(
                    &repository,
                    &workspace,
                    "revert",
                    |repository, cx| repository.revert(vec![sha.to_string()], cx),
                    cx,
                );
            }
        })
        .submenu("Reset Current Branch to Commit", {
            let repository = repository.clone();
            let workspace = workspace.clone();
            let sha_short = sha_short.clone();
            move |menu, _window, _cx| {
                menu.entry("Soft: Keep Changes Staged", None, {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "reset",
                            |repository, cx| repository.reset(sha.to_string(), ResetMode::Soft, cx),
                            cx,
                        );
                    }
                })
                .entry("Mixed: Keep Changes Unstaged", None, {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "reset",
                            |repository, cx| repository.reset(sha.to_string(), ResetMode::Mixed, cx),
                            cx,
                        );
                    }
                })
                .entry("Hard: Discard Changes", None, {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    let sha_short = sha_short.clone();
                    move |window, cx| {
                        let answer = window.prompt(
                            PromptLevel::Warning,
                            &format!("Reset the current branch to {sha_short}?"),
                            Some("A hard reset discards all uncommitted changes in the working tree."),
                            &["Reset", "Cancel"],
                            cx,
                        );
                        let repository = repository.clone();
                        let workspace = workspace.clone();
                        cx.spawn(async move |cx| {
                            if answer.await != Ok(0) {
                                return;
                            }
                            cx.update(|cx| {
                                run_repository_job(
                                    &repository,
                                    &workspace,
                                    "reset",
                                    |repository, cx| {
                                        repository.reset(sha.to_string(), ResetMode::Hard, cx)
                                    },
                                    cx,
                                );
                            });
                        })
                        .detach();
                    }
                })
            }
        })
        .entry("Create and Switch to Branch at Commit…", None, {
            let repository = repository.clone();
            let workspace = workspace.clone();
            let sha_short = sha_short.clone();
            move |window, cx| {
                let name = text_prompt(
                    format!("Create branch at {sha_short}"),
                    "Branch name",
                    workspace.clone(),
                    window,
                    cx,
                );
                let repository = repository.clone();
                let workspace = workspace.clone();
                cx.spawn(async move |cx| {
                    let Some(name) = name.await.filter(|name| !name.is_empty()) else {
                        return;
                    };
                    cx.update(|cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "switch -c",
                            |repository, _| repository.create_branch(name, Some(sha.to_string())),
                            cx,
                        );
                    });
                })
                .detach();
            }
        })
        .entry("Create Tag at Commit…", None, {
            move |window, cx| {
                let name = text_prompt(
                    format!("Create tag at {sha_short}"),
                    "Tag name",
                    workspace.clone(),
                    window,
                    cx,
                );
                let repository = repository.clone();
                let workspace = workspace.clone();
                window
                    .spawn(cx, async move |cx| {
                        let Some(name) = name.await.filter(|name| !name.is_empty()) else {
                            return;
                        };
                        let Ok(message) = cx.update(|window, cx| {
                            text_prompt(
                                format!("Message for tag {name} (empty for a lightweight tag)"),
                                "Tag message",
                                workspace.clone(),
                                window,
                                cx,
                            )
                        }) else {
                            return;
                        };
                        let Some(message) = message.await else {
                            return;
                        };
                        let message = (!message.is_empty()).then_some(message);
                        let Some(repository) = repository.upgrade() else {
                            return;
                        };
                        cx.update(|_, cx| {
                            let task = repository.update(cx, |repository, cx| {
                                repository.create_tag(name, sha.to_string(), message, cx)
                            });
                            report_git_operation(workspace.clone(), "tag", task, cx);
                        })
                        .ok();
                    })
                    .detach();
            }
        })
}

fn ref_operation_entries(
    menu: ContextMenu,
    ref_name: SharedString,
    ref_kind: RefKind,
    repository: WeakEntity<Repository>,
    workspace: WeakEntity<Workspace>,
) -> ContextMenu {
    match ref_kind {
        RefKind::LocalBranch { is_head: true } | RefKind::Unknown => menu,
        RefKind::LocalBranch { is_head: false } | RefKind::RemoteBranch => {
            let is_local = matches!(ref_kind, RefKind::LocalBranch { .. });
            menu.separator()
                .entry("Switch to Branch", None, {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    let ref_name = ref_name.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "switch",
                            |repository, _| repository.change_branch(ref_name.to_string()),
                            cx,
                        );
                    }
                })
                .submenu("Merge into Current Branch", {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    let ref_name = ref_name.clone();
                    move |mut menu, _window, _cx| {
                        for (label, kind) in [
                            ("Merge", MergeKind::Default),
                            ("Merge (No Fast-Forward)", MergeKind::NoFastForward),
                            ("Merge (Fast-Forward Only)", MergeKind::FastForwardOnly),
                            ("Squash Merge", MergeKind::Squash),
                        ] {
                            let repository = repository.clone();
                            let workspace = workspace.clone();
                            let ref_name = ref_name.clone();
                            menu = menu.entry(label, None, move |_window, cx| {
                                run_repository_job(
                                    &repository,
                                    &workspace,
                                    "merge",
                                    |repository, cx| {
                                        repository.merge(ref_name.to_string(), kind, cx)
                                    },
                                    cx,
                                );
                            });
                        }
                        menu
                    }
                })
                .entry(format!("Rebase Current Branch onto {ref_name}"), None, {
                    let repository = repository.clone();
                    let workspace = workspace.clone();
                    let ref_name = ref_name.clone();
                    move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "rebase",
                            |repository, cx| repository.rebase(ref_name.to_string(), cx),
                            cx,
                        );
                    }
                })
                .when(is_local, |menu| {
                    menu.entry("Delete Branch", None, move |_window, cx| {
                        run_repository_job(
                            &repository,
                            &workspace,
                            "branch -d",
                            |repository, _| {
                                repository.delete_branch(false, ref_name.to_string(), false)
                            },
                            cx,
                        );
                    })
                })
        }
        RefKind::Tag => menu
            .separator()
            .entry("Checkout Tag (Detached HEAD)", None, {
                let repository = repository.clone();
                let workspace = workspace.clone();
                let ref_name = ref_name.clone();
                move |_window, cx| {
                    run_repository_job(
                        &repository,
                        &workspace,
                        "checkout",
                        |repository, cx| repository.checkout_commit(ref_name.to_string(), cx),
                        cx,
                    );
                }
            })
            .entry("Delete Tag", None, move |_window, cx| {
                let Some(repository) = repository.upgrade() else {
                    return;
                };
                let task = repository.update(cx, |repository, cx| {
                    repository.delete_tag(ref_name.to_string(), cx)
                });
                report_git_operation(workspace.clone(), "tag -d", task, cx);
            }),
    }
}

fn git_task_context(
    repository: &Option<WeakEntity<Repository>>,
    commit_sha: git::Oid,
    ref_name: Option<&str>,
    cx: &App,
) -> Option<TaskContext> {
    let repository_path = repository
        .as_ref()?
        .upgrade()?
        .read(cx)
        .work_directory_abs_path
        .to_path_buf();
    let repository_name = repository_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string);
    let mut task_variables = TaskVariables::from_iter([
        (VariableName::GitSha, commit_sha.to_string()),
        (VariableName::GitShaShort, commit_sha.display_short()),
        (
            VariableName::GitRepositoryPath,
            repository_path.to_string_lossy().into_owned(),
        ),
    ]);

    if let Some(repository_name) = repository_name {
        task_variables.insert(VariableName::GitRepositoryName, repository_name);
    }
    if let Some(ref_name) = ref_name {
        task_variables.insert(VariableName::GitRef, ref_name.to_string());
    }

    Some(TaskContext {
        cwd: Some(repository_path),
        task_variables,
        ..TaskContext::default()
    })
}

fn git_context_menu_tasks(
    task_context: Option<TaskContext>,
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> Vec<(project::TaskSourceKind, task::ResolvedTask)> {
    let Some(task_context) = task_context else {
        return Vec::new();
    };
    let Some(workspace) = workspace.upgrade() else {
        return Vec::new();
    };
    let project = workspace.read(cx).project().clone();
    let task_inventory = project.read_with(cx, |project, cx| {
        project.task_store().read(cx).task_inventory().cloned()
    });
    let Some(task_inventory) = task_inventory else {
        return Vec::new();
    };

    task_inventory
        .read(cx)
        .resolve_global_tasks_with_tag(GIT_COMMAND_TASK_TAG, &task_context)
}
