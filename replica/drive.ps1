# Drives the running Wercode window for checks. Reference tooling, never shipped.
#
# Unlike shot.ps1 this never touches the keyboard focus or other windows: input
# is posted to the Wercode window only, and the picture is taken from that
# window only (PrintWindow), so it works while Wercode is in the background.
#
#   powershell -File replica\drive.ps1 "click:343,89; text:echo hi; key:13; sleep:800; shot:C:\tmp\a.png"
#
# Steps (coordinates are client pixels):
#   click:x,y   rclick:x,y   move:x,y   drag:x1,y1,x2,y2   wheel:x,y,delta
#   text:...    key:<virtual key code>   sleep:<ms>   shot:<path.png>   size
param([string]$Steps = "size")
$p = Get-Process wercode -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "wercode is not running"; exit 1 }
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class Drive {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  public struct RECT { public int L, T, R, B; }
  public struct POINT { public int X, Y; }
  public static IntPtr Pos(int x, int y) { return (IntPtr)((y << 16) | (x & 0xFFFF)); }
}
'@
[Drive]::SetProcessDPIAware() | Out-Null
$h = $p.MainWindowHandle
$post = { param($msg, $w, $l) [Drive]::PostMessage($h, $msg, [IntPtr]$w, $l) | Out-Null }

foreach ($step in $Steps -split ';') {
  $step = $step.Trim()
  if (-not $step) { continue }
  $name, $arg = $step -split ':', 2
  $n = @(); if ($arg -and $name -ne 'text' -and $name -ne 'shot') { $n = $arg -split ',' | ForEach-Object { [int]$_ } }
  switch ($name) {
    'size' {
      $r = New-Object Drive+RECT; [Drive]::GetClientRect($h, [ref]$r) | Out-Null
      "client $($r.R)x$($r.B), RAM $([math]::Round($p.WorkingSet64/1MB)) MB"
    }
    'move' { & $post 0x200 0 ([Drive]::Pos($n[0], $n[1])) }
    'click' {
      & $post 0x200 0 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x201 1 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x202 0 ([Drive]::Pos($n[0], $n[1]))
    }
    'rclick' {
      & $post 0x200 0 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x204 2 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x205 0 ([Drive]::Pos($n[0], $n[1]))
    }
    'drag' {
      & $post 0x200 0 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x201 1 ([Drive]::Pos($n[0], $n[1]))
      for ($i = 1; $i -le 8; $i++) {
        $x = [int]($n[0] + ($n[2] - $n[0]) * $i / 8); $y = [int]($n[1] + ($n[3] - $n[1]) * $i / 8)
        Start-Sleep -Milliseconds 40; & $post 0x200 1 ([Drive]::Pos($x, $y))
      }
      Start-Sleep -Milliseconds 60; & $post 0x202 0 ([Drive]::Pos($n[2], $n[3]))
    }
    'wheel' {
      $pt = New-Object Drive+POINT; $pt.X = $n[0]; $pt.Y = $n[1]
      [Drive]::ClientToScreen($h, [ref]$pt) | Out-Null
      & $post 0x200 0 ([Drive]::Pos($n[0], $n[1])); Start-Sleep -Milliseconds 60
      & $post 0x20A ($n[2] -shl 16) ([Drive]::Pos($pt.X, $pt.Y))
    }
    'text' { foreach ($c in $arg.ToCharArray()) { & $post 0x102 ([int]$c) ([IntPtr]1); Start-Sleep -Milliseconds 15 } }
    'key' { & $post 0x100 $n[0] ([IntPtr]1); Start-Sleep -Milliseconds 30; & $post 0x101 $n[0] ([IntPtr]0xC0000001L) }
    'sleep' { Start-Sleep -Milliseconds $n[0] }
    'shot' {
      $r = New-Object Drive+RECT; [Drive]::GetClientRect($h, [ref]$r) | Out-Null
      $bmp = New-Object System.Drawing.Bitmap $r.R, $r.B
      $g = [System.Drawing.Graphics]::FromImage($bmp)
      $hdc = $g.GetHdc()
      # 3 = client area only + render full content (needed for GPU-drawn windows).
      $ok = [Drive]::PrintWindow($h, $hdc, 3)
      $g.ReleaseHdc($hdc); $g.Dispose()
      $bmp.Save($arg); $bmp.Dispose()
      "saved $arg ($($r.R)x$($r.B)) ok=$ok"
    }
    default { "unknown step: $step" }
  }
}
