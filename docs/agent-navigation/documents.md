<!-- agent-navigation: {"domain": "documents", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6735ba44b027435b6014ef03d5bcb8a85563717a7dab994ccb5bdb1f114bb4fb"} -->
# Documents, editor, and Markdown

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared text-document registry, Markdown parsing/rendering, and the editor/viewer routes that bind documents to app commands and syntax-aware UI.

Canonical docs: [docs/text-editor-design.md](../../docs/text-editor-design.md), [docs/markdown-viewer-design.md](../../docs/markdown-viewer-design.md)

## Shared docs

Use for: opening or reusing a file across windows, tabs, and splits

- [`TextDocument`](../../crates/festerm-document/src/text.rs) — shared mutable buffer, undo, dirty/saved state, and line-ending/encoding normalization
- [`DocumentRegistry`](../../app/festerm/src/documents.rs) — deduplicates open files by origin and source identity while tracking reload/save state

Evidence:
- [`opening_one_path_twice_shares_a_single_buffer`](../../app/festerm/src/documents.rs) — proves same path reuses a single document buffer
- [`docs/text-editor-design.md`](../../docs/text-editor-design.md) — documents are shared across views and save/reload semantics

Caveats:
- aliasing by path/key must remain stable across reloads
- stale writes cannot replace newer text or generations
- dirty and saved points must stay coherent under undo/redo

## Editor routing

Use for: opening a local text file or issuing save/refresh commands from the GUI

- [`AppCommand`](../../app/festerm/src/tabs.rs) — app-level text-document intent, including open/save/refresh and Markdown preview actions
- [`TextEditorTab`](../../app/festerm/src/text_editor.rs) — view state that binds to the shared document while keeping caret, scroll, and selection private

Evidence:
- [`open_remote_markdown_snapshot_opens_a_new_tab`](../../app/festerm/src/tabs.rs) — proves command-driven file opening is routed into new tabs
- [`docs/text-editor-design.md`](../../docs/text-editor-design.md) — editor layout and shared document semantics across views

Caveats:
- UI actions must not mutate document state directly
- save-as changes the view binding without rewriting other aliases
- closing a view must not close the shared document until the last view is gone

## Markdown load

Use for: opening local or remote Markdown and deciding whether to preview or open as a standalone viewer

- [`MarkdownLoader`](../../crates/festerm-markdown/src/lib.rs) — reads bounded bytes, validates UTF-8/binary/size, and parses a read-only snapshot
- [`MarkdownViewerTab`](../../app/festerm/src/markdown_viewer.rs) — tab owns source identity, preview/source modes, image policy, and remote/local load results

Evidence:
- [`open_remote_parses_in_memory_bytes_without_touching_disk`](../../app/festerm/src/markdown_viewer.rs) — proves remote Markdown loads from verified bytes without a temp file
- [`docs/markdown-viewer-design.md`](../../docs/markdown-viewer-design.md) — saved local Markdown reuses editor Preview; remote snapshots stay read-only

Caveats:
- source identity and stale snapshot checks must not silently retarget a different remote origin
- resource approvals are per document and must not inherit across reloads
- size, UTF-8, and binary bounds are hard policy gates

## Highlighting

Use for: building syntax spans or rendering Markdown previews with code fences and tables

- [`DocumentSyntax`](../../crates/festerm-syntax/src/lib.rs) — language detection, Tree-sitter reparse-on-revision, and viewport-scoped spans
- [`MarkdownRenderState`](../../app/festerm/src/markdown_viewer.rs) — preview renderer preserves exact source spans while managing table and code layout state

Evidence:
- [`docs/markdown-viewer-design.md`](../../docs/markdown-viewer-design.md) — Preview reuses a bounded renderer and fenced highlighting shares the syntax engine

Caveats:
- syntax span caches are revision-bound and must invalidate on text or layout changes
- next-parse and range-query logic must remain viewport-scoped to avoid whole-file churn
- layout epochs invalidate on context/theme/atlas changes; stale cached tables are easy to reuse
