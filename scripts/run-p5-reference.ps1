[CmdletBinding()]
param(
    [string] $ResultPath = $(if ($env:FESTERM_P5_REFERENCE_RESULT_PATH) {
        $env:FESTERM_P5_REFERENCE_RESULT_PATH
    } else {
        'p5-reference-result.txt'
    })
)

# Optional P5 PTY reference-application probe. This does not replace manual
# native-window, vttest, tack, or Copilot CLI validation.
$apps = if ($env:FESTERM_P5_REFERENCE_APPS) {
    $env:FESTERM_P5_REFERENCE_APPS -split ','
} elseif ($env:OS -eq 'Windows_NT') {
    @('nvim', 'less')
} else {
    @('less', 'nvim', 'htop', 'tmux')
}
$status = 'pass'
$ran = $false
Set-Content -Path $ResultPath -Value 'status=running' -NoNewline

foreach ($app in $apps) {
    if ($app -notin @('less', 'nvim', 'htop', 'tmux')) {
        Add-Content -Path $ResultPath -Value "`napp=$app status=not-run reason=unsupported-selector"
        if ($status -eq 'pass') { $status = 'partial' }
        continue
    }
    if (-not (Get-Command "$app.exe" -ErrorAction SilentlyContinue) -and
        -not (Get-Command $app -ErrorAction SilentlyContinue)) {
        Add-Content -Path $ResultPath -Value "`napp=$app status=not-run reason=unavailable"
        if ($status -eq 'pass') { $status = 'partial' }
        continue
    }

    $ran = $true
    $env:FESTERM_P5_REFERENCE_APP = $app
    cargo test -p festerm p5_reference_application_pty_probe -- --ignored
    if ($LASTEXITCODE -eq 0) {
        Add-Content -Path $ResultPath -Value "`napp=$app status=pass"
    } else {
        Add-Content -Path $ResultPath -Value "`napp=$app status=fail"
        $status = 'fail'
    }
}
Remove-Item Env:FESTERM_P5_REFERENCE_APP -ErrorAction Ignore

if (-not $ran -and $status -eq 'pass') { $status = 'not-run' }
Add-Content -Path $ResultPath -Value "`nstatus=$status"
if ($status -eq 'fail') { exit 1 }
