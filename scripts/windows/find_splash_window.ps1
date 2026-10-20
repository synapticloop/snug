Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class W {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder buf, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder buf, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
}
"@

$targetPid = $args[0]
$found = $false

$cb = [W+EnumWindowsProc]{
    param($hwnd, $lparam)
    $pid_ = [UInt32]0
    [void][W]::GetWindowThreadProcessId($hwnd, [ref]$pid_)
    if ($pid_ -eq $targetPid) {
        $title = New-Object System.Text.StringBuilder 256
        [void][W]::GetWindowText($hwnd, $title, 256)
        $class = New-Object System.Text.StringBuilder 256
        [void][W]::GetClassName($hwnd, $class, 256)
        $visible = [W]::IsWindowVisible($hwnd)
        Write-Host ("  hwnd={0:X8} visible={1} class='{2}' title='{3}'" -f $hwnd.ToInt64(), $visible, $class.ToString(), $title.ToString())
        if ($class.ToString() -like '*SnugSplash*') { $script:found = $true }
    }
    return $true
}

Write-Host "Enumerating windows for PID $targetPid"
[void][W]::EnumWindows($cb, [IntPtr]::Zero)
if ($found) {
    Write-Host "FOUND splash window (SnugSplash class)"
    exit 0
} else {
    Write-Host "No splash window visible"
    exit 1
}
