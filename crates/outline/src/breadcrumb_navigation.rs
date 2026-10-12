use std::{path::Path, sync::Arc};

use editor::Editor;
use file_icons::FileIcons;
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, DismissEvent, Entity, Focusable, Font,
    Task, WeakEntity, Window,
};
use picker::{Picker, PickerDelegate};
use project::{Project, ProjectEntryId, ProjectPath, WorktreeId};
use settings::Settings;
use ui::{
    ButtonLike, HighlightedLabel, ListItem, ListItemSpacing, PopoverMenu, Tooltip, prelude::*,
};
use util::{ResultExt, paths::compare_rel_paths, rel_path::RelPath};
use workspace::{
    Workspace,
    item::{HighlightedText, ItemHandle},
};

use crate::outline_popover;

enum SegmentKind {
    Directory(ProjectPath),
    File(ProjectPath),
    Symbol(usize),
}

struct Segment {
    text: HighlightedText,
    kind: SegmentKind,
}

/// Replaces the editor's breadcrumb renderer: every path level and symbol gets
/// its own popover. Anything that is not a project file in a single-buffer
/// editor keeps the editor's own rendering.
pub(crate) fn render_breadcrumb_text(
    segments: Vec<HighlightedText>,
    breadcrumb_font: Option<Font>,
    prefix: Option<AnyElement>,
    active_item: &dyn ItemHandle,
    multibuffer_header: bool,
    window: &mut Window,
    cx: &App,
) -> AnyElement {
    let navigable = active_item
        .downcast::<Editor>()
        .filter(|_| !multibuffer_header)
        .and_then(|editor| Some((path_segments(editor.read(cx), cx)?, editor)));
    let Some((path_segments, editor)) = navigable else {
        return editor::render_breadcrumb_text(
            segments,
            breadcrumb_font,
            prefix,
            active_item,
            multibuffer_header,
            window,
            cx,
        );
    };

    let emphasize_file_name =
        !workspace::TabBarSettings::get_global(cx).show && active_item.is_dirty(cx);
    let symbol_segments = segments
        .into_iter()
        .skip(1)
        .enumerate()
        .map(|(index, text)| Segment {
            text,
            kind: SegmentKind::Symbol(index),
        });

    let mut children = Vec::new();
    for (index, segment) in path_segments.into_iter().chain(symbol_segments).enumerate() {
        if index > 0 {
            children.push(Label::new("›").color(Color::Placeholder).into_any_element());
        }
        children.push(render_segment(
            index,
            segment,
            &editor,
            breadcrumb_font.as_ref(),
            emphasize_file_name,
            window,
            cx,
        ));
    }

    let breadcrumbs = h_flex().gap_1().children(children);
    let breadcrumbs = match prefix {
        Some(prefix) => h_flex().gap_1p5().child(prefix).child(breadcrumbs),
        None => breadcrumbs,
    };
    h_flex()
        .id("breadcrumb_container")
        .flex_grow_1()
        .text_ui(cx)
        .overflow_x_scroll()
        .child(breadcrumbs)
        .into_any_element()
}

fn path_segments(editor: &Editor, cx: &App) -> Option<Vec<Segment>> {
    if editor.breadcrumb_header().is_some() {
        return None;
    }
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = project::File::from_dyn(buffer.read(cx).file())?;
    let worktree_id = file.worktree_id(cx);
    let plain = |text: &str| HighlightedText {
        text: text.to_string().into(),
        highlights: Vec::new(),
    };
    let project_path = |path: &RelPath| ProjectPath {
        worktree_id,
        path: path.into(),
    };

    let root_name = file.worktree.read(cx).root_name_str();
    if file.path.is_empty() {
        // A single-file worktree: the root entry is the file itself.
        return Some(vec![Segment {
            text: plain(root_name),
            kind: SegmentKind::File(project_path(RelPath::empty())),
        }]);
    }

    let mut segments = Vec::new();
    let show_root = editor
        .project()
        .is_some_and(|project| project.read(cx).visible_worktrees(cx).count() > 1);
    if show_root {
        segments.push(Segment {
            text: plain(root_name),
            kind: SegmentKind::Directory(project_path(RelPath::empty())),
        });
    }
    for ancestor in file
        .path
        .ancestors()
        .rev()
        .filter(|ancestor| !ancestor.is_empty())
    {
        let kind = if ancestor == file.path.as_ref() {
            SegmentKind::File(project_path(ancestor))
        } else {
            SegmentKind::Directory(project_path(ancestor))
        };
        segments.push(Segment {
            text: plain(ancestor.file_name()?),
            kind,
        });
    }
    Some(segments)
}

fn render_segment(
    index: usize,
    segment: Segment,
    editor: &Entity<Editor>,
    breadcrumb_font: Option<&Font>,
    emphasize_file_name: bool,
    window: &mut Window,
    cx: &App,
) -> AnyElement {
    let is_file = matches!(segment.kind, SegmentKind::File(_));
    let text = editor::render_breadcrumb_segment(
        segment.text,
        breadcrumb_font,
        emphasize_file_name && is_file,
        window,
        cx,
    );
    let trigger = ButtonLike::new(("breadcrumb-segment", index)).child(text);
    let menu_id = ("breadcrumb-segment-menu", index);

    match segment.kind {
        SegmentKind::Directory(project_path) => {
            let editor = editor.downgrade();
            let trigger =
                trigger.on_right_click(copy_absolute_path(editor.clone(), project_path.clone()));
            PopoverMenu::new(menu_id)
                .menu(move |window, cx| {
                    directory_picker(editor.upgrade()?, project_path.clone(), window, cx)
                })
                .trigger_with_tooltip(trigger, |_, cx| {
                    Tooltip::with_meta("Show Folder Contents", None, "Right-Click to Copy Path", cx)
                })
                .anchor(gpui::Anchor::TopLeft)
                .into_any_element()
        }
        SegmentKind::File(project_path) => {
            let focus_handle = editor.focus_handle(cx);
            let editor = editor.downgrade();
            let trigger = trigger.on_right_click(copy_absolute_path(editor.clone(), project_path));
            PopoverMenu::new(menu_id)
                .menu(move |window, cx| outline_popover(editor.upgrade()?, None, window, cx))
                .trigger_with_tooltip(trigger, move |_, cx| {
                    Tooltip::with_meta_in(
                        "Show Symbol Outline",
                        Some(&zed_actions::outline::ToggleOutline),
                        "Right-Click to Copy Path",
                        &focus_handle,
                        cx,
                    )
                })
                .anchor(gpui::Anchor::TopLeft)
                .into_any_element()
        }
        SegmentKind::Symbol(symbol_index) => {
            let focus_handle = editor.focus_handle(cx);
            let editor = editor.downgrade();
            PopoverMenu::new(menu_id)
                .menu(move |window, cx| {
                    let editor = editor.upgrade()?;
                    let range = editor
                        .read(cx)
                        .outline_symbols_at_cursor(cx)
                        .and_then(|symbols| symbols.get(symbol_index))
                        .map(|symbol| symbol.range.clone());
                    outline_popover(editor, range, window, cx)
                })
                .trigger_with_tooltip(trigger, move |_, cx| {
                    Tooltip::for_action_in(
                        "Show Symbol Outline",
                        &zed_actions::outline::ToggleOutline,
                        &focus_handle,
                        cx,
                    )
                })
                .anchor(gpui::Anchor::TopLeft)
                .into_any_element()
        }
    }
}

fn copy_absolute_path(
    editor: WeakEntity<Editor>,
    project_path: ProjectPath,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) {
    move |_, _, cx| {
        let abs_path = editor.upgrade().and_then(|editor| {
            let editor = editor.read(cx);
            editor.project()?.read(cx).absolute_path(&project_path, cx)
        });
        if let Some(abs_path) = abs_path.as_deref().and_then(Path::to_str) {
            cx.write_to_clipboard(ClipboardItem::new_string(abs_path.to_string()));
        }
    }
}

pub(crate) fn size_popover<D: PickerDelegate>(picker: Picker<D>, window: &Window) -> Picker<D> {
    // Leave the editor visible under the popover, even on short windows.
    let half_viewport_height = window.viewport_size().height / 2.;
    let max_height = if half_viewport_height < px(480.) {
        half_viewport_height
    } else {
        px(480.)
    };
    picker
        .popover()
        .initial_width(Rems::from_pixels(px(320.), window))
        .max_height(Rems::from_pixels(max_height, window))
        .show_scrollbar(true)
}

fn directory_picker(
    editor: Entity<Editor>,
    directory: ProjectPath,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<Picker<DirectoryPickerDelegate>>> {
    let (project, workspace, current_file) = {
        let editor = editor.read(cx);
        let buffer = editor.buffer().read(cx).as_singleton()?;
        let current_file = project::File::from_dyn(buffer.read(cx).file())?
            .path
            .clone();
        (
            editor.project()?.clone(),
            editor.workspace()?.downgrade(),
            current_file,
        )
    };
    let delegate = DirectoryPickerDelegate::new(project, workspace, directory, current_file, cx);
    Some(cx.new(|cx| size_popover(Picker::uniform_list(delegate, window, cx), window)))
}

struct DirectoryEntry {
    id: ProjectEntryId,
    path: Arc<RelPath>,
    name: SharedString,
    is_dir: bool,
    is_unloaded: bool,
}

struct DirectoryPickerDelegate {
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    worktree_id: WorktreeId,
    directory: Arc<RelPath>,
    current_file: Arc<RelPath>,
    entries: Vec<DirectoryEntry>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl DirectoryPickerDelegate {
    fn new(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        directory: ProjectPath,
        current_file: Arc<RelPath>,
        cx: &App,
    ) -> Self {
        let mut this = Self {
            project,
            workspace,
            worktree_id: directory.worktree_id,
            directory: directory.path,
            current_file,
            entries: Vec::new(),
            matches: Vec::new(),
            selected_index: 0,
        };
        this.load_entries(cx);
        this
    }

    fn load_entries(&mut self, cx: &App) {
        self.entries.clear();
        let Some(worktree) = self.project.read(cx).worktree_for_id(self.worktree_id, cx) else {
            return;
        };
        self.entries.extend(
            worktree
                .read(cx)
                .child_entries(&self.directory)
                .filter(|entry| !entry.is_hidden)
                .map(|entry| DirectoryEntry {
                    id: entry.id,
                    name: entry
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string()
                        .into(),
                    path: entry.path.clone(),
                    is_dir: entry.is_dir(),
                    is_unloaded: entry.kind.is_unloaded(),
                }),
        );
        self.entries.sort_by(|left, right| {
            compare_rel_paths((&left.path, !left.is_dir), (&right.path, !right.is_dir))
        });
    }
}

impl PickerDelegate for DirectoryPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "breadcrumb directory"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search folder contents...".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        if query.is_empty() {
            self.matches = self
                .entries
                .iter()
                .enumerate()
                .map(|(candidate_id, entry)| StringMatch {
                    candidate_id,
                    score: 0.,
                    positions: Vec::new(),
                    string: entry.name.clone(),
                })
                .collect();
            self.selected_index = self
                .entries
                .iter()
                .position(|entry| self.current_file.starts_with(&entry.path))
                .unwrap_or(0);
        } else {
            let candidates = self
                .entries
                .iter()
                .enumerate()
                .map(|(candidate_id, entry)| {
                    StringMatchCandidate::new(candidate_id, entry.name.clone())
                })
                .collect::<Vec<_>>();
            self.matches = match_strings(
                &candidates,
                &query,
                Case::Smart,
                LengthPenalty::Off,
                candidates.len(),
            );
            self.selected_index = 0;
        }
        cx.notify();
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self
            .matches
            .get(self.selected_index)
            .and_then(|string_match| self.entries.get(string_match.candidate_id))
        else {
            return;
        };
        let worktree_id = self.worktree_id;

        if entry.is_dir {
            let directory = entry.path.clone();
            // Ignored directories are not scanned until something expands them.
            let expansion = entry.is_unloaded.then(|| {
                let entry_id = entry.id;
                self.project.update(cx, |project, cx| {
                    project.expand_entry(worktree_id, entry_id, cx)
                })
            });
            cx.spawn_in(window, async move |picker, cx| {
                if let Some(expansion) = expansion.flatten() {
                    expansion.await.log_err();
                }
                picker
                    .update_in(cx, |picker, window, cx| {
                        picker.delegate.directory = directory;
                        picker.delegate.load_entries(cx);
                        picker.set_query("", window, cx);
                        picker.refresh(window, cx);
                    })
                    .ok();
            })
            .detach();
        } else {
            let project_path = ProjectPath {
                worktree_id,
                path: entry.path.clone(),
            };
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace
                        .open_path(project_path, None, true, window, cx)
                        .detach_and_log_err(cx);
                })
                .log_err();
            cx.emit(DismissEvent);
        }
    }

    fn dismissed(&mut self, _: &mut Window, _: &mut Context<Picker<Self>>) {}

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let string_match = self.matches.get(ix)?;
        let entry = self.entries.get(string_match.candidate_id)?;
        let icon = if entry.is_dir {
            FileIcons::get_folder_icon(false, entry.path.as_std_path(), cx)
                .map(Icon::from_path)
                .unwrap_or_else(|| Icon::new(IconName::Folder))
        } else {
            FileIcons::get_icon(entry.path.as_std_path(), cx)
                .map(Icon::from_path)
                .unwrap_or_else(|| Icon::new(IconName::File))
        };

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(icon.color(Color::Muted))
                .child(HighlightedLabel::new(
                    entry.name.clone(),
                    string_match.positions.clone(),
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use serde_json::json;
    use util::{path, rel_path::rel_path};
    use workspace::{AppState, MultiWorkspace, Workspace};

    #[gpui::test]
    async fn test_path_segments_single_worktree_omits_root(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({ "src": { "main.rs": "fn main() {}" } }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let (workspace, cx) = open_workspace(project, cx);
        let editor = open_editor(&workspace, path!("/root/src/main.rs"), cx).await;

        let segments = cx.read(|cx| path_segments(editor.read(cx), cx)).unwrap();
        assert_eq!(segment_texts(&segments), vec!["src", "main.rs"]);
        assert!(matches!(
            &segments[0].kind,
            SegmentKind::Directory(path) if path.path.as_ref() == rel_path("src")
        ));
        assert!(matches!(
            &segments[1].kind,
            SegmentKind::File(path) if path.path.as_ref() == rel_path("src/main.rs")
        ));
    }

    #[gpui::test]
    async fn test_path_segments_multiple_worktrees_include_root(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/roota"),
            json!({ "src": { "main.rs": "fn main() {}" } }),
        )
        .await;
        fs.insert_tree(path!("/rootb"), json!({ "lib.rs": "" }))
            .await;
        let project =
            Project::test(fs, [path!("/roota").as_ref(), path!("/rootb").as_ref()], cx).await;
        let (workspace, cx) = open_workspace(project, cx);
        let editor = open_editor(&workspace, path!("/roota/src/main.rs"), cx).await;

        let segments = cx.read(|cx| path_segments(editor.read(cx), cx)).unwrap();
        assert_eq!(segment_texts(&segments), vec!["roota", "src", "main.rs"]);
        assert!(matches!(
            &segments[0].kind,
            SegmentKind::Directory(path) if path.path.is_empty()
        ));
    }

    #[gpui::test]
    async fn test_path_segments_fall_back_for_custom_header_and_untitled_buffer(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({ "src": { "main.rs": "fn main() {}" } }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let (workspace, cx) = open_workspace(project, cx);

        let editor = open_editor(&workspace, path!("/root/src/main.rs"), cx).await;
        editor.update(cx, |editor, _| {
            editor.set_breadcrumb_header("Custom Header".to_string())
        });
        assert!(
            cx.read(|cx| path_segments(editor.read(cx), cx)).is_none(),
            "A custom breadcrumb header should keep the editor's rendering"
        );

        workspace.update_in(cx, |workspace, window, cx| {
            Editor::new_file(workspace, &Default::default(), window, cx)
        });
        cx.run_until_parked();
        let untitled = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .unwrap()
                .downcast::<Editor>()
                .unwrap()
        });
        assert_ne!(
            untitled, editor,
            "The new untitled buffer should be the active item"
        );
        assert!(
            cx.read(|cx| path_segments(untitled.read(cx), cx)).is_none(),
            "An untitled buffer should keep the editor's rendering"
        );
    }

    #[gpui::test]
    async fn test_directory_picker_lists_children_and_drills_down(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/root"),
            json!({
                "a_dir": {},
                "docs": { "guide.md": "# Guide" },
                "src": { "main.rs": "fn main() {}" },
                "b_file.txt": "",
                "readme.md": "",
            }),
        )
        .await;
        let project = Project::test(fs, [path!("/root").as_ref()], cx).await;
        let (workspace, cx) = open_workspace(project.clone(), cx);
        let editor = open_editor(&workspace, path!("/root/src/main.rs"), cx).await;
        let worktree_id = project.read_with(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).id()
        });

        let root = ProjectPath {
            worktree_id,
            path: RelPath::empty().into(),
        };
        let picker =
            cx.update(|window, cx| directory_picker(editor.clone(), root, window, cx).unwrap());
        cx.run_until_parked();
        assert_eq!(
            listed_names(&picker, cx),
            vec!["a_dir", "docs", "src", "b_file.txt", "readme.md"],
            "Directories should come first, each group in natural order"
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.delegate.selected_index),
            2,
            "The directory on the current file's path should be preselected"
        );

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("doc".to_string(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(listed_names(&picker, cx), vec!["docs"]);

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            listed_names(&picker, cx),
            vec!["guide.md"],
            "Confirming a directory should list that directory's children"
        );
        assert!(picker.read_with(cx, |picker, _| {
            picker.delegate.directory.as_ref() == rel_path("docs")
        }));

        let dismissed = Rc::new(Cell::new(false));
        cx.update(|_, cx| {
            cx.subscribe(&picker, {
                let dismissed = dismissed.clone();
                move |_, _: &DismissEvent, _| dismissed.set(true)
            })
            .detach();
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx)
        });
        cx.run_until_parked();
        assert!(
            dismissed.get(),
            "Confirming a file should dismiss the picker"
        );
        let active_path = workspace.read_with(cx, |workspace, cx| {
            workspace.active_item(cx).unwrap().project_path(cx).unwrap()
        });
        assert_eq!(
            active_path,
            ProjectPath {
                worktree_id,
                path: rel_path("docs/guide.md").into(),
            },
            "Confirming a file should open it"
        );
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            editor::init(cx);
            crate::init(cx);
        });
    }

    fn open_workspace(
        project: Entity<Project>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = cx.read(|cx| multi_workspace.read(cx).workspace().clone());
        (workspace, cx)
    }

    async fn open_editor(
        workspace: &Entity<Workspace>,
        abs_path: &str,
        cx: &mut VisualTestContext,
    ) -> Entity<Editor> {
        let project_path = workspace.read_with(cx, |workspace, cx| {
            workspace
                .project()
                .read(cx)
                .find_project_path(abs_path, cx)
                .unwrap()
        });
        let editor = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        cx.run_until_parked();
        editor
    }

    fn segment_texts(segments: &[Segment]) -> Vec<&str> {
        segments
            .iter()
            .map(|segment| segment.text.text.as_ref())
            .collect()
    }

    fn listed_names(
        picker: &Entity<Picker<DirectoryPickerDelegate>>,
        cx: &mut VisualTestContext,
    ) -> Vec<SharedString> {
        picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .iter()
                .map(|string_match| {
                    picker.delegate.entries[string_match.candidate_id]
                        .name
                        .clone()
                })
                .collect()
        })
    }
}
