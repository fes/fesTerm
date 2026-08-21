# Runs every scriptable M6 evidence suite on this machine (Windows) and
# bundles the results into a single timestamped, content-free evidence
# directory. See docs/m6-evidence-collection.md for what this does and does
# not prove, and docs/m6-manual-evidence-instructions.md for the remaining
# evidence that cannot be scripted.
[CmdletBinding()]
param(
    [string] $OutputDir,
    [switch] $SkipOsInputSmoke
)

$repositoryRoot = (Resolve-Path "$PSScriptRoot\..").Path
Push-Location $repositoryRoot
try {
    $commitSha = (git rev-parse HEAD).Trim()
    $shortSha = (git rev-parse --short HEAD).Trim()
    $timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
    git diff --quiet -- . 2>$null
    $workingTreeDirty = ($LASTEXITCODE -ne 0)
    git diff --cached --quiet -- . 2>$null
    $workingTreeDirty = $workingTreeDirty -or ($LASTEXITCODE -ne 0)

    if (-not $OutputDir) {
        $OutputDir = Join-Path $repositoryRoot "m6-evidence\windows-$timestamp-$shortSha"
    }
    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
    $OutputDir = (Resolve-Path $OutputDir).Path

    $manifestPath = Join-Path $OutputDir 'manifest.txt'
    $summaryPath = Join-Path $OutputDir 'summary.txt'
    Set-Content -Path $summaryPath -Value $null -NoNewline
    $overallStatus = 'pass'

    $osVersion = (Get-CimInstance Win32_OperatingSystem).Caption
    @(
        "commit_sha=$commitSha"
        "working_tree=$(if ($workingTreeDirty) { 'dirty' } else { 'clean' })"
        "collected_at_utc=$timestamp"
        'platform=windows'
        "os_version=$osVersion"
        "arch=$env:PROCESSOR_ARCHITECTURE"
        "rustc=$(try { (rustc --version) } catch { 'unavailable' })"
        "cargo=$(try { (cargo --version) } catch { 'unavailable' })"
    ) | Set-Content -Path $manifestPath

    function Add-Record {
        param(
            [string] $Suite,
            [string] $Status,
            [string] $Detail
        )
        $line = "suite=$Suite status=$Status"
        if ($Detail) { $line = "$line $Detail" }
        Add-Content -Path $summaryPath -Value $line
        if ($Status -eq 'fail') { $script:overallStatus = 'fail' }
    }

    function Invoke-Logged {
        param(
            [string] $Name,
            [scriptblock] $ScriptBlock
        )
        $logPath = Join-Path $OutputDir "$Name.log"
        try {
            & $ScriptBlock *>&1 | Out-File -FilePath $logPath -Encoding utf8
            if ($LASTEXITCODE -eq 0) {
                Add-Record -Suite $Name -Status 'pass'
            } else {
                Add-Record -Suite $Name -Status 'fail' -Detail "see $Name.log"
            }
        } catch {
            $_ | Out-File -FilePath $logPath -Encoding utf8 -Append
            Add-Record -Suite $Name -Status 'fail' -Detail "see $Name.log"
        }
    }

    Write-Host "Collecting M6 evidence into: $OutputDir"

    Invoke-Logged -Name 'fmt-check' -ScriptBlock { cargo fmt --all -- --check }
    Invoke-Logged -Name 'clippy' -ScriptBlock { cargo clippy --workspace --all-targets -- -D warnings }
    Invoke-Logged -Name 'workspace-tests' -ScriptBlock { cargo test --workspace -- --test-threads=1 }

    $optionalValidationResultPath = Join-Path $OutputDir 'optional-validation-result.txt'
    $env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
    $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH = $optionalValidationResultPath
    $env:FESTERM_P5_REFERENCE_RESULT_PATH = (Join-Path $OutputDir 'p5-reference-result.txt')
    $env:FESTERM_P6_RENDER_RESULT_PATH = (Join-Path $OutputDir 'p6-render-result.txt')
    $env:FESTERM_OPENSSH_INTEROP_RESULT_PATH = (Join-Path $OutputDir 'openssh-interop-result.txt')
    $optionalValidationLog = Join-Path $OutputDir 'optional-validation.log'
    try {
        & "$PSScriptRoot\run-optional-validation.ps1" *>&1 | Out-File -FilePath $optionalValidationLog -Encoding utf8
        if ($LASTEXITCODE -eq 0) {
            Add-Record -Suite 'optional-validation' -Status 'pass'
        } else {
            Add-Record -Suite 'optional-validation' -Status 'fail' -Detail 'see optional-validation.log and optional-validation-result.txt'
        }
    } catch {
        $_ | Out-File -FilePath $optionalValidationLog -Encoding utf8 -Append
        Add-Record -Suite 'optional-validation' -Status 'fail' -Detail 'see optional-validation.log'
    } finally {
        Remove-Item Env:FESTERM_RUN_OPTIONAL_VALIDATION -ErrorAction Ignore
        Remove-Item Env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH -ErrorAction Ignore
        Remove-Item Env:FESTERM_P5_REFERENCE_RESULT_PATH -ErrorAction Ignore
        Remove-Item Env:FESTERM_P6_RENDER_RESULT_PATH -ErrorAction Ignore
        Remove-Item Env:FESTERM_OPENSSH_INTEROP_RESULT_PATH -ErrorAction Ignore
    }
    # run-optional-validation.ps1 already runs Windows OS-input smoke as part
    # of its own sequence; fold its recorded sub-suite outcomes in so a single
    # failing sub-suite (p5, p6-renderer, openssh-interop, p4-native-window,
    # p5-windows-os-input) is visible without opening the raw result file.
    if (Test-Path $optionalValidationResultPath) {
        Get-Content $optionalValidationResultPath | Where-Object { $_ -like 'suite=*' } |
            ForEach-Object { Add-Content -Path $summaryPath -Value $_ }
    }

    if ($SkipOsInputSmoke) {
        Add-Record -Suite 'os-input-smoke' -Status 'skipped' -Detail 'reason=requested via -SkipOsInputSmoke (already covered by optional-validation on Windows)'
    }

    Add-Content -Path $summaryPath -Value "overall_status=$overallStatus"
    Write-Host ''
    Write-Host "== M6 evidence summary ($OutputDir) =="
    Get-Content $summaryPath | Write-Host
    Write-Host ''
    Write-Host 'Remaining evidence that cannot be scripted (reference-application screen'
    Write-Host 'semantics, vttest, and usability judgment) is in'
    Write-Host 'docs/m6-manual-evidence-instructions.md.'

    if ($overallStatus -ne 'pass') { exit 1 }
} finally {
    Pop-Location
}
