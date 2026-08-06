# Golden Fixture Format

`festerm-test-support` discovers every `*.fixture` file under this directory.
Fixtures exercise the GUI-independent terminal core and must stay readable in
reviews.

## Required Fields

```text
name: descriptive scenario name
size: 80x24
input: "bytes received from the session"
grid:
  - "first expected row"
  - "second expected row"
cursor: 0,0
replies: "expected bytes sent toward the session"
```

`input` and `replies` accept quoted UTF-8 text plus `\\xNN`, `\\r`, `\\n`,
`\\t`, `\\\"`, and `\\\\` escapes. `\\xNN` adds one raw byte, including bytes
outside UTF-8.

Grid rows must exactly match the final dimensions. A width-two cell’s
continuation is represented as a space in `grid`; use a `cells` assertion to
check its width role.

## Optional Fields

```text
resize: 100x30
dirty: "0,1,2"
modes: "auto_wrap=true,origin_mode=false,alternate_screen=false,cursor_visible=true"
cursor_style: steady-bar
title: "session title"
cells:
  - "column,row|text|foreground|background|attributes|width"
```

`resize` applies once after all input. `dirty` checks rows dirtied by input and
the optional resize after constructor dirtiness has been cleared.

`modes` can also assert `application_cursor`, `application_keypad`,
`bracketed_paste`, `focus_reporting`, `mouse_tracking`, and `sgr_mouse`.
Unlisted modes use their documented defaults. `mouse_tracking` is one of
`none`, `x10`, `button-event`, `button-motion`, or `any-motion`.

`cursor_style` is optional and is one of `blinking-block`, `steady-block`,
`blinking-underline`, `steady-underline`, `blinking-bar`, or `steady-bar`.

`title` optionally asserts the sanitized OSC 0/2 title.

Cell colors are `default`, `indexed:0` through `indexed:255`, or
`rgb:red,green,blue`. Attributes are `none` or comma-separated names such as
`bold,underline`. Width is optional and is `single`, `double`, or
`continuation`.

## Adding a Regression

1. Create the smallest fixture that exposes the behavior in the appropriate
   `core`, `m2`, or later milestone directory.
2. State the user-visible scenario in `name` or a comment.
3. Assert attributes, cell width, modes, dirty rows, and replies whenever they
   are relevant; do not rely solely on parser recognition.
4. Run `cargo test -p festerm-test-support` and the workspace validation
   commands.
