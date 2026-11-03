# 信頼した GitHub ソースから取得して実行する初回取得用。既定ではインストールしない。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$WorkRoot = 'D:/Codex/remnifold/bootstrap',
    [switch]$Apply,
    [switch]$DownloadOnly
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($Apply -and $DownloadOnly) { throw 'Apply and DownloadOnly are mutually exclusive.' }
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'Use bootstrap-plugin.sh on macOS/Linux.' }
if ($Version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$') { throw 'Version must be SemVer.' }
$core = ($Version -split '\+', 2)[0]
if ($core.Contains('-')) {
    foreach ($part in (($core -split '-', 2)[1] -split '\.')) {
        if ($part -cmatch '^0[0-9]+$') { throw 'Numeric prerelease identifiers must not have leading zeroes.' }
    }
}
$Repository = '1llum1n4t1s/remnifold'
$Origin = 'https://remnifold-updates.kagayoi.com'
$Target = 'x86_64-pc-windows-msvc'
if ([IntPtr]::Size -ne 8 -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64') { throw 'Windows x64 is required.' }
$Gh = (Get-Command gh -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
$Tar = Join-Path $env:SystemRoot 'System32/tar.exe'
if (-not [IO.File]::Exists($Tar)) { throw 'The Windows System32 tar executable is required.' }

function Assert-NoLinks([string]$Path) {
    $scan = $Path
    while ($scan) {
        if (Test-Path -LiteralPath $scan) {
            $item = Get-Item -LiteralPath $scan -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Linked work-root ancestor rejected.' }
        }
        $parent = [IO.Path]::GetDirectoryName($scan)
        if ($parent -eq $scan) { break }
        $scan = $parent
    }
}
function Test-Overlap([string]$A, [string]$B) {
    $aPrefix = $A.TrimEnd('\', '/') + '\'
    $bPrefix = $B.TrimEnd('\', '/') + '\'
    return $aPrefix.StartsWith($bPrefix, [StringComparison]::OrdinalIgnoreCase) -or $bPrefix.StartsWith($aPrefix, [StringComparison]::OrdinalIgnoreCase)
}
if ($WorkRoot -notmatch '^[dD]:[\\/]' -or $WorkRoot -match '[\x00-\x1f]' -or $WorkRoot -match '(^|[\\/])\.{1,2}([\\/]|$)') { throw 'Work root must be an absolute canonical path under D:/Codex/remnifold.' }
$WorkRoot = [IO.Path]::GetFullPath($WorkRoot).TrimEnd('\', '/')
if (-not $WorkRoot.StartsWith('D:\Codex\remnifold\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Work root must be below D:/Codex/remnifold.' }
Assert-NoLinks $WorkRoot
$scriptFile = [IO.Path]::GetFullPath($PSCommandPath)
if ($scriptFile.StartsWith($WorkRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Save the trusted bootstrap script outside the work root.' }
$sourceRoot = [IO.Path]::GetFullPath($PSScriptRoot)
while ($sourceRoot) {
    if ((Test-Path -LiteralPath (Join-Path $sourceRoot '.git')) -or (Test-Path -LiteralPath (Join-Path $sourceRoot '.codex-plugin/plugin.json'))) {
        if (Test-Overlap $WorkRoot $sourceRoot) { throw 'Work root overlaps the bootstrap repository or plugin source.' }
        break
    }
    $sourceRoot = [IO.Path]::GetDirectoryName($sourceRoot)
}
$profileRoot = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
if (-not $profileRoot -or -not [IO.Path]::IsPathRooted($profileRoot)) { throw 'USERPROFILE or HOME must be absolute, matching the native updater.' }
$codexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $profileRoot '.codex' }
if (-not [IO.Path]::IsPathRooted($codexHome)) { throw 'CODEX_HOME must be absolute.' }
$codexHome = [IO.Path]::GetFullPath($codexHome)
Assert-NoLinks $codexHome
if (Test-Overlap $WorkRoot $codexHome) { throw 'Work root overlaps Codex home.' }
$scan = $WorkRoot
while ($scan) {
    if ((Test-Path -LiteralPath (Join-Path $scan '.git')) -or (Test-Path -LiteralPath (Join-Path $scan '.codex-plugin/plugin.json'))) { throw 'Work root is inside a repository or plugin.' }
    $scan = [IO.Path]::GetDirectoryName($scan)
}
function New-PrivateDirectory([string]$Path) {
    $security = [Security.AccessControl.DirectorySecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $security.SetOwner($currentSid)
    foreach ($sid in @($currentSid, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'), [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))) {
        $rule = [Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $security.AddAccessRule($rule)
    }
    # .NET Framework (Windows PowerShell 5) と現行 .NET の双方で、作成時に ACL を設定する。
    $method = [IO.Directory].GetMethod('CreateDirectory', [type[]]@([string], [Security.AccessControl.DirectorySecurity]))
    if ($method) { $null = $method.Invoke($null, @($Path, $security)) }
    else { $null = [IO.FileSystemAclExtensions]::CreateDirectory($security, $Path) }
}
function Assert-PrivateDirectory([string]$Path) {
    $security = Get-Acl -LiteralPath $Path
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $trusted = @($currentSid, 'S-1-5-18', 'S-1-5-32-544')
    if ($security.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $trusted) { throw 'Untrusted work-root owner.' }
    foreach ($rule in $security.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted -and (([long]$rule.FileSystemRights -band 0x500D0156) -ne 0)) { throw 'Work root permits another user to modify bootstrap state; select a new private work root.' }
    }
}
if (-not ('RemnifoldBootstrapLocksV1' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class RemnifoldBootstrapLocksV1 {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern SafeFileHandle CreateFileW(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern uint GetFinalPathNameByHandleW(SafeFileHandle handle, System.Text.StringBuilder path, uint length, uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateJobObjectW(IntPtr security, string name);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool SetInformationJobObject(SafeFileHandle job, int kind, IntPtr data, uint length);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool AssignProcessToJobObject(SafeFileHandle job, IntPtr process);
    [StructLayout(LayoutKind.Sequential)]
    struct SecurityAttributes { public int Length; public IntPtr Descriptor; public int Inherit; }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    struct StartupInfo {
        public int Size;
        public string Reserved, Desktop, Title;
        public uint X, Y, Width, Height, XCount, YCount, Fill, Flags;
        public ushort Show, ReservedSize;
        public IntPtr ReservedBytes, Input, Output, Error;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessInfo { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool CreatePipe(out SafeFileHandle read, out SafeFileHandle write, ref SecurityAttributes attributes, uint size);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool SetHandleInformation(SafeFileHandle handle, uint mask, uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool CreateProcessW(string app, System.Text.StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inherit, uint flags, IntPtr environment, string directory, ref StartupInfo startup, out ProcessInfo process);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);
    [StructLayout(LayoutKind.Sequential)]
    struct BasicLimits {
        public long ProcessTime, JobTime;
        public uint Flags;
        public UIntPtr MinimumWorkingSet, MaximumWorkingSet;
        public uint ActiveProcesses;
        public UIntPtr Affinity;
        public uint Priority, Scheduling;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct IoCounters { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
    [StructLayout(LayoutKind.Sequential)]
    struct ExtendedLimits {
        public BasicLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }
    public static SafeFileHandle Contain(System.Diagnostics.Process process) {
        SafeFileHandle job = CreateJobObjectW(IntPtr.Zero, null);
        if (job.IsInvalid) throw new InvalidOperationException("Cannot create subprocess job");
        ExtendedLimits limits = new ExtendedLimits();
        limits.Basic.Flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        int size = Marshal.SizeOf(typeof(ExtendedLimits));
        IntPtr data = Marshal.AllocHGlobal(size);
        try {
            Marshal.StructureToPtr(limits, data, false);
            if (!SetInformationJobObject(job, 9, data, (uint)size) || !AssignProcessToJobObject(job, process.Handle)) {
                job.Dispose();
                try { process.Kill(); } catch { }
                throw new InvalidOperationException("Cannot contain subprocess tree");
            }
            return job;
        } finally { Marshal.FreeHGlobal(data); }
    }
    public sealed class Child : IDisposable {
        public System.Diagnostics.Process Process;
        public SafeFileHandle Job;
        public System.IO.Stream Output, Error;
        public void Dispose() {
            if (Job != null) Job.Dispose();
            if (Output != null) Output.Dispose();
            if (Error != null) Error.Dispose();
            if (Process != null) Process.Dispose();
        }
    }
    public static Child Start(string executable, string arguments, string directory) {
        SafeFileHandle outputRead = null, outputWrite = null, errorRead = null, errorWrite = null, inputRead = null, inputWrite = null;
        ProcessInfo native = new ProcessInfo();
        Child child = new Child();
        bool created = false;
        try {
            SecurityAttributes attributes = new SecurityAttributes();
            attributes.Length = Marshal.SizeOf(typeof(SecurityAttributes)); attributes.Inherit = 1;
            if (!CreatePipe(out outputRead, out outputWrite, ref attributes, 0) ||
                !CreatePipe(out errorRead, out errorWrite, ref attributes, 0) ||
                !CreatePipe(out inputRead, out inputWrite, ref attributes, 0) ||
                !SetHandleInformation(outputRead, 1, 0) || !SetHandleInformation(errorRead, 1, 0))
                throw new InvalidOperationException("Cannot create subprocess pipes");
            inputWrite.Dispose(); // 非対話 CLI の stdin は EOF。
            StartupInfo startup = new StartupInfo();
            startup.Size = Marshal.SizeOf(typeof(StartupInfo)); startup.Flags = 0x100; // STARTF_USESTDHANDLES
            startup.Input = inputRead.DangerousGetHandle(); startup.Output = outputWrite.DangerousGetHandle(); startup.Error = errorWrite.DangerousGetHandle();
            var command = new System.Text.StringBuilder("\"" + executable + "\" " + arguments);
            if (!CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, true, 0x08000004, IntPtr.Zero, directory, ref startup, out native))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
            created = true; // CREATE_SUSPENDED | CREATE_NO_WINDOW: Job 参加前に子コードを実行させない。
            child.Process = System.Diagnostics.Process.GetProcessById((int)native.ProcessId);
            child.Job = Contain(child.Process);
            child.Output = new System.IO.FileStream(outputRead, System.IO.FileAccess.Read, 65536, false); outputRead = null;
            child.Error = new System.IO.FileStream(errorRead, System.IO.FileAccess.Read, 65536, false); errorRead = null;
            outputWrite.Dispose(); errorWrite.Dispose(); inputRead.Dispose();
            if (ResumeThread(native.Thread) == uint.MaxValue) throw new InvalidOperationException("Cannot resume contained subprocess");
            return child;
        } catch {
            if (created) TerminateProcess(native.Process, 1);
            child.Dispose();
            throw;
        } finally {
            if (outputRead != null) outputRead.Dispose(); if (outputWrite != null) outputWrite.Dispose();
            if (errorRead != null) errorRead.Dispose(); if (errorWrite != null) errorWrite.Dispose();
            if (inputRead != null) inputRead.Dispose(); if (inputWrite != null) inputWrite.Dispose();
            if (native.Thread != IntPtr.Zero) CloseHandle(native.Thread);
            if (native.Process != IntPtr.Zero) CloseHandle(native.Process);
        }
    }
    public static async System.Threading.Tasks.Task<long> Pump(System.IO.Stream input, string output, long cap) {
        using (var file = new System.IO.FileStream(output, System.IO.FileMode.CreateNew, System.IO.FileAccess.Write, System.IO.FileShare.Read)) {
            byte[] buffer = new byte[65536];
            long total = 0;
            while (true) {
                int count = await input.ReadAsync(buffer, 0, (int)Math.Min(buffer.Length, cap + 1 - total)).ConfigureAwait(false);
                if (count == 0) return total;
                await file.WriteAsync(buffer, 0, count).ConfigureAwait(false);
                total += count;
                if (total > cap) throw new InvalidOperationException("Subprocess output bound exceeded");
            }
        }
    }
}
'@
}
function Resolve-PhysicalPath([string]$Path) {
    $existing = [IO.Path]::GetFullPath($Path)
    $missing = [Collections.Generic.List[string]]::new()
    while (-not (Test-Path -LiteralPath $existing)) {
        $leaf = [IO.Path]::GetFileName($existing)
        if (-not $leaf -or $leaf.EndsWith('.') -or $leaf.EndsWith(' ') -or $leaf.IndexOfAny([IO.Path]::GetInvalidFileNameChars()) -ge 0) { throw 'Unsupported path component.' }
        $missing.Insert(0, $leaf)
        $existing = [IO.Path]::GetDirectoryName($existing)
        if (-not $existing) { throw 'No existing path ancestor.' }
    }
    Assert-NoLinks $existing
    $handle = [RemnifoldBootstrapLocksV1]::CreateFileW($existing, 0, 7, [IntPtr]::Zero, 3, 0x02200000, [IntPtr]::Zero)
    if ($handle.IsInvalid) { $handle.Dispose(); throw 'Cannot resolve physical path identity.' }
    try {
        $buffer = [Text.StringBuilder]::new(32768)
        $length = [RemnifoldBootstrapLocksV1]::GetFinalPathNameByHandleW($handle, $buffer, $buffer.Capacity, 0)
        if ($length -eq 0 -or $length -ge $buffer.Capacity) { throw 'Cannot resolve physical path identity.' }
        $resolved = $buffer.ToString()
        if ($resolved.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) { $resolved = '\\' + $resolved.Substring(8) }
        elseif ($resolved.StartsWith('\\?\', [StringComparison]::Ordinal)) { $resolved = $resolved.Substring(4) }
        foreach ($leaf in $missing) { $resolved = Join-Path $resolved $leaf }
        return $resolved.TrimEnd('\', '/')
    } finally { $handle.Dispose() }
}
function Assert-StableDeliveryAncestors([string]$Path) {
    $trusted = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, 'S-1-5-18', 'S-1-5-32-544', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
    $scan = $Path
    while ($scan) {
        $security = Get-Acl -LiteralPath $scan
        if ($security.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $trusted) { throw 'Untrusted delivery ancestor owner.' }
        foreach ($rule in $security.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
            if (($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) { continue }
            if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted -and (([long]$rule.FileSystemRights -band 0x500D0040) -ne 0)) { throw 'Delivery ancestor permits replacement by another user.' }
        }
        $scan = [IO.Path]::GetDirectoryName($scan)
    }
}
function Get-Sha256([string]$Path) {
    $hash = [Security.Cryptography.SHA256]::Create()
    $inputFile = [IO.File]::OpenRead($Path)
    try { return ([BitConverter]::ToString($hash.ComputeHash($inputFile))).Replace('-', '').ToLowerInvariant() }
    finally { $inputFile.Dispose(); $hash.Dispose() }
}
# GetFullPath だけでは 8.3 名を解決できない。存在最長 prefix の実体に対して境界を比較する。
$WorkRoot = Resolve-PhysicalPath $WorkRoot
$codexHome = Resolve-PhysicalPath $codexHome
$scriptFile = Resolve-PhysicalPath $scriptFile
if ($scriptFile.StartsWith($WorkRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or (Test-Overlap $WorkRoot $codexHome)) { throw 'Physical work-root boundary mismatch.' }
if ($sourceRoot -and (Test-Overlap $WorkRoot (Resolve-PhysicalPath $sourceRoot))) { throw 'Physical repository/plugin boundary mismatch.' }
$directoryLocks = [Collections.Generic.List[IDisposable]]::new()
try {
    # 共有親が改名・リンクへ差し替えられないよう、祖先を DELETE 共有なしで保持する。
    $rootPart = [IO.Path]::GetPathRoot($WorkRoot)
    $parts = @($rootPart)
    $walk = $rootPart
    foreach ($part in $WorkRoot.Substring($rootPart.Length).Split('\')) {
        if ($part) { $walk = Join-Path $walk $part; $parts += $walk }
    }
    foreach ($path in $parts) {
        if (-not (Test-Path -LiteralPath $path)) { New-PrivateDirectory $path }
        $handle = [RemnifoldBootstrapLocksV1]::CreateFileW($path, 1, 3, [IntPtr]::Zero, 3, 0x02200000, [IntPtr]::Zero)
        if ($handle.IsInvalid) { $handle.Dispose(); throw 'Cannot lock work-root ancestor against replacement.' }
        $directoryLocks.Add($handle)
        Assert-NoLinks $path
        if (-not (Get-Item -LiteralPath $path -Force).PSIsContainer) { throw 'Directory required.' }
    }
Assert-PrivateDirectory $WorkRoot
$Session = Join-Path $WorkRoot ('bootstrap-' + [Guid]::NewGuid().ToString('N'))
New-PrivateDirectory $Session
Assert-PrivateDirectory $Session
Write-Output "Bootstrap state retained at $Session"
# 個人 Prunlet に依存させず、取得物は保持する。エージェントの清掃は親が担当する。
$script:Counter = 0
function ConvertTo-ProcessArgument([string]$Value) {
    if ($Value -notmatch '[\s"]' -and $Value.Length -gt 0) { return $Value }
    return '"' + [regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function Invoke-Bounded([string]$Executable, [string[]]$Arguments, [string]$Output, [long]$MaxBytes = 1048576, [int]$Seconds = 120) {
    $errPath = $Output + '.stderr'
    $quoted = ($Arguments | ForEach-Object { ConvertTo-ProcessArgument $_ }) -join ' '
    $process = $null
    $child = $null
    $job = $null
    $stdout = $null
    $stderr = $null
    $started = $false
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        $child = [RemnifoldBootstrapLocksV1]::Start($Executable, $quoted, $Session)
        $process = $child.Process
        $started = $true
        $job = $child.Job
        $stdout = [RemnifoldBootstrapLocksV1]::Pump($child.Output, $Output, $MaxBytes)
        $stderr = [RemnifoldBootstrapLocksV1]::Pump($child.Error, $errPath, 65536)
        while (-not ($process.HasExited -and $stdout.IsCompleted -and $stderr.IsCompleted)) {
            if ($timer.Elapsed.TotalSeconds -gt $Seconds -or $stdout.IsFaulted -or $stderr.IsFaulted) { throw 'Subprocess time/output bound exceeded.' }
            $null = $process.WaitForExit(50)
        }
        if ($process.ExitCode -ne 0 -or $stdout.IsFaulted -or $stderr.IsFaulted) { throw 'Subprocess failed; diagnostics retained without printing credential-bearing output.' }
    } finally {
        if ($job) { $job.Dispose() }
        if ($started) {
            if (-not $process.HasExited) { try { $process.Kill() } catch { } }
            $null = $process.WaitForExit(5000)
        }
        if ($stdout) { try { $null = $stdout.Wait(5000) } catch { } }
        if ($stderr) { try { $null = $stderr.Wait(5000) } catch { } }
        if ($child) { $child.Dispose() }
    }
}
function Read-Text([string]$Path) { return [IO.File]::ReadAllText($Path, [Text.UTF8Encoding]::new($false, $true)).TrimEnd("`r", "`n") }
function Get-GitHub([string]$Endpoint) {
    $script:Counter++
    $out = Join-Path $Session ('query-' + $script:Counter + '.json')
    Invoke-Bounded $Gh @('api', '--hostname', 'github.com', "repos/$Repository/$Endpoint", '--jq', '.') $out
    return (Read-Text $out | ConvertFrom-Json)
}
function Assert-Run($Run) {
    if ($Run.name -cne 'CI' -or $Run.head_sha -cne $script:Commit -or $Run.event -cne 'push' -or $Run.head_branch -cne "release/$Version" -or $Run.path -cne '.github/workflows/ci.yml' -or $Run.workflow_id -ne $script:WorkflowId -or $Run.repository.full_name -cne $Repository -or $Run.head_repository.full_name -cne $Repository -or $Run.repository.id -ne $script:RepositoryId -or $Run.head_repository.id -ne $script:RepositoryId) { throw 'CI identity mismatch.' }
}
function Assert-Positive($Value) { if ([string]$Value -cnotmatch '^[1-9][0-9]*$') { throw 'Invalid positive identifier.' } }
$repositoryInfo = Get-GitHub ''
if ($repositoryInfo.full_name -cne $Repository) { throw 'Repository identity mismatch.' }
$script:RepositoryId = $repositoryInfo.id
Assert-Positive $script:RepositoryId
$workflow = Get-GitHub 'actions/workflows/ci.yml'
if ($workflow.path -cne '.github/workflows/ci.yml' -or $workflow.name -cne 'CI') { throw 'Workflow identity mismatch.' }
$script:WorkflowId = $workflow.id
Assert-Positive $script:WorkflowId
$ref = Get-GitHub "git/ref/heads/release/$Version"
$script:Commit = [string]$ref.object.sha
if ($ref.object.type -cne 'commit' -or $script:Commit -cnotmatch '^[0-9a-f]{40}$') { throw 'Release commit identity mismatch.' }
$pluginContent = Get-GitHub "contents/.codex-plugin/plugin.json?ref=$script:Commit"
$plugin = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($pluginContent.content)) | ConvertFrom-Json
if ($plugin.version -cne $Version) { throw 'Release version mismatch.' }
$configContent = Get-GitHub "contents/.github/remnifold-distribution.json?ref=$script:Commit"
$config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($configContent.content)) | ConvertFrom-Json
if ($config.schema -ne 1 -or $config.repository -cne $Repository -or $config.stagingBucket -cne 'remnifold-builds' -or $config.publicBucket -cne 'remnifold-updates' -or $config.publicBaseUrl -cne $Origin -or $config.statusContext -cne 'remnifold/r2/v1' -or $config.workflowPath -cne '.github/workflows/ci.yml') { throw 'R2 configuration mismatch; fallback is forbidden.' }
$runs = Get-GitHub "actions/runs?head_sha=$script:Commit&event=push&per_page=100"
if (@($runs.workflow_runs).Count -ge 100) { throw 'CI discovery limit reached.' }
$matching = @($runs.workflow_runs | Where-Object {
    try { Assert-Run $_; $true } catch { $false }
} | Sort-Object @{Expression = { [long]$_.id }}, @{Expression = { [long]$_.run_attempt }})
if ($matching.Count -eq 0) { throw 'Matching CI not found.' }
$latest = $matching[-1]
Assert-Positive $latest.id
Assert-Positive $latest.run_attempt
if ($latest.status -cne 'completed' -or $latest.conclusion -cne 'success') { throw 'Latest matching CI is not successful.' }
$jobs = Get-GitHub "actions/runs/$($latest.id)/attempts/$($latest.run_attempt)/jobs?per_page=100"
if (@($jobs.jobs).Count -ge 100 -or $jobs.total_count -ne @($jobs.jobs).Count) { throw 'Job discovery limit reached.' }
$publishers = @($jobs.jobs | Where-Object { $_.name -ceq 'publish-r2' })
if ($publishers.Count -ne 1) { throw 'Publisher job must be unique.' }
$job = $publishers[0]
Assert-Positive $job.id
if ($job.run_id -ne $latest.id -or $job.head_sha -cne $script:Commit -or $job.status -cne 'completed' -or $job.conclusion -cne 'success') { throw 'Publisher job identity mismatch.' }
$helpPath = Join-Path $Session 'gh-api-help.txt'
Invoke-Bounded $Gh @('api', '--help') $helpPath 1048576 30
$logArgs = @('api', '--hostname', 'github.com')
if ((Read-Text $helpPath).Contains('--allow-escape-sequences')) { $logArgs += '--allow-escape-sequences' }
$logArgs += "repos/$Repository/actions/jobs/$($job.id)/logs"
$logs = Join-Path $Session 'publisher.log'
Invoke-Bounded $Gh $logArgs $logs 4194304 120
$manifest = $null
$assets = @{}
foreach ($raw in [IO.File]::ReadAllLines($logs, [Text.UTF8Encoding]::new($false, $true))) {
    if (-not $raw.Contains('REMNIFOLD_R2_MANIFEST') -and -not $raw.Contains('REMNIFOLD_R2_ASSET')) { continue }
    $line = [regex]::Replace($raw, '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z ', '')
    if ($line -cmatch '^REMNIFOLD_R2_MANIFEST_V1 ([^ ]+) ([0-9a-f]{40}) ([1-9][0-9]*) ([1-9][0-9]*) ([0-9a-f]{64})$') {
        if ($null -ne $manifest -or $Matches[1] -cne $Version -or $Matches[2] -cne $script:Commit) { throw 'Manifest witness mismatch or duplicate.' }
        $manifest = @{ Run = $Matches[3]; Attempt = $Matches[4]; Hash = $Matches[5] }
    } elseif ($line -cmatch '^REMNIFOLD_R2_ASSET_V1 (x86_64-pc-windows-msvc|aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ([0-9a-f]{64}) ([1-9][0-9]{0,8}) ([a-zA-Z0-9_./+-]+)$') {
        $t = $Matches[1]
        if ($assets.ContainsKey($t) -or [long]$Matches[3] -gt 134217728) { throw 'Duplicate target or oversized archive.' }
        $assets[$t] = @{ Hash = $Matches[2]; Size = [long]$Matches[3]; Key = $Matches[4] }
    } else { throw 'Malformed publication witness.' }
}
if ($null -eq $manifest -or $assets.Count -ne 5 -or -not $assets.ContainsKey($Target)) { throw 'Incomplete publication witness.' }
foreach ($t in $assets.Keys) {
    $expected = "releases/$Version/$script:Commit/$($manifest.Run)-$($manifest.Attempt)/$t/$($assets[$t].Hash).tar.gz"
    if ($assets[$t].Key -cne $expected) { throw 'Noncanonical archive key.' }
}
$publication = Get-GitHub "actions/runs/$($manifest.Run)/attempts/$($manifest.Attempt)"
Assert-Run $publication
if ([string]$publication.id -cne $manifest.Run -or [string]$publication.run_attempt -cne $manifest.Attempt -or $publication.status -cne 'completed' -or $publication.conclusion -cne 'success') { throw 'Publication run identity mismatch.' }
$asset = $assets[$Target]
$url = $Origin + '/' + (($asset.Key -split '/' | ForEach-Object { [Uri]::EscapeDataString($_) }) -join '/')
$archive = Join-Path $Session 'archive.tar.gz'
$request = [Net.HttpWebRequest]::Create($url)
$request.AllowAutoRedirect = $false
$request.Timeout = 120000
$request.ReadWriteTimeout = 20000
$request.UseDefaultCredentials = $false
$request.Credentials = $null
$response = $null
$stream = $null
$file = $null
try {
    $response = $request.GetResponse()
    if ([int]$response.StatusCode -ne 200 -or $response.ContentLength -gt 134217728 -or ($response.ContentLength -ge 0 -and $response.ContentLength -ne $asset.Size)) { throw 'Archive response/size rejected.' }
    $stream = $response.GetResponseStream()
    $file = [IO.File]::Open($archive, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
    $buffer = New-Object byte[] 65536
    $total = 0L
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while (($count = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
        $total += $count
        if ($total -gt $asset.Size -or $total -gt 134217728 -or $timer.Elapsed.TotalSeconds -gt 120) { throw 'Archive download bound exceeded.' }
        $file.Write($buffer, 0, $count)
    }
    if ($total -ne $asset.Size) { throw 'Archive size mismatch.' }
} finally {
    if ($file) { $file.Dispose() }
    if ($stream) { $stream.Dispose() }
    if ($response) { $response.Dispose() }
}
$digest = Get-Sha256 $archive
if ($digest -cne $asset.Hash) { throw 'Archive SHA-256 mismatch.' }
if ($DownloadOnly) {
    # 正式取得物は共有 D: 作業親へ残して返さない。OS の実ユーザープロファイルを起点にする。
    $knownProfile = Resolve-PhysicalPath ([Environment]::GetFolderPath('UserProfile'))
    Assert-NoLinks $knownProfile
    $deliveryBase = Join-Path $knownProfile 'Remnifold/Downloads'
    $delivery = Join-Path $deliveryBase ('bootstrap-' + [Guid]::NewGuid().ToString('N'))
    if ((Test-Overlap $delivery $codexHome) -or ($sourceRoot -and (Test-Overlap $delivery (Resolve-PhysicalPath $sourceRoot)))) { throw 'Delivery overlaps Codex home or source.' }
    $rootPart = [IO.Path]::GetPathRoot($delivery)
    $paths = @($rootPart)
    $walk = $rootPart
    foreach ($part in $delivery.Substring($rootPart.Length).Split('\')) {
        if ($part) { $walk = Join-Path $walk $part; $paths += $walk }
    }
    foreach ($path in $paths) {
        if (-not (Test-Path -LiteralPath $path)) { New-PrivateDirectory $path }
        $handle = [RemnifoldBootstrapLocksV1]::CreateFileW($path, 1, 3, [IntPtr]::Zero, 3, 0x02200000, [IntPtr]::Zero)
        if ($handle.IsInvalid) { $handle.Dispose(); throw 'Cannot lock delivery ancestor.' }
        $directoryLocks.Add($handle)
        Assert-NoLinks $path
    }
    Assert-PrivateDirectory $knownProfile
    Assert-StableDeliveryAncestors $delivery
    Assert-PrivateDirectory $deliveryBase
    Assert-PrivateDirectory $delivery
    $delivered = Join-Path $delivery 'remnifold.tar.gz'
    [IO.File]::Copy($archive, $delivered, $false)
    if ((Get-Item -LiteralPath $delivered).Length -ne $asset.Size -or (Get-Sha256 $delivered) -cne $asset.Hash) { throw 'Delivered archive identity mismatch.' }
    Write-Output "Verified archive: $delivered"
    return
}
$member = 'remnifold/bin/remnifold.exe'
$listing = Join-Path $Session 'member.txt'
Invoke-Bounded $Tar @('-tzf', $archive, $member) $listing 1024 60
if ((Read-Text $listing) -cne $member) { throw 'Executable member must be unique.' }
$typeListing = Join-Path $Session 'member-type.txt'
Invoke-Bounded $Tar @('-tvzf', $archive, $member) $typeListing 4096 60
if (-not (Read-Text $typeListing).StartsWith('-')) { throw 'Executable must be a regular file.' }
$binaryDir = Join-Path $Session 'remnifold/bin'
$null = [IO.Directory]::CreateDirectory($binaryDir)
$binary = Join-Path $binaryDir 'remnifold.exe'
# 既知の通常ファイル一件だけを、新しい所有ディレクトリへ展開する。
# PowerShell のテキスト出力リダイレクトを実行ファイルのバイト列には使わない。
Invoke-Bounded $Tar @('-xzf', $archive, '-C', $Session, $member) (Join-Path $Session 'extract.txt') 4096 120
if ((Get-Item -LiteralPath $binary).Length -eq 0) { throw 'Empty executable.' }
Write-Output "Verified $Target archive from CI $($latest.id) attempt $($latest.run_attempt), publisher job $($job.id)."
$updateRoot = Join-Path $Session 'update'
Invoke-Bounded $binary @('plugin-update', '--version', $Version, '--work-root', $updateRoot, '--check-published') (Join-Path $Session 'check-published.txt') 1048576 600
if ($Apply) {
    Invoke-Bounded $binary @('plugin-update', '--version', $Version, '--work-root', $updateRoot, '--apply') (Join-Path $Session 'apply.txt') 1048576 600
    Invoke-Bounded $binary @('plugin-update', '--version', $Version, '--work-root', $updateRoot, '--verify-only') (Join-Path $Session 'verify-only.txt') 1048576 600
    Write-Output 'Installation verified. Start a new Codex task and review the current hook trust settings.'
    Write-Output "Keep the verification receipt under $updateRoot for verification without retained R2 objects or CI logs; GitHub access remains required."
} else { Write-Output 'Publication verified; installation was not changed and no verification receipt was created. Use -Apply to install.' }
} finally {
    foreach ($handle in $directoryLocks) { $handle.Dispose() }
}
