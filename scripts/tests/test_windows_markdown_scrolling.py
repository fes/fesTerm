"""Pure PowerShell profiling helpers: no app launch, Win32 call or native probe."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-windows-markdown-scrolling.ps1"


@unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 is required")
class MarkdownScrollingTests(unittest.TestCase):
    def probe(self, body):
        environment = os.environ.copy()
        environment["FESTERM_SCROLL_SCRIPT"] = str(SCRIPT)
        environment["FESTERM_OPTIONAL_SCRIPT"] = str(SCRIPT.with_name("run-optional-validation.ps1"))
        result = subprocess.run(
            [
                shutil.which("pwsh"),
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r"""
$ErrorActionPreference = 'Stop'
. $env:FESTERM_SCROLL_SCRIPT -HelperOnly
function Assert($condition, $message) { if (-not $condition) { throw $message } }
function Reject($action) {
    $rejected = $false
    try { & $action } catch { $rejected = $true }
    Assert $rejected 'Expected rejection'
}
function Sample($offset) {
    "DEBUG festerm::markdown_scroll: Markdown viewer scroll state markdown_preview_offset_y=$offset markdown_preview_content_height=2e4 markdown_preview_viewport_height=400"
}
"""
                + body,
            ],
            env=environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_aggregate_native_preflight_rejects_platform_and_missing_artifacts(self):
        self.probe(
            r"""
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $env:FESTERM_OPTIONAL_SCRIPT, [ref]$tokens, [ref]$errors)
Assert ($errors.Count -eq 0) 'Aggregate script parses'
$function = $ast.Find({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Invoke-MarkdownScrollingValidation'
}, $true)
Assert ($null -ne $function) 'Aggregate native entry point exists'
. ([scriptblock]::Create($function.Extent.Text))
$env:OS = 'not-windows'
Reject { Invoke-MarkdownScrollingValidation }
$env:OS = 'Windows_NT'
foreach ($name in @('FESTERM_MARKDOWN_SCROLL_EXE', 'FESTERM_MARKDOWN_SCROLL_RECEIPT',
    'FESTERM_MARKDOWN_SCROLL_SOURCE_ROOT', 'FESTERM_MARKDOWN_SCROLL_OUT')) {
    [Environment]::SetEnvironmentVariable($name, $null)
}
$message = ''
try { Invoke-MarkdownScrollingValidation } catch { $message = $_.Exception.Message }
Assert ($message -like '*FESTERM_MARKDOWN_SCROLL_EXE*') 'Missing artifact rejected before any native work'
"""
        )

    def test_invariant_offsets_scientific_notation_ansi_and_pane(self):
        self.probe(
            r"""
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::GetCultureInfo('de-DE')
$text = (Sample '1.25e2') + "`n" + ((Sample '126.5') -replace 'viewer', 'preview pane')
$samples = @(Get-MarkdownScrollSamples ("`e[32m" + $text + "`e[0m"))
Assert ($samples.Count -eq 2) 'Expected both supported diagnostics'
Assert ($samples[0].Offset -eq 125 -and $samples[1].Offset -eq 126.5) 'Invariant numbers'
Assert ($samples[0].Content -eq 20000 -and $samples[0].Viewport -eq 400) 'Geometry'
Assert (@(Get-MarkdownScrollSamples 'unrelated offset=100').Count -eq 0) 'Unrelated log'
"""
        )

    def test_invalid_missing_nonfinite_and_impossible_samples_fail(self):
        self.probe(
            r"""
foreach ($bad in @('NaN', 'Infinity', '-1', '1e999', '1,25', '99999', '1junk')) {
    Reject { Get-MarkdownScrollSamples (Sample $bad) }
}
Reject { Get-MarkdownScrollSamples ((Sample '0') -replace ' markdown_preview_viewport_height=400', '') }
Reject { Get-MarkdownScrollSamples ((Sample '0') -replace 'viewport_height=400', 'viewport_height=0') }
"""
        )

    def test_preview_hit_point_uses_actual_rectangle_and_physical_dpi(self):
        self.probe(
            r"""
$line = (Sample '0') + ' markdown_preview_left=216 markdown_preview_top=165.5 markdown_preview_right=640 markdown_preview_bottom=376'
$sample = @(Get-MarkdownScrollSamples $line)[0]
$point = Get-MarkdownPhysicalClientPoint $sample 192 1280 800
Assert ($point[0] -eq 856 -and $point[1] -eq 542) 'Physical center of actual preview'
Reject { Get-MarkdownPhysicalClientPoint @(Get-MarkdownScrollSamples (Sample '0'))[0] 192 1280 800 }
Reject { Get-MarkdownPhysicalClientPoint $sample 0 1280 800 }
Reject { Get-MarkdownPhysicalClientPoint $sample 192 400 800 }
Reject { Get-MarkdownScrollSamples ($line -replace ' markdown_preview_bottom=376', '') }
$sample.Rect = @(-10, -20, 100, 120)
$point = Get-MarkdownPhysicalClientPoint $sample 144 120 150
Assert ($point[0] -eq 60 -and $point[1] -eq 75) 'Center is clipped to visible client'
$sample.Rect = @(100, 100, 50, 50)
Reject { Get-MarkdownPhysicalClientPoint $sample 96 640 400 }
"""
        )

    def test_later_sample_and_actual_monotonic_progress_required(self):
        self.probe(
            r"""
$samples = @(Get-MarkdownScrollSamples ((Sample '0') + "`n" + (Sample '40') + "`n" + (Sample '100')))
Assert ((Assert-MarkdownScrollProgress $samples 1 0) -eq 100) 'Actual coalesced progress'
Reject { Assert-MarkdownScrollProgress $samples 3 100 }
Reject { Assert-MarkdownScrollProgress @($samples[0], $samples[0]) 1 0 }
Reject { Assert-MarkdownScrollProgress @($samples[0], $samples[2], $samples[1]) 1 0 }
Reject { Assert-MarkdownScrollProgress @($samples[0]) 0 0 }
"""
        )

    def test_requires_exact_adapter_backend_device_in_one_record(self):
        self.probe(
            r"""
$proof = 'INFO festerm::app: wgpu selected a rendering adapter adapter_name=Microsoft Basic Render Driver backend=Dx12 device_type=Cpu'
Assert-MarkdownWarpAdapter $proof
Assert-MarkdownWarpAdapter ($proof -replace 'adapter_name=Microsoft Basic Render Driver', 'adapter_name="Microsoft Basic Render Driver"')
foreach ($bad in @(
    ($proof -replace 'Dx12', 'Vulkan'),
    ($proof -replace 'Cpu', 'DiscreteGpu'),
    ($proof -replace 'Microsoft Basic Render Driver', 'WARP CPU'),
    ($proof -replace 'Microsoft Basic Render Driver', 'Microsoft Basic Render Driver Fake'),
    ($proof -replace ' backend=', "`nbackend="),
    'WARP CPU software adapter')) { Reject { Assert-MarkdownWarpAdapter $bad } }
"""
        )

    def test_upward_progress_endpoints_and_wrong_direction(self):
        self.probe(
            r"""
$samples = @(Get-MarkdownScrollSamples ((Sample '100') + "`n" + (Sample '40') + "`n" + (Sample '0') + "`n" + (Sample '0')))
Assert ((Assert-MarkdownScrollProgress $samples 1 100 -1) -eq 0) 'Upward progress reaches endpoint'
Reject { Assert-MarkdownScrollProgress $samples 1 100 1 }
Reject { Assert-MarkdownScrollProgress @($samples[0], $samples[0]) 1 100 -1 }
Reject { Assert-MarkdownScrollProgress @($samples[0], $samples[2], $samples[1]) 1 100 -1 }
$reverse = @(Get-MarkdownScrollSamples ((Sample '100') + "`n" + (Sample '40') + "`n" + (Sample '40.001') + "`n" + (Sample '0')))
Reject { Assert-MarkdownScrollProgress $reverse 1 100 -1 }
$down = @($samples[2], $samples[1], $samples[0], $samples[0])
Assert ((Assert-MarkdownScrollProgress $down 1 0) -eq 100) 'Downward progress can remain at endpoint'
"""
        )

    def test_drain_requires_minimum_duration_and_quiet_window(self):
        self.probe(
            r"""
$samples = @(Get-MarkdownScrollSamples ((Sample '0') + "`n" + (Sample '40')))
$state = Get-MarkdownScrollDrainState $samples 1 0 0 100
Assert (-not $state.Settled -and $state.LastChangeMs -eq 100) 'Motion resets quiet window'
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 400
Assert (-not $state.Settled) 'Quiet alone does not bypass 500ms drain'
$samples += @(Get-MarkdownScrollSamples (Sample '50'))
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 450
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 500
Assert (-not $state.Settled) 'Recent progress does not finish drain'
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 650
Assert ($state.Settled -and -not $state.TimedOut) '200ms quiet and 500ms elapsed'
$samples += @(Get-MarkdownScrollSamples (Sample '50'))
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 700
Assert ($state.Settled -and $state.LastChangeMs -eq 450) 'Repeated endpoint diagnostic is not motion'
$samples += @(Get-MarkdownScrollSamples (Sample '51'))
$state = Get-MarkdownScrollDrainState $samples $state.Count $state.Offset $state.LastChangeMs 3000
Assert ($state.TimedOut -and -not $state.Settled) 'Changing at deadline fails'
$still = Get-MarkdownScrollDrainState @($samples[0]) 1 0 0 500
Assert $still.Settled 'No motion can settle, but progress remains separately mandatory'
Reject { Assert-MarkdownScrollProgress @($samples[0]) 1 0 }
"""
        )

    def test_signed_coordinate_packing_and_range(self):
        self.probe(
            r"""
foreach ($point in @(@(-1200, -400), @(100, 200), @(-32768, 32767), @(32767, -32768))) {
    $packed = (ConvertTo-MarkdownPackedPoint $point[0] $point[1]).ToInt64()
    $x = $packed -band 65535
    $y = ($packed -shr 16) -band 65535
    if ($x -ge 32768) { $x -= 65536 }
    if ($y -ge 32768) { $y -= 65536 }
    Assert ($x -eq $point[0] -and $y -eq $point[1]) 'Signed point roundtrip'
}
Reject { ConvertTo-MarkdownPackedPoint 32768 0 }
Reject { ConvertTo-MarkdownPackedPoint 0 -32769 }
"""
        )

    def test_root_retention_counters(self):
        self.probe(
            r"""
$counters = Get-MarkdownFrameCounters @'
DEBUG egui_wgpu::frame_timing root_paint completed_frames=10 primitives=20 total_ms=1.2
DEBUG egui_wgpu::retained_ui retained_ui_reused_frames=4 retained_ui_rebuilt_frames=6
DEBUG egui_wgpu::frame_timing root_paint completed_frames=12 primitives=21 total_ms=1.8
'@
Assert ($counters.RootCompleted -eq 12 -and $counters.RootSamples -eq 2) 'Root counters'
Assert ($counters.Reused -eq 4 -and $counters.Rebuilt -eq 6) 'Retention counters'
Assert ($null -eq $counters.PartialReused -and $null -eq $counters.CachedPrefixJobs -and
    $null -eq $counters.CachedSignatureJobs) 'Old source has no new counters'
$modern = Get-MarkdownFrameCounters @'
DEBUG egui_wgpu::retained_ui retained_ui_reused_frames=4 retained_ui_rebuilt_frames=6 retained_ui_texture_bytes=1024 whole_frame=true retained_ui_partial_reused_frames=3 retained_ui_cached_prefix_jobs=8 retained_ui_cached_signature_jobs=10
'@
Assert ($modern.Reused -eq 4 -and $modern.Rebuilt -eq 6 -and $modern.PartialReused -eq 3) 'Exact and partial are separate'
Assert ($modern.CachedPrefixJobs -eq 8 -and $modern.CachedSignatureJobs -eq 10) 'Raw cached-job counters'
$mixed = Get-MarkdownFrameCounters @'
DEBUG egui_wgpu::retained_ui retained_ui_reused_frames=4 retained_ui_rebuilt_frames=6 whole_frame=true retained_ui_partial_reused_frames=3 retained_ui_cached_prefix_jobs=8 retained_ui_cached_signature_jobs=10
DEBUG egui_wgpu::retained_ui retained_ui_reused_frames=5 retained_ui_rebuilt_frames=7 whole_frame=false
'@
Assert ($mixed.Reused -eq 5 -and $null -eq $mixed.PartialReused) 'Latest record never inherits missing counters'
$empty = Get-MarkdownFrameCounters ''
Assert ($null -eq $empty.Reused -and $empty.RetentionSamples -eq 0 -and
    $null -eq $empty.PartialReused -and $null -eq $empty.CachedPrefixJobs -and
    $null -eq $empty.CachedSignatureJobs) 'Absence is not zero'
"""
        )

    def test_host_frame_quantiles_only_cover_measured_root_range(self):
        self.probe(
            r"""
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::GetCultureInfo('de-DE')
function Frame($frame, $number, $primitives) {
    "DEBUG egui_wgpu::frame_timing root_paint completed_frames=$frame primitives=$primitives prepare_ms=$number configure_ms=999 acquire_ms=$number encode_ms=$number submit_ms=$number release_ms=999 present_ms=$number total_ms=$number"
}
$text = @(
    (Frame 9 'NaN' 1000),
    (Frame 10 '999' 1000),
    'DEBUG egui_wgpu::frame_timing frame_begin completed_frames=11 primitives=1000 prepare_ms=NaN',
    (Frame 11 '1e-1' 3),
    (Frame 12 '4.5' 9),
    (Frame 13 'Infinity' 1000)
) -join "`n"
$summary = Get-MarkdownHostFrameTiming $text 10 12
Assert ($summary.samples -eq 2) 'Only measured completed root paints'
Assert ($summary.root_frame_exclusive_start -eq 10 -and $summary.root_frame_inclusive_end -eq 12) 'Frame bounds'
Assert ($summary.primitives.p50 -eq 3 -and $summary.primitives.p95 -eq 9) 'Raw primitive quantiles'
foreach ($field in @('prepare_ms', 'acquire_ms', 'encode_ms', 'submit_ms', 'present_ms', 'total_ms')) {
    Assert ($summary.$field.p50 -eq 0.1 -and $summary.$field.p95 -eq 4.5) 'Invariant nearest-rank duration quantiles'
}
$empty = Get-MarkdownHostFrameTiming $text 20 30
Assert ($empty.samples -eq 0 -and $null -eq $empty.total_ms.p50) 'No invented samples'
"""
        )

    def test_measured_frame_invalid_negative_nonfinite_or_missing_fields_fail(self):
        self.probe(
            r"""
function Frame($number) {
    "DEBUG egui_wgpu::frame_timing root_paint completed_frames=11 primitives=2 prepare_ms=$number acquire_ms=1 encode_ms=1 submit_ms=1 present_ms=1 total_ms=1"
}
foreach ($bad in @('-1', 'NaN', 'Infinity', '1e999', '1,25', '1junk')) {
    Reject { Get-MarkdownHostFrameTiming (Frame $bad) 10 12 }
}
Reject { Get-MarkdownHostFrameTiming ((Frame '1') -replace ' total_ms=1', '') 10 12 }
Reject { Get-MarkdownHostFrameTiming ((Frame '1') -replace 'primitives=2', 'primitives=-1') 10 12 }
Reject { Get-MarkdownHostFrameTiming ((Frame '1') -replace 'primitives=2', 'primitives=1.5') 10 12 }
Reject { Get-MarkdownHostFrameTiming (Frame '1') 12 10 }
"""
        )

    def test_artifact_hashes_and_recipe_fixture_receipt_identity_failures(self):
        self.probe(
            r"""
$hashes = @{ recipe = ('A' * 64); fixture = ('B' * 64); receipt = ('C' * 64) }
function Get-FileHash { param($LiteralPath, $Algorithm) @{Hash=$hashes[$LiteralPath]} }
$expected = Get-MarkdownArtifactIdentity 'recipe' 'fixture' 'receipt'
Assert ($expected.recipe_sha256 -ceq ('a' * 64)) 'Normalized recipe SHA256'
Assert ($expected.fixture_sha256 -eq ('b' * 64) -and $expected.receipt_sha256 -eq ('c' * 64)) 'Fixture and receipt SHA256'
Assert-MarkdownArtifactIdentity $expected (Get-MarkdownArtifactIdentity 'recipe' 'fixture' 'receipt')
foreach ($artifact in @('recipe', 'fixture', 'receipt')) {
    $saved = $hashes[$artifact]
    $hashes[$artifact] = 'D' * 64
    Reject { Assert-MarkdownArtifactIdentity $expected (Get-MarkdownArtifactIdentity 'recipe' 'fixture' 'receipt') }
    $hashes[$artifact] = $saved
}
"""
        )

    def test_cleanup_failures_do_not_skip_owned_pid_drain_or_disposal(self):
        self.probe(
            r"""
$result = [ordered]@{status='passed'}
$trace = [Collections.Generic.List[string]]::new()
$steps = @(
    @{Name='pointer_restore';Action={ $trace.Add('restore'); throw 'Private path C:\personal\pointer' }},
    @{Name='owned_pid';Action={ $trace.Add('pid') }},
    @{Name='raw_log_drain';Action={ $trace.Add('drain'); throw 'Private path C:\personal\log' }},
    @{Name='raw_log_dispose';Action={ $trace.Add('dispose') }},
    @{Name='source_revalidation';Action={ $trace.Add('source') }},
    @{Name='process_dispose';Action={ $trace.Add('process') }}
)
function Run-FailingProfile {
    $primary = $null
    try { throw 'primary profile failure' }
    catch { $primary = $_ }
    finally { Invoke-MarkdownCleanup $result $steps }
    if ($null -ne $primary) { throw $primary }
}
$caught = $null
try { Run-FailingProfile } catch { $caught = $_ }
Assert ($caught.Exception.Message -eq 'primary profile failure') 'Cleanup does not mask primary failure'
Assert (($trace -join ',') -eq 'restore,pid,drain,dispose,source,process') 'All cleanup stages attempted'
Assert ($result.status -eq 'failed' -and $result.cleanup_errors.Count -eq 2) 'No stale passed status'
Assert ($result.cleanup_errors[0].stage -eq 'pointer_restore' -and
    $result.cleanup_errors[1].stage -eq 'raw_log_drain') 'Failed stages retained'
Assert (($result | ConvertTo-Json -Depth 5) -notmatch 'personal') 'No private exception paths in result'
$successful = [ordered]@{status='passed'}
Invoke-MarkdownCleanup $successful @(@{Name='no_op';Action={}})
Assert ($successful.status -eq 'passed' -and $successful.cleanup_errors.Count -eq 0) 'Successful cleanup remains passed'
"""
        )

    def test_exact_fixture_shapes_and_realistic_source_size_bounds(self):
        fixtures = json.loads(
            self.probe(
                r"""
$fixtures = @{}
foreach ($name in @('rust20', 'rust300', 'table30', 'prose')) {
    $fixture = New-MarkdownScrollFixture $name
    Assert ($fixture -eq (New-MarkdownScrollFixture $name)) 'Deterministic fixture'
    $fixtures[$name] = $fixture
}
$fixtures | ConvertTo-Json
"""
            )
        )
        bounds = {
            "rust20": (500, 1200),
            "rust300": (8000, 16000),
            "table30": (1000, 2500),
            "prose": (3000, 7000),
        }
        for name, fixture in fixtures.items():
            with self.subTest(case=name):
                minimum, maximum = bounds[name]
                self.assertGreaterEqual(len(fixture), minimum)
                self.assertLessEqual(len(fixture), maximum)
                if name.startswith("rust"):
                    fences = re.findall(r"^```.*$", fixture, re.MULTILINE)
                    self.assertEqual(fences, ["```rust", "```"])
                    code = fixture.split("```rust\n", 1)[1].split("\n```", 1)[0]
                    self.assertEqual(len(code.splitlines()), int(name[4:]))
                    self.assertTrue(all(line.startswith("const VALUE_") for line in code.splitlines()))
                elif name == "table30":
                    rows = [line for line in fixture.splitlines() if line.startswith("|")]
                    self.assertEqual(rows[0], "| Column | Value | Note |")
                    self.assertEqual(rows[1], "| --- | --- | --- |")
                    self.assertEqual(len(rows[2:]), 30)
                    for index, row in enumerate(rows[2:]):
                        self.assertTrue(row.startswith(f"| Row {index} |"))
                else:
                    paragraphs = fixture.strip().split("\n\n")
                    self.assertEqual(paragraphs[0], "# Synthetic scrolling fixture")
                    self.assertEqual(len(paragraphs[1:]), 20)
                    self.assertTrue(
                        all(paragraph.startswith("Synthetic paragraph") for paragraph in paragraphs[1:])
                    )

    def test_native_entry_point_requires_opt_in_before_side_effects(self):
        environment = os.environ.copy()
        environment.pop("FESTERM_RUN_OPTIONAL_VALIDATION", None)
        result = subprocess.run(
            [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-File", str(SCRIPT)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FESTERM_RUN_OPTIONAL_VALIDATION=1", result.stderr)

    @unittest.skipUnless(os.name == "nt", "Windows file sharing semantics are required")
    def test_private_staging_acl_and_immutable_artifact_lock(self):
        self.probe(
            r"""
$directory = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
[void][IO.Directory]::CreateDirectory($directory)
Protect-MarkdownPrivateDirectory $directory
$directoryLock = [FesTermDirectoryLock]::Open($directory)
$owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
$system = [Security.Principal.SecurityIdentifier]::new(
    [Security.Principal.WellKnownSidType]::LocalSystemSid, $null)
$acl = Get-Acl -LiteralPath $directory
Assert $acl.AreAccessRulesProtected 'Private staging disables inherited access'
foreach ($rule in $acl.Access) {
    Assert (-not $rule.IsInherited) 'Private staging has no inherited rules'
    Assert ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) 'Only allow rules'
    Assert ($rule.IdentityReference -eq $owner -or $rule.IdentityReference -eq $system) 'Only owner and SYSTEM'
}
$source = Join-Path $directory 'source.bin'
$artifact = Join-Path $directory 'artifact.bin'
[IO.File]::WriteAllText($source, 'frozen')
$lock = Copy-MarkdownImmutableArtifact $source $artifact
try {
    Reject {
        $writer = [IO.File]::Open($artifact, [IO.FileMode]::Open, [IO.FileAccess]::Write,
            [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
        $writer.Dispose()
    }
    Reject { [IO.File]::Delete($artifact) }
    Assert ((Get-Content -LiteralPath $artifact -Raw) -eq 'frozen') 'Locked artifact remains readable'
        Reject { Move-Item -LiteralPath $directory -Destination "$directory-moved" }
    } finally {
        $lock.Dispose()
        $directoryLock.Dispose()
    Remove-Item -LiteralPath $source
    Remove-Item -LiteralPath $artifact
    Remove-Item -LiteralPath $directory
}
"""
        )

    def test_receipt_requires_hash_clean_source_and_actual_git_identity(self):
        self.probe(
            r"""
$script:revision = 'a' * 40
$script:tree = 'b' * 40
$script:hash = 'c' * 64
$script:dirty = $false
$script:gitFailure = $false
$script:receiptObject = @{
    schema_version = 1
    executable_sha256 = $script:hash
    source_revision = $script:revision
    source_tree = $script:tree
}
function Get-Content { param($LiteralPath, [switch]$Raw) $script:receiptObject | ConvertTo-Json }
function Get-FileHash { param($LiteralPath, $Algorithm) @{Hash=$script:hash} }
function git {
    $global:LASTEXITCODE = if ($script:gitFailure) { 1 } else { 0 }
    if ($args -contains 'HEAD^{tree}') { return $script:tree }
    if ($args -contains 'HEAD') { return $script:revision }
    if ($script:dirty) { return '?? untracked.rs' }
}
$valid = Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source'
Assert ($valid.source_revision -eq $script:revision -and $valid.source_tree -eq $script:tree) 'Git identities'
Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' $valid | Out-Null
foreach ($field in @('source_revision', 'source_tree', 'executable_sha256')) {
    $saved = $script:receiptObject[$field]
    $script:receiptObject[$field] = 'd' * $saved.Length
    Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' }
    $script:receiptObject[$field] = $saved
}
$script:revision = 'd' * 40
$script:receiptObject.source_revision = $script:revision
Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' $valid }
$script:revision = 'a' * 40
$script:receiptObject.source_revision = $script:revision
$script:hash = 'd' * 64
$script:receiptObject.executable_sha256 = $script:hash
Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' $valid }
$script:hash = 'c' * 64
$script:receiptObject.executable_sha256 = $script:hash
$script:dirty = $true
Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' }
$script:dirty = $false
$script:gitFailure = $true
Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' }
$script:gitFailure = $false
$script:receiptObject.source_revision = 'main'
Reject { Assert-MarkdownSourceReceipt 'binary' 'receipt' 'source' }
"""
        )


if __name__ == "__main__":
    unittest.main()
