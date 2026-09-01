import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_unicode_emoji_data as emoji_data


class UnicodeEmojiDataTests(unittest.TestCase):
    def fixture(self, root: Path) -> dict:
        data = b"# emoji-test.txt\n# Version: 15.1\n1F600 ; fully-qualified\n"
        path = root / "emoji-test.txt"
        path.write_bytes(data)
        return {
            "schema_version": 1,
            "unicode_version": "15.1",
            "files": [
                {
                    "source_url": "https://example.test/emoji-test.txt",
                    "path": "emoji-test.txt",
                    "size_bytes": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                }
            ],
        }

    def test_verify_accepts_matching_versioned_data(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            emoji_data.verify(root, self.fixture(root))

    def test_verify_rejects_changed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture(root)
            (root / "emoji-test.txt").write_bytes(b"changed")
            with self.assertRaisesRegex(emoji_data.EmojiDataError, "size differs"):
                emoji_data.verify(root, manifest)

    def test_verify_rejects_changed_package_mirror(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self.fixture(root)
            mirror = root / "crate" / "emoji-test.txt"
            mirror.parent.mkdir()
            mirror.write_bytes(b"changed")
            manifest["files"][0]["package_mirrors"] = ["crate/emoji-test.txt"]
            with self.assertRaisesRegex(emoji_data.EmojiDataError, "differs from"):
                emoji_data.verify(root, manifest)

    def test_load_manifest_rejects_unknown_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps({"schema_version": 2}), encoding="utf-8")
            with self.assertRaisesRegex(emoji_data.EmojiDataError, "schema_version"):
                emoji_data.load_manifest(path)


if __name__ == "__main__":
    unittest.main()
