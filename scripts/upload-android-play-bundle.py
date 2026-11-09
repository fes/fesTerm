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
import re
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
MAX_VERSION_CODE = 2_100_000_000


class PlayError(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        return None


class Client:
    def __init__(self, token, opener=None):
        self.token = token
        self.opener = opener or urllib.request.build_opener(NoRedirect()).open

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
        result = json.loads(data) if data.strip() else {}
        if not isinstance(result, dict):
            raise PlayError(f"{method} returned an invalid Play response")
        return result


def existing_versions(client, edit_url):
    versions = []
    for collection in ("bundles", "apks", "tracks"):
        response = client.call("GET", edit_url + "/" + collection)
        items = response.get(collection, [])
        if not isinstance(items, list):
            raise PlayError(f"Play returned an invalid {collection} list")
        for item in items:
            if not isinstance(item, dict):
                raise PlayError(f"Play returned an invalid {collection} entry")
            if collection == "tracks":
                releases = item.get("releases", [])
                if not isinstance(releases, list):
                    raise PlayError("Play returned an invalid track release list")
                for release in releases:
                    if not isinstance(release, dict):
                        raise PlayError("Play returned an invalid track release")
                    if item.get("track") == TRACK and release.get("status") in ("draft", "inProgress", "halted"):
                        raise PlayError("Internal track has an unfinished release; resolve it in Play Console")
                    codes = release.get("versionCodes", [])
                    if not isinstance(codes, list):
                        raise PlayError("Play returned invalid track version codes")
                    versions.extend(codes)
            else:
                versions.append(item.get("versionCode"))
    if any(not re.fullmatch(r"[1-9][0-9]*", str(code)) or int(code) > MAX_VERSION_CODE for code in versions):
        raise PlayError("Play returned an invalid versionCode")
    return [int(code) for code in versions]


def upload(client, bundle, version_code, release_name, status, package=PACKAGE):
    if status not in STATUSES:
        raise PlayError(f"Release status must be one of {', '.join(STATUSES)}")
    if not 1 <= version_code <= MAX_VERSION_CODE:
        raise PlayError("Play versionCode is outside the accepted range")
    if not 1 <= len(release_name) <= 50:
        raise PlayError("Play release name must contain 1 to 50 characters")
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
    commit_started = False
    try:
        versions = existing_versions(client, edit_url)
        if not versions:
            raise PlayError("The first app upload must be completed manually in Play Console")
        if version_code <= max(versions):
            raise PlayError(f"Play versionCode {version_code} must exceed existing versionCode {max(versions)}")
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
        commit_started = True
        client.call("POST", edit_url + ":commit")
    except (OSError, ValueError, PlayError) as error:
        if commit_started:
            raise PlayError(f"Play commit failed or its outcome is unknown; check Play Console before retrying: {error}") from error
        try:
            client.call("DELETE", edit_url)
        except (OSError, ValueError, PlayError) as cleanup:
            raise PlayError(f"{error}; additionally failed to abandon the Play edit: {cleanup}") from error
        raise
    return {"package": package, "track": TRACK, "status": status,
            "version_code": version_code, "sha256": digest}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--version-code", type=int, required=True)
    parser.add_argument("--release-name", required=True)
    parser.add_argument("--status", required=True, choices=STATUSES)
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
