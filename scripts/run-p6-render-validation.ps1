[CmdletBinding()]
param(
    [string] $ResultPath = $(if ($env:FESTERM_P6_RENDER_RESULT_PATH) {
        $env:FESTERM_P6_RENDER_RESULT_PATH
    } else {
        'p6-render-result.txt'
    })
)

Set-Content -Path $ResultPath -Value 'status=running' -NoNewline
& cargo test -p festerm-ui-egui
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=p6-renderer status=pass`nstatus=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=p6-renderer status=fail`nstatus=fail"
    exit 1
}
