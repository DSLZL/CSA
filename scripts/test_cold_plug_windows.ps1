param(
    [Parameter(Mandatory = $true)]
    [string] $ActivationBin,

    [Parameter(Mandatory = $true)]
    [string] $TempRoot,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [string] $ExpectedExecutable,

    [string] $ExpectedOutput = 'codex-cli 0.147.0'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Sha256Text([string] $Value) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [Convert]::ToHexString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)))
    }
    finally {
        $sha.Dispose()
    }
}

$activation = [IO.Path]::GetFullPath($ActivationBin)
$temp = [IO.Path]::GetFullPath($TempRoot)
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$tempLeaf = [IO.Path]::GetFileName($temp.TrimEnd([IO.Path]::DirectorySeparatorChar))
if (-not $temp.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase) -or
    -not $tempLeaf.StartsWith('csa-path-probe-', [StringComparison]::Ordinal)) {
    throw "unsafe temp root: $temp"
}

$output = [IO.Path]::GetFullPath($OutputPath)
$outputParent = [IO.Path]::GetDirectoryName($output)
if ([string]::IsNullOrEmpty($outputParent)) {
    throw 'output path has no parent'
}

if (Test-Path -LiteralPath $temp) {
    Remove-Item -LiteralPath $temp -Recurse -Force
}
$childHome = Join-Path $temp 'home'
$childCwd = Join-Path $temp 'cwd'
$childCodexHome = Join-Path $childHome '.codex'
New-Item -ItemType Directory -Path $childHome, $childCwd, $childCodexHome -Force | Out-Null

$parent = @{
    PATH = $env:PATH
    HOME = $env:HOME
    USERPROFILE = $env:USERPROFILE
    CODEX_HOME = $env:CODEX_HOME
}
$pathBeforeHash = Get-Sha256Text $parent.PATH
$childJson = $null

try {
    $powershell = (Get-Process -Id $PID).Path
    $env:PATH = $activation + [IO.Path]::PathSeparator + $parent.PATH
    $env:HOME = $childHome
    $env:USERPROFILE = $childHome
    $env:CODEX_HOME = $childCodexHome
    $env:CSA_CHILD_CWD = $childCwd
    $env:CSA_ACTIVATION_BIN = $activation
    $env:CSA_PARENT_PATH = $parent.PATH

    $childCommand = @'
$ErrorActionPreference = 'Stop'
$env:PATH = $env:CSA_ACTIVATION_BIN + [IO.Path]::PathSeparator + $env:CSA_PARENT_PATH
$resolved = (Get-Command codex -CommandType Application | Select-Object -First 1).Source
Set-Location -LiteralPath $env:CSA_CHILD_CWD
$output = (& $resolved --version 2>&1 | Out-String).Trim()
[pscustomobject]@{
    resolved = $resolved
    output = $output
    exit_code = $LASTEXITCODE
    cwd = (Get-Location).Path
    home = $env:HOME
    codex_home = $env:CODEX_HOME
    path_first = ($env:PATH -split [IO.Path]::PathSeparator)[0]
} | ConvertTo-Json -Compress
'@
    $childJson = & $powershell -NoProfile -Command $childCommand
}
finally {
    $env:PATH = $parent.PATH
    $env:HOME = $parent.HOME
    $env:USERPROFILE = $parent.USERPROFILE
    $env:CODEX_HOME = $parent.CODEX_HOME
    Remove-Item Env:CSA_CHILD_CWD -ErrorAction SilentlyContinue
    Remove-Item Env:CSA_ACTIVATION_BIN -ErrorAction SilentlyContinue
    Remove-Item Env:CSA_PARENT_PATH -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $temp) {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}

$child = $childJson | ConvertFrom-Json
$expected = if ([string]::IsNullOrEmpty($ExpectedExecutable)) {
    (Resolve-Path -LiteralPath (Join-Path $activation 'codex.exe')).Path
}
else {
    (Resolve-Path -LiteralPath $ExpectedExecutable).Path
}
$resolvedPath = [IO.Path]::GetFullPath([string] $child.resolved)
$expectedPath = [IO.Path]::GetFullPath([string] $expected)
if (-not $resolvedPath.Equals($expectedPath, [StringComparison]::OrdinalIgnoreCase) -or
    $child.exit_code -ne 0 -or
    -not $child.output.Contains($ExpectedOutput, [StringComparison]::Ordinal)) {
    throw "child activation assertion failed: $childJson"
}

$record = [ordered]@{
    schema = 1
    child = $child
    expected_executable = $expected
    parent_path_sha256_before = $pathBeforeHash
    parent_path_sha256_after = Get-Sha256Text $env:PATH
    parent_path_unchanged = ($env:PATH -eq $parent.PATH)
    parent_home_unchanged = ($env:HOME -eq $parent.HOME)
    parent_userprofile_unchanged = ($env:USERPROFILE -eq $parent.USERPROFILE)
    parent_codex_home_unchanged = ($env:CODEX_HOME -eq $parent.CODEX_HOME)
    cleanup = -not (Test-Path -LiteralPath $temp)
    result = 'pass'
}
New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
$json = $record | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText($output, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
$json
