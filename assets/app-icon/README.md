# fesTerm Application Icon

`app-icon.svg` is the platform-neutral 1024 × 1024 application-icon master.
It extends the compact UI `AppMark` into a launch-surface identity rather than
depicting a terminal window or borrowing an operating-system logo.

## Design

- The structural `F` and prompt/baseline are the same product idea as
  `assets/icons/source/app-mark.svg`.
- The graphite tile is deliberately quiet so the mark remains legible in busy
  docks, taskbars, launchers, and application lists.
- Warm white identifies fesTerm; cyan is reserved for the prompt. The app icon
  is a brand asset, so it is not constrained by the monochrome/current-color
  rule for UI action icons.
- The tile occupies the central 90.6% of the canvas and all essential artwork
  stays well inside the central safe area. Platform packaging may apply its own
  mask without clipping the mark.
- The source avoids filters, raster effects, text, and third-party paths. It
  remains deterministic and renders without font dependencies.

## Raster review assets

Committed PNGs are direct renders of the master at 1024, 512, 256, 128, 64,
32, and 16 pixels. Review the 32 px and 16 px outputs at native scale; they are
the constraint on future detail. Do not hand-edit the PNGs.

Platform packaging should consume the master or a generated PNG and create the
native container required by that platform (`.icns`, `.ico`, or desktop
packaging resources). Keep those packaging recipes with the application build
configuration rather than changing the master artwork per operating system.
Regenerate the checked-in Windows container with
`python scripts/generate_windows_icon.py`; CI checks that it still matches the
canonical PNGs.
