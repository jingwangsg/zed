# Agent Canvas

The native Agent authors a standalone `.canvas.tsx` report through the regular
file tools. The built-in Canvas skill and `runtime/sdk.d.ts` define its authoring
contract. macOS displays the report in a main-editor tab using `gpui_webview`.

Source files live under Zed's local data directory in `canvases/<project hash>/`,
including in SSH workspaces. The Agent and source editor share a local project
for these files; remote workspace paths are used only for workspace links.
The adjacent `.canvas.data.json` file stores filters and other persistent controls.
Zed maintains a `tsconfig.json` in that directory for the source editor's SDK types.
SQLite indexes artifacts, conversations, revisions and checked source snapshots;
the files remain authoritative. Changes trigger TypeScript checking and preview
compilation. The browser loads a fixed React runtime with network access disabled.
Page actions and element feedback append an unsent draft to the owning conversation.

## Runtime development

From `crates/agent_canvas/runtime`:

```sh
npm ci
npm run build
npx playwright install webkit
npm test
```

Regenerate and include `runtime/dist` when changing the SDK, compiler or shell.
It includes the compiler, WebAssembly binary, declarations and third-party licenses,
so normal Rust builds do not run npm or download browser packages. Zed uses its
existing Node runtime to type-check and compile source without evaluating it.

## Rust checks

```sh
cargo test -p agent_canvas --lib
cargo test -p agent --lib canvas
cargo test -p agent_skills --lib
./script/clippy -p agent_canvas -p gpui_webview -p gpui_macos -p agent -p agent_ui
```

`cargo run -p gpui_webview --example embedded` exercises native text input,
scrolling and GPUI overlays. End-to-end acceptance uses a native Agent conversation
to generate a report, change persistent controls, attach element feedback, edit the
same source file and reopen the application.
