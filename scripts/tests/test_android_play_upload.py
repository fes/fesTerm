import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import urllib.error


SCRIPT = Path(__file__).resolve().parents[1] / "upload-android-play-bundle.py"
SPEC = importlib.util.spec_from_file_location("android_play_upload", SCRIPT)
play = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(play)


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakePlay:
    def __init__(self, version_code=1001, fail=None, sha256=hashlib.sha256(b"aab").hexdigest()):
        self.version_code = version_code
        self.sha256 = sha256
        self.fail = fail
        self.calls = []
        self.timeouts = []

    def __call__(self, request, timeout):
        body = request.data.read() if hasattr(request.data, "read") else request.data
        self.calls.append((request.get_method(), request.full_url, body, dict(request.header_items())))
        self.timeouts.append(timeout)
        path = request.full_url.split("/applications/io.github.fes.festerm", 1)[1]
        if self.fail and self.fail in path:
            raise urllib.error.HTTPError(request.full_url, 403, "denied", {}, io.BytesIO(b'{"error":"denied"}'))
        if path == "/edits":
            return Response(b'{"id": "edit/1"}')
        if "/bundles" in path:
            return Response(json.dumps({"versionCode": self.version_code, "sha256": self.sha256}).encode())
        return Response(b"{}")


class PlayUploadTests(unittest.TestCase):
    def run_upload(self, fake, status="draft"):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "fesTerm.aab"
            bundle.write_bytes(b"aab")
            return play.upload(play.Client("secret-token", fake), bundle, 1001, "0.16.0 (1001)", status)

    def test_upload_commits_internal_release(self):
        fake = FakePlay()
        result = self.run_upload(fake)
        self.assertEqual([(method, url.split("festerm")[1]) for method, url, _, _ in fake.calls], [
            ("POST", "/edits"),
            ("POST", "/edits/edit%2F1/bundles?uploadType=media"),
            ("PUT", "/edits/edit%2F1/tracks/internal"),
            ("POST", "/edits/edit%2F1:commit"),
        ])
        self.assertEqual(fake.calls[1][2], b"aab")
        self.assertEqual(fake.calls[1][3]["Content-type"], "application/octet-stream")
        self.assertEqual(fake.calls[1][3]["Content-length"], "3")
        self.assertEqual(fake.timeouts[1], 900)
        self.assertEqual(json.loads(fake.calls[2][2]), {"track": "internal", "releases": [
            {"name": "0.16.0 (1001)", "versionCodes": ["1001"], "status": "draft"}]})
        self.assertTrue(all(h["Authorization"] == "Bearer secret-token" for _, _, _, h in fake.calls))
        self.assertEqual(result["track"], "internal")

    def test_failures_abandon_the_edit(self):
        for fake, message in ((FakePlay(version_code=999), "versionCode 999"),
                              (FakePlay(sha256=None), "sha256"),
                              (FakePlay(sha256="0" * 64), "sha256"),
                              (FakePlay(fail="/tracks/"), "HTTP 403")):
            with self.subTest(message=message):
                with self.assertRaisesRegex(play.PlayError, message):
                    self.run_upload(fake)
                self.assertEqual(fake.calls[-1][:2][0], "DELETE")
                self.assertNotIn(":commit", " ".join(url for _, url, _, _ in fake.calls))

    def test_unknown_status_is_rejected_before_any_request(self):
        fake = FakePlay()
        with self.assertRaisesRegex(play.PlayError, "Release status"):
            self.run_upload(fake, status="inProgress")
        self.assertEqual(fake.calls, [])


if __name__ == "__main__":
    unittest.main()
