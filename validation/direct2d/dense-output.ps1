$ErrorActionPreference = 'Stop'
if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 for the controlled output fixture.'
}

$end = [DateTime]::UtcNow.AddSeconds(180)
$escape = [char]27
$frameNumber = 0
[Console]::Write("$escape[?25l")
while ([DateTime]::UtcNow -lt $end) {
    $frame = [Text.StringBuilder]::new()
    for ($row = 1; $row -le 24; $row++) {
        $line = ('Frame {0:D6} row {1:D2} ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 abcdefghijklmnopqrstuvwxyz' -f $frameNumber, $row).Substring(0, 79)
        [void] $frame.Append(("$escape[{0};1H{1}" -f $row, $line))
    }
    [Console]::Write($frame.ToString())
    $frameNumber++
    Start-Sleep -Milliseconds 100
}
