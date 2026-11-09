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
    def __init__(self, version_code=1001, fail=None, sha256=hashlib.sha256(b"aab").hexdigest(),
                 existing=900, track_status="completed", cleanup_fail=False):
        self.version_code = version_code
        self.sha256 = sha256
        self.fail = fail
        self.existing = existing
        self.track_status = track_status
        self.cleanup_fail = cleanup_fail
        self.calls = []
        self.timeouts = []

    def __call__(self, request, timeout):
        body = request.data.read() if hasattr(request.data, "read") else request.data
        self.calls.append((request.get_method(), request.full_url, body, dict(request.header_items())))
        self.timeouts.append(timeout)
        path = request.full_url.split("/applications/io.github.fes.festerm", 1)[1]
        if self.cleanup_fail and request.get_method() == "DELETE":
            raise urllib.error.HTTPError(request.full_url, 500, "cleanup failed", {}, io.BytesIO(b"cleanup failed"))
        if self.fail and self.fail in path:
            raise urllib.error.HTTPError(request.full_url, 403, "denied", {}, io.BytesIO(b'{"error":"denied"}'))
        if path == "/edits":
            return Response(b'{"id": "edit/1"}')
        if request.get_method() == "GET" and path.endswith("/bundles"):
            bundles = [] if self.existing is None else [{"versionCode": self.existing}]
            return Response(json.dumps({"bundles": bundles}).encode())
        if request.get_method() == "GET" and path.endswith("/apks"):
            return Response(b'{"apks": []}')
        if request.get_method() == "GET" and path.endswith("/tracks"):
            releases = [] if self.existing is None else [
                {"versionCodes": [str(self.existing)], "status": self.track_status}]
            return Response(json.dumps({"tracks": [{"track": "internal", "releases": releases}]}).encode())
        if request.get_method() == "POST" and "/bundles" in path:
            return Response(json.dumps({"versionCode": self.version_code, "sha256": self.sha256}).encode())
        return Response(b"{}")


class PlayUploadTests(unittest.TestCase):
    def run_upload(self, fake, status="completed"):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "fesTerm.aab"
            bundle.write_bytes(b"aab")
            return play.upload(play.Client("fixture-token", fake), bundle, 1001, "0.16.0 (1001)", status)

    def test_upload_commits_internal_release(self):
        fake = FakePlay()
        result = self.run_upload(fake)
        self.assertEqual([(method, url.split("festerm")[1]) for method, url, _, _ in fake.calls], [
            ("POST", "/edits"),
            ("GET", "/edits/edit%2F1/bundles"),
            ("GET", "/edits/edit%2F1/apks"),
            ("GET", "/edits/edit%2F1/tracks"),
            ("POST", "/edits/edit%2F1/bundles?uploadType=media"),
            ("PUT", "/edits/edit%2F1/tracks/internal"),
            ("POST", "/edits/edit%2F1:commit"),
        ])
        self.assertEqual(fake.calls[4][2], b"aab")
        self.assertEqual(fake.calls[4][3]["Content-type"], "application/octet-stream")
        self.assertEqual(fake.calls[4][3]["Content-length"], "3")
        self.assertEqual(fake.timeouts[4], 900)
        self.assertEqual(json.loads(fake.calls[5][2]), {"track": "internal", "releases": [
            {"name": "0.16.0 (1001)", "versionCodes": ["1001"], "status": "completed"}]})
        self.assertTrue(all(h["Authorization"].startswith("Bearer ") for _, _, _, h in fake.calls))
        self.assertEqual(result["track"], "internal")
        self.assertEqual(result["status"], "completed")

    def test_failures_abandon_the_edit(self):
        for fake, message in ((FakePlay(version_code=999), "versionCode 999"),
                              (FakePlay(sha256=None), "sha256"),
                              (FakePlay(sha256="0" * 64), "sha256"),
                              (FakePlay(fail="/tracks/"), "HTTP 403")):
            with self.subTest(message=message):
                with self.assertRaisesRegex(play.PlayError, message):
                    self.run_upload(fake)
                self.assertEqual(fake.calls[-1][0], "DELETE")
                self.assertNotIn(":commit", " ".join(url for _, url, _, _ in fake.calls))

    def test_initial_upload_old_version_and_unfinished_release_are_refused(self):
        for fake, message in (
            (FakePlay(existing=None), "first app upload"),
            (FakePlay(existing=1001), "must exceed"),
            (FakePlay(existing=2000), "must exceed"),
            (FakePlay(track_status="draft"), "unfinished release"),
        ):
            with self.subTest(message=message), self.assertRaisesRegex(play.PlayError, message):
                self.run_upload(fake)
            self.assertEqual(fake.calls[-1][0], "DELETE")
            self.assertNotIn("uploadType=media", " ".join(url for _, url, _, _ in fake.calls))

    def test_cleanup_and_uncertain_commit_failures_are_reported(self):
        fake = FakePlay(sha256=None, cleanup_fail=True)
        with self.assertRaisesRegex(play.PlayError, "sha256.*failed to abandon"):
            self.run_upload(fake)
        fake = FakePlay(fail=":commit")
        with self.assertRaisesRegex(play.PlayError, "outcome is unknown.*Play Console"):
            self.run_upload(fake)
        self.assertEqual(fake.calls[-1][0], "POST")
        self.assertIn(":commit", fake.calls[-1][1])

    def test_unknown_status_is_rejected_before_any_request(self):
        fake = FakePlay()
        with self.assertRaisesRegex(play.PlayError, "Release status"):
            self.run_upload(fake, status="inProgress")
        self.assertEqual(fake.calls, [])


if __name__ == "__main__":
    unittest.main()
