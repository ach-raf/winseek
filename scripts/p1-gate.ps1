#Requires -RunAsAdministrator
<#
.SYNOPSIS
  P1 gate: deterministic Tier 0 tests on a throwaway 2 GB VHDX (trap files, USN storm, journal wrap).
.PARAMETER NoBuild   skip `cargo build`
.PARAMETER Keep      do not detach/delete the VHDX at the end
.PARAMETER Everything  also diff `es.exe -path <L>:\` against the index listing (needs es.exe on PATH)
.NOTES
  PowerShell 5.1 compatible. Output is also written to docs/bench/p1-gate-<date>.txt.
  Run from an elevated prompt:  powershell -ExecutionPolicy Bypass -File scripts\p1-gate.ps1
#>
param([switch]$NoBuild, [switch]$Keep, [switch]$Everything)

$ErrorActionPreference = 'Continue'
$Root = Split-Path -Parent $PSScriptRoot
$BenchDir = Join-Path $Root 'docs\bench'
New-Item -ItemType Directory -Force -Path $BenchDir | Out-Null
$LogFile = Join-Path $BenchDir ("p1-gate-{0}.txt" -f (Get-Date -Format 'yyyy-MM-dd'))
Set-Content -Path $LogFile -Value ("p1-gate started {0}" -f (Get-Date -Format 's')) -Encoding UTF8
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}

function Log([string]$m) {
    Write-Host $m
    Add-Content -Path $LogFile -Value $m -Encoding UTF8
}

$Results = New-Object System.Collections.ArrayList
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    [void]$Results.Add([pscustomobject]@{ Result = $(if ($ok) { 'PASS' } else { 'FAIL' }); Check = $name; Detail = $detail })
    Log ("  [{0}] {1} {2}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name, $detail)
}

$Searchd = Join-Path $Root 'target\release\searchd.exe'
$QueryExe = Join-Path $Root 'target\release\examples\query.exe'
$ServiceName = 'winseek-searchd'

$Work = Join-Path $env:TEMP ("p1gate-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 8)))
$DataDir = Join-Path $Work 'data'
$SearchdLog = Join-Path $Work 'searchd.log'
$VhdxPath = Join-Path $Work 'p1gate.vhdx'
New-Item -ItemType Directory -Force -Path $Work, $DataDir | Out-Null

$script:L = $null           # drive letter
$script:Proc = $null        # searchd process
$script:ServiceWasRunning = $false
$script:Mounted = $false
$script:UsedDiskpart = $false

# ---------------------------------------------------------------- helpers
function Q {   # run query.exe, return output lines
    $o = & $QueryExe @args 2>$null
    return @($o | ForEach-Object { "$_" })
}
function Q-Count([string]$q) {
    $o = Q '--count' $q
    foreach ($l in $o) { if ($l -match '^COUNT (\d+)$') { return [int]$Matches[1] } }
    return -1
}
function Q-List([string]$q) {
    $pre = "$($script:L):\"
    return @(Q '--list' $q | Where-Object { $_.StartsWith($pre, [StringComparison]::OrdinalIgnoreCase) })
}
function Q-State {
    foreach ($l in (Q '--status')) {
        if ($l -match "^STATUS $($script:L) (\d+) (\d+) (.*)$") { return $Matches[3] }
    }
    return $null
}
function Wait-Live([int]$timeoutS = 180) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $timeoutS) {
        if ($script:Proc.HasExited) { return $false }
        if ((Q-State) -eq 'Live') { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Skip-Path([string]$p) {   # system dirs / NTFS metafiles at the volume root
    $rel = $p.Substring(3)
    if ($rel.Length -eq 0) { return $true }
    $first = $rel.Split('\')[0]
    return ($first.StartsWith('$') -or $first -ieq 'System Volume Information')
}

function Get-FsList([string]$letter) {   # recursive walk with \\?\ so long paths work; dirs end in '\'
    $res = New-Object 'System.Collections.Generic.List[string]'
    $stack = New-Object 'System.Collections.Generic.Stack[string]'
    $stack.Push("${letter}:\")
    while ($stack.Count -gt 0) {
        $d = $stack.Pop()
        $long = '\\?\' + $d
        try {
            foreach ($e in [IO.Directory]::EnumerateDirectories($long)) {
                $p = $e.Substring(4)
                if (Skip-Path $p) { continue }
                $res.Add($p + '\')
                $stack.Push($p + '\')
            }
            foreach ($e in [IO.Directory]::EnumerateFiles($long)) {
                $p = $e.Substring(4)
                if (Skip-Path $p) { continue }
                $res.Add($p)
            }
        } catch { }
    }
    return $res
}
function Get-IndexList {
    $l = Q-List ("$($script:L):\")
    $res = New-Object 'System.Collections.Generic.List[string]'
    foreach ($p in $l) { if (-not (Skip-Path $p)) { $res.Add($p) } }
    return $res
}
function Diff-Sets($expected, $actual) {   # returns @{Missing=..;Extra=..}
    $e = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($x in $expected) { [void]$e.Add($x) }
    $a = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($x in $actual) { [void]$a.Add($x) }
    $missing = New-Object 'System.Collections.Generic.HashSet[string]' ($e, [StringComparer]::OrdinalIgnoreCase)
    $missing.ExceptWith($a)
    $extra = New-Object 'System.Collections.Generic.HashSet[string]' ($a, [StringComparer]::OrdinalIgnoreCase)
    $extra.ExceptWith($e)
    return @{ Missing = @($missing); Extra = @($extra); NExp = $e.Count; NAct = $a.Count }
}
function Report-Diff([string]$name, $d) {
    $ok = ($d.Missing.Count -eq 0 -and $d.Extra.Count -eq 0)
    Check $name $ok ("expected={0} index={1} missing={2} extra={3}" -f $d.NExp, $d.NAct, $d.Missing.Count, $d.Extra.Count)
    if (-not $ok) {
        $d.Missing | Select-Object -First 8 | ForEach-Object { Log "      missing: $_" }
        $d.Extra | Select-Object -First 8 | ForEach-Object { Log "      extra:   $_" }
    }
}

function Free-Letter {
    $used = [IO.DriveInfo]::GetDrives() | ForEach-Object { $_.Name.Substring(0, 1).ToUpper() }
    foreach ($c in [char[]](90..68)) { if ($used -notcontains [string]$c) { return [string]$c } }
    throw 'no free drive letter'
}

# ---------------------------------------------------------------- VHDX
function New-TestVolume {
    $script:VhdxPath = $VhdxPath
    if (Get-Command New-VHD -ErrorAction SilentlyContinue) {
        Log 'creating VHDX with Hyper-V cmdlets'
        New-VHD -Path $VhdxPath -SizeBytes 2GB -Dynamic | Out-Null
        $disk = Mount-VHD -Path $VhdxPath -Passthru | Get-Disk
        $script:Mounted = $true
        $disk | Initialize-Disk -PartitionStyle GPT -PassThru | Out-Null
        # Format before assigning a letter, else Explorer pops "You need to format the disk".
        $part = New-Partition -DiskNumber $disk.Number -UseMaximumSize
        Format-Volume -Partition $part -FileSystem NTFS -NewFileSystemLabel 'p1gate' -Confirm:$false | Out-Null
        $letter = Free-Letter
        $part | Set-Partition -NewDriveLetter $letter
        $script:L = $letter
    } else {
        Log 'New-VHD unavailable; creating VHDX with diskpart'
        $script:UsedDiskpart = $true
        $letter = Free-Letter
        $dp = Join-Path $Work 'create.dp'
        @(
            "create vdisk file=`"$VhdxPath`" maximum=2048 type=expandable",
            "select vdisk file=`"$VhdxPath`"",
            'attach vdisk',
            'create partition primary',
            'format fs=ntfs label=p1gate quick',
            "assign letter=$letter"
        ) | Set-Content -Path $dp -Encoding ASCII
        $out = & diskpart /s $dp 2>&1 | Out-String
        Log $out
        $script:Mounted = $true
        $script:L = $letter
    }
    for ($i = 0; $i -lt 40 -and -not (Test-Path "$($script:L):\"); $i++) { Start-Sleep -Milliseconds 500 }
    if (-not (Test-Path "$($script:L):\")) { throw "volume $($script:L): did not appear" }
    Log "test volume: $($script:L):"
    # make sure a USN journal exists (searchd needs one)
    & fsutil usn createjournal m=1048576 a=65536 "$($script:L):" | Out-Null
}
function Remove-TestVolume {
    if (-not $script:Mounted) { return }
    if ($script:UsedDiskpart) {
        $dp = Join-Path $Work 'detach.dp'
        @("select vdisk file=`"$VhdxPath`"", 'detach vdisk') | Set-Content -Path $dp -Encoding ASCII
        & diskpart /s $dp 2>&1 | Out-Null
    } else {
        Dismount-VHD -Path $VhdxPath -ErrorAction SilentlyContinue
    }
    $script:Mounted = $false
}

# ---------------------------------------------------------------- searchd control
$CtrlCScript = @'
param([uint32]$ProcId)
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class CtrlC {
  [DllImport("kernel32.dll")] static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll")] static extern bool FreeConsole();
  [DllImport("kernel32.dll")] static extern bool SetConsoleCtrlHandler(IntPtr h, bool add);
  [DllImport("kernel32.dll")] static extern bool GenerateConsoleCtrlEvent(uint ev, uint grp);
  public static bool Send(uint pid) {
    FreeConsole();
    if (!AttachConsole(pid)) return false;
    SetConsoleCtrlHandler(IntPtr.Zero, true);
    bool ok = GenerateConsoleCtrlEvent(0, 0);
    System.Threading.Thread.Sleep(300);
    FreeConsole();
    return ok;
  }
}
"@
[CtrlC]::Send($ProcId) | Out-Null
'@

function Start-Searchd {
    $argl = "--console --volumes $($script:L) --data `"$DataDir`" --log `"$SearchdLog`""
    $script:Proc = Start-Process -FilePath $Searchd -ArgumentList $argl -PassThru -WindowStyle Hidden
    Log "searchd started pid=$($script:Proc.Id)"
}
function Stop-Searchd {   # graceful Ctrl+C (so the snapshot is saved); kill as a fallback
    if ($null -eq $script:Proc -or $script:Proc.HasExited) { return $true }
    $helper = Join-Path $Work 'ctrlc.ps1'
    Set-Content -Path $helper -Value $CtrlCScript -Encoding ASCII
    Start-Process -FilePath 'powershell.exe' -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$helper`" -ProcId $($script:Proc.Id)" -WindowStyle Hidden -Wait | Out-Null
    $graceful = $script:Proc.WaitForExit(20000)
    if (-not $graceful) { Stop-Process -Id $script:Proc.Id -Force -ErrorAction SilentlyContinue; Log 'WARNING: searchd did not exit on Ctrl+C; killed (no snapshot)' }
    return $graceful
}

# ---------------------------------------------------------------- trap files
function Make-Traps {
    $L = $script:L
    $P = { param($rel) "\\?\${L}:\$rel" }
    function Touch($rel, $text = 'x') { [IO.File]::WriteAllText((& $P $rel), $text) }
    function Mkdir2($rel) { [void][IO.Directory]::CreateDirectory((& $P $rel)) }

    $script:Traps = @{}
    Mkdir2 'traps'
    $e = [string][char]0x00E9
    $cjk = -join ([char]0x6587, [char]0x6863, '_', [char]0x6D4B, [char]0x8BD5)
    $emoji = 'party_' + [char]::ConvertFromUtf32(0x1F389)
    $rtl = -join ([char]0x05E9, [char]0x05DC, [char]0x05D5, [char]0x05DD)
    $arab = -join ([char]0x0645, [char]0x0631, [char]0x062D, [char]0x0628, [char]0x0627)
    $uni = @{ 'e-acute' = "caf${e}_trap.txt"; 'cjk' = "$cjk.txt"; 'emoji' = "$emoji.txt"; 'hebrew-rtl' = "${rtl}_trap.txt"; 'arabic-rtl' = "${arab}_trap.txt" }
    Mkdir2 'traps\unicode'
    foreach ($k in $uni.Keys) { Touch "traps\unicode\$($uni[$k])"; $script:Traps["unicode $k"] = @{ Query = $uni[$k]; Path = "${L}:\traps\unicode\$($uni[$k])" } }

    Mkdir2 'traps\odd'
    $odd = @{ 'spaces' = 'name  with   spaces trap.txt'; 'dots' = 'a.b.c.trap.tar.gz'; 'leading-dot' = '.dotfile_trap'; 'many-dots' = 'x...y_trap.txt' }
    foreach ($k in $odd.Keys) { Touch "traps\odd\$($odd[$k])"; $script:Traps["odd $k"] = @{ Query = $(if ($odd[$k] -match ' ') { 'spaces' } else { $odd[$k] }); Path = "${L}:\traps\odd\$($odd[$k])" } }

    # long path (> 260 chars) via \\?\
    $seg = 'd' * 50
    $rel = 'traps\long'
    1..6 | ForEach-Object { $rel += "\$seg$_" }
    Mkdir2 $rel
    Touch "$rel\longleaf_unique.txt"
    $longPath = "${L}:\$rel\longleaf_unique.txt"
    $script:Traps['long path'] = @{ Query = 'longleaf_unique'; Path = $longPath }
    $script:LongLen = $longPath.Length

    # duplicates in different folders
    foreach ($d in 'dupA', 'dupB', 'dupC') { Mkdir2 "traps\$d"; Touch "traps\$d\dupname_trap.txt" }

    # hardlinks
    Mkdir2 'traps\hl1'; Mkdir2 'traps\hl2'
    Touch 'traps\hl1\hl_orig_trap.txt' 'hardlink data'
    New-Item -ItemType HardLink -Path "${L}:\traps\hl2\hl_link_trap.txt" -Target "${L}:\traps\hl1\hl_orig_trap.txt" | Out-Null

    # alternate data stream
    Mkdir2 'traps\ads'
    Touch 'traps\ads\file_ads_trap.txt' 'main'
    Set-Content -Path "${L}:\traps\ads\file_ads_trap.txt" -Stream 'adsstream_hidden' -Value 'stream data'

    # ~20k plain files, nested: 20 x 10 x 100
    $n = 0
    for ($a = 0; $a -lt 20; $a++) {
        for ($b = 0; $b -lt 10; $b++) {
            $dir = "plain\pd{0:00}\pd{1:00}" -f $a, $b
            Mkdir2 $dir
            for ($c = 0; $c -lt 100; $c++) {
                [IO.File]::Create((& $P ("$dir\plain_{0:00000}.dat" -f $n))).Dispose()
                $n++
            }
        }
    }
    $script:PlainCount = $n
    Log "created trap set ($n plain files)"
}

# ---------------------------------------------------------------- main
$exitCode = 1
try {
    Log "== p1-gate $(Get-Date -Format s) on $env:COMPUTERNAME =="

    # 1. build
    if (-not $NoBuild) {
        Log 'building (release)...'
        Push-Location $Root
        $out = & cmd /c 'cargo build --release -p searchd --bins --examples 2>&1'
        $rc = $LASTEXITCODE
        Pop-Location
        $out | Select-Object -Last 8 | ForEach-Object { Log "  $_" }
        Check 'build' ($rc -eq 0) "cargo exit $rc"
        if ($rc -ne 0) { throw 'build failed' }
    }
    if (-not (Test-Path $Searchd) -or -not (Test-Path $QueryExe)) { throw "missing $Searchd or $QueryExe (build first)" }

    # real service owns the pipe: stop it, restart at the end
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -eq 'Running') {
        Log "stopping service $ServiceName (restarted at the end)"
        $script:ServiceWasRunning = $true
        Stop-Service -Name $ServiceName -Force
        $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    if (Get-Process -Name searchd -ErrorAction SilentlyContinue) { throw 'another searchd.exe is running; stop it first' }

    # 2. volume
    New-TestVolume
    $L = $script:L

    # 3. traps
    Make-Traps

    # 4. start searchd on the VHDX, wait until Live
    Start-Searchd
    $live = Wait-Live 180
    Check 'searchd reaches Live' $live "volume ${L}: state=$(Q-State)"
    if (-not $live) { throw 'searchd not live' }

    # 5. trap assertions
    Log '-- trap files'
    foreach ($k in ($script:Traps.Keys | Sort-Object)) {
        $t = $script:Traps[$k]
        $hits = Q-List $t.Query
        $found = @($hits | Where-Object { $_ -ieq $t.Path }).Count -eq 1
        Check "found: $k" $found "query=$($t.Query) hits=$($hits.Count)"
    }
    Check 'long path really > 260 chars' ($script:LongLen -gt 260) "len=$($script:LongLen)"
    Check 'duplicates: 3 folders' ((Q-Count 'dupname_trap') -eq 3) "count=$(Q-Count 'dupname_trap')"
    $o = Q-List 'hl_orig_trap'; $lk = Q-List 'hl_link_trap'
    Check 'hardlink: original name' ($o.Count -eq 1 -and $o[0] -ieq "${L}:\traps\hl1\hl_orig_trap.txt") "hits=$($o.Count)"
    Check 'hardlink: second name' ($lk.Count -eq 1 -and $lk[0] -ieq "${L}:\traps\hl2\hl_link_trap.txt") "hits=$($lk.Count)"
    Check 'ADS: main file once' ((Q-Count 'file_ads_trap') -eq 1) "count=$(Q-Count 'file_ads_trap')"
    Check 'ADS: stream name not indexed' ((Q-Count 'adsstream_hidden') -eq 0) "count=$(Q-Count 'adsstream_hidden')"
    $pc = Q-Count 'plain_'
    Check 'plain prefix count == files created' ($pc -eq $script:PlainCount) "index=$pc created=$($script:PlainCount)"
    Report-Diff 'full listing == filesystem (after setup)' (Diff-Sets (Get-FsList $L) (Get-IndexList))

    # 6. USN storm
    Log '-- USN storm'
    $P = { param($rel) "\\?\${L}:\$rel" }
    [void][IO.Directory]::CreateDirectory((& $P 'storm\src'))
    [void][IO.Directory]::CreateDirectory((& $P 'storm\src\sub'))
    [void][IO.Directory]::CreateDirectory((& $P 'storm\dst'))
    for ($i = 0; $i -lt 2000; $i++) { [IO.File]::Create((& $P ("storm\src\storm_{0:0000}.tmp" -f $i))).Dispose() }
    for ($i = 0; $i -lt 10; $i++) { [IO.File]::Create((& $P ("storm\src\sub\sub_{0:000}.tmp" -f $i))).Dispose() }
    for ($i = 0; $i -lt 500; $i++) { [IO.File]::Move((& $P ("storm\src\storm_{0:0000}.tmp" -f $i)), (& $P ("storm\src\ren_{0:0000}.tmp" -f $i))) }
    for ($i = 500; $i -lt 800; $i++) { [IO.File]::Move((& $P ("storm\src\storm_{0:0000}.tmp" -f $i)), (& $P ("storm\dst\storm_{0:0000}.tmp" -f $i))) }
    for ($i = 800; $i -lt 1200; $i++) { [IO.File]::Delete((& $P ("storm\src\storm_{0:0000}.tmp" -f $i))) }
    [IO.Directory]::Move((& $P 'storm\src'), (& $P 'storm\src_renamed'))
    $stormT = [Diagnostics.Stopwatch]::StartNew()
    $expected = Get-FsList $L
    $d = $null
    do {
        $d = Diff-Sets $expected (Get-IndexList)
        if ($d.Missing.Count -eq 0 -and $d.Extra.Count -eq 0) { break }
        Start-Sleep -Milliseconds 250
    } while ($stormT.Elapsed.TotalSeconds -lt 5)
    Log ("  storm converged after {0:N1}s (walk included)" -f $stormT.Elapsed.TotalSeconds)
    Report-Diff 'storm: index == filesystem within 5 s' $d
    Check 'storm: renamed files (ren_)' ((Q-Count 'ren_') -eq 500) "count=$(Q-Count 'ren_') want 500"
    Check 'storm: moved files under storm\dst\' ((Q-Count 'storm\dst\') -eq 300) "count=$(Q-Count 'storm\dst\') want 300"
    Check 'storm: remaining storm_ names' ((Q-Count 'storm_') -eq 1100) "count=$(Q-Count 'storm_') want 1100"
    Check 'storm: renamed folder carries children' ((Q-Count 'storm\src_renamed\') -eq 1311) "count=$(Q-Count 'storm\src_renamed\') want 1311"
    Check 'storm: old folder path gone' ((Q-Count 'storm\src\') -eq 0) "count=$(Q-Count 'storm\src\')"

    # 7. journal wrap
    Log '-- journal wrap'
    $graceful = Stop-Searchd
    Check 'searchd stopped gracefully (snapshot saved)' $graceful
    $snap = Join-Path $DataDir 'index.wsix'
    Check 'snapshot file exists' (Test-Path $snap)
    # changes while searchd is down, then destroy the journal
    for ($i = 0; $i -lt 50; $i++) { [IO.File]::Create((& $P ("storm\dst\downtime_{0:000}.tmp" -f $i))).Dispose() }
    for ($i = 500; $i -lt 510; $i++) { [IO.File]::Delete((& $P ("storm\dst\storm_{0:0000}.tmp" -f $i))) }
    & fsutil usn deletejournal /d "${L}:" | Out-Null
    & fsutil usn createjournal m=1048576 a=65536 "${L}:" | Out-Null
    $logLinesBefore = @(Get-Content -Path $SearchdLog -ErrorAction SilentlyContinue).Count
    Start-Searchd
    $live = Wait-Live 180
    Check 'searchd live after wrap' $live "state=$(Q-State)"
    $newLog = @(Get-Content -Path $SearchdLog -ErrorAction SilentlyContinue | Select-Object -Skip $logLinesBefore)
    $rescanLine = $newLog | Where-Object { $_ -match "journal id changed; full scan|outside journal|USN journal reset; rescanning" } | Select-Object -First 1
    Check 'log shows full rescan' ($null -ne $rescanLine) "$rescanLine"
    Check 'log shows scan completed' (@($newLog | Where-Object { $_ -match "${L}: scanned \d+ entries" }).Count -ge 1)
    Check 'wrap: downtime files found' ((Q-Count 'downtime_') -eq 50) "count=$(Q-Count 'downtime_') want 50"
    Check 'wrap: storm_ names after downtime deletes' ((Q-Count 'storm_') -eq 1090) "count=$(Q-Count 'storm_') want 1090"
    Check 'wrap: plain prefix count' ((Q-Count 'plain_') -eq $script:PlainCount) "count=$(Q-Count 'plain_')"
    Report-Diff 'wrap: full listing == filesystem' (Diff-Sets (Get-FsList $L) (Get-IndexList))

    # optional: Everything differential on the quiesced volume
    if ($Everything) {
        Log '-- Everything differential'
        $es = Get-Command es.exe -ErrorAction SilentlyContinue
        if (-not $es) { Log 'skipped: es.exe not found' } else {
            Start-Sleep -Seconds 3
            $esOut = @(& $es.Source -path "${L}:\" 2>$null | ForEach-Object { "$_" })
            $esList = New-Object 'System.Collections.Generic.List[string]'
            foreach ($p in $esOut) {
                if ($p.Length -lt 4 -or -not $p.StartsWith("${L}:\", [StringComparison]::OrdinalIgnoreCase)) { continue }
                $q = $p
                try { if ([IO.Directory]::Exists('\\?\' + $p.TrimEnd('\'))) { $q = $p.TrimEnd('\') + '\' } } catch {}
                if (-not (Skip-Path $q)) { $esList.Add($q) }
            }
            Report-Diff 'Everything (es.exe) vs index listing' (Diff-Sets $esList (Get-IndexList))
        }
    }
}
catch {
    Check 'script error' $false "$($_.Exception.Message)"
    Log ($_ | Out-String)
}
finally {
    Log '-- cleanup'
    try { Stop-Searchd | Out-Null } catch {}
    Get-Process -Name searchd -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Searchd } | Stop-Process -Force -ErrorAction SilentlyContinue
    if (-not $Keep) {
        Remove-TestVolume
        if (Test-Path $VhdxPath) { Remove-Item -Force $VhdxPath -ErrorAction SilentlyContinue }
        # keep searchd.log next to the bench file for post-mortem
        if (Test-Path $SearchdLog) { Copy-Item $SearchdLog (Join-Path $BenchDir ("p1-gate-{0}-searchd.log" -f (Get-Date -Format 'yyyy-MM-dd'))) -Force -ErrorAction SilentlyContinue }
        Remove-Item -Recurse -Force $Work -ErrorAction SilentlyContinue
    } else {
        Log "kept: volume $($script:L): vhdx=$VhdxPath work=$Work"
    }
    if ($script:ServiceWasRunning) {
        Log "restarting service $ServiceName"
        Start-Service -Name $ServiceName -ErrorAction SilentlyContinue
    }
}

Log ''
Log '== summary =='
$table = ($Results | Format-Table -AutoSize -Wrap | Out-String -Width 200)
Log $table
$fail = @($Results | Where-Object { $_.Result -eq 'FAIL' }).Count
Log ("{0} checks, {1} failed -> {2}" -f $Results.Count, $fail, $(if ($fail -eq 0 -and $Results.Count -gt 0) { 'P1 GATE PASS' } else { 'P1 GATE FAIL' }))
Log "log: $LogFile"
if ($fail -eq 0 -and $Results.Count -gt 0) { exit 0 } else { exit 1 }
