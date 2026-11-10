# Saves a screenshot of the running Wercode window. Reference tooling, never shipped.
#   powershell -File replica\shot.ps1 W01 ["text to type first"]
param([string]$Name = "W01", [string]$Type = "")
$p = Get-Process wercode -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $p) { "wercode is not running"; exit 1 }
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public class Win {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int L, T, R, B; }
}
'@
[Win]::SetProcessDPIAware() | Out-Null
$h = $p.MainWindowHandle
[Win]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 600
if ($Type) { [System.Windows.Forms.SendKeys]::SendWait($Type); Start-Sleep -Milliseconds 1500 }
$r = New-Object Win+RECT
[Win]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $ht = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
$dir = Join-Path $PSScriptRoot "clone-screens"
New-Item -ItemType Directory -Force $dir | Out-Null
$out = Join-Path $dir "$Name.png"
$bmp.Save($out)
"saved $out (${w}x${ht}), RAM $([math]::Round($p.WorkingSet64/1MB)) MB"
