#!/usr/bin/env python3
"""Build and supervise a branded, data-isolated macOS development application."""

from __future__ import annotations

import argparse
import json
import os
import platform
import plistlib
import signal
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGER_VERSION = "cargo-packager 0.11.8"
PRODUCT_NAME = "fesTerm Development"
IDENTIFIER = "dev.fes.festerm.development"
STARTUP_TIMEOUT = 30
QUIT_TIMEOUT = 20

NATIVE_APPLICATION = r"""
ObjC.import('AppKit');
ObjC.import('CoreGraphics');
function run(args) {
    const pid = Number(args[0]);
    const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
    if (!app || app.isNil()) return JSON.stringify(null);
    const text = value => value && !value.isNil() ? ObjC.unwrap(value) : null;
    if (text(app.executableURL.path) !== args[1] ||
        text(app.bundleIdentifier) !== args[2] ||
        text(app.bundleURL.path) !== args[3]) {
        throw Error('Owned frontend identity does not match; refusing native control');
    }
    if (args[4] === 'quit') {
        return JSON.stringify({quitRequested: Boolean(app.terminate)});
    }
    const raw = $.CGWindowListCopyWindowInfo(1, 0);
    const windows = ObjC.deepUnwrap(ObjC.castRefToObject(raw))
        .filter(w => w.kCGWindowOwnerPID === pid && w.kCGWindowLayer === 0)
        .map(w => w.kCGWindowNumber);
    const generic = $.NSWorkspace.sharedWorkspace.iconForFile('/usr/bin/true');
    return JSON.stringify({
        pid, identifier: text(app.bundleIdentifier),
        name: text(app.localizedName), bundle: text(app.bundleURL.path),
        regular: Number(app.activationPolicy) === 0, windows,
        genericIcon: Boolean(app.icon.TIFFRepresentation
            .isEqualToData(generic.TIFFRepresentation))
    });
}
"""


class LaunchError(RuntimeError):
    pass


def build_environment() -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("APPLE_", "CARGO_PACKAGER_SIGN_"))
        and key != "FESTERM_UPDATE_PUBLIC_KEY"
    }
    environment["FESTERM_INSTALLATION_KIND"] = "development"
    return environment


def application_environment(root: Path) -> dict[str, str]:
    directories = {
        "HOME": root / "home",
        "XDG_CONFIG_HOME": root / "config",
        "XDG_DATA_HOME": root / "data",
        "XDG_CACHE_HOME": root / "cache",
        "XDG_STATE_HOME": root / "state",
        "XDG_RUNTIME_DIR": root / "runtime",
        "TMPDIR": root / "tmp",
        "FESTERM_SESSIOND_RUNTIME_ROOT": root / "sessiond",
    }
    for directory in directories.values():
        directory.mkdir(mode=0o700)
    return {
        **{key: str(value) for key, value in directories.items()},
        "ZDOTDIR": str(directories["HOME"]),
        "FESTERM_CONFIG_PATH": str(root / "config" / "festerm.toml"),
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "LANG": "en_US.UTF-8",
        "LC_ALL": "en_US.UTF-8",
    }


def validate_root(root: Path) -> None:
    # macOS sockaddr_un.sun_path has 104 bytes, including its terminating NUL.
    endpoint = root / "sessiond/2147483647-18446744073709551615.sock"
    if len(os.fsencode(endpoint)) >= 104:
        raise LaunchError("Output path is too long for native session sockets; "
                          "use a short new directory under your home")
    if platform.system() == "Darwin":
        for parent in root.parents:
            metadata = parent.stat()
            if metadata.st_uid not in (0, os.geteuid()) or metadata.st_mode & 0o022:
                raise LaunchError(
                    f"Untrusted output ancestor: {parent}; native hosting requires "
                    "root/user-owned ancestors without group/world write access "
                    "(including sticky directories such as /tmp)"
                )


def render_config(config: dict, binaries: Path, output: Path) -> str:
    lines = [
        f"{key} = {json.dumps(config[key], ensure_ascii=False)}"
        for key in ("name", "version", "description", "homepage", "authors")
    ]
    lines.extend(
        [
            f"product-name = {json.dumps(PRODUCT_NAME, ensure_ascii=False)}",
            f"identifier = {json.dumps(IDENTIFIER, ensure_ascii=False)}",
            f"out-dir = {json.dumps(str(output), ensure_ascii=False)}",
            'formats = ["app"]',
            "icons = " + json.dumps(
                [str((ROOT / "packaging" / icon).resolve()) for icon in config["icons"]],
                ensure_ascii=False,
            ),
            "binaries = [",
        ]
    )
    for binary in config["binaries"]:
        name = binary["path"]
        if Path(name).name != name or name in ("", ".", ".."):
            raise LaunchError("macOS manifest binaries must be sibling filenames")
        lines.append(
            "  { path = " + json.dumps(str(binaries / name), ensure_ascii=False)
            + ", main = " + str(binary["main"]).lower() + " },"
        )
    lines.extend(
        [
            "]",
            "[macos]",
            'signing-identity = "-"',
            "info-plist-path = " + json.dumps(str(
                (ROOT / "packaging" / config["macos"]["info-plist-path"]).resolve()
            ), ensure_ascii=False),
        ]
    )
    return "\n".join(lines) + "\n"


def package(root: Path, environment: dict[str, str]) -> tuple[Path, list[str]]:
    config = tomllib.loads((ROOT / "packaging/macos.toml").read_text(encoding="utf-8"))
    version = subprocess.run(
        ["cargo", "packager", "--version"], check=True, capture_output=True,
        text=True, cwd=ROOT, env=environment,
    ).stdout.strip()
    if version != PACKAGER_VERSION:
        raise LaunchError(f"Expected {PACKAGER_VERSION}; see packaging/README.md")
    if environment.get("CARGO_BUILD_TARGET"):
        raise LaunchError("Unset CARGO_BUILD_TARGET: this launcher requires a native host build")
    artifacts = {}
    for packages in (
        ["-p", "festerm", "-p", "festerm-sessiond"],
        ["-p", "festerm-tunnel-bridge", "--no-default-features", "--features",
         "relay", "--bins"],
    ):
        result = subprocess.run(
            ["cargo", "build", "--locked", "--message-format=json-render-diagnostics",
             *packages],
            check=True, stdout=subprocess.PIPE, text=True, cwd=ROOT, env=environment,
        )
        for line in result.stdout.splitlines():
            message = json.loads(line)
            if message["reason"] == "compiler-artifact" and message.get("executable"):
                artifacts[message["target"]["name"]] = Path(message["executable"])
    expected = [binary["path"] for binary in config["binaries"]]
    if any(name not in artifacts for name in expected):
        raise LaunchError("Cargo did not produce every canonical macOS binary")
    directories = {artifacts[name].parent for name in expected}
    if len(directories) != 1:
        raise LaunchError("Cargo produced helpers outside the frontend binary directory")
    binaries = directories.pop()
    generated = root / "macos-development.toml"
    generated.write_text(render_config(config, binaries, root / "bundle"), encoding="utf-8")
    subprocess.run(
        ["cargo", "packager", "--config", str(generated), "--formats", "app"],
        check=True, cwd=ROOT, env=environment,
    )
    bundle = root / "bundle" / f"{PRODUCT_NAME}.app"
    with (bundle / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if (info["CFBundleIdentifier"] != IDENTIFIER
            or info["CFBundleExecutable"] != "festerm"
            or info["CFBundleName"] != PRODUCT_NAME
            or info["CFBundleShortVersionString"] != config["version"]):
        raise LaunchError("Packaged development identity/version does not match")
    icon = bundle / "Contents/Resources" / info["CFBundleIconFile"]
    if not icon.is_file():
        raise LaunchError("Development application icon is missing")
    for binary in config["binaries"]:
        if not os.access(bundle / "Contents/MacOS" / binary["path"], os.X_OK):
            raise LaunchError(f"Development payload is missing {binary['path']}")
    subprocess.run(
        ["/usr/bin/codesign", "--verify", "--deep", "--strict", str(bundle)],
        check=True, env=environment,
    )
    return bundle, [
        binary["path"] for binary in config["binaries"] if not binary["main"]
    ]


def native_application(pid: int, executable: Path, action: str) -> dict | None:
    result = subprocess.run(
        ["/usr/bin/osascript", "-l", "JavaScript", "-", str(pid),
         str(executable), IDENTIFIER, str(executable.parents[2]), action],
        input=NATIVE_APPLICATION, capture_output=True, text=True, check=True, timeout=5,
    )
    return json.loads(result.stdout)


def wait_ready(process: subprocess.Popen, executable: Path) -> dict:
    deadline = time.monotonic() + STARTUP_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise LaunchError(f"Frontend exited before readiness: {process.returncode}")
        info = native_application(process.pid, executable, "inspect")
        if (info and info["regular"] and info["windows"] and not info["genericIcon"]
                and info["name"] == PRODUCT_NAME):
            return info
        time.sleep(0.2)
    raise LaunchError("Native development identity/window/icon readiness timed out")


def remaining_helpers(bundle: Path, helpers: list[str]) -> list[int]:
    result = subprocess.run(
        ["/bin/ps", "-axww", "-o", "pid=,comm="],
        check=True, capture_output=True, text=True, timeout=5,
    )
    paths = {str(bundle / "Contents/MacOS" / name) for name in helpers}
    owned = []
    for line in result.stdout.splitlines():
        fields = line.strip().split(maxsplit=1)
        if len(fields) == 2 and fields[1] in paths:
            owned.append(int(fields[0]))
    return owned


def write_status(root: Path, state: dict) -> None:
    temporary = root / "launch.json.tmp"
    temporary.write_text(json.dumps(state, indent=2) + "\n")
    temporary.replace(root / "launch.json")


def request_quit(process: subprocess.Popen, executable: Path) -> None:
    if process.poll() is not None:
        return
    result = native_application(process.pid, executable, "quit")
    if result is None or not result["quitRequested"]:
        raise LaunchError("Native Quit was not accepted; close the owned app manually")


def supervise(root: Path, bundle: Path, helpers: list[str], check: bool,
              provenance: dict) -> None:
    environment = application_environment(root)
    executable = bundle / "Contents/MacOS/festerm"
    state = {**provenance, "root": str(root), "bundle": str(bundle), "status": "starting",
             "shutdown_verified": False}
    with (root / "app.log").open("wb") as log:
        process = subprocess.Popen(
            [str(executable)], cwd=root / "home", env=environment,
            stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
        )
        state["pid"] = process.pid
        try:
            write_status(root, state)
            try:
                state["native"] = wait_ready(process, executable)
                state["status"] = "running"
                write_status(root, state)
                print(json.dumps(state), flush=True)
                if check:
                    request_quit(process, executable)
                code = process.wait(timeout=QUIT_TIMEOUT if check else None)
            except KeyboardInterrupt:
                state["interrupted"] = True
                request_quit(process, executable)
                code = process.wait(timeout=QUIT_TIMEOUT)
            if code != 0:
                raise LaunchError(f"Frontend exited unsuccessfully: {code}")
            deadline = time.monotonic() + 2
            owned = remaining_helpers(bundle, helpers)
            while owned and time.monotonic() < deadline:
                time.sleep(0.1)
                owned = remaining_helpers(bundle, helpers)
            state["remaining_helpers"] = owned
            if owned:
                raise LaunchError(
                    f"Owned helpers remain: {owned}; retained data at {root}. "
                    "Persistent sessions require explicit cleanup, not automatic killing"
                )
            state.update(exit_code=code, shutdown_verified=True)
            if check and state.get("interrupted"):
                raise LaunchError("Native check interrupted; validation did not complete")
            state["status"] = "exited"
        except (LaunchError, OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            state.update(
                status="failed",
                frontend_running=process.poll() is None, error=str(error),
            )
            print(f"Launch failed; inspect {root / 'launch.json'} and app.log. "
                  f"Owned frontend PID: {process.pid}; no forced termination performed.",
                  file=sys.stderr, flush=True)
            try:
                state["remaining_helpers"] = remaining_helpers(bundle, helpers)
            except (OSError, subprocess.SubprocessError) as inspection_error:
                state["helper_inspection_error"] = str(inspection_error)
                print(f"Owned helper inspection failed: {inspection_error}", file=sys.stderr)
            write_status(root, state)
            raise
        write_status(root, state)
        print(json.dumps(state), flush=True)


def interrupt(_signal, _frame) -> None:
    raise KeyboardInterrupt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true",
                        help="verify native readiness and bounded normal Quit")
    parser.add_argument("--output", type=Path,
                        help="new private launch/evidence directory; must not exist")
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("this launcher requires macOS and a logged-in GUI session")
    root = None
    try:
        if args.output:
            destination = args.output.expanduser().resolve()
            validate_root(destination)
            destination.mkdir(mode=0o700)
            root = destination
        else:
            root = Path(tempfile.mkdtemp(prefix=".fs-dev-", dir=Path.home())).resolve()
            validate_root(root)
        print(f"Private development launch: {root}", flush=True)
        environment = build_environment()
        provenance = {}
        for key, command in (
            ("revision", ["git", "rev-parse", "HEAD"]),
            ("dirty", ["git", "status", "--porcelain"]),
        ):
            result = subprocess.run(
                command, cwd=ROOT, env=environment, check=True,
                capture_output=True, text=True,
            ).stdout.strip()
            provenance[key] = bool(result) if key == "dirty" else result
        write_status(root, {**provenance, "status": "building"})
        try:
            bundle, helpers = package(root, environment)
        except (LaunchError, OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            write_status(root, {**provenance, "status": "failed", "error": str(error),
                                "shutdown_verified": False})
            raise
        signal.signal(signal.SIGTERM, interrupt)
        supervise(root, bundle, helpers, args.check, provenance)
    except (LaunchError, OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"macOS development launch failed: {error}", file=sys.stderr)
        if root:
            print(f"Evidence retained at {root}; no permissions were changed.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
