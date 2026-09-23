# ADR 0036: The Core Reports Colors From an Embedder-Supplied Scheme

- **Status:** Accepted
- **Date:** 2026-09-22
- **Supersedes:** None

## Context

`festerm-core` owns terminal protocol semantics and answers every device
request a program makes: DA/DA2, DSR, DECRQM, DECRQSS. It has been able to do
this without knowing anything about appearance, because none of those answers
describe how the screen looks.

`OSC 4/10/11/12` break that pattern. They ask the terminal to describe its own
colors:

| Request | Question |
| --- | --- |
| `OSC 4 ; <index> ; ? ST` | What RGB is palette entry `<index>`? |
| `OSC 10 ; ? ST` | What is the default foreground? |
| `OSC 11 ; ? ST` | What is the default background? |
| `OSC 12 ; ? ST` | What is the cursor color? |

These are how a TUI discovers whether it is running light or dark, and which
shades it can safely blend against. vim and neovim use `OSC 11` for
`background=` autodetection; bat, delta, fzf, and several Node-based CLIs do
the same. Left unanswered (issue #222), applications guess, and the common
guess is to drop background styling entirely — the symptom that prompted this
work was inline code spans rendering as bare padding in a CLI running inside
fesTerm.

The invariant in tension: **terminal protocol semantics belong in
`festerm-core`, never in GUI widgets** (`AGENTS.md`, Architectural Invariant 1),
yet the only component that knows the actual RGB values is the renderer. The
concrete colors lived entirely in `festerm-ui-egui`: `DEFAULT_FOREGROUND` and
`DEFAULT_BACKGROUND` in `src/lib.rs`, and a 16-entry ANSI table plus the
256-color resolution rules in `src/renderer.rs`.

## Decision

The core owns the *reporting*, the embedder owns the *values*.

`festerm-core` gains `ColorScheme`, a plain value type holding a foreground, a
background, a cursor color, and the 16 ANSI entries. `Terminal` holds one and
exposes `set_color_scheme`. The composition root hands it the front end's real
colors when it builds a session terminal. Answering `OSC 4/10/11/12` stays
entirely inside the core, which is where protocol semantics belong.

Palette entries 16 through 255 are *not* part of the scheme. The 6x6x6 color
cube and the 24-step gray ramp are defined by the protocol, not by a theme, so
the core computes them. `festerm-ui-egui::resolve_color` now resolves through
the same `ColorScheme`, which makes the renderer a consumer of the core's
palette rather than a second implementation of it.

**Only the query forms are honored.** `OSC 4/10/11/12` carrying an actual
color, and the `OSC 104/110/111/112` resets, remain ignored. The renderer does
not read a scheme back at paint time, so accepting a set would make the next
query report a color that nothing on screen uses. This is the same reasoning
as the DECRQM work in issue #214: a confidently wrong answer costs a program
more than a missing one, because a missing answer has a documented fallback
and a wrong one does not.

This ADR does not move ownership of the theme. `festerm-ui-egui` remains the
source of truth for what fesTerm looks like; the core simply holds a copy of
the part it has to be able to describe.

## Alternatives considered

- **Answer from constants duplicated in the core.** Simplest, and it needs no
  new API, but the reported color and the painted color would be two
  independently maintained tables. The first theme change makes the terminal
  lie about itself, and nothing would fail to catch it.
- **Move the whole theme into `festerm-core`.** This would make the core the
  single source of truth, but it inverts the dependency the project has
  deliberately kept: appearance decisions would land in the crate that is
  supposed to be GUI-independent, and chrome colors would have to live there
  too.
- **Answer from the renderer.** The renderer already knows the colors, so it
  could format the reply itself. This puts protocol semantics in presentation
  code, which Architectural Invariant 1 forbids, and it would need a second
  path into the reply queue that bypasses the single-writer model.
- **Honor the set forms too.** More complete on paper, and it is what xterm
  does. It requires the renderer to resolve colors through a per-terminal
  scheme at paint time, invalidate its glyph and row caches when the scheme
  changes, and decide what a reset means for a theme the user chose. That is a
  real feature with real cache-invalidation consequences, not a side effect of
  answering a question, so it stays out until it is asked for on its own
  merits.

## Consequences

- **Migration:** additive. `Terminal` gains two methods and a field defaulted
  to the current theme's values; existing callers are unaffected. The
  composition root gains one line.
- **Compatibility:** applications that probe for a background now get one.
  Programs that attempt a set see no change in behavior — the request is still
  ignored, and it still produces no reply.
- **Boundaries:** the renderer/core boundary is preserved and slightly
  strengthened. The 256-color resolution rules now have one implementation
  instead of two, and `resolve_color` keeps its existing public signature.
- **Bounds:** replies are queued through the existing bounded transport. A
  single request may name many colors, so the number of queries honored from
  one string is capped at 64; the remainder are discarded rather than queued.
- **Security:** the reply describes only colors the user already sees. No
  configuration, secret, or path is newly reachable by an untrusted program,
  and the query forms carry no attacker-controlled data into the reply beyond
  a validated palette index.
- **Performance:** `resolve_color` does the same arithmetic it did before, now
  behind one call. No allocation is added to the paint path.

## Validation impact

- **Invariants introduced or changed:** the core may hold concrete colors for
  the sole purpose of reporting them, and what it reports must be what the
  embedder paints. Protocol semantics remain in the core; the renderer gains
  no reply path.
- **GUI/action edges affected:** None. No user-visible workflow changes; a
  program's own rendering choices may change once it can detect the
  background.
- **Automated tests required:**
  `festerm_core::tests::osc_color_queries_report_the_colors_the_embedder_paints`,
  `a_color_reply_mirrors_the_terminator_of_its_request`,
  `consecutive_dynamic_colors_are_answered_in_request_order`,
  `a_color_set_is_ignored_and_never_changes_a_later_report`,
  `the_answerable_half_of_a_mixed_palette_request_is_still_answered`,
  `malformed_and_out_of_range_color_requests_are_silent`,
  `a_color_query_burst_is_bounded`,
  `an_abandoned_color_query_is_never_answered`, and
  `festerm_ui_egui::renderer::tests::the_reported_scheme_is_the_palette_the_renderer_paints`
  plus `the_cores_stand_in_scheme_still_matches_this_theme`.
- **Native/manual evidence required:** None. The behavior is byte-exact and
  fully covered headlessly.
- **Coverage superseded:** None.
