import hashlib
import importlib.util
import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "recover_desktop_release", ROOT / "scripts/recover-desktop-release.py"
)
recovery = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(recovery)


class DesktopReleaseRecoveryTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".test-work"
        scratch.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "--quiet")
        (self.root / "scripts").mkdir()
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        (self.root / "scripts/build_tunnel_bundle.py").write_text("tagged packager\n")
        (self.root / "scripts/build_host_manifest.py").write_text("SCHEMA = 2\n")
        self.commit("tagged source")
        self.source = self.git("rev-parse", "HEAD")
        self.git("tag", "v1.2.3")
        (self.root / "scripts/build_tunnel_bundle.py").write_text("fixed packager\n")
        (self.root / ".github/workflows").mkdir(parents=True)
        (self.root / ".github/workflows/ci.yml").write_text("notice validation\n")
        self.commit("repair packaging")
        self.head = self.git("rev-parse", "HEAD")
        self.runs = [
            {"headSha": self.head, "status": "completed", "conclusion": "success"},
            {"headSha": self.source, "status": "completed", "conclusion": "success"},
        ]

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.root, check=True, capture_output=True, text=True,
        ).stdout.strip()

    def commit(self, subject):
        self.git("add", "--all")
        self.git(
            "-c", "user.name=Release Test", "-c", "user.email=release@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "--quiet", "-m",
            f"{subject}\n\nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>",
        )

    def validate(self, **overrides):
        values = {
            "version": "1.2.3", "head": self.head, "ref": "refs/heads/main",
            "main_head": self.head, "runs": self.runs, "root": self.root,
        }
        values.update(overrides)
        return recovery.validate(**values)

    def workflow_outputs(self, event, ref, sha, resume):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        step = workflow.split("      - name: Resolve and validate release version\n", 1)[1]
        step = step.split("      - name: Install Linux validation dependencies\n", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        output = self.root / "workflow-output"
        output.unlink(missing_ok=True)
        env = dict(os.environ, GITHUB_EVENT_NAME=event, GITHUB_REF=ref, GITHUB_SHA=sha,
                   GITHUB_REF_NAME="v1.2.3" if event == "push" else "main",
                   GITHUB_REPOSITORY="example/repo", GITHUB_OUTPUT=str(output),
                   DISPATCH_VERSION="1.2.3", RESUME_PUBLISH=resume)
        subprocess.run(["bash", "-e", "-c", script], cwd=self.root, env=env, check=True,
                       capture_output=True, text=True)
        return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_recovers_exact_tag_after_tooling_only_change(self):
        self.assertEqual(self.validate(), self.source)

    @unittest.skipUnless(shutil.which("bash") and shutil.which("sha256sum"), "requires Bash and sha256sum")
    def test_workflow_keeps_normal_dispatch_dry_and_pins_recovery_source(self):
        outputs = self.workflow_outputs("push", "refs/tags/v1.2.3", self.source, "")
        self.assertEqual((outputs["publish"], outputs["recovery"], outputs["source_commit"]),
                         ("true", "false", self.source))
        outputs = self.workflow_outputs("workflow_dispatch", "refs/heads/main", self.head, "false")
        self.assertEqual((outputs["publish"], outputs["recovery"], outputs["source_commit"]),
                         ("false", "false", self.head))
        (self.root / "scripts/recover-desktop-release.py").write_text(f"print('{self.source}')\n")
        outputs = self.workflow_outputs("workflow_dispatch", "refs/heads/main", self.head, "true")
        self.assertEqual((outputs["publish"], outputs["recovery"], outputs["source_commit"]),
                         ("true", "true", self.source))
        expected = hashlib.sha256((self.root / "scripts/build_tunnel_bundle.py").read_bytes()).hexdigest()
        self.assertEqual(outputs["helper_sha256"], expected)
        self.assertEqual(outputs["authenticated_helpers"], "true")

    @unittest.skipUnless(shutil.which("bash") and shutil.which("sha256sum"), "requires Bash and sha256sum")
    def test_workflow_only_exempts_explicit_recovery_of_pre_authentication_source(self):
        self.git("rm", "scripts/build_host_manifest.py")
        self.commit("legacy source fixture")
        self.source = self.git("rev-parse", "HEAD")
        self.git("tag", "--force", "v1.2.3")
        (self.root / "scripts/build_tunnel_bundle.py").write_text("legacy tooling repair\n")
        self.commit("repair legacy packaging")
        self.head = self.git("rev-parse", "HEAD")
        (self.root / "scripts/recover-desktop-release.py").write_text(f"print('{self.source}')\n")
        with self.assertRaises(subprocess.CalledProcessError):
            self.workflow_outputs("push", "refs/tags/v1.2.3", self.source, "")
        outputs = self.workflow_outputs("workflow_dispatch", "refs/heads/main", self.head, "true")
        self.assertEqual(outputs["authenticated_helpers"], "false")
        self.assertEqual(outputs["headless_helpers"], "false")
        self.assertEqual(outputs["source_commit"], self.source)

    @unittest.skipUnless(shutil.which("bash") and shutil.which("sha256sum"), "requires Bash and sha256sum")
    def test_schema_one_recovery_preserves_payload_without_claiming_host_cli(self):
        self.git("reset", "--hard", "--quiet", self.source)
        (self.root / "scripts/build_host_manifest.py").write_text("SCHEMA = 1\n")
        self.commit("legacy authenticated helpers")
        self.source = self.git("rev-parse", "HEAD")
        self.git("tag", "--force", "v1.2.3")
        (self.root / "scripts/build_tunnel_bundle.py").write_text("legacy tooling repair\n")
        self.commit("repair legacy packaging")
        self.head = self.git("rev-parse", "HEAD")
        (self.root / "scripts/recover-desktop-release.py").write_text(f"print('{self.source}')\n")
        with self.assertRaises(subprocess.CalledProcessError):
            self.workflow_outputs("push", "refs/tags/v1.2.3", self.source, "")
        outputs = self.workflow_outputs("workflow_dispatch", "refs/heads/main", self.head, "true")
        self.assertEqual(outputs["authenticated_helpers"], "true")
        self.assertEqual(outputs["headless_helpers"], "false")

    def test_rejects_source_changes(self):
        (self.root / "application.rs").write_text("different runtime\n")
        self.commit("change runtime")
        head = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(ValueError, "non-tooling changes"):
            self.validate(head=head, main_head=head)

    def test_rejects_whitespace_prefixed_paths_and_dirty_checkout(self):
        path = self.root / " scripts/build_tunnel_bundle.py"
        path.parent.mkdir()
        path.write_text("not the approved tooling path\n")
        with self.assertRaisesRegex(ValueError, "clean checkout"):
            self.validate()
        self.commit("add unexpected path")
        head = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(ValueError, "non-tooling changes"):
            self.validate(head=head, main_head=head)

    def test_requires_latest_green_ci_for_both_commits(self):
        for commit in (self.head, self.source):
            with self.subTest(commit=commit):
                runs = [{"headSha": commit, "status": "in_progress", "conclusion": None}, *self.runs]
                with self.assertRaisesRegex(ValueError, "main CI is not green"):
                    self.validate(runs=runs)

    def test_requires_current_main_and_an_existing_stable_tag(self):
        with self.assertRaisesRegex(ValueError, "current main"):
            self.validate(ref="refs/heads/other")
        with self.assertRaisesRegex(ValueError, "current main"):
            self.validate(main_head=self.source)
        with self.assertRaisesRegex(ValueError, "stable semver"):
            self.validate(version="1.2.3-rc")
        with self.assertRaises(subprocess.CalledProcessError):
            self.validate(version="1.2.4")


if __name__ == "__main__":
    unittest.main()
