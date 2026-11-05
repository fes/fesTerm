import importlib.util
import os
import signal
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "check_packaging.py"
SPEC = importlib.util.spec_from_file_location("check_packaging", SCRIPT)
packaging = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(packaging)
SMOKE_SCRIPT = Path(__file__).resolve().parents[1] / "smoke_linux_automation_worker.py"
SMOKE_SPEC = importlib.util.spec_from_file_location("smoke_linux_automation_worker", SMOKE_SCRIPT)
smoke_worker = importlib.util.module_from_spec(SMOKE_SPEC)
assert SMOKE_SPEC.loader is not None
SMOKE_SPEC.loader.exec_module(smoke_worker)
BASH = shutil.which("bash")


class PackagingMetadataTests(unittest.TestCase):
    def test_native_renderer_changes_trigger_package_smoke(self):
        workflow = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        triggers = workflow.split("\npermissions:", 1)[0]
        self.assertRegex(
            triggers,
            r"(?m)^      - 'crates/festerm-windows-direct2d/\*\*'$",
        )

    @unittest.skipUnless(BASH, "Linux package smoke requires bash")
    def test_linux_appimage_smoke_has_valid_shell_and_heredoc_syntax(self):
        workflow = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        step = workflow.split("      - name: Build unsigned Linux AppImage\n", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        assert BASH is not None
        result = subprocess.run(
            [BASH, "-n"],
            input=script,
            text=True,
            capture_output=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("here-document", result.stderr)

    def test_repository_packaging_metadata_is_consistent(self):
        packaging.verify()

    def test_linux_automation_helper_is_built_and_probed_for_both_packages(self):
        helper = {"path": "festerm-automation-js-worker", "main": False}
        linux = packaging.load_toml(packaging.CONFIGS["linux"])
        self.assertIn(helper, linux["binaries"])
        smoke = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        release = (packaging.ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        for workflow, steps in (
            (
                smoke,
                (
                    "Build unsigned Linux AppImage",
                    "Rebuild package-managed Linux binary and Debian package",
                ),
            ),
            (
                release,
                ("Build AppImage updater artifact", "Build package-managed Debian artifact"),
            ),
        ):
            for name in steps:
                step = workflow.split(f"      - name: {name}\n", 1)[1]
                step = step.split("\n      - name:", 1)[0]
                self.assertIn("-p festerm-automation-js", step)
        self.assertIn("'crates/festerm-automation-js/**'", smoke)
        self.assertIn(
            "smoke_linux_automation_worker.py squashfs-root/usr/bin/festerm-automation-js-worker",
            smoke,
        )
        self.assertIn(
            "smoke_linux_automation_worker.py /usr/bin/festerm-automation-js-worker",
            smoke,
        )

    def test_linux_worker_probe_requires_strict_qualified_frames(self):
        ready = b'{"type":"ready","profile":"linux-seccomp-stdio-rlimit-v1"}'
        protocol = b'{"type":"failed","code":"protocol"}'
        def encode(frames):
            return b"".join(len(frame).to_bytes(4, "big") + frame for frame in frames)
        smoke_worker.validate_frames(encode([ready, protocol]))
        for frames in (
            [b'{"type":"failed","code":"unavailable"}'],
            [ready, b'{"type":"failed","code":"unavailable"}'],
            [ready.decode().encode("utf-16"), protocol],
            [
                b'{"type":"ready","type":"ready",'
                b'"profile":"linux-seccomp-stdio-rlimit-v1"}',
                protocol,
            ],
            [ready],
            [ready, protocol, ready],
        ):
            with self.subTest(frames=frames):
                with self.assertRaises(smoke_worker.SmokeFailure):
                    smoke_worker.validate_frames(encode(frames))

    def test_linux_worker_probe_cleans_up_after_pipe_close_failure(self):
        process = mock.Mock(pid=12345, returncode=None)
        process.stdin.close.side_effect = OSError("pipe close failed")
        with (
            mock.patch.object(smoke_worker, "enable_subreaper"),
            mock.patch.object(smoke_worker.subprocess, "Popen", return_value=process),
            mock.patch.object(smoke_worker.os, "pipe", return_value=(111, 112)),
            mock.patch.object(smoke_worker.os, "write", return_value=1),
            mock.patch.object(smoke_worker.os, "pidfd_open", return_value=123, create=True),
            mock.patch.object(smoke_worker.os, "close") as close,
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(smoke_worker.signal, "pidfd_send_signal", create=True) as signal_worker,
            mock.patch.object(smoke_worker.os, "kill") as kill,
            mock.patch.object(smoke_worker, "cleanup_children", return_value=False) as cleanup,
        ):
            with self.assertRaisesRegex(smoke_worker.SmokeFailure, "could not be cleaned up") as error:
                smoke_worker.probe("worker")
        self.assertIn("pipe close failed", str(error.exception.__cause__))
        kill.assert_not_called()
        signal_worker.assert_called_once_with(123, 9)
        self.assertEqual(close.call_args_list, [mock.call(111), mock.call(112), mock.call(123)])
        process.stdout.close.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=smoke_worker.CLEANUP_TIMEOUT)
        cleanup.assert_called_once_with()

    def test_linux_worker_probe_attempts_descendant_cleanup_after_reap_timeout(self):
        process = mock.Mock(pid=12345, returncode=None)
        process.wait.side_effect = subprocess.TimeoutExpired("worker", 1)
        with (
            mock.patch.object(smoke_worker, "enable_subreaper"),
            mock.patch.object(smoke_worker.subprocess, "Popen", return_value=process),
            mock.patch.object(smoke_worker.os, "pipe", return_value=(111, 112)),
            mock.patch.object(smoke_worker.os, "write", return_value=1),
            mock.patch.object(smoke_worker.os, "pidfd_open", return_value=123, create=True),
            mock.patch.object(smoke_worker.os, "close"),
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(smoke_worker.signal, "pidfd_send_signal", create=True),
            mock.patch.object(smoke_worker, "read_output", return_value=b""),
            mock.patch.object(
                smoke_worker,
                "wait_for_worker",
                side_effect=smoke_worker.SmokeFailure("worker exit timed out"),
            ),
            mock.patch.object(smoke_worker, "cleanup_children", return_value=False) as cleanup,
        ):
            with self.assertRaisesRegex(smoke_worker.SmokeFailure, "could not be cleaned up"):
                smoke_worker.probe("worker")
        cleanup.assert_called_once_with()

    def test_worker_is_stopped_if_pidfd_open_fails_after_launch(self):
        process = mock.Mock(pid=12345, returncode=None)
        with (
            mock.patch.object(smoke_worker, "enable_subreaper"),
            mock.patch.object(smoke_worker.subprocess, "Popen", return_value=process),
            mock.patch.object(
                smoke_worker.os, "pidfd_open", side_effect=OSError("fd limit"), create=True
            ),
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(smoke_worker.os, "kill") as kill,
            mock.patch.object(smoke_worker, "cleanup_children", return_value=False),
            self.assertRaisesRegex(OSError, "fd limit"),
        ):
            smoke_worker.probe("worker")
        kill.assert_called_once_with(process.pid, 9)
        process.wait.assert_called_once_with(timeout=smoke_worker.CLEANUP_TIMEOUT)

    @unittest.skipUnless(os.name == "posix", "POSIX launch descriptors required")
    def test_worker_launcher_requires_parent_admission(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            marker = Path(directory) / "executed"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os\n"
                "from pathlib import Path\n"
                f"Path({str(marker)!r}).write_text("
                "os.environ.get('MALLOC_ARENA_MAX', 'missing') + '|' + "
                "os.environ.get('INJECTED_TEST', 'clean'))\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            for admitted in (False, True):
                read_fd, write_fd = os.pipe()
                try:
                    process = subprocess.Popen(
                        [
                            sys.executable,
                            "-I",
                            "-S",
                            "-c",
                            smoke_worker.LAUNCHER,
                            str(read_fd),
                            str(worker),
                        ],
                        pass_fds=(read_fd,),
                        env={"MALLOC_ARENA_MAX": "1", "INJECTED_TEST": "1"},
                    )
                finally:
                    os.close(read_fd)
                try:
                    if admitted:
                        os.write(write_fd, b"\x01")
                finally:
                    os.close(write_fd)
                self.assertEqual(process.wait(timeout=5), 0 if admitted else 1)
                self.assertEqual(marker.exists(), admitted)
                if admitted:
                    self.assertEqual(marker.read_text(), "1|clean")

    def test_descendant_signal_failure_does_not_skip_other_children(self):
        def kill(fd, _signal):
            if fd == 201:
                raise PermissionError("cannot signal child")

        with (
            mock.patch.object(
                smoke_worker,
                "owned_descendants",
                side_effect=[([(101, 201), (102, 202)], True, None), ([], False, None)],
            ),
            mock.patch.object(
                smoke_worker.signal, "pidfd_send_signal", side_effect=kill, create=True
            ) as signal_child,
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(smoke_worker.os, "close"),
            mock.patch.object(smoke_worker.os, "WNOHANG", 1, create=True),
            mock.patch.object(
                smoke_worker.os, "waitpid", side_effect=ChildProcessError, create=True
            ),
            self.assertRaisesRegex(smoke_worker.SmokeFailure, "could not inspect or signal"),
        ):
            smoke_worker.cleanup_children()
        self.assertEqual(
            signal_child.call_args_list,
            [mock.call(201, 9), mock.call(202, 9)],
        )

    def test_descendant_scan_includes_grandchildren(self):
        root = os.getpid()
        tree = {root: [101], 101: [102], 102: []}
        with (
            mock.patch.object(
                smoke_worker, "child_pids", side_effect=lambda pid, _deadline: tree[pid]
            ),
            mock.patch.object(
                smoke_worker.os, "pidfd_open", side_effect=lambda pid: pid + 100, create=True
            ),
            mock.patch.object(smoke_worker, "pidfd_exited", return_value=False),
            mock.patch.object(smoke_worker, "parent_pid", side_effect={101: root, 102: 101}.get),
        ):
            descendants, found, failure = smoke_worker.owned_descendants(time.monotonic() + 1)
        self.assertEqual(descendants, [(101, 201), (102, 202)])
        self.assertTrue(found)
        self.assertIsNone(failure)

    def test_incomplete_descendant_scan_keeps_discovered_children(self):
        root = os.getpid()

        def children(pid, _deadline):
            if pid == root:
                raise smoke_worker.IncompleteScan("scan limit", [101])
            return []

        with (
            mock.patch.object(smoke_worker, "child_pids", side_effect=children),
            mock.patch.object(
                smoke_worker.os, "pidfd_open", side_effect=lambda pid: pid + 100, create=True
            ),
            mock.patch.object(smoke_worker, "pidfd_exited", return_value=False),
            mock.patch.object(smoke_worker, "parent_pid", return_value=root),
        ):
            descendants, found, failure = smoke_worker.owned_descendants(time.monotonic() + 1)
        self.assertEqual(descendants, [(101, 201)])
        self.assertTrue(found)
        self.assertIsInstance(failure, smoke_worker.IncompleteScan)

    def test_deadline_limited_scan_still_pins_discovered_children(self):
        root = os.getpid()
        now = [0.0]

        def children(_pid, _deadline):
            now[0] = 2.1
            raise smoke_worker.IncompleteScan("scan limit", [101])

        with (
            mock.patch.object(smoke_worker.time, "monotonic", side_effect=lambda: now[0]),
            mock.patch.object(smoke_worker, "child_pids", side_effect=children),
            mock.patch.object(
                smoke_worker.os, "pidfd_open", return_value=201, create=True
            ),
            mock.patch.object(smoke_worker, "pidfd_exited", return_value=False),
            mock.patch.object(smoke_worker, "parent_pid", return_value=root),
        ):
            descendants, found, failure = smoke_worker.owned_descendants(4.0)
        self.assertEqual(descendants, [(101, 201)])
        self.assertTrue(found)
        self.assertIsInstance(failure, smoke_worker.IncompleteScan)

    def test_missing_children_file_for_live_thread_fails_the_scan(self):
        thread = mock.Mock(path="/proc/test/task/live")
        scan = mock.MagicMock()
        scan.__enter__.return_value = iter([thread])
        with (
            mock.patch.object(smoke_worker.os, "scandir", return_value=scan),
            mock.patch("builtins.open", side_effect=FileNotFoundError),
            mock.patch.object(smoke_worker.os, "stat"),
            self.assertRaises(smoke_worker.IncompleteScan),
        ):
            smoke_worker.child_pids()

    def test_reaping_yields_for_rescan_and_honors_cleanup_deadline(self):
        now = 0.0
        reaps = 0

        def clock():
            nonlocal now
            now += 0.001
            return now

        def reap(_pid, _flags):
            nonlocal reaps
            reaps += 1
            if reaps > 20:
                raise AssertionError("cleanup failed to honor its deadline")
            return 500, 0

        with (
            mock.patch.object(smoke_worker, "CLEANUP_TIMEOUT", 0.01),
            mock.patch.object(smoke_worker, "MAX_REAPS_PER_SCAN", 2),
            mock.patch.object(smoke_worker.time, "monotonic", side_effect=clock),
            mock.patch.object(smoke_worker.time, "sleep"),
            mock.patch.object(
                smoke_worker, "owned_descendants", return_value=([(101, 201)], True, None)
            ) as scan,
            mock.patch.object(smoke_worker.signal, "pidfd_send_signal", create=True),
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(smoke_worker.os, "close"),
            mock.patch.object(smoke_worker.os, "WNOHANG", 1, create=True),
            mock.patch.object(smoke_worker.os, "waitpid", side_effect=reap, create=True),
            self.assertRaisesRegex(smoke_worker.SmokeFailure, "could not be reaped"),
        ):
            smoke_worker.cleanup_children()
        self.assertGreater(scan.call_count, 1)

    def test_interruption_before_start_does_not_launch_worker(self):
        stop = smoke_worker.StopRequest()
        stop(signal.SIGTERM, None)
        with (
            mock.patch.object(smoke_worker.subprocess, "Popen") as launch,
            self.assertRaisesRegex(smoke_worker.SmokeFailure, "interrupted"),
        ):
            smoke_worker.probe("worker", stop)
        launch.assert_not_called()

    def test_pidfd_preflight_rejects_unavailable_signal_support(self):
        with (
            mock.patch.object(smoke_worker.os, "pipe", return_value=(11, 12)),
            mock.patch.object(smoke_worker.os, "fork", return_value=456, create=True),
            mock.patch.object(smoke_worker.os, "pidfd_open", return_value=123, create=True),
            mock.patch.object(smoke_worker.os, "close") as close,
            mock.patch.object(smoke_worker.os, "WNOHANG", 1, create=True),
            mock.patch.object(smoke_worker.os, "waitpid", return_value=(456, 0), create=True),
            mock.patch.object(smoke_worker.signal, "SIGKILL", 9, create=True),
            mock.patch.object(
                smoke_worker.signal,
                "pidfd_send_signal",
                side_effect=PermissionError("blocked"),
                create=True,
            ) as signal_child,
            self.assertRaisesRegex(smoke_worker.SmokeFailure, "pidfd cleanup is unavailable"),
        ):
            smoke_worker.verify_pidfd_cleanup()
        signal_child.assert_called_once_with(123, 9)
        self.assertEqual(close.call_args_list, [mock.call(11), mock.call(123), mock.call(12)])

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux pidfds required")
    def test_pidfd_preflight_child_survives_delayed_parent(self):
        source = (
            "import os, runpy, signal, time\n"
            "signal.signal(signal.SIGCHLD, signal.SIG_DFL)\n"
            f"smoke = runpy.run_path({str(SMOKE_SCRIPT)!r})\n"
            "original = os.pidfd_open\n"
            "def delayed(pid):\n"
            "    time.sleep(1.2)\n"
            "    return original(pid)\n"
            "os.pidfd_open = delayed\n"
            "smoke['verify_pidfd_cleanup']()\n"
        )
        result = subprocess.run(
            [sys.executable, "-c", source],
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux pidfds required")
    def test_worker_does_not_survive_pidfd_exhaustion(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            started = Path(directory) / "started"
            fork_allowed = Path(directory) / "fork-allowed"
            descendant = Path(directory) / "descendant"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os, signal, time\n"
                "from pathlib import Path\n"
                f"Path({str(started)!r}).touch()\n"
                f"while not Path({str(fork_allowed)!r}).exists():\n"
                "    time.sleep(0.01)\n"
                "if os.fork() == 0:\n"
                "    signal.alarm(5)\n"
                "    os.setsid()\n"
                f"    Path({str(descendant)!r}).write_text(str(os.getpid()))\n"
                "    time.sleep(30)\n"
                "    os._exit(0)\n"
                "time.sleep(30)\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            source = (
                "import importlib.util, os, select, signal, time\n"
                "from pathlib import Path\n"
                f"spec = importlib.util.spec_from_file_location('smoke', {str(SMOKE_SCRIPT)!r})\n"
                "smoke = importlib.util.module_from_spec(spec)\n"
                "spec.loader.exec_module(smoke)\n"
                "original = os.pidfd_open\n"
                "calls = 0\n"
                "guard = None\n"
                "worker_pid = None\n"
                "descendant_guard = None\n"
                "descendant_pid = None\n"
                "def exhausted(pid, flags=0):\n"
                "    global calls, guard, worker_pid, descendant_guard, descendant_pid\n"
                "    calls += 1\n"
                "    if calls == 1:\n"
                "        return original(pid)\n"
                "    if calls == 2:\n"
                "        worker_pid = pid\n"
                "        guard = original(pid)\n"
                "        deadline = time.monotonic() + 1\n"
                f"        while not Path({str(started)!r}).exists() and time.monotonic() < deadline:\n"
                "            time.sleep(0.01)\n"
                f"        if Path({str(started)!r}).exists():\n"
                f"            Path({str(fork_allowed)!r}).touch()\n"
                "            deadline = time.monotonic() + 2\n"
                "            while time.monotonic() < deadline:\n"
                "                try:\n"
                f"                    descendant_pid = int(Path({str(descendant)!r}).read_text())\n"
                "                    descendant_guard = original(descendant_pid)\n"
                "                    break\n"
                "                except (FileNotFoundError, ValueError, ProcessLookupError):\n"
                "                    time.sleep(0.01)\n"
                "    raise OSError(24, 'pidfd limit')\n"
                "os.pidfd_open = exhausted\n"
                "try:\n"
                f"    smoke.probe({str(worker)!r})\n"
                "except (OSError, smoke.SmokeFailure):\n"
                "    pass\n"
                "else:\n"
                "    raise SystemExit('probe unexpectedly passed')\n"
                "finally:\n"
                "    if descendant_guard is not None:\n"
                "        signal.pidfd_send_signal(descendant_guard, signal.SIGKILL)\n"
                "        try:\n"
                "            os.waitpid(descendant_pid, 0)\n"
                "        except ChildProcessError:\n"
                "            pass\n"
                "        os.close(descendant_guard)\n"
                "    if guard is not None:\n"
                "        poller = select.poll()\n"
                "        poller.register(guard, select.POLLIN)\n"
                "        survived = not poller.poll(0)\n"
                "        if survived:\n"
                "            signal.pidfd_send_signal(guard, signal.SIGKILL)\n"
                "            os.waitpid(worker_pid, 0)\n"
                "        os.close(guard)\n"
                "        if survived:\n"
                "            raise SystemExit('worker survived pidfd exhaustion')\n"
                f"    if Path({str(started)!r}).exists():\n"
                "        raise SystemExit('worker executed before pidfd acquisition')\n"
            )
            result = subprocess.run(
                [sys.executable, "-c", source],
                capture_output=True,
                text=True,
                check=False,
                timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux subreaper required")
    def test_linux_worker_probe_requires_qualified_ready_and_exit(self):
        ready = b'{"type":"ready","profile":"linux-seccomp-stdio-rlimit-v1"}'
        protocol = b'{"type":"failed","code":"protocol"}'
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            for frames, exit_code, expected in (
                ([ready, protocol], 70, 0),
                ([b'{"type":"failed","code":"unavailable"}'], 70, 1),
                ([ready, b'{"type":"failed","code":"unavailable"}'], 70, 1),
                ([ready, protocol], 0, 1),
            ):
                worker.write_text(
                    f"#!{sys.executable}\n"
                    "import sys\n"
                    f"frames = {frames!r}\n"
                    "for payload in frames:\n"
                    "    sys.stdout.buffer.write(len(payload).to_bytes(4, 'big') + payload)\n"
                    "sys.stdout.buffer.flush()\n"
                    f"raise SystemExit({exit_code})\n",
                    encoding="utf-8",
                )
                worker.chmod(0o700)
                result = subprocess.run(
                    [sys.executable, str(SMOKE_SCRIPT), str(worker)],
                    capture_output=True,
                    text=True,
                    check=False,
                    timeout=15,
                )
                self.assertEqual(result.returncode, expected, result.stderr)
                if expected == 0:
                    inherited_ignore = subprocess.run(
                        [sys.executable, str(SMOKE_SCRIPT), str(worker)],
                        capture_output=True,
                        text=True,
                        check=False,
                        timeout=15,
                        preexec_fn=lambda: signal.signal(signal.SIGCHLD, signal.SIG_IGN),
                    )
                    self.assertEqual(inherited_ignore.returncode, 0, inherited_ignore.stderr)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux subreaper required")
    def test_linux_worker_probe_bounds_inherited_stdout_after_worker_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os, sys, time\n"
                "if os.fork() == 0:\n"
                "    time.sleep(30)\n"
                "    os._exit(0)\n"
                "frames = ["
                "b'{\"type\":\"ready\",\"profile\":\"linux-seccomp-stdio-rlimit-v1\"}',"
                "b'{\"type\":\"failed\",\"code\":\"protocol\"}']\n"
                "for payload in frames:\n"
                "    sys.stdout.buffer.write(len(payload).to_bytes(4, 'big') + payload)\n"
                "sys.stdout.buffer.flush()\n"
                "os._exit(70)\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            started = time.monotonic()
            result = subprocess.run(
                [sys.executable, str(SMOKE_SCRIPT), str(worker)],
                capture_output=True,
                text=True,
                check=False,
                timeout=25,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertLess(time.monotonic() - started, 20)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux subreaper required")
    def test_linux_worker_probe_refuses_and_reaps_detached_descendants(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            pid_file = Path(directory) / "child-pid"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os, sys, time\n"
                "read_fd, write_fd = os.pipe()\n"
                "if os.fork() == 0:\n"
                "    os.setsid()\n"
                "    os.close(read_fd)\n"
                "    os.close(0)\n"
                "    os.close(1)\n"
                "    os.close(2)\n"
                f"    open({str(pid_file)!r}, 'w').write(str(os.getpid()))\n"
                "    os.write(write_fd, b'1')\n"
                "    time.sleep(30)\n"
                "    os._exit(0)\n"
                "os.close(write_fd)\n"
                "os.read(read_fd, 1)\n"
                "frames = ["
                "b'{\"type\":\"ready\",\"profile\":\"linux-seccomp-stdio-rlimit-v1\"}',"
                "b'{\"type\":\"failed\",\"code\":\"protocol\"}']\n"
                "for payload in frames:\n"
                "    sys.stdout.buffer.write(len(payload).to_bytes(4, 'big') + payload)\n"
                "sys.stdout.buffer.flush()\n"
                "os._exit(70)\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            result = subprocess.run(
                [sys.executable, str(SMOKE_SCRIPT), str(worker)],
                capture_output=True,
                text=True,
                check=False,
                timeout=15,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unexpected descendants", result.stderr)
            child = int(pid_file.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(child, 0)

    @unittest.skipUnless(
        sys.platform.startswith("linux")
        and hasattr(os, "pidfd_open")
        and hasattr(signal, "pidfd_send_signal"),
        "Linux pidfds required",
    )
    def test_linux_worker_probe_cleans_detached_descendant_on_sigterm(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            pid_file = Path(directory) / "child-pid"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os, time\n"
                "if os.fork() == 0:\n"
                "    os.setsid()\n"
                f"    open({str(pid_file)!r}, 'w').write(str(os.getpid()))\n"
                "    time.sleep(20)\n"
                "    os._exit(0)\n"
                "time.sleep(20)\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            probe = subprocess.Popen(
                [sys.executable, str(SMOKE_SCRIPT), str(worker)],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            pidfd = None
            try:
                deadline = time.monotonic() + 5
                child_text = ""
                while not child_text and time.monotonic() < deadline:
                    try:
                        child_text = pid_file.read_text()
                    except FileNotFoundError:
                        pass
                    time.sleep(0.01)
                self.assertTrue(child_text, "worker descendant did not start")
                child = int(child_text)
                pidfd = os.pidfd_open(child, 0)
                probe.send_signal(signal.SIGTERM)
                _stdout, stderr = probe.communicate(timeout=10)
                self.assertNotEqual(probe.returncode, 0)
                self.assertIn("interrupted", stderr)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child, 0)
            finally:
                if probe.poll() is None:
                    probe.send_signal(signal.SIGTERM)
                    probe.communicate(timeout=10)
                if pidfd is not None:
                    try:
                        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    os.close(pidfd)

    @unittest.skipUnless(
        sys.platform.startswith("linux")
        and hasattr(os, "pidfd_open")
        and hasattr(signal, "pidfd_send_signal"),
        "Linux pidfds required",
    )
    def test_linux_cleanup_reaches_grandchild_behind_unsignalable_parent(self):
        smoke_worker.enable_subreaper()
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory) / "worker"
            pid_file = Path(directory) / "grandchild-pid"
            worker.write_text(
                f"#!{sys.executable}\n"
                "import os, signal, time\n"
                "signal.signal(signal.SIGCHLD, signal.SIG_IGN)\n"
                "if os.fork() == 0:\n"
                "    os.setsid()\n"
                f"    open({str(pid_file)!r}, 'w').write(str(os.getpid()))\n"
                "    time.sleep(20)\n"
                "    os._exit(0)\n"
                "time.sleep(20)\n",
                encoding="utf-8",
            )
            worker.chmod(0o700)
            parent = subprocess.Popen([str(worker)])
            grandchild_fd = None
            send_signal = signal.pidfd_send_signal
            try:
                deadline = time.monotonic() + 5
                child_text = ""
                while not child_text and time.monotonic() < deadline:
                    try:
                        child_text = pid_file.read_text()
                    except FileNotFoundError:
                        pass
                    time.sleep(0.01)
                self.assertTrue(child_text, "worker grandchild did not start")
                grandchild_fd = os.pidfd_open(int(child_text))

                def refuse_parent(fd, sig):
                    info = Path(f"/proc/self/fdinfo/{fd}").read_text()
                    if f"Pid:\t{parent.pid}\n" in info:
                        raise PermissionError("test refuses parent")
                    return send_signal(fd, sig)

                with (
                    mock.patch.object(
                        smoke_worker.signal, "pidfd_send_signal", side_effect=refuse_parent
                    ),
                    self.assertRaisesRegex(smoke_worker.SmokeFailure, "could not be reaped"),
                ):
                    smoke_worker.cleanup_children()
                self.assertTrue(smoke_worker.pidfd_exited(grandchild_fd))
            finally:
                if parent.poll() is None:
                    parent.kill()
                    parent.wait(timeout=5)
                if grandchild_fd is not None:
                    try:
                        send_signal(grandchild_fd, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    os.close(grandchild_fd)
                smoke_worker.cleanup_children()

    def test_all_native_platforms_have_explicit_formats(self):
        self.assertEqual(
            set(packaging.EXPECTED_FORMATS),
            {"macos", "windows", "linux"},
        )
        self.assertNotIn("all", {
            package_format
            for formats in packaging.EXPECTED_FORMATS.values()
            for package_format in formats
        })

    def test_windows_helper_name_is_tied_to_the_workspace_release(self):
        version = packaging.workspace_version()
        windows = packaging.load_toml(packaging.CONFIGS["windows"])
        self.assertEqual(windows["binaries"], [{"path": "festerm", "main": True}])
        self.assertIn(
            {
                "src": f"../target/release/festerm-sessiond-{version}.exe",
                "target": f"festerm-sessiond-{version}.exe",
            },
            windows["resources"],
        )
        self.assertNotIn(
            "festerm-sessiond.exe",
            {resource["target"] for resource in windows["resources"]},
        )

    def test_markdown_open_with_packaging_is_alternative_only(self):
        macos = packaging.load_toml(packaging.CONFIGS["macos"])
        windows = packaging.load_toml(packaging.CONFIGS["windows"])
        linux = packaging.load_toml(packaging.CONFIGS["linux"])

        self.assertEqual(
            macos["macos"]["info-plist-path"],
            "../packaging/macos-info.plist",
        )
        self.assertNotIn("file-associations", macos)

        self.assertEqual(
            windows["nsis"]["template"],
            "../packaging/windows-open-with.nsi",
        )
        self.assertNotIn("file-associations", windows)

        self.assertEqual(
            linux["file-associations"],
            [packaging.EXPECTED_MARKDOWN_ASSOCIATION],
        )
        self.assertEqual(
            linux["deb"]["desktop-template"],
            "../packaging/linux-desktop-entry.desktop",
        )


if __name__ == "__main__":
    unittest.main()
