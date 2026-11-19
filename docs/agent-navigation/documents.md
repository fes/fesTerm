<!-- agent-navigation: {"domain": "documents", "generator": "2", "model": "mai-code-1.1-flash", "sources": "c789fd751330a1e9297732bb88d748f988849401261312123fd498a19bdc97de"} -->
# Documents, editor, and Markdown

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns shared document identity, editor/viewer routing, and Markdown parsing and preview state for open text files.

Canonical docs: [docs/text-editor-design.md](../../docs/text-editor-design.md), [docs/markdown-viewer-design.md](../../docs/markdown-viewer-design.md)

## Startup activation

Use for: When a new process is launched with --open paths or needs to forward file-open requests to a live app

- [`DocumentActivationServer`](../../app/festerm/src/document_activation.rs) — owns the activation socket and cross-process queue for file-open requests
- [`ActivationQueue`](../../app/festerm/src/document_activation.rs) — buffers startup paths and wakes the app when a batch is ready

Evidence:
- [`parses_open_and_dash_prefixed_paths_without_utf8_loss`](../../app/festerm/src/document_activation.rs) — CLI parsing accepts document paths and preserves byte-level content
- [`bounded_queue_reports_full_without_dropping`](../../app/festerm/src/document_activation.rs) — startup queue enforces capacity and reports full batches truthfully

Caveats:
- startup forwarding must not silently drop or reorder document batches
- activation endpoints must be single-owner and safe-mode must not share normal-mode state

## Shared document state

Use for: When the same file is opened across tabs, splits, or reloads

- [`DocumentRegistry`](../../app/festerm/src/documents.rs) — deduplicates local and remote documents by origin and tracks save and reload state
- [`TextDocument`](../../crates/festerm-document/src/text.rs) — shared mutable buffer, dirty/save revision, and undo history

Evidence:
- [`opening_one_path_twice_shares_a_single_buffer`](../../app/festerm/src/documents.rs) — same path reuses one document buffer and view count
- [`opening_a_symlink_alias_reuses_the_loaded_source_authority`](../../app/festerm/src/documents.rs) — equivalent aliases stay bound to one source identity

Caveats:
- aliasing by path or source authority must stay stable across reloads
- dirty/saved points and revision counters must stay coherent under undo and reload

## Editor view routing

Use for: When a user opens, saves, refreshes, previews, or splits a text document

- [`TextEditorTab`](../../app/festerm/src/text_editor.rs) — view-local caret, scroll, options, and search state bound to a shared document
- [`AppCommand`](../../app/festerm/src/tabs.rs) — app-level actions fan out to open, save, refresh, and preview flows

Evidence:
- [`a_markdown_file_opens_in_preview_and_a_plain_one_opens_in_edit`](../../app/festerm/src/text_editor.rs) — Markdown opens in preview mode while plain text stays in edit mode

Caveats:
- UI actions must not mutate document state directly
- closing a view must not close the shared document until the last view is gone

## Markdown preview load

Use for: When a file is opened as Markdown or a remote snapshot is loaded

- [`MarkdownLoader`](../../crates/festerm-markdown/src/lib.rs) — reads bounded bytes, validates UTF-8/binary limits, and parses a read-only snapshot
- [`MarkdownViewerTab`](../../app/festerm/src/markdown_viewer.rs) — keeps source identity, preview/source mode, and stale snapshot checks for the tab

Evidence:
- [`open_remote_parses_in_memory_bytes_without_touching_disk`](../../app/festerm/src/markdown_viewer.rs) — remote Markdown loads from verified bytes without a temp file
- [`matches_remote_source_requires_full_remote_identity`](../../app/festerm/src/markdown_viewer.rs) — stale remote snapshots are rejected on owner, hash, path, or generation change

Caveats:
- source identity and stale snapshot checks must not silently retarget a different remote origin
- resource approvals are per document and must not inherit across reloads
