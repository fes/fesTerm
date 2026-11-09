#!/usr/bin/env python3
"""Upload a signed fesTerm AAB to the Google Play Internal testing track.

Uses the Android Publisher v3 edits API with a short-lived OAuth access token
in PLAY_ACCESS_TOKEN (minted by GitHub OIDC workload identity federation).
Promotion beyond Internal testing stays a manual Play Console action.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request

PACKAGE = "io.github.fes.festerm"
TRACK = "internal"
STATUSES = ("draft", "completed")
API = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/"
UPLOAD_API = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/"
RESPONSE_LIMIT = 1024 * 1024
BUNDLE_LIMIT = 200 * 1024 * 1024
TOKEN = "PLAY_ACCESS_TOKEN"


class PlayError(Exception):
    pass


class Client:
    def __init__(self, token, opener=urllib.request.urlopen):
        self.token = token
        self.opener = opener

    def call(self, method, url, body=None, content_type="application/json", length=None, timeout=60):
        headers = {"Authorization": "Bearer " + self.token, "Accept": "application/json"}
        if body is not None:
            headers["Content-Type"] = content_type
            if length is not None:
                headers["Content-Length"] = str(length)
        request = urllib.request.Request(url, data=body, method=method, headers=headers)
        try:
            with self.opener(request, timeout=timeout) as response:
                data = response.read(RESPONSE_LIMIT + 1)
        except urllib.error.HTTPError as error:
            with error:
                detail = error.read(4096).decode("utf-8", "replace")
            raise PlayError(f"{method} {url.split('?')[0]} failed: HTTP {error.code}: {detail}") from None
        if len(data) > RESPONSE_LIMIT:
            raise PlayError(f"{method} response exceeds {RESPONSE_LIMIT} bytes")
        return json.loads(data) if data.strip() else {}


def upload(client, bundle, version_code, release_name, status, package=PACKAGE):
    if status not in STATUSES:
        raise PlayError(f"Release status must be one of {', '.join(STATUSES)}")
    size = bundle.stat().st_size
    if not 0 < size <= BUNDLE_LIMIT:
        raise PlayError(f"Bundle size {size} is outside the accepted bound")
    with bundle.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    app = API + urllib.parse.quote(package, safe="")
    edit = client.call("POST", app + "/edits", b"{}").get("id")
    if not isinstance(edit, str) or not edit:
        raise PlayError("Play did not return an edit id")
    edit_url = app + "/edits/" + urllib.parse.quote(edit, safe="")
    committed = False
    try:
        with bundle.open("rb") as source:
            uploaded = client.call(
                "POST", UPLOAD_API + urllib.parse.quote(package, safe="") + "/edits/"
                + urllib.parse.quote(edit, safe="") + "/bundles?uploadType=media",
                source, "application/octet-stream", size, timeout=900)
        if str(uploaded.get("versionCode")) != str(version_code):
            raise PlayError(f"Play accepted versionCode {uploaded.get('versionCode')}, expected {version_code}")
        returned = uploaded.get("sha256")
        if not isinstance(returned, str) or returned.lower() != digest:
            raise PlayError("Play bundle sha256 is missing or does not match the local artifact")
        track = {"track": TRACK, "releases": [
            {"name": release_name, "versionCodes": [str(version_code)], "status": status}]}
        client.call("PUT", edit_url + "/tracks/" + TRACK, json.dumps(track).encode())
        client.call("POST", edit_url + ":commit")
        committed = True
    finally:
        if not committed:
            try:
                client.call("DELETE", edit_url)
            except Exception:  # Best effort: preserve the original failure.
                pass
    return {"package": package, "track": TRACK, "status": status,
            "version_code": version_code, "sha256": digest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--version-code", type=int, required=True)
    parser.add_argument("--release-name", required=True)
    parser.add_argument("--status", default=os.environ.get("ANDROID_PLAY_RELEASE_STATUS", "draft"))
    args = parser.parse_args()
    token = os.environ.get(TOKEN)
    if not token:
        parser.exit(1, f"Play upload failed: {TOKEN} is not set\n")
    try:
        result = upload(Client(token), args.bundle, args.version_code, args.release_name, args.status)
    except (OSError, ValueError, PlayError) as error:
        parser.exit(1, f"Play upload failed: {error}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
