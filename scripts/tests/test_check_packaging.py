import copy
import hashlib
import importlib.util
import io
import json
import os
import re
import signal
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
import zipfile
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

    def test_missing_notice_resource_is_rejected_on_every_platform(self):
        load_toml = packaging.load_toml
        for platform in packaging.CONFIGS:
            config = copy.deepcopy(load_toml(packaging.CONFIGS[platform]))
            config["resources"].remove(packaging.EXPECTED_NOTICE_RESOURCE)

            def altered(path):
                return config if path == packaging.CONFIGS[platform] else load_toml(path)

            with self.subTest(platform=platform), mock.patch.object(packaging, "load_toml", side_effect=altered):
                with self.assertRaises(packaging.PackagingError):
                    packaging.verify()

    def test_missing_or_relocated_tunnel_helper_is_rejected(self):
        load_toml = packaging.load_toml
        for platform in packaging.CONFIGS:
            for helper in (
                packaging.WINDOWS_TUNNEL_HELPERS
                if platform == "windows"
                else packaging.TUNNEL_HELPERS
            ):
                for relocated in (False, True):
                    with self.subTest(platform=platform, helper=helper, relocated=relocated):
                        config = copy.deepcopy(load_toml(packaging.CONFIGS[platform]))
                        key = "resources" if platform == "windows" else "binaries"
                        field = "target" if platform == "windows" else "path"
                        name = helper + (".exe" if platform == "windows" else "")
                        if relocated:
                            next(
                                entry for entry in config[key] if entry[field] == name
                            )[field] = f"helpers/{name}"
                        else:
                            config[key] = [
                                entry for entry in config[key] if entry[field] != name
                            ]

                        def altered(path):
                            return config if path == packaging.CONFIGS[platform] else load_toml(path)

                        with mock.patch.object(packaging, "load_toml", side_effect=altered):
                            with self.assertRaises(packaging.PackagingError):
                                packaging.verify()

    def test_downloaded_cli_must_not_be_bundled(self):
        load_toml = packaging.load_toml
        for platform in packaging.CONFIGS:
            for resource in (False, True):
                with self.subTest(platform=platform, resource=resource):
                    config = copy.deepcopy(load_toml(packaging.CONFIGS[platform]))
                    if resource:
                        config.setdefault("resources", []).append(
                            {"src": "../target/release/devtunnel", "target": "devtunnel"}
                        )
                    else:
                        config["binaries"].append({"path": "devtunnel", "main": False})

                    def altered(path):
                        return config if path == packaging.CONFIGS[platform] else load_toml(path)

                    with mock.patch.object(packaging, "load_toml", side_effect=altered):
                        with self.assertRaises(packaging.PackagingError):
                            packaging.verify()

    def test_each_desktop_package_build_requires_relay_helpers(self):
        smoke = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        release = (packaging.ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        for workflow_index, workflow in enumerate((smoke, release)):
            for marker in re.finditer(re.escape(packaging.TUNNEL_BUILD_COMMAND), workflow):
                altered = workflow[:marker.start()] + workflow[marker.end():]
                workflows = [smoke, release]
                workflows[workflow_index] = altered
                errors = []
                packaging.verify_desktop_helper_workflows(*workflows, errors)
                self.assertTrue(errors)
                self.assertIn("must build relay-enabled tunnel helpers", "\n".join(errors))

    def test_desktop_helper_signing_and_native_payload_probes_cannot_be_omitted(self):
        smoke = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        release = (packaging.ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        omissions = [
            *(
                (1, f"\\target\\release\\{helper}.exe")
                for helper in packaging.WINDOWS_TUNNEL_HELPERS
            ),
            *(
                (1, f"            target/release/{helper}.exe\n")
                for helper in packaging.WINDOWS_TUNNEL_HELPERS
            ),
            (1, "$signature.SignerCertificate.Subject -ne "),
            (1, "codesign --verify --strict -R='anchor apple generic' --verbose=2 \"$binary\""),
            (1, "grep -q 'flags=.*runtime'"),
            (1, ' = "$app_team"'),
            (1, "xcrun stapler validate target/release/fesTerm.app"),
            (0, "test -x target/release/fesTerm.app/Contents/MacOS/festerm-tunnel-setup"),
            (0, "test -x squashfs-root/usr/bin/festerm-tunnel-bridge"),
            (0, "grep -q 'usr/bin/festerm-tunnel-setup$'"),
            (0, "test ! -e /usr/bin/festerm-tunnel-bridge"),
            (0, "NSIS uninstall left tunnel helper installed"),
        ]
        for index, omission in omissions:
            with self.subTest(omission=omission):
                workflows = [smoke, release]
                self.assertIn(omission, workflows[index])
                workflows[index] = workflows[index].replace(omission, "", 1)
                errors = []
                packaging.verify_desktop_helper_workflows(*workflows, errors)
                self.assertTrue(errors)

    def test_macos_runtime_apple_anchor_cannot_be_weakened(self):
        smoke = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        release = (packaging.ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        for requirement in ("", "-R='anchor apple' ", "-R='anchor trusted' "):
            with self.subTest(requirement=requirement):
                errors = []
                altered = release.replace("-R='anchor apple generic' ", requirement, 1)
                packaging.verify_desktop_helper_workflows(smoke, altered, errors)
                self.assertIn(
                    "macOS release must verify signed, same-team, hardened helpers in the notarized app",
                    errors,
                )
        for requirement in ("", "-R=anchor apple"):
            with self.subTest(cli_requirement=requirement):
                errors = []
                altered = smoke.replace("-R=anchor apple generic", requirement, 1)
                packaging.verify_desktop_helper_workflows(altered, release, errors)
                self.assertIn("runtime signature prerequisite", "\n".join(errors))

    def _run_macos_cli_smoke(
        self, archive_bytes, *, archive_digest=None, executable_digest=None,
        fail_architecture=None,
    ):
        workflow = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        step = packaging.workflow_step(workflow, "Smoke reviewed macOS CLI signature prerequisites")
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        script = script.split("python - <<'PY'\n", 1)[1].rsplit("\nPY", 1)[0]
        manifest = {"pins": [
            {
                "platform": "macos", "architecture": architecture, "format": "zip",
                "download": f"https://example.invalid/{architecture}",
                "archive_sha256": archive_digest or hashlib.sha256(archive_bytes).hexdigest(),
                "executable_sha256": executable_digest or hashlib.sha256(b"reviewed-cli").hexdigest(),
            }
            for architecture in ("x86_64", "aarch64")
        ]}
        calls = []
        directories = set()

        def run(args, *, check, timeout):
            self.assertEqual(check, "-dv" not in args)
            self.assertLessEqual(timeout, 100)
            calls.append(args)
            if args[0] == "curl":
                output = Path(args[args.index("--output") + 1])
                directories.add(output.parent.parent)
                output.write_bytes(archive_bytes)
            elif check and fail_architecture == Path(args[-1]).parent.name:
                raise subprocess.CalledProcessError(1, args)

        with (
            mock.patch.object(Path, "read_text", return_value=json.dumps(manifest)),
            mock.patch.object(Path, "cwd", return_value=packaging.ROOT),
            mock.patch.object(subprocess, "run", side_effect=run),
            mock.patch("builtins.print"),
        ):
            try:
                exec(compile(script, "<macos-cli-signature-smoke>", "exec"), {})
            except SystemExit as error:
                failure = str(error)
            else:
                failure = None
        self.assertTrue(directories)
        self.assertTrue(all(not directory.exists() for directory in directories))
        return calls, failure

    def test_macos_cli_smoke_checks_both_architectures_and_exact_runtime_signature(self):
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as source:
            source.writestr("devtunnel", b"reviewed-cli")
        for fail_architecture in (None, "x86_64"):
            with self.subTest(fail_architecture=fail_architecture):
                calls, failure = self._run_macos_cli_smoke(
                    archive.getvalue(), fail_architecture=fail_architecture
                )
                self.assertEqual(failure is None, fail_architecture is None)
                self.assertEqual(
                    [args[-1] for args in calls if args[0] == "curl"],
                    ["https://example.invalid/x86_64", "https://example.invalid/aarch64"],
                )
                signatures = [args for args in calls if args[0] == "/usr/bin/codesign"]
                self.assertEqual(len(signatures), 4)
                for args in signatures:
                    self.assertEqual(
                        args[1:-1],
                        ["-dv"] if "-dv" in args else
                        ["--verify", "--strict", "-R=anchor apple generic"],
                    )

    def test_macos_cli_smoke_rejects_unsafe_archives_and_hash_mismatches(self):
        symlink = zipfile.ZipInfo("devtunnel")
        symlink.create_system = 3
        symlink.external_attr = 0o120777 << 16
        for entries, digests in (
            ([("../devtunnel", b"reviewed-cli")], {}),
            ([(symlink, b"reviewed-cli")], {}),
            ([("devtunnel", b"reviewed-cli"), ("extra", b"extra")], {}),
            ([("devtunnel", b"reviewed-cli")], {"archive_digest": "0" * 64}),
            ([("devtunnel", b"reviewed-cli")], {"executable_digest": "0" * 64}),
        ):
            with self.subTest(entries=entries, digests=digests):
                archive = io.BytesIO()
                with zipfile.ZipFile(archive, "w") as source:
                    for name, data in entries:
                        source.writestr(name, data)
                calls, failure = self._run_macos_cli_smoke(archive.getvalue(), **digests)
                self.assertIsNotNone(failure)
                self.assertFalse(any(args[0] == "/usr/bin/codesign" for args in calls))
                self.assertEqual(sum(args[0] == "curl" for args in calls), 2)

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
