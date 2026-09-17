---
name: canvas
description: >-
  Create and refine durable analytical artifacts beside the conversation: reports,
  comparisons, metrics, architecture reviews, diagrams and interactive explorations.
  Use when the artifact itself is the deliverable and the system provides a managed
  Canvas directory. Read this skill before authoring or changing .canvas.tsx files.
---
# Canvas authoring

A Canvas is a single React/TSX document, saved outside the project and attached to the conversation. Use it for analysis that the user will explore, revisit or refine. Honor an explicitly requested delivery format. A code fix, message draft, change in another application, or brief answer should remain that deliverable; encountering a table during the work does not change the task into Canvas creation.

## Workflow

1. Gather the actual data and establish what the report needs to show. Do not generate a blank Canvas, example data, empty charts, or placeholder sections. If the necessary data is unavailable, explain what is missing.
2. Read the SDK declarations with `read_file` at `<built-in>/canvas/sdk.d.ts`. Use its actual exports and prop types.
3. Use `write_file` to write one complete, descriptive `<name>.canvas.tsx` directly in the managed Canvas directory given by the system. That directory is provisioned by Zed. Source files outside it are ordinary code files.
4. Import only from `@zed/canvas`, including React hooks and types. Default-export the top-level React component. Embed data directly. Use the SDK's layouts, tables, charts, stats, forms and diffs. Raw HTML and SVG are available for content the SDK does not cover. Do not create helper files or import packages, local modules or network resources.
5. Check the file tool's `Canvas TypeScript check` result and fix reported errors. For later changes, read and edit the same file with `read_file` and `edit_file`; preserve existing state keys and user choices.
6. End with a short conclusion and a Markdown link to the absolute `.canvas.tsx` path. Zed associates the artifact with this conversation and opens its preview beside it. Do not duplicate the whole report in the transcript.

## Components and design

The SDK includes Stack, Row, Grid, H1/H2/H3, Text, Card/CardHeader/CardBody, Stat, Table, LineChart, BarChart, PieChart, Callout, Pill, DiffView/DiffStats, TodoList, UsageBar, form controls and computeDAGLayout. Exact signatures are in the declarations.

Use `useHostTheme()` for colors: `theme.text.primary`, `theme.text.secondary`, `theme.bg.editor`, `theme.stroke.primary`, `theme.accent.primary`, `theme.status`, and `theme.category`. Keep the composition flat and readable, with a clear primary finding, supporting evidence, and appropriate variation in layout. Avoid decorative gradients, shadows, emojis and uniformly repeated cards. Every plot needs a specific title, axes and units, series names, and a source/time-range caption. Explain aggregations such as averages or percentiles.

Use `useCanvasState(key, initialValue)` for JSON state that should survive edits and reopening, such as filters, selected runs or checked items. Choose stable keys; use a new key when a stored value's shape changes. Ordinary `useState` is for transient state. Add stable `data-canvas-id` attributes to important HTML/SVG sections so element feedback is identifiable.

`useCanvasAction()` returns a dispatcher. `dispatch({type: 'askAgent', prompt: 'Refresh the source data for this report'})` adds the Canvas reference and request to the original conversation's draft. The user sends it. File navigation is available through `openFile`; no action can execute commands or directly send a message.

Saved source, passed type checking, and a loaded preview are distinct. If the preview has an error, the user can bring its diagnostic back to the Agent. Correct the source of that artifact, preserving its identity and data provenance.
