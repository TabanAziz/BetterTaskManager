# Run only on a clean Windows desktop. Launches the actual release executable.
param([string] $Executable = (Join-Path $PSScriptRoot '..\target\release\BTM.exe'))
$ErrorActionPreference = 'Stop'
$Executable = (Resolve-Path -LiteralPath $Executable).Path

Add-Type @'
using System;
using System.Runtime.InteropServices;

public static class BtmCaptionProbe {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)]
    struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    struct TitleBar {
        public uint Size;
        public Rect Bounds;
        [MarshalAs(UnmanagedType.ByValArray, SizeConst = 6)]
        public uint[] State;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")]
    public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")]
    public static extern bool IsZoomed(IntPtr hwnd);
    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr hwnd, int command);
    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")]
    static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")]
    static extern bool GetTitleBarInfo(IntPtr hwnd, ref TitleBar title);
    [DllImport("user32.dll")]
    static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")]
    static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam,
        uint flags, uint milliseconds, out IntPtr result);

    public static void AssertNativeClose(IntPtr hwnd) {
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4)); // Per-monitor v2
        if (previous == IntPtr.Zero)
            throw new InvalidOperationException("Could not select per-monitor DPI awareness.");
        try { AssertNativeCloseCore(hwnd); }
        finally { SetThreadDpiAwarenessContext(previous); }
    }

    static void AssertNativeCloseCore(IntPtr hwnd) {
        var title = new TitleBar { Size = (uint)Marshal.SizeOf(typeof(TitleBar)), State = new uint[6] };
        if (!GetTitleBarInfo(hwnd, ref title) || (title.State[5] & 0x18001) != 0)
            throw new InvalidOperationException("Windows reports the close button hidden, off-screen or disabled.");

        var origin = new Point();
        if (!ClientToScreen(hwnd, ref origin))
            throw new InvalidOperationException("Could not locate the BTM client area.");
        var r = title.Bounds;
        if (r.Bottom > origin.Y || r.Bottom <= r.Top || r.Right <= r.Left)
            throw new InvalidOperationException("The native caption overlaps BTM's client paint area.");

        // Find the native close hit target within the trailing part of the title bar.
        var y = r.Top + (r.Bottom - r.Top) / 2;
        for (int x = r.Right - 2; x >= Math.Max(r.Left, r.Right - (r.Bottom - r.Top) * 5); x -= 3) {
            int pos = ((y & 0xffff) << 16) | (x & 0xffff);
            IntPtr hit;
            if (SendMessageTimeout(hwnd, 0x0084, IntPtr.Zero, new IntPtr(pos), 2, 1500, out hit) == IntPtr.Zero)
                throw new InvalidOperationException("BTM did not answer WM_NCHITTEST.");
            if (hit.ToInt64() == 20) return; // HTCLOSE
        }
        throw new InvalidOperationException("No native close hit target in the caption.");
    }
}
'@

function Wait-Until([scriptblock] $Condition, [string] $Failure) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 50
    } while ($clock.Elapsed.TotalSeconds -lt 15)
    throw $Failure
}

$class = 'BetterTaskManager.Window'
if ([BtmCaptionProbe]::FindWindow($class, 'Better Task Manager') -ne [IntPtr]::Zero) {
    throw 'Close the existing Better Task Manager before running this test.'
}
$process = Start-Process -FilePath $Executable -PassThru
try {
    Wait-Until {
        $window = [BtmCaptionProbe]::FindWindow($class, 'Better Task Manager')
        [uint32] $owner = 0
        [void] [BtmCaptionProbe]::GetWindowThreadProcessId($window, [ref] $owner)
        ($owner -eq $process.Id) -and [BtmCaptionProbe]::IsWindowVisible($window)
    } 'The test executable did not show its main window.'
    $window = [BtmCaptionProbe]::FindWindow($class, 'Better Task Manager')
    [void] [BtmCaptionProbe]::ShowWindowAsync($window, 9) # SW_RESTORE
    Wait-Until { -not [BtmCaptionProbe]::IsIconic($window) -and -not [BtmCaptionProbe]::IsZoomed($window) } 'Restore timed out.'
    [BtmCaptionProbe]::AssertNativeClose($window)
    [void] [BtmCaptionProbe]::ShowWindowAsync($window, 3) # SW_MAXIMIZE
    Wait-Until { [BtmCaptionProbe]::IsZoomed($window) } 'Maximize timed out.'
    [BtmCaptionProbe]::AssertNativeClose($window)
    [void] [BtmCaptionProbe]::ShowWindowAsync($window, 6) # SW_MINIMIZE
    Wait-Until { [BtmCaptionProbe]::IsIconic($window) } 'Minimize timed out.'
    [void] [BtmCaptionProbe]::ShowWindowAsync($window, 9)
    Wait-Until { -not [BtmCaptionProbe]::IsIconic($window) } 'Restore from minimize timed out.'
    [BtmCaptionProbe]::AssertNativeClose($window)
    # Issue the same system close command used by the window caption.
    if (-not [BtmCaptionProbe]::PostMessage($window, 0x0112, [IntPtr] 0xF060, [IntPtr]::Zero) -or
        -not $process.WaitForExit(10000) -or $process.ExitCode -ne 0) {
        throw 'The caption close command did not exit BTM successfully.'
    }
    'Native close button is available in restored and maximized states and after minimize/restore.'
}
finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    $process.Dispose()
}
