param(
    [Parameter(Mandatory = $true)]
    [string] $MetaTarball,

    [Parameter(Mandatory = $true)]
    [string] $PlatformTarball,

    [Parameter(Mandatory = $true)]
    [string] $TempRoot,

    [Parameter(Mandatory = $true)]
    [string] $OutputPath,

    [Parameter(Mandatory = $true)]
    [string] $Official,

    [Parameter(Mandatory = $true)]
    [string] $OfficialNative,

    [Parameter(Mandatory = $true)]
    [string] $Manifest,

    [Parameter(Mandatory = $true)]
    [string] $Artifact,

    [string] $TrellisSource
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Sha256([string] $Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

function Get-TextSha256([string] $Value) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return [Convert]::ToHexString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)))
    }
    finally {
        $sha.Dispose()
    }
}

function Get-ProfileSnapshot([string[]] $Paths) {
    $snapshot = [ordered]@{}
    foreach ($path in $Paths | Where-Object { $_ } | Sort-Object -Unique) {
        $fullPath = [IO.Path]::GetFullPath($path)
        $snapshot[$fullPath] = if (Test-Path -LiteralPath $fullPath -PathType Leaf) {
            Get-Sha256 $fullPath
        }
        else {
            '<missing>'
        }
    }
    return $snapshot
}

function Invoke-External(
    [string] $Executable,
    [string[]] $Arguments,
    [string] $WorkingDirectory = ''
) {
    $previous = Get-Location
    try {
        if (-not [string]::IsNullOrEmpty($WorkingDirectory)) {
            Set-Location -LiteralPath $WorkingDirectory
        }
        $text = (& $Executable @Arguments 2>&1 | Out-String).Trim()
        $code = $LASTEXITCODE
        return [pscustomobject]@{ output = $text; exit_code = $code }
    }
    finally {
        Set-Location -LiteralPath $previous.Path
    }
}

function Assert-Success([string] $Name, $Result) {
    if ($Result.exit_code -ne 0) {
        throw "$Name failed with exit $($Result.exit_code): $($Result.output)"
    }
}

function Get-NpmPrefix([string] $Npm) {
    $result = Invoke-External $Npm @('config', 'get', 'prefix')
    Assert-Success 'npm config get prefix' $result
    return ($result.output -split "`r?`n" | Where-Object { $_ } | Select-Object -Last 1)
}

$meta = [IO.Path]::GetFullPath($MetaTarball)
$platform = [IO.Path]::GetFullPath($PlatformTarball)
$temp = [IO.Path]::GetFullPath($TempRoot)
$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$tempLeaf = [IO.Path]::GetFileName($temp.TrimEnd([IO.Path]::DirectorySeparatorChar))
$output = [IO.Path]::GetFullPath($OutputPath)
$officialPath = [IO.Path]::GetFullPath($Official)
$officialNativePath = [IO.Path]::GetFullPath($OfficialNative)
$manifestPath = [IO.Path]::GetFullPath($Manifest)
$artifactPath = [IO.Path]::GetFullPath($Artifact)
$repository = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$trellisPath = if ([string]::IsNullOrEmpty($TrellisSource)) {
    Join-Path $repository '.trellis'
}
else {
    [IO.Path]::GetFullPath($TrellisSource)
}

if (-not $temp.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase) -or
    -not $tempLeaf.StartsWith('csa-e2e-', [StringComparison]::Ordinal)) {
    throw "unsafe temp root: $temp"
}
foreach ($required in @($meta, $platform, $officialPath, $officialNativePath, $manifestPath, $artifactPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "required file is missing: $required"
    }
}
foreach ($required in @('scripts', 'spec', 'config.yaml', 'workflow.md')) {
    if (-not (Test-Path -LiteralPath (Join-Path $trellisPath $required))) {
        throw "required Trellis fixture input is missing: $required"
    }
}
$outputParent = [IO.Path]::GetDirectoryName($output)
if ([string]::IsNullOrEmpty($outputParent) -or -not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    throw "output parent is missing: $outputParent"
}

$npm = (Get-Command npm.cmd -CommandType Application | Select-Object -First 1).Source
$python = (Get-Command python.exe -CommandType Application | Select-Object -First 1).Source
$git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
$powershell = (Get-Process -Id $PID).Path
$parent = @{
    PATH = $env:PATH
    HOME = $env:HOME
    USERPROFILE = $env:USERPROFILE
    CODEX_HOME = $env:CODEX_HOME
    TRELLIS_CONTEXT_ID = $env:TRELLIS_CONTEXT_ID
    npm_config_cache = $env:npm_config_cache
    npm_config_userconfig = $env:npm_config_userconfig
}
$pathBefore = Get-TextSha256 $parent.PATH
$officialBefore = Get-Sha256 $officialPath
$officialNativeBefore = Get-Sha256 $officialNativePath
$globalPrefixBefore = Get-NpmPrefix $npm
$profilePaths = @(
    $PROFILE.AllUsersAllHosts,
    $PROFILE.AllUsersCurrentHost,
    $PROFILE.CurrentUserAllHosts,
    $PROFILE.CurrentUserCurrentHost,
    (Join-Path $env:USERPROFILE '.profile'),
    (Join-Path $env:USERPROFILE '.bashrc'),
    (Join-Path $env:USERPROFILE '.zshrc')
)
$profilesBefore = Get-ProfileSnapshot $profilePaths
$cleanup = $false
$result = $null

if (Test-Path -LiteralPath $temp) {
    Remove-Item -LiteralPath $temp -Recurse -Force
}

try {
    $childHome = Join-Path $temp 'home'
    $prefix = Join-Path $temp 'prefix'
    $managerRoot = Join-Path $temp 'manager'
    $codexHome = Join-Path $temp 'codex-home'
    $cwd = Join-Path $temp 'cwd'
    $logs = Join-Path $temp 'logs'
    $state = Join-Path $temp 'state'
    $record = Join-Path $temp 'isolated-exec.json'
    $nestedNpmPrefix = Join-Path $temp 'isolated-npm-prefix'
    $cache = Join-Path $temp 'npm-cache'
    $userConfig = Join-Path $temp 'npmrc'
    $fixture = Join-Path $temp 'fixture'
    $fixtureTrellis = Join-Path $fixture '.trellis'
    New-Item -ItemType Directory -Path $childHome, $prefix, $cwd, $logs, $state, $nestedNpmPrefix, $cache -Force | Out-Null

    New-Item -ItemType Directory -Path $fixtureTrellis -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $trellisPath 'scripts') -Destination $fixtureTrellis -Recurse
    Copy-Item -LiteralPath (Join-Path $trellisPath 'spec') -Destination $fixtureTrellis -Recurse
    Copy-Item -LiteralPath (Join-Path $trellisPath 'config.yaml') -Destination $fixtureTrellis
    Copy-Item -LiteralPath (Join-Path $trellisPath 'workflow.md') -Destination $fixtureTrellis
    $env:TRELLIS_CONTEXT_ID = 'csa-e2e'

    $gitInit = Invoke-External $git @('init', '--initial-branch=main', '.') $fixture
    Assert-Success 'throwaway git init' $gitInit
    $taskScript = Join-Path $fixtureTrellis 'scripts\task.py'
    $contextScript = Join-Path $fixtureTrellis 'scripts\get_context.py'
    $developerScript = Join-Path $fixtureTrellis 'scripts\init_developer.py'
    $developerInit = Invoke-External $python @($developerScript, 'csa-e2e') $fixture
    Assert-Success 'throwaway Trellis developer init' $developerInit
    $taskCreate = Invoke-External $python @(
        $taskScript, 'create', 'Native Join E2E',
        '--slug', 'native-join-e2e',
        '--assignee', 'csa-e2e',
        '--description', 'Throwaway integration context',
        '--base-branch', 'main',
        '--no-start'
    ) $fixture
    Assert-Success 'throwaway Trellis task create' $taskCreate
    $fixtureTasks = @(Get-ChildItem -LiteralPath (Join-Path $fixtureTrellis 'tasks') -Directory |
        Where-Object { $_.Name.EndsWith('-native-join-e2e', [StringComparison]::Ordinal) })
    if ($fixtureTasks.Count -ne 1) {
        throw "expected one throwaway Trellis task, found $($fixtureTasks.Count)"
    }
    $fixtureTask = $fixtureTasks[0].FullName
    foreach ($entry in @(
        @('implement', '.trellis/spec/csa/development-isolation.md', 'Official control plane and patched SUT isolation'),
        @('check', '.trellis/spec/csa/test-and-release-policy.md', 'Integration acceptance and evidence policy')
    )) {
        $addContext = Invoke-External $python @(
            $taskScript, 'add-context', $fixtureTask, $entry[0], $entry[1], $entry[2]
        ) $fixture
        Assert-Success "throwaway Trellis $($entry[0]) context" $addContext
    }
    $taskStart = Invoke-External $python @($taskScript, 'start', $fixtureTask) $fixture
    Assert-Success 'throwaway Trellis task start' $taskStart
    $taskCurrent = Invoke-External $python @($taskScript, 'current', '--json') $fixture
    Assert-Success 'throwaway Trellis active-task recovery' $taskCurrent
    $currentTask = ($taskCurrent.output -split "`r?`n" |
        Where-Object { $_.TrimStart().StartsWith('{', [StringComparison]::Ordinal) } |
        Select-Object -Last 1) | ConvertFrom-Json
    if ($currentTask.current_task.id -ne 'native-join-e2e' -or $currentTask.current_task.status -ne 'in_progress') {
        throw "throwaway Trellis active-task recovery is incomplete: $($taskCurrent.output)"
    }
    $taskValidate = Invoke-External $python @($taskScript, 'validate', $fixtureTask) $fixture
    Assert-Success 'throwaway Trellis context validation' $taskValidate
    $taskContext = Invoke-External $python @($taskScript, 'list-context', $fixtureTask) $fixture
    Assert-Success 'throwaway Trellis context recovery' $taskContext
    if ($taskContext.output -notmatch 'development-isolation\.md' -or
        $taskContext.output -notmatch 'test-and-release-policy\.md') {
        throw "throwaway Trellis context recovery is incomplete: $($taskContext.output)"
    }
    $sessionContext = Invoke-External $python @($contextScript, '--json') $fixture
    Assert-Success 'throwaway Trellis session context' $sessionContext
    if ($sessionContext.output -notmatch 'native-join-e2e') {
        throw "throwaway Trellis session context omitted the active task: $($sessionContext.output)"
    }
    $packageContext = Invoke-External $python @($contextScript, '--mode', 'packages', '--json') $fixture
    Assert-Success 'throwaway Trellis package context' $packageContext
    $fixtureTaskState = Get-Content -LiteralPath (Join-Path $fixtureTask 'task.json') -Raw | ConvertFrom-Json
    if ($fixtureTaskState.status -ne 'in_progress') {
        throw "throwaway Trellis task did not enter in_progress: $($fixtureTaskState.status)"
    }

    $env:HOME = $childHome
    $env:USERPROFILE = $childHome
    $env:CODEX_HOME = $null
    $env:npm_config_cache = $cache
    $env:npm_config_userconfig = $userConfig

    $install = Invoke-External $npm @(
        'install', '--prefix', $prefix, '--offline', '--no-audit', '--no-fund',
        $platform, $meta
    )
    Assert-Success 'temporary npm install' $install
    if (Test-Path -LiteralPath $managerRoot) {
        throw "npm install created manager state before invocation: $managerRoot"
    }

    $launcher = Join-Path $prefix 'node_modules\.bin\csa.cmd'
    $metaInstalledPath = Join-Path $prefix 'node_modules\@dslzl\csa\package.json'
    $platformInstalledPath = Join-Path $prefix 'node_modules\@dslzl\csa-win32-x64\package.json'
    foreach ($installed in @($launcher, $metaInstalledPath, $platformInstalledPath)) {
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf)) {
            throw "expected installed file is missing: $installed"
        }
    }
    $metaInstalled = Get-Content -LiteralPath $metaInstalledPath -Raw | ConvertFrom-Json
    $platformInstalled = Get-Content -LiteralPath $platformInstalledPath -Raw | ConvertFrom-Json
    if ('scripts' -in $metaInstalled.PSObject.Properties.Name -or
        'scripts' -in $platformInstalled.PSObject.Properties.Name) {
        throw 'packed packages must not contain lifecycle scripts'
    }

    $version = Invoke-External $launcher @('--version')
    Assert-Success 'packaged manager version' $version
    if ($version.output -notmatch 'csa 0\.1\.3') {
        throw "unexpected packaged manager version: $($version.output)"
    }

    $identityArgs = @(
        '--manager-root', $managerRoot,
        '--official', $officialPath,
        '--official-native', $officialNativePath,
        '--manifest', $manifestPath
    )
    $doctor = Invoke-External $launcher (@('doctor') + $identityArgs)
    Assert-Success 'packaged doctor' $doctor

    $coldInstall = Invoke-External $launcher (@('install') + $identityArgs + @('--artifact', $artifactPath))
    Assert-Success 'packaged cold install' $coldInstall

    $status = Invoke-External $launcher @('status', '--manager-root', $managerRoot)
    Assert-Success 'packaged status' $status
    if ($status.output -notmatch 'prepared' -or $status.output -notmatch 'plugged') {
        throw "packaged status did not report prepared and plugged: $($status.output)"
    }

    $activationBin = Join-Path $managerRoot 'bin'
    $shim = Join-Path $activationBin 'codex.exe'
    $coldPlugScript = Join-Path $PSScriptRoot 'test_cold_plug_windows.ps1'
    $pluggedChildPath = Join-Path $temp 'plugged-child.json'
    $pluggedChildTemp = Join-Path $tempBase ("csa-path-probe-plug-$([Guid]::NewGuid().ToString('N'))")
    $pluggedChild = Invoke-External $powershell @(
        '-NoProfile', '-File', $coldPlugScript,
        '-ActivationBin', $activationBin,
        '-TempRoot', $pluggedChildTemp,
        '-OutputPath', $pluggedChildPath,
        '-ExpectedExecutable', $shim,
        '-ExpectedOutput', 'codex-cli 0.149.0'
    )
    Assert-Success 'packaged plugged child PATH probe' $pluggedChild

    $isolated = Invoke-External $launcher @(
        'exec', '--isolated',
        '--manager-root', $managerRoot,
        '--codex-home', $codexHome,
        '--cwd', $cwd,
        '--logs-dir', $logs,
        '--state-dir', $state,
        '--record', $record,
        '--npm-prefix', $nestedNpmPrefix,
        '--', '--version'
    )
    Assert-Success 'packaged isolated exec' $isolated
    if ($isolated.output -notmatch 'codex-cli 0\.149\.0' -or -not (Test-Path -LiteralPath $record -PathType Leaf)) {
        throw "packaged isolated exec evidence is incomplete: $($isolated.output)"
    }

    $coldUninstall = Invoke-External $launcher @('uninstall', '--manager-root', $managerRoot)
    Assert-Success 'packaged cold uninstall' $coldUninstall
    if (Test-Path -LiteralPath $shim) {
        throw "activation shim remains after cold uninstall: $shim"
    }
    $unpluggedChildPath = Join-Path $temp 'unplugged-child.json'
    $unpluggedChildTemp = Join-Path $tempBase ("csa-path-probe-uninstall-$([Guid]::NewGuid().ToString('N'))")
    $unpluggedChild = Invoke-External $powershell @(
        '-NoProfile', '-File', $coldPlugScript,
        '-ActivationBin', $activationBin,
        '-TempRoot', $unpluggedChildTemp,
        '-OutputPath', $unpluggedChildPath,
        '-ExpectedExecutable', $officialPath,
        '-ExpectedOutput', 'codex-cli 0.149.0'
    )
    Assert-Success 'packaged uninstalled child PATH probe' $unpluggedChild

    $npmUninstall = Invoke-External $npm @(
        'uninstall', '--prefix', $prefix, '--offline', '--no-audit', '--no-fund',
        '@dslzl/csa', '@dslzl/csa-win32-x64'
    )
    Assert-Success 'temporary npm uninstall' $npmUninstall
    if ((Test-Path -LiteralPath $launcher) -or
        (Test-Path -LiteralPath $metaInstalledPath) -or
        (Test-Path -LiteralPath $platformInstalledPath)) {
        throw 'npm uninstall left package files in the temporary prefix'
    }

    $result = [ordered]@{
        schema = 1
        result = 'pass'
        packages = [ordered]@{
            meta = '@dslzl/csa@0.1.4'
            platform = '@dslzl/csa-win32-x64@0.1.4'
            lifecycle_scripts = $false
        }
        install = [ordered]@{
            prefix = $prefix
            offline = $true
            global = $false
            launcher = $launcher
            manager_state_created = $false
        }
        manager = [ordered]@{
            version = 'csa 0.1.4'
            doctor = 'pass'
            cold_install = 'pass'
            status = 'prepared_and_plugged'
            plugged_child_resolution = $shim
            isolated_exec = 'codex-cli 0.149.0'
            isolated_record_created = $true
            cold_uninstall = 'pass'
            uninstalled_child_resolution = $officialPath
            activation_shim_remaining = $false
        }
        trellis = [ordered]@{
            fixture = 'throwaway'
            developer_init = 'pass'
            task_create = 'pass'
            task_status = 'in_progress'
            implement_context = 'pass'
            check_context = 'pass'
            context_validation = 'pass'
            context_recovery = 'pass'
            active_task_recovery = 'pass'
            session_context_recovery = 'pass'
            package_context = 'pass'
            live_child_agent = 'not_verified_by_policy'
        }
        uninstall = [ordered]@{
            result = 'pass'
            package_files_remaining = $false
        }
    }
}
finally {
    $env:PATH = $parent.PATH
    $env:HOME = $parent.HOME
    $env:USERPROFILE = $parent.USERPROFILE
    $env:CODEX_HOME = $parent.CODEX_HOME
    $env:TRELLIS_CONTEXT_ID = $parent.TRELLIS_CONTEXT_ID
    $env:npm_config_cache = $parent.npm_config_cache
    $env:npm_config_userconfig = $parent.npm_config_userconfig
    if (Test-Path -LiteralPath $temp) {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
    $cleanup = -not (Test-Path -LiteralPath $temp)
}

$officialAfter = Get-Sha256 $officialPath
$officialNativeAfter = Get-Sha256 $officialNativePath
$globalPrefixAfter = Get-NpmPrefix $npm
$profilesAfter = Get-ProfileSnapshot $profilePaths
$profilesUnchanged = (($profilesBefore | ConvertTo-Json -Compress) -eq
    ($profilesAfter | ConvertTo-Json -Compress))
$result.isolation = [ordered]@{
    parent_path_sha256_before = $pathBefore
    parent_path_sha256_after = Get-TextSha256 $env:PATH
    parent_path_unchanged = ($pathBefore -eq (Get-TextSha256 $env:PATH))
    parent_home_unchanged = ($parent.HOME -eq $env:HOME)
    parent_userprofile_unchanged = ($parent.USERPROFILE -eq $env:USERPROFILE)
    parent_codex_home_unchanged = ($parent.CODEX_HOME -eq $env:CODEX_HOME)
    parent_trellis_context_id_unchanged = ($parent.TRELLIS_CONTEXT_ID -eq $env:TRELLIS_CONTEXT_ID)
    npm_global_prefix_before = $globalPrefixBefore
    npm_global_prefix_after = $globalPrefixAfter
    npm_global_prefix_unchanged = ($globalPrefixBefore -eq $globalPrefixAfter)
    profile_files_unchanged = $profilesUnchanged
    official_launcher_sha256_before = $officialBefore
    official_launcher_sha256_after = $officialAfter
    official_native_sha256_before = $officialNativeBefore
    official_native_sha256_after = $officialNativeAfter
    official_unchanged = ($officialBefore -eq $officialAfter -and $officialNativeBefore -eq $officialNativeAfter)
    cleanup = $cleanup
}
if (-not $result.isolation.parent_path_unchanged -or
    -not $result.isolation.parent_home_unchanged -or
    -not $result.isolation.parent_userprofile_unchanged -or
    -not $result.isolation.parent_codex_home_unchanged -or
    -not $result.isolation.parent_trellis_context_id_unchanged -or
    -not $result.isolation.npm_global_prefix_unchanged -or
    -not $result.isolation.profile_files_unchanged -or
    -not $result.isolation.official_unchanged -or
    -not $cleanup) {
    throw 'temporary npm E2E isolation invariant failed'
}

$json = $result | ConvertTo-Json -Depth 8
Set-Content -LiteralPath $output -Value $json -Encoding UTF8
$json
