# Unicode Emoji 15.1 Test Data

These files are the canonical Unicode 15.1 emoji test and sequence data used
to validate fesTerm's pinned Unicode 15.1 width contract:

- `emoji-test.txt`
- `emoji-sequences.txt`
- `emoji-zwj-sequences.txt`

They are copied verbatim from `https://www.unicode.org/Public/emoji/15.1/`.
`LICENSE.txt` is the Unicode License v3. `manifest.json` records the exact
source URLs, byte sizes, SHA-256 hashes, and package-local mirrors used by
crate tests so packaged sources remain self-contained.

Run `python scripts/check_unicode_emoji_data.py` for local verification or add
`--check-upstream` to compare the checked-in bytes with unicode.org.
