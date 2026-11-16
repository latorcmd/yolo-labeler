# Run manually in PowerShell when this workspace cannot write to Documents\GitHub.
$ErrorActionPreference = 'Stop'
$sourceRepo = (Resolve-Path -LiteralPath $PSScriptRoot).Path
$targetRepo = 'C:\Users\lator\Documents\GitHub\yolo-labeler'

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw 'Git is required. Install Git for Windows first.'
}
if ($sourceRepo -eq $targetRepo) {
    Write-Output 'Already installed in the requested folder.'
    exit 0
}
if (Test-Path -LiteralPath $targetRepo) {
    $targetItem = Get-Item -LiteralPath $targetRepo -Force
    if (-not $targetItem.PSIsContainer -or ($targetItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Destination must be a regular directory.'
    }
    if (@(Get-ChildItem -LiteralPath $targetRepo -Force).Count -ne 0) {
        throw 'Destination is not empty. No existing files were changed.'
    }
}

# Clone committed sources and complete history, excluding generated datasets,
# temporary files and Python caches. Keep the original repository as a backup.
& git clone --no-hardlinks -- $sourceRepo $targetRepo
if ($LASTEXITCODE -ne 0) { throw 'Git clone failed. The source repository is unchanged.' }
$sourceCommit = & git -C $sourceRepo rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot read source commit.' }
$targetCommit = & git -C $targetRepo rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $sourceCommit -ne $targetCommit) {
    throw 'Commit verification failed.'
}
& git -C $targetRepo remote remove origin
if ($LASTEXITCODE -ne 0) { throw 'Cannot remove the temporary local source remote.' }
Write-Output "Installed: $targetRepo"
Write-Output "Commit: $targetCommit"
Write-Output "Launch: $targetRepo\launch.bat"
