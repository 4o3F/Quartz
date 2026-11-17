# Windows release baseline: core idle, UI open, then 20 tray-driven open/close cycles.
# Private WS uses PROCESS_MEMORY_COUNTERS_EX2 (Windows 10/11 with September 2023 updates).
param(
    [Parameter(Mandatory)] [string] $Exe,
    [ValidateRange(1, 3600)] [int] $SettleSeconds = 30,
    [ValidateRange(1, 3600)] [int] $IdleSeconds = 600,
    [string] $ScreenshotPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$exePath = (Resolve-Path $Exe).Path
$workDir = Split-Path $exePath

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
public static class QuartzMeasure {
    public enum Message : uint { Close = 0x10, TrayCallback = 0x8001 }
    // shellapi.h: NIN_SELECT = WM_USER; NOTIFYICON_VERSION_4 uses LOWORD(lParam).
    public enum TrayEvent : int { Select = 0x400 }
    public enum WindowAttribute : uint { ExtendedFrameBounds = 9 }
    [StructLayout(LayoutKind.Sequential)]
    public struct Memory {
        public uint Size, PageFaultCount;
        public UIntPtr PeakWorkingSet, WorkingSet, PeakPagedPool, PagedPool;
        public UIntPtr PeakNonPagedPool, NonPagedPool, Pagefile, PeakPagefile;
        public UIntPtr PrivateBytes, PrivateWorkingSet;
        public ulong SharedCommit;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessBasicInformation {
        public IntPtr Reserved1, PebBaseAddress, Reserved2a, Reserved2b;
        public UIntPtr ProcessId, ParentProcessId;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("psapi.dll", SetLastError = true)]
    static extern bool GetProcessMemoryInfo(IntPtr process, ref Memory counters, uint size);
    [DllImport("ntdll.dll")]
    static extern int NtQueryInformationProcess(IntPtr process, int infoClass,
        ref ProcessBasicInformation info, int size, out int returned);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindow(string className, string title);
    public static IntPtr FindCore() { return FindWindow("Quartz.Core", null); }
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PostMessage(IntPtr window, Message message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("dwmapi.dll")]
    public static extern int DwmGetWindowAttribute(IntPtr window, WindowAttribute attribute,
        out Rect rect, int size);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    public static Memory ReadMemory(Process process) {
        var counters = new Memory();
        counters.Size = (uint)Marshal.SizeOf(counters);
        if (!GetProcessMemoryInfo(process.Handle, ref counters, counters.Size))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        return counters;
    }
    public static uint ParentId(Process process) {
        var info = new ProcessBasicInformation();
        int returned;
        int status = NtQueryInformationProcess(process.Handle, 0, ref info, Marshal.SizeOf(info), out returned);
        if (status != 0) throw new InvalidOperationException("NtQueryInformationProcess: " + status);
        return (uint)info.ParentProcessId.ToUInt64();
    }
}
'@

function Get-Snapshot([System.Diagnostics.Process] $Process, [string] $Scenario) {
    $Process.Refresh()
    $memory = [QuartzMeasure]::ReadMemory($Process)
    [pscustomobject]@{
        Scenario = $Scenario
        PrivateBytes = $memory.PrivateBytes.ToUInt64()
        PrivateWS = $memory.PrivateWorkingSet.ToUInt64()
        Threads = $Process.Threads.Count
        Handles = $Process.HandleCount
        CpuMs = $Process.TotalProcessorTime.TotalMilliseconds
        IdleMs = $null
    }
}

function Get-UiProcesses {
    @(Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($exePath)) -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $exePath -and $_.Id -ne $core.Id -and [QuartzMeasure]::ParentId($_) -eq $core.Id })
}

function Post-WindowMessage([IntPtr] $Window, [QuartzMeasure+Message] $Message, [IntPtr] $Data) {
    if (-not [QuartzMeasure]::PostMessage($Window, $Message, [UIntPtr]::Zero, $Data)) {
        throw "PostMessage failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
}

function Open-Ui {
    Post-WindowMessage $coreWindow ([QuartzMeasure+Message]::TrayCallback) ([IntPtr][int][QuartzMeasure+TrayEvent]::Select)
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        if ($core.HasExited) { throw 'Core exited while opening UI' }
        $children = @(Get-UiProcesses)
        if ($children.Count -gt 1) { throw "Expected one UI child, found $($children.Count)" }
        if ($children.Count -eq 1) {
            $child = $children[0]
            $child.Refresh()
            if ($child.MainWindowHandle -ne [IntPtr]::Zero) { return $child }
            if ($child.HasExited) { throw "UI exited before opening a window: $($child.ExitCode)" }
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Timed out waiting for UI window; inspect Quartz/logs/ui.log'
}

function Close-Ui([System.Diagnostics.Process] $Process) {
    $Process.Refresh()
    $timer = [Diagnostics.Stopwatch]::StartNew()
    Post-WindowMessage $Process.MainWindowHandle ([QuartzMeasure+Message]::Close) ([IntPtr]::Zero)
    if (-not $Process.WaitForExit(1000)) { throw "UI PID $($Process.Id) did not exit within one second of WM_CLOSE" }
    $timer.Stop()
    if ($Process.ExitCode -ne 0) { throw "UI exit code $($Process.ExitCode)" }
    $script:maxCloseMs = [Math]::Max($script:maxCloseMs, $timer.Elapsed.TotalMilliseconds)
    # Allow the core's process-handle notification to restore its launch state.
    Start-Sleep -Milliseconds 150
    if (@(Get-UiProcesses).Count -ne 0) { throw 'UI children remain after WM_CLOSE' }
}

function Save-Window([System.Diagnostics.Process] $Process, [string] $Path) {
    Add-Type -AssemblyName System.Drawing
    # PrintWindow renders physical pixels; use matching coordinates on scaled displays.
    $previousDpi = [QuartzMeasure]::SetThreadDpiAwarenessContext([IntPtr](-4))
    if ($previousDpi -eq [IntPtr]::Zero) { throw 'SetThreadDpiAwarenessContext failed' }
    $rect = New-Object QuartzMeasure+Rect
    $visible = New-Object QuartzMeasure+Rect
    try {
        if (-not [QuartzMeasure]::GetWindowRect($Process.MainWindowHandle, [ref]$rect)) { throw 'GetWindowRect failed' }
        $result = [QuartzMeasure]::DwmGetWindowAttribute($Process.MainWindowHandle,
            [QuartzMeasure+WindowAttribute]::ExtendedFrameBounds, [ref]$visible,
            [Runtime.InteropServices.Marshal]::SizeOf($visible))
        if ($result -ne 0) { throw "DwmGetWindowAttribute failed: $result" }
    } finally {
        [QuartzMeasure]::SetThreadDpiAwarenessContext($previousDpi) | Out-Null
    }
    $bitmap = [Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $dc = $graphics.GetHdc()
        try {
            if (-not [QuartzMeasure]::PrintWindow($Process.MainWindowHandle, $dc, 2)) { throw 'PrintWindow failed' }
        } finally { $graphics.ReleaseHdc($dc) }
        $fullPath = [IO.Path]::GetFullPath($Path)
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($fullPath)) | Out-Null
        $bounds = [Drawing.Rectangle]::new($visible.Left - $rect.Left, $visible.Top - $rect.Top,
            $visible.Right - $visible.Left, $visible.Bottom - $visible.Top)
        $capture = $bitmap.Clone($bounds, $bitmap.PixelFormat)
        try { $capture.Save($fullPath, [Drawing.Imaging.ImageFormat]::Png) }
        finally { $capture.Dispose() }
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

if ([QuartzMeasure]::FindCore() -ne [IntPtr]::Zero) {
    throw 'A Quartz core is already running; close it before measuring'
}
if (@(Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($exePath)) -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $exePath }).Count -ne 0) {
    throw 'The executable is already running; close it before measuring'
}

# Machine metadata is outside resource sampling. No WMI performance counters are used.
$cpu = (Get-CimInstance Win32_Processor | Select-Object -ExpandProperty Name) -join ', '
$gpu = (Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name) -join ', '
$os = Get-CimInstance Win32_OperatingSystem
$script:maxCloseMs = 0.0
$core = Start-Process -FilePath $exePath -WorkingDirectory $workDir -PassThru
try {
    Start-Sleep -Seconds $SettleSeconds
    $coreWindow = [QuartzMeasure]::FindCore()
    if ($coreWindow -eq [IntPtr]::Zero) { throw 'Core hidden window was not created' }
    [uint32]$windowProcessId = 0
    [QuartzMeasure]::GetWindowThreadProcessId($coreWindow, [ref]$windowProcessId) | Out-Null
    if ($windowProcessId -ne $core.Id) { throw 'Hidden window belongs to a different core' }
    $start = Get-Snapshot $core 'core idle start'
    Start-Sleep -Seconds $IdleSeconds
    $before = Get-Snapshot $core 'core idle'
    $before.IdleMs = $before.CpuMs - $start.CpuMs
    $modulesBefore = @($core.Modules | ForEach-Object ModuleName | Sort-Object -Unique)

    $ui = Open-Ui
    Start-Sleep -Seconds $SettleSeconds
    $uiStart = Get-Snapshot $ui 'UI idle start'
    $uiIdleSeconds = [Math]::Min(30, $IdleSeconds)
    Start-Sleep -Seconds $uiIdleSeconds
    $uiOpen = Get-Snapshot $ui 'UI open'
    $uiOpen.IdleMs = $uiOpen.CpuMs - $uiStart.CpuMs
    $uiModules = @($ui.Modules | ForEach-Object ModuleName | Sort-Object -Unique)
    if ($ScreenshotPath) { Save-Window $ui $ScreenshotPath }
    Close-Ui $ui
    $cycleBefore = Get-Snapshot $core 'core before 20 cycles (first UI closed)'

    $cycleRows = @()
    for ($cycle = 1; $cycle -le 20; $cycle++) {
        $ui = Open-Ui
        # Hold the visible window long enough to render; never trim its working set.
        Start-Sleep -Milliseconds 500
        Close-Ui $ui
        if ($cycle % 5 -eq 0) { $cycleRows += Get-Snapshot $core "core after $cycle cycles" }
    }
    Start-Sleep -Seconds $SettleSeconds
    $afterStart = Get-Snapshot $core 'core after cycles idle start'
    Start-Sleep -Seconds $uiIdleSeconds
    $after = Get-Snapshot $core 'core after 20 cycles, settled'
    $after.IdleMs = $after.CpuMs - $afterStart.CpuMs
    $modulesAfter = @($core.Modules | ForEach-Object ModuleName | Sort-Object -Unique)
    $coreModules = @($modulesBefore + $modulesAfter | Sort-Object -Unique)
    $nvModules = @($coreModules | Where-Object { $_ -like 'nv*.dll' })
    $d3dModules = @($coreModules | Where-Object { $_ -like 'd3d*.dll' })
    $childrenLeft = @(Get-UiProcesses).Count
    $privateDelta = ($after.PrivateBytes - $cycleBefore.PrivateBytes) / 1MB
    $wsDelta = ($after.PrivateWS - $cycleBefore.PrivateWS) / 1MB
    $threadDelta = $after.Threads - $cycleBefore.Threads
    $handleDelta = $after.Handles - $cycleBefore.Handles

    "Date: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz')"
    "CPU: $cpu; GPU: $gpu; OS: $($os.Caption), build $($os.BuildNumber)"
    "Settle: $SettleSeconds s; core idle: $IdleSeconds s; UI/post-cycle idle: $uiIdleSeconds s"
    '| Scenario | Private Bytes (MiB) | Private WS (MiB) | Threads | Handles | Total CPU (ms) | Idle CPU (ms) |'
    '|---|---:|---:|---:|---:|---:|---:|'
    foreach ($row in @($before, $uiOpen, $cycleBefore) + $cycleRows + @($after)) {
        $idle = if ($null -eq $row.IdleMs) { '-' } else { '{0:F3}' -f $row.IdleMs }
        '| {0} | {1:F3} | {2:F3} | {3} | {4} | {5:F3} | {6} |' -f
            $row.Scenario, ($row.PrivateBytes / 1MB), ($row.PrivateWS / 1MB), $row.Threads, $row.Handles, $row.CpuMs, $idle
    }
    "Core 20-cycle deltas (from first UI closed): Private Bytes $([math]::Round($privateDelta, 3)) MiB; Private WS $([math]::Round($wsDelta, 3)) MiB; threads $threadDelta; handles $handleDelta."
    "Core first-UI deltas (from initial idle): Private Bytes $([math]::Round(($cycleBefore.PrivateBytes - $before.PrivateBytes) / 1MB, 3)) MiB; Private WS $([math]::Round(($cycleBefore.PrivateWS - $before.PrivateWS) / 1MB, 3)) MiB; threads $($cycleBefore.Threads - $before.Threads); handles $($cycleBefore.Handles - $before.Handles)."
    "UI children remaining: $childrenLeft; maximum WM_CLOSE-to-exit: $([math]::Round($maxCloseMs, 3)) ms."
    "Core nv*.dll check: $(if ($nvModules.Count) { 'FAIL: ' + ($nvModules -join ', ') } else { 'PASS (none)' })."
    "Core d3d*.dll check: $(if ($d3dModules.Count) { 'PRESENT (static-import observation): ' + ($d3dModules -join ', ') } else { 'PASS (none)' })."
    "Core Private Bytes growth: $(if ($privateDelta -le 1) { 'PASS (target <= 1 MiB)' } elseif ($privateDelta -le 2) { 'above target, within 2 MiB limit' } else { 'FAIL (> 2 MiB)' })."
    "Core threads/handles growth: $(if ($threadDelta -le 0 -and $handleDelta -le 0) { 'PASS (no growth)' } else { 'FAIL' })."
    "Core loaded modules before UI: ``$($modulesBefore -join '`, `')``."
    "Core loaded modules after cycles: ``$($modulesAfter -join '`, `')``."
    "UI loaded modules: ``$($uiModules -join '`, `')``."
    $hostExe = Join-Path $workDir 'quartz-host.exe'
    "quartz.exe: $((Get-Item $exePath).Length) bytes."
    if (Test-Path $hostExe) { "quartz-host.exe: $((Get-Item $hostExe).Length) bytes." }
    if ($nvModules.Count -or $childrenLeft -or $privateDelta -gt 2 -or $threadDelta -gt 0 -or $handleDelta -gt 0) {
        throw 'Resource acceptance failed; see the measurements above'
    }
} finally {
    foreach ($child in @(Get-UiProcesses)) { Stop-Process -Id $child.Id -Force -ErrorAction SilentlyContinue }
    if (-not $core.HasExited) {
        $window = [QuartzMeasure]::FindCore()
        if ($window -ne [IntPtr]::Zero) {
            Post-WindowMessage $window ([QuartzMeasure+Message]::Close) ([IntPtr]::Zero)
        }
        if (-not $core.WaitForExit(5000)) { Stop-Process -Id $core.Id -Force }
    }
}
