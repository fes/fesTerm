#!/usr/bin/env python3
"""Opt-in local Running Sessions churn, never the user's provider namespace."""

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import uuid


def run_checked(command, *, cwd, env, timeout):
    """Bound the entire owned Cargo/test process tree, not just Cargo itself."""
    options = {"start_new_session": True} if os.name != "nt" else {}
    process = subprocess.Popen(command, cwd=cwd, env=env, **options)
    try:
        status = process.wait(timeout=timeout)
        if status:
            raise subprocess.CalledProcessError(status, command)
    except BaseException:
        if process.poll() is None:
            if os.name == "nt":
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                               capture_output=True, timeout=10, check=False)
            else:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.wait(timeout=10)
        raise


def owned_targets(tool, output):
    targets = []
    for line in output.splitlines():
        key = line.strip().split()[0] if line.strip() else ""
        if tool == "tmux" and key.startswith("$") and key[1:].isdigit():
            targets.append(key)
        elif tool == "screen" and "." in key and key.split(".", 1)[0].isdigit():
            if "(Attached)" in line or "(Detached)" in line:
                targets.append(key)
    return targets


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--batch", type=int, default=os.environ.get("FESTERM_SESSION_CHURN_BATCH", 8))
    parser.add_argument("--cycles", type=int, default=os.environ.get("FESTERM_SESSION_CHURN_CYCLES", 3))
    args = parser.parse_args()
    if not 1 <= args.batch <= 128 or not 1 <= args.cycles <= 100:
        parser.error("batch must be 1..128 and cycles 1..100")
    repo = Path(__file__).resolve().parent.parent
    env = dict(os.environ, FESTERM_SESSION_CHURN_BATCH=str(args.batch),
               FESTERM_SESSION_CHURN_CYCLES=str(args.cycles))
    root = repo / (".mux-" + uuid.uuid4().hex[:8])
    root.mkdir(mode=0o700)
    binary_dir = root / "bin"
    binary_dir.mkdir(mode=0o700)
    screen_dir = root / "screen"
    screen_dir.mkdir(mode=0o700)
    tmux_dir = root / "tmux"
    tmux_dir.mkdir(mode=0o700)
    (root / "owned-running-session-validation").write_text("isolated\n")
    tools = {}
    env["FESTERM_SESSIOND_TEST_RUNTIME_ROOT"] = str(root / "n")
    try:
        run_checked(["cargo", "build", "--quiet", "-p", "festerm-sessiond", "-p",
                        "festerm-pty-test-child"], cwd=repo, env=env, timeout=900)
        run_checked(["cargo", "test", "--quiet", "-p", "festerm-sessiond", "--test",
                        "native_daemon", "native_discovery_churn", "--", "--ignored",
                        "--nocapture"], cwd=repo, env=env,
                       timeout=120 + args.batch * args.cycles * 15)
        if os.name == "nt":
            print("provider=tmux status=skipped reason=not-native-windows")
            print("provider=screen status=skipped reason=not-native-windows")
            return
        for tool in ("tmux", "screen"):
            path = shutil.which(tool)
            if path:
                tools[tool] = path
                extra = " -f /dev/null -L festerm-churn" if tool == "tmux" else " -c /dev/null"
                wrapper = binary_dir / tool
                wrapper.write_text("#!/bin/sh\nexec " + shlex.quote(path) + extra + ' "$@"\n')
                wrapper.chmod(0o700)
            else:
                print(f"provider={tool} status=skipped reason=binary-unavailable", flush=True)
        env.update(PATH=str(binary_dir) + os.pathsep + env.get("PATH", ""),
                   SCREENDIR=str(screen_dir), TMUX_TMPDIR=str(tmux_dir),
                   FESTERM_MUX_CHURN_ROOT=str(root), TERM="xterm-256color",
                   XDG_STATE_HOME=str(root / "native-empty"))
        env.pop("TMUX", None)
        env.pop("STY", None)
        run_checked(["cargo", "test", "--quiet", "-p", "festerm",
                        "isolated_multiplexer_discovery_churn", "--", "--ignored",
                        "--nocapture"], cwd=repo, env=env,
                       timeout=120 + args.batch * args.cycles * 30)
    finally:
        # Rust owns normal/failure cleanup. This also handles a killed/timed-out
        # test runner, using captured entries from *only* these private sockets.
        cleanup_errors = []
        for registry in (root / "n").glob("*/*/sessiond/registry.json"):
            try:
                records = json.loads(registry.read_text())["sessions"]
                for name in records:
                    scoped = dict(env)
                    scoped["LOCALAPPDATA" if os.name == "nt" else "XDG_STATE_HOME"] = str(registry.parents[2])
                    target = Path(env.get("CARGO_TARGET_DIR", str(repo / "target")))
                    if not target.is_absolute():
                        target = repo / target
                    executable = target / "debug" / ("festerm-sessiond.exe" if os.name == "nt" else "festerm-sessiond")
                    subprocess.run([str(executable), "kill", "--name", name],
                                   env=scoped, capture_output=True, timeout=5, check=True)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                cleanup_errors.append(f"native cleanup: {error}")
        for tool in tools:
            try:
                listing = subprocess.run([str(binary_dir / tool)] +
                    (["list-sessions", "-F", "#{session_id}"] if tool == "tmux" else ["-ls"]),
                    env=env, text=True, capture_output=True, timeout=5)
                for key in owned_targets(tool, listing.stdout):
                    if tool == "tmux":
                        command = ["kill-session", "-t", key]
                    else:
                        command = ["-S", key, "-X", "quit"]
                    subprocess.run([str(binary_dir / tool)] + command, env=env,
                                   capture_output=True, timeout=5, check=False)
                remaining = subprocess.run([str(binary_dir / tool)] +
                    (["list-sessions", "-F", "#{session_id}"] if tool == "tmux" else ["-ls"]),
                    env=env, text=True, capture_output=True, timeout=5)
                if owned_targets(tool, remaining.stdout):
                    cleanup_errors.append(f"{tool} still lists live owned sessions")
            except (OSError, subprocess.SubprocessError) as error:
                cleanup_errors.append(f"{tool} cleanup: {error}")
        if cleanup_errors:
            raise RuntimeError(f"Cleanup incomplete; owned namespace retained at {root}: {cleanup_errors}")
        shutil.rmtree(root)


if __name__ == "__main__":
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt("validation interrupted; cleaning owned sessions")
    signal.signal(signal.SIGTERM, interrupted)
    main()
