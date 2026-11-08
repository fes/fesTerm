#!/usr/bin/env python3
"""Run a Linux or Windows development build with separate config and sessiond state."""

from __future__ import annotations

import os
from pathlib import Path
import platform
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


class LaunchError(RuntimeError):
    pass


def prepare_state(root: Path) -> None:
    target = root / "target"
    state = target / "festerm-dev"
    for path in (target, state, state / "config.toml", state / "sessiond", state / "state"):
        if path.is_symlink():
            raise LaunchError(f"Refusing a symbolic link in development state: {path}")
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    if os.name != "nt":
        metadata = state.stat()
        if metadata.st_uid != os.geteuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
            raise LaunchError(f"Development state must be owned by this user and private: {state}")


def development_environment(root: Path, inherited: dict[str, str], system: str) -> dict[str, str]:
    if system == "Darwin":
        raise LaunchError("Use scripts/run-macos-development.py for macOS development")
    if system not in ("Linux", "Windows"):
        raise LaunchError(f"Unsupported development platform: {system}")
    state = root / "target" / "festerm-dev"
    environment = {
        key: value
        for key, value in inherited.items()
        if not key.startswith(("APPLE_", "CARGO_PACKAGER_SIGN_"))
        and key != "FESTERM_UPDATE_PUBLIC_KEY"
    }
    environment.update(
        FESTERM_INSTALLATION_KIND="development",
        FESTERM_CONFIG_PATH=str(state / "config.toml"),
        FESTERM_SESSIOND_RUNTIME_ROOT=str(state / "sessiond"),
    )
    if system == "Linux":
        environment["XDG_STATE_HOME"] = str(state / "state")
    else:
        environment["LOCALAPPDATA"] = str(state / "state")
    return environment


def main(arguments: list[str]) -> int:
    try:
        environment = development_environment(ROOT, dict(os.environ), platform.system())
        prepare_state(ROOT)
        print(f"Development config and sessiond state: {ROOT / 'target' / 'festerm-dev'}", flush=True)
        subprocess.run(
            ["cargo", "build", "--locked", "-p", "festerm-sessiond"],
            cwd=ROOT, env=environment, check=True,
        )
        subprocess.run(
            ["cargo", "build", "--locked", "-p", "festerm-tunnel-bridge",
             "--no-default-features", "--features", "relay", "--bins"],
            cwd=ROOT, env=environment, check=True,
        )
        if arguments and arguments[0] == "--":
            arguments = arguments[1:]
        subprocess.run(
            ["cargo", "run", "--locked", "-p", "festerm", "--", *arguments],
            cwd=ROOT, env=environment, check=True,
        )
        return 0
    except (LaunchError, OSError, subprocess.CalledProcessError) as error:
        print(f"Development launch failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
