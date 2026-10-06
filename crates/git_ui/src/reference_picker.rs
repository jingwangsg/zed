//! Picks a branch, tag, or commit the way GitLens's reference picker does: the working tree
//! and HEAD first, then branches, tags, and remote branches, with a typed revision looked up
//! and offered as a commit.

use crate::branch_diff::BranchDiff;
use crate::commit_view::{CommitView, short_ref};
use anyhow::Result;
use futures::channel::oneshot;
use fuzzy::{StringMatch, StringMatchCandidate};
use git::Oid;
use git::repository::{Branch, CommitDetails, Tag};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Subscription, Task,
    WeakEntity, Window, rems,
};
use picker::{Picker, PickerDelegate};
use project::git_store::{Repository, diff_buffer_list::DiffBase};
use std::{cmp::Reverse, sync::Arc, time::Duration};
use time::OffsetDateTime;
use ui::{HighlightedLabel, ListItem, ListItemSpacing, prelude::*};
use util::ResultExt;
use workspace::{ModalView, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReferenceKind {
    WorkingTree,
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
    /// A commit or any other revision the user typed.
    Revision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickedReference {
    pub kind: ReferenceKind,
    /// What git is given: empty for the working tree, `HEAD`, a branch or tag name, or a
    /// typed revision.
    pub name: SharedString,
}

impl PickedReference {
    pub(crate) fn working_tree() -> Self {
        Self {
            kind: ReferenceKind::WorkingTree,
            name: SharedString::default(),
        }
    }

    pub(crate) fn head() -> Self {
        Self {
            kind: ReferenceKind::Head,
            name: "HEAD".into(),
        }
    }

    pub(crate) fn revision(name: impl Into<SharedString>) -> Self {
        Self {
            kind: ReferenceKind::Revision,
            name: name.into(),
        }
    }

    pub(crate) fn label(&self) -> SharedString {
        match self.kind {
            ReferenceKind::WorkingTree => "Working Tree".into(),
            ReferenceKind::Revision => short_ref(&self.name).to_string().into(),
            _ => self.name.clone(),
        }
    }
}

/// `head` is the side being compared and `base` what it is compared with, so the diff reads
/// as the changes from `base` to `head`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Comparison {
    pub head: PickedReference,
    pub base: PickedReference,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReferencePick {
    Reference(PickedReference),
    Range(Comparison),
}

pub(crate) struct ReferencePickerOptions {
    pub title: SharedString,
    pub placeholder: SharedString,
    pub include_working_tree: bool,
    pub exclude: Option<SharedString>,
    pub allow_range: bool,
    pub picked: Option<SharedString>,
}

struct Entry {
    reference: PickedReference,
    description: SharedString,
}

fn relative_time(timestamp: i64) -> Option<String> {
    let time = OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    Some(time_format::format_localized_timestamp(
        time,
        OffsetDateTime::now_utc(),
        local_offset,
        time_format::TimestampFormat::Relative,
    ))
}

fn build_entries(
    branches: &[Branch],
    tags: &[Tag],
    options: &ReferencePickerOptions,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    if options.include_working_tree {
        entries.push(Entry {
            reference: PickedReference::working_tree(),
            description: SharedString::default(),
        });
    }
    entries.push(Entry {
        reference: PickedReference::head(),
        description: SharedString::default(),
    });

    let mut branches: Vec<&Branch> = branches.iter().collect();
    // The current branch first, then the most recently committed to, as GitLens sorts them.
    branches.sort_by_key(|branch| Reverse(branch.priority_key()));
    let branch_entry = |branch: &Branch| {
        let mut parts = Vec::new();
        if branch.is_remote() {
            parts.push("remote branch".to_string());
        } else if branch.is_head {
            parts.push("current branch".to_string());
        }
        if let Some(status) = branch.tracking_status()
            && (status.ahead > 0 || status.behind > 0)
        {
            parts.push(format!("↑{} ↓{}", status.ahead, status.behind));
        }
        if let Some(time) = branch
            .most_recent_commit
            .as_ref()
            .and_then(|commit| relative_time(commit.commit_timestamp))
        {
            parts.push(time);
        }
        let kind = if branch.is_remote() {
            ReferenceKind::RemoteBranch
        } else {
            ReferenceKind::LocalBranch
        };
        Entry {
            reference: PickedReference {
                kind,
                name: branch.name().to_string().into(),
            },
            description: parts.join(" · ").into(),
        }
    };

    entries.extend(
        branches
            .iter()
            .filter(|branch| !branch.is_remote())
            .map(|branch| branch_entry(branch)),
    );
    entries.extend(tags.iter().map(|tag| {
        let mut parts = vec!["tag".to_string(), tag.sha.display_short()];
        if let Some(time) = relative_time(tag.commit_timestamp) {
            parts.push(time);
        }
        Entry {
            reference: PickedReference {
                kind: ReferenceKind::Tag,
                name: tag.name.clone(),
            },
            description: parts.join(" · ").into(),
        }
    }));
    entries.extend(
        branches
            .iter()
            .filter(|branch| branch.is_remote())
            .map(|branch| branch_entry(branch)),
    );

    if let Some(exclude) = &options.exclude {
        entries.retain(|entry| entry.reference.name != *exclude);
    }
    entries
}

enum TypedEntry {
    Commit(CommitDetails),
    Range(Comparison),
}

pub(crate) struct ReferencePickerDelegate {
    repository: Entity<Repository>,
    options: ReferencePickerOptions,
    entries: Vec<Entry>,
    matches: Vec<StringMatch>,
    typed: Option<TypedEntry>,
    query: String,
    selected_index: usize,
    _lookup: Task<()>,
    tx: Option<oneshot::Sender<ReferencePick>>,
}

impl ReferencePickerDelegate {
    fn new(
        repository: Entity<Repository>,
        options: ReferencePickerOptions,
        entries: Vec<Entry>,
        tx: oneshot::Sender<ReferencePick>,
    ) -> Self {
        Self {
            repository,
            options,
            entries,
            matches: Vec::new(),
            typed: None,
            query: String::new(),
            selected_index: 0,
            _lookup: Task::ready(()),
            tx: Some(tx),
        }
    }

    fn typed_offset(&self) -> usize {
        self.typed.is_some() as usize
    }

    fn pick_at(&self, index: usize) -> Option<ReferencePick> {
        if let Some(typed) = &self.typed
            && index == 0
        {
            return Some(match typed {
                TypedEntry::Commit(details) => {
                    ReferencePick::Reference(PickedReference::revision(details.sha.clone()))
                }
                TypedEntry::Range(comparison) => ReferencePick::Range(comparison.clone()),
            });
        }
        let string_match = self.matches.get(index.checked_sub(self.typed_offset())?)?;
        let entry = self.entries.get(string_match.candidate_id)?;
        Some(ReferencePick::Reference(entry.reference.clone()))
    }

    fn look_up_typed_revision(&mut self, query: &str, cx: &mut Context<Picker<Self>>) {
        self.typed = None;
        let text = query.trim();
        let (text, forced) = match text.strip_prefix('#') {
            Some(rest) => (rest.trim(), true),
            None => (text, false),
        };
        if text.is_empty() {
            self._lookup = Task::ready(());
            return;
        }
        let range = self
            .options
            .allow_range
            .then(|| text.split_once(".."))
            .flatten()
            .map(|(left, right)| {
                // Either end may be left out for HEAD, as git allows.
                let end = |end: &str| {
                    if end.is_empty() {
                        "HEAD".to_string()
                    } else {
                        end.to_string()
                    }
                };
                (end(left), end(right))
            });
        // Listed refs are already in the list; `#` asks for the commit anyway.
        if range.is_none()
            && !forced
            && self
                .entries
                .iter()
                .any(|entry| entry.reference.name.as_ref() == text)
        {
            self._lookup = Task::ready(());
            return;
        }

        let repository = self.repository.clone();
        let query = query.to_string();
        let text = text.to_string();
        self._lookup = cx.spawn(async move |picker, cx| {
            // Every keystroke replaces this task, so only a pause in typing reaches git.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let resolved = |oid: Result<Result<Option<Oid>>, oneshot::Canceled>| {
                oid.ok().and_then(|oid| oid.log_err()).flatten()
            };
            let typed = match range {
                Some((left, right)) => {
                    let (left_oid, right_oid) = repository.update(cx, |repository, _| {
                        (
                            repository.revparse(left.clone()),
                            repository.revparse(right.clone()),
                        )
                    });
                    let (left_oid, right_oid) = futures::join!(left_oid, right_oid);
                    (resolved(left_oid).is_some() && resolved(right_oid).is_some()).then(|| {
                        TypedEntry::Range(Comparison {
                            base: PickedReference::revision(left),
                            head: PickedReference::revision(right),
                        })
                    })
                }
                None => {
                    let oid = repository
                        .update(cx, |repository, _| repository.revparse(text))
                        .await;
                    match resolved(oid) {
                        Some(oid) => repository
                            .update(cx, |repository, _| repository.show(oid.to_string()))
                            .await
                            .ok()
                            .and_then(|details| details.log_err())
                            .map(TypedEntry::Commit),
                        None => None,
                    }
                }
            };
            picker
                .update(cx, |picker, cx| {
                    // A slower lookup for an earlier query must not replace the current one.
                    if picker.delegate.query == query {
                        if typed.is_some() {
                            picker.delegate.selected_index = 0;
                        }
                        picker.delegate.typed = typed;
                        cx.notify();
                    }
                })
                .ok();
        });
    }
}

impl PickerDelegate for ReferencePickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "reference picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        let hint = if self.options.allow_range {
            " (or enter a range, or a revision prefixed with #)"
        } else {
            " (or enter a revision prefixed with #)"
        };
        format!("{}{hint}", self.options.placeholder).into()
    }

    fn render_header(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<AnyElement> {
        Some(
            h_flex()
                .px_3()
                .pt_2()
                .pb_1()
                .w_full()
                .child(Headline::new(self.options.title.clone()).size(HeadlineSize::XSmall))
                .into_any_element(),
        )
    }

    fn match_count(&self) -> usize {
        self.matches.len() + self.typed_offset()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = index;
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        let offset = self.typed_offset();
        let mut separators: Vec<usize> = (offset > 0).then_some(0).into_iter().collect();
        // Fuzzy matching reorders the list by score, so the groups only line up unfiltered.
        if !self.query.is_empty() {
            return separators;
        }
        let group_of = |string_match: &StringMatch| {
            self.entries
                .get(string_match.candidate_id)
                .map(|entry| match entry.reference.kind {
                    ReferenceKind::WorkingTree | ReferenceKind::Head => 0,
                    ReferenceKind::LocalBranch => 1,
                    ReferenceKind::Tag => 2,
                    ReferenceKind::RemoteBranch => 3,
                    ReferenceKind::Revision => 4,
                })
        };
        for (index, pair) in self.matches.windows(2).enumerate() {
            if let [first, second] = pair
                && group_of(first) != group_of(second)
            {
                separators.push(index + offset);
            }
        }
        separators
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some(if self.query.trim().is_empty() {
            "No branches or tags".into()
        } else {
            "Enter a reference or commit SHA".into()
        })
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let candidates: Vec<StringMatchCandidate> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatchCandidate::new(index, &entry.reference.label()))
            .collect();
        let picked_index = self.options.picked.as_ref().and_then(|picked| {
            self.entries
                .iter()
                .position(|entry| entry.reference.name == *picked)
        });
        cx.spawn_in(window, async move |picker, cx| {
            let matches: Vec<StringMatch> = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                fuzzy::match_strings(
                    &candidates,
                    &query,
                    true,
                    true,
                    10000,
                    &Default::default(),
                    cx.background_executor().clone(),
                )
                .await
            };
            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.matches = matches;
                    delegate.selected_index = if query.is_empty() {
                        picked_index.unwrap_or(0)
                    } else {
                        0
                    };
                    delegate.query = query.clone();
                    delegate.look_up_typed_revision(&query, cx);
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(pick) = self.pick_at(self.selected_index) else {
            return;
        };
        if let Some(tx) = self.tx.take() {
            tx.send(pick).ok();
        }
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let (icon, label, positions, description) = match (self.typed.as_ref(), ix) {
            (Some(TypedEntry::Commit(details)), 0) => {
                let subject = details
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let mut description = details.author_name.to_string();
                if let Some(time) = relative_time(details.commit_timestamp) {
                    description = format!("{description}, {time}");
                }
                (
                    IconName::GitCommit,
                    subject,
                    Vec::new(),
                    format!("{description} · {}", short_ref(&details.sha)),
                )
            }
            (Some(TypedEntry::Range(comparison)), 0) => (
                IconName::Diff,
                format!("{}..{}", comparison.base.label(), comparison.head.label()),
                Vec::new(),
                format!(
                    "Changes from {} to {}",
                    comparison.base.label(),
                    comparison.head.label()
                ),
            ),
            _ => {
                let string_match = self.matches.get(ix.checked_sub(self.typed_offset())?)?;
                let entry = self.entries.get(string_match.candidate_id)?;
                let icon = match entry.reference.kind {
                    ReferenceKind::WorkingTree => IconName::Folder,
                    ReferenceKind::Head | ReferenceKind::Revision => IconName::GitCommit,
                    ReferenceKind::LocalBranch | ReferenceKind::RemoteBranch => IconName::GitBranch,
                    ReferenceKind::Tag => IconName::Bookmark,
                };
                (
                    icon,
                    entry.reference.label().to_string(),
                    string_match.positions.clone(),
                    entry.description.to_string(),
                )
            }
        };

        Some(
            ListItem::new(("reference", ix))
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(HighlightedLabel::new(label, positions))
                        .when(!description.is_empty(), |this| {
                            this.child(
                                Label::new(description)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        }),
                ),
        )
    }
}

pub(crate) struct ReferencePicker {
    picker: Entity<Picker<ReferencePickerDelegate>>,
    _subscription: Subscription,
}

impl ReferencePicker {
    fn new(delegate: ReferencePickerDelegate, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker =
            cx.new(|cx| Picker::uniform_list(delegate, window, cx).initial_width(rems(34.)));
        let _subscription = cx.subscribe(&picker, |_, _, _, cx| cx.emit(DismissEvent));
        Self {
            picker,
            _subscription,
        }
    }
}

impl ModalView for ReferencePicker {}
impl EventEmitter<DismissEvent> for ReferencePicker {}

impl Focusable for ReferencePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ReferencePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .child(self.picker.clone())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.picker.update(cx, |this, cx| {
                    this.cancel(&Default::default(), window, cx);
                })
            }))
    }
}

/// Resolves to `None` when the picker is dismissed.
pub(crate) fn pick_reference(
    repository: Entity<Repository>,
    options: ReferencePickerOptions,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<Option<ReferencePick>>> {
    let tags = repository.update(cx, |repository, _| repository.tags());
    let branches = repository.read(cx).branch_list.to_vec();
    window.spawn(cx, async move |cx| {
        let tags = tags
            .await
            .log_err()
            .and_then(|tags| tags.log_err())
            .unwrap_or_default();
        let entries = build_entries(&branches, &tags, &options);
        let (tx, rx) = oneshot::channel();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                let delegate = ReferencePickerDelegate::new(repository, options, entries, tx);
                ReferencePicker::new(delegate, window, cx)
            })
        })?;
        Ok(rx.await.ok())
    })
}

/// Asks for the side to compare unless `head` is given, then for what to compare it with.
/// Resolves to `None` when either picker is dismissed.
pub(crate) fn pick_comparison(
    repository: Entity<Repository>,
    head: Option<PickedReference>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Result<Option<Comparison>>> {
    window.spawn(cx, async move |cx| {
        let head = match head {
            Some(head) => head,
            None => {
                let pick = cx
                    .update(|window, cx| {
                        pick_reference(
                            repository.clone(),
                            ReferencePickerOptions {
                                title: "Compare".into(),
                                placeholder: "Choose a reference (branch, tag, etc) to compare"
                                    .into(),
                                include_working_tree: true,
                                exclude: None,
                                allow_range: true,
                                picked: None,
                            },
                            workspace.clone(),
                            window,
                            cx,
                        )
                    })?
                    .await?;
                match pick {
                    None => return Ok(None),
                    Some(ReferencePick::Range(comparison)) => return Ok(Some(comparison)),
                    Some(ReferencePick::Reference(head)) => head,
                }
            }
        };

        // GitLens suggests the branch's merge target as the base; the default branch is the
        // closest this repository tracks.
        let default_branch = repository
            .update(cx, |repository, _| repository.default_branch(true))
            .await
            .ok()
            .and_then(|default_branch| default_branch.log_err())
            .flatten()
            .filter(|default_branch| *default_branch != head.name);
        let pick = cx
            .update(|window, cx| {
                pick_reference(
                    repository.clone(),
                    ReferencePickerOptions {
                        title: format!("Compare {} with", head.label()).into(),
                        placeholder: "Choose a reference (branch, tag, etc) to compare with".into(),
                        include_working_tree: false,
                        exclude: Some(head.name.clone()),
                        allow_range: false,
                        picked: default_branch,
                    },
                    workspace,
                    window,
                    cx,
                )
            })?
            .await?;
        match pick {
            Some(ReferencePick::Reference(base)) => Ok(Some(Comparison { head, base })),
            _ => Ok(None),
        }
    })
}

/// Shows `comparison`: the working tree against a ref in the branch diff view, two refs in
/// the commit view.
pub(crate) fn open_comparison(
    comparison: Comparison,
    repository: Entity<Repository>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    match comparison.head.kind {
        ReferenceKind::WorkingTree => workspace.update(cx, |workspace, cx| {
            let project = workspace.project().clone();
            BranchDiff::deploy_branch_diff_with_base(
                workspace,
                project,
                repository,
                DiffBase::Ref {
                    base_ref: comparison.base.name,
                },
                None,
                window,
                cx,
            );
        }),
        _ => {
            CommitView::open_range(
                comparison.base.name,
                comparison.head.name,
                repository.downgrade(),
                workspace,
                window,
                cx,
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::repository::CommitSummary;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use serde_json::json;
    use settings::SettingsStore;
    use std::path::Path;
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
    }

    fn branch(ref_name: &str, is_head: bool, commit_timestamp: i64) -> Branch {
        Branch {
            is_head,
            ref_name: ref_name.to_string().into(),
            upstream: None,
            most_recent_commit: Some(CommitSummary {
                sha: SharedString::default(),
                subject: SharedString::default(),
                commit_timestamp,
                author_name: SharedString::default(),
                has_parent: true,
            }),
        }
    }

    fn options(allow_range: bool, exclude: Option<&str>) -> ReferencePickerOptions {
        ReferencePickerOptions {
            title: "Compare".into(),
            placeholder: "Choose a reference".into(),
            include_working_tree: true,
            exclude: exclude.map(SharedString::from),
            allow_range,
            picked: None,
        }
    }

    #[test]
    fn test_entries_are_grouped_like_gitlens() {
        let branches = vec![
            branch("refs/remotes/origin/main", false, 30),
            branch("refs/heads/feature", false, 20),
            branch("refs/heads/main", true, 10),
            branch("refs/heads/old", false, 5),
        ];
        let tags = vec![Tag {
            name: "v1".into(),
            sha: format!("{:040x}", 1).parse::<Oid>().unwrap(),
            commit_timestamp: 40,
        }];

        let entries = build_entries(&branches, &tags, &options(false, Some("old")));
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| entry.reference.label().to_string())
            .collect();
        assert_eq!(
            labels,
            [
                "Working Tree",
                "HEAD",
                "main",
                "feature",
                "v1",
                "origin/main"
            ]
        );
        assert_eq!(entries[2].reference.kind, ReferenceKind::LocalBranch);
        assert!(entries[2].description.starts_with("current branch"));
        assert_eq!(entries[4].reference.kind, ReferenceKind::Tag);
        assert!(entries[4].description.starts_with("tag · "));
        assert_eq!(entries[5].reference.kind, ReferenceKind::RemoteBranch);
    }

    #[gpui::test]
    async fn test_typed_revisions_become_entries(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({ ".git": {} }))
            .await;
        let sha = format!("{:040x}", 7).parse::<Oid>().unwrap();
        fs.set_head_for_repo(Path::new(path!("/project/.git")), &[], sha.to_string());
        let project = Project::test(fs, [Path::new(path!("/project"))], cx).await;
        cx.run_until_parked();
        let repository =
            project.read_with(cx, |project, cx| project.active_repository(cx).unwrap());

        let (tx, _rx) = oneshot::channel();
        let picker = cx.add_window(|window, cx| {
            let options = options(true, None);
            let entries = build_entries(&[], &[], &options);
            let delegate = ReferencePickerDelegate::new(repository, options, entries, tx);
            Picker::uniform_list(delegate, window, cx)
        });
        let query = |picker: &gpui::WindowHandle<Picker<ReferencePickerDelegate>>,
                     cx: &mut TestAppContext,
                     text: &str| {
            let text = text.to_string();
            picker
                .update(cx, |picker, window, cx| {
                    picker.delegate.update_matches(text, window, cx)
                })
                .unwrap()
        };

        // `#` looks the revision up even though HEAD is listed; the `#` itself matches no
        // listed label, so the commit is the only entry.
        query(&picker, cx, "#HEAD").await;
        cx.executor().advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        picker
            .read_with(cx, |picker, _| {
                assert_eq!(picker.delegate.match_count(), 1);
                assert_eq!(
                    picker.delegate.pick_at(0),
                    Some(ReferencePick::Reference(PickedReference::revision(
                        sha.to_string()
                    )))
                );
            })
            .unwrap();

        query(&picker, cx, "HEAD..HEAD").await;
        cx.executor().advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        picker
            .read_with(cx, |picker, _| {
                assert_eq!(
                    picker.delegate.pick_at(0),
                    Some(ReferencePick::Range(Comparison {
                        base: PickedReference::revision("HEAD"),
                        head: PickedReference::revision("HEAD"),
                    }))
                );
            })
            .unwrap();

        query(&picker, cx, "no-such-ref").await;
        cx.executor().advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        picker
            .read_with(cx, |picker, _| {
                assert_eq!(picker.delegate.match_count(), 0);
            })
            .unwrap();
    }
}
