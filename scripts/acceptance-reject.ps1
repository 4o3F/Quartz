param([Parameter(Mandatory = $true)][string] $DataRoot)
$ErrorActionPreference = 'Stop'
# Connect only after confirming that the supplied directory is an isolated acceptance root.
$root = [System.IO.Path]::GetFullPath($DataRoot)
$production = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Quartz'
if ($root.TrimEnd('\') -eq $production.TrimEnd('\')) { throw 'Use an isolated acceptance DataRoot' }
$log = Join-Path $root 'logs\core.log'
if (-not (Test-Path -LiteralPath $log)) { throw 'Acceptance core log does not exist' }
$before = (Get-Content -LiteralPath $log -Raw).Length
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$pipeName = 'Quartz.' + $sid + '.v1'
$connectTimeoutMs = 5000
$readTimeoutMs = 5000
$logPollMs = 100
$win32CodeMask = 0xffff
$errorBrokenPipe = 109
$errorNoData = 232
$errorPipeNotConnected = 233
$pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
try {
    $pipe.Connect($connectTimeoutMs)
    $buffer = [byte[]]::new(1)
    $pending = $pipe.ReadAsync($buffer, 0, $buffer.Length)
    try {
        if (-not $pending.Wait($readTimeoutMs)) { throw 'Core did not close the non-Quartz connection within 5 seconds' }
        if ($pending.GetAwaiter().GetResult() -ne 0) { throw 'Core replied to the non-Quartz client' }
    } catch [System.AggregateException] {
        # A reset is also a closed connection. Inspect typed I/O error codes, never messages.
        $cause = $_.Exception.GetBaseException()
        $code = $cause.HResult -band $win32CodeMask
        if (-not ($cause -is [System.IO.IOException]) -or $code -notin @($errorBrokenPipe, $errorNoData, $errorPipeNotConnected)) { throw }
    }
} finally {
    $pipe.Dispose()
}
$deadline = [System.Diagnostics.Stopwatch]::StartNew()
$rejection = $null
while ($deadline.ElapsedMilliseconds -lt $readTimeoutMs) {
    $content = Get-Content -LiteralPath $log -Raw
    if ($content.Length -lt $before) { throw 'Core log rotated during the probe; retry' }
    $rejection = $content.Substring($before) -split "`r?`n" | Where-Object {
        $_ -match 'rejected pipe client' -and $_ -match ('pid=' + $PID + '(\D|$)') -and $_ -match 'peer executable is not allowed'
    } | Select-Object -First 1
    if ($rejection) { break }
    Start-Sleep -Milliseconds $logPollMs
}
if (-not $rejection) { throw 'Missing rejection log with this PowerShell PID and executable reason in acceptance DataRoot' }
Write-Output ('PASS closed without reply; pid=' + $PID + '; pipe=\\.\pipe\' + $pipeName + '; reason=peer executable is not allowed')
# Emit only the rejection entry; do not copy capture titles into the acceptance record.
Write-Output $rejection
