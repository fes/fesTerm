# fesTerm Product Positioning

**Status:** Draft

This note records the product posture selected for fesTerm and the terminal landscape used to inform it. It is not a feature-parity commitment or competitive scorecard.

## Landscape Observations

Modern terminals tend to emphasize different combinations of capabilities:

- A focused terminal emulator can prioritize rendering speed and a relatively small product surface.
- A cross-platform terminal workstation can combine tabs, panes, native SSH, multiplexing, ligatures, hot-reloaded configuration, and advanced graphics.
- A platform terminal can emphasize deep operating-system integration, profiles, tabs, panes, Unicode, customization, and GPU-backed rendering.
- A workflow-oriented terminal can layer shell integration, searchable profiles, triggers, scripting, automation, and command-aware behavior on top of emulation.

Representative official documentation:

- [Alacritty](https://alacritty.org/) describes a cross-platform OpenGL terminal emulator with a focused terminal-emulation product surface.
- [WezTerm features](https://wezterm.org/features.html) include cross-platform operation, tabs and panes, native SSH, multiplexing, ligatures, font fallback, true color, SGR mouse reporting, hot-reloaded configuration, and optional graphics protocols.
- [Windows Terminal overview](https://learn.microsoft.com/en-us/windows/terminal/) describes tabs, panes, profiles, Unicode and UTF-8, customization, and GPU-accelerated text rendering.
- [iTerm2 features](https://iterm2.com/features.html) and [scripting documentation](https://iterm2.com/documentation-scripting.html) show a workflow-rich direction with searchable profiles, triggers, shell integration, tmux integration, and automation.

These examples show that terminal emulation, session management, rendering, workflow automation, and multiplexing are separable product dimensions. fesTerm does not need to adopt all of them at once.

## Selected Position

fesTerm will occupy the middle ground between a minimal emulator and an all-encompassing command-line environment:

> A traditional, high-correctness terminal and native SSH client that grows into a capable cross-platform workstation without forgetting that terminal behavior is its foundation.

This means:

- More integrated than a minimal window around a local shell.
- Less workflow-opinionated than products that reinterpret the command line around proprietary blocks or mandatory cloud services.
- More focused initially than terminals that combine emulation, remote multiplexing, scripting, graphics, and extensive shell integration from the outset.
- Local-first, with native SSH, profiles, tabs, restoration, and diagnostics treated as natural terminal capabilities.

## Initial Product Commitments

- Correct operation with advanced full-screen terminal applications.
- Cross-platform support from a shared Rust codebase.
- Tabs with first-class local and SSH sessions.
- Native SSH without an external SSH executable dependency.
- Fast interactive rendering and scrollback behavior.
- Human-readable, versioned configuration.
- Ligature support after cell mapping and shaping correctness are established.
- Test fixtures, CI, and observability from the foundation phase.

## Deliberately Deferred Dimensions

The architecture should preserve room for these, but they do not define the initial product:

- Built-in detachable multiplexing or a tmux replacement.
- General scripting and automation APIs.
- Third-party plugins.
- Command-block or shell-history-centered UX.
- Inline graphics protocols.
- Mandatory account identity or cloud synchronization.

## Evaluation Rule

New feature proposals should be assessed in this order:

1. Does the feature improve terminal correctness or interoperability?
2. Does it improve local or SSH session usability?
3. Does it preserve interactive performance and privacy?
4. Can it remain optional and architecturally separate?
5. Is there a concrete user workflow that justifies its complexity now?

A feature that is merely present in another terminal is not, by itself, a requirement for fesTerm.
