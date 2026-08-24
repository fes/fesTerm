# Iosevka Term terminal font

fesTerm bundles four static faces built reproducibly from **Iosevka 34.8.1**:
Regular, Bold, Italic, and Bold Italic. The checked-in build plan retains Term
spacing and assigns a conservative, explicit programming-ligature set to the
standard contextual-alternates feature supported by egui's shaping path.

Source: <https://github.com/be5invis/Iosevka/releases/tag/v34.8.1>.
`manifest.json` records the exact source archive, release digest, build target,
file sizes, and checksums. Rebuild from the pinned source by copying
`private-build-plans.toml` to its root, running `npm install`, then
`npm run build -- ttf-unhinted::FesTermIosevka --jCmd=2`.

The derivative uses the distinct internal family name `fesTerm Iosevka Term`.
Iosevka is distributed under the SIL Open Font License 1.1 and declares no
Reserved Font Name; `LICENSE.md` is copied unchanged from the pinned tag.

The weekly bundled-font workflow monitors official Iosevka releases and opens
or updates a family-specific review issue. Updates require a deliberate rebuild
from the checked-in plan and review; they are never merged automatically.
