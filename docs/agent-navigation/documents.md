<!-- agent-navigation: {"domain": "documents", "generator": "2", "model": "mai-code-1.1-flash", "sources": "97c0c1212dbf38b341e2d9a7dc0648639e950d40f9f5b37e9e5508210cfe25d4"} -->
# Documents, editor, and Markdown

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns the shared document registry, editor/viewer routing, Markdown parsing and rendering, and syntax-bound document state.

Canonical docs: [docs/text-editor-design.md](../../docs/text-editor-design.md), [docs/markdown-viewer-design.md](../../docs/markdown-viewer-design.md)

## Shared documents

Use for: Opening, reusing, or refreshing a file across tabs and splits

- [`TextDocument`](../../crates/festerm-document/src/text.rs) — shared mutable buffer, normalization, dirty/save revision, and undo state
- [`DocumentRegistry`](../../app/festerm/src/documents.rs) — deduplicates open files by origin and tracks reload and save state

Evidence:
- [`opening_one_path_twice_shares_a_single_buffer`](../../app/festerm/src/documents.rs) — same path reuses one buffer and shared view count
- [`opening_a_symlink_alias_reuses_the_loaded_source_authority`](../../app/festerm/src/documents.rs) — equivalent local aliases stay bound to the same source identity

Caveats:
- aliasing by path or source authority must stay stable across reloads
- dirty/saved points and revision counters must remain coherent under undo and reload
- stale writes must not replace newer text or generations

## Editor routing

Use for: Opening a local text file or issuing save, refresh, preview, and split commands

- [`AppCommand`](../../app/festerm/src/tabs.rs) — app-level document intent including open, save, refresh, and Markdown preview actions
- [`TextEditorTab`](../../app/festerm/src/text_editor.rs) — view state binds to the shared document while keeping caret, scroll, and selection private

Evidence:
- [`open_remote_markdown_snapshot_opens_a_new_tab`](../../app/festerm/src/tabs.rs) — remote Markdown commands route into a new Markdown viewer tab
- [`open_remote_text_snapshot_opens_a_new_editor_tab`](../../app/festerm/src/tabs.rs) — remote text commands route into a new editor tab

Caveats:
- UI actions must not mutate document state directly
- save-as changes the view binding without rewriting other aliases
- closing a view must not close the shared document until the last view is gone

## Markdown load

Use for: Opening local or remote Markdown and deciding whether to preview or open as a standalone viewer

- [`MarkdownLoader`](../../crates/festerm-markdown/src/lib.rs) — reads bounded bytes, validates UTF-8/binary/size, and parses a read-only snapshot
- [`MarkdownViewerTab`](../../app/festerm/src/markdown_viewer.rs) — tab owns source identity, preview/source modes, file state, and remote/local load results

Evidence:
- [`open_remote_parses_in_memory_bytes_without_touching_disk`](../../app/festerm/src/markdown_viewer.rs) — remote Markdown loads from verified bytes without a temp file
- [`matches_remote_source_requires_full_remote_identity`](../../app/festerm/src/markdown_viewer.rs) — stale remote snapshots are rejected when origin, owner, hash, or generation changes

Caveats:
- source identity and stale snapshot checks must not silently retarget a different remote origin
- resource approvals are per document and must not inherit across reloads
- size, UTF-8, and binary bounds are hard policy gates

## Highlighting

Use for: Building syntax spans and rendering previews with fenced code, tables, and viewport-scoped ranges

- [`DocumentSyntax`](../../crates/festerm-syntax/src/lib.rs) — language detection, Tree-sitter reparse on revision, and viewport-scoped span queries
- [`MarkdownViewerTab`](../../app/festerm/src/markdown_viewer.rs) — preview renderer keeps source spans exact while managing table and code layout state

Evidence:
- [`spans`](../../crates/festerm-syntax/src/lib.rs) — query results are cached by revision and viewport range
- [`docs/markdown-viewer-design.md`](../../docs/markdown-viewer-design.md) — Preview reuses bounded rendering and fenced highlighting shares the syntax engine

Caveats:
- syntax span caches are revision-bound and must invalidate on text or layout changes
- range queries must stay viewport-scoped to avoid whole-file churn
- layout epochs invalidate on theme or atlas changes; stale cached tables are easy to reuse
