# SPDX-License-Identifier: GPL-3.0-only
# Copy an already online-verified runtime into explicit executable directories.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Runtime, [Parameter(Mandatory)][string[]]$Destination)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$inventory = Get-Content -LiteralPath (Join-Path $repository 'licenses/conpty/runtime-manifest.json') -Raw | ConvertFrom-Json
function Assert-Plain([string]$Path, [bool]$Directory) {
 $item = Get-Item -LiteralPath $Path -Force
 Require (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -and $item.PSIsContainer -eq $Directory) 'runtime_reparse_or_kind'
}
function Assert-Runtime([string]$Base) {
 Assert-Plain $Base $true
 foreach ($arch in @('x64','arm64')) { Assert-Plain (Join-Path $Base $arch) $true }
 $top = @((Get-ChildItem -LiteralPath $Base -Force).Name | Sort-Object)
 Require (($top -join '|') -ceq 'arm64|conpty.dll|x64') 'runtime_layout'
 foreach ($arch in @('x64','arm64')) {
  $entries = @(Get-ChildItem -LiteralPath (Join-Path $Base $arch) -Force)
  Require ($entries.Count -eq 1 -and $entries[0].Name -ceq 'OpenConsole.exe') 'runtime_host_layout'
 }
 foreach ($member in $inventory.members) {
  $path = Join-Path $Base $member.path
  Assert-Plain $path $false
  Require ((Get-Item -LiteralPath $path).Length -eq $member.size -and (Hash $path) -ceq $member.sha256) 'runtime_offline_identity'
 }
}
$runtimePath = [IO.Path]::GetFullPath($Runtime)
Assert-Runtime $runtimePath
foreach ($destinationPath in $Destination) {
 $root = [IO.Path]::GetFullPath($destinationPath)
 [void][IO.Directory]::CreateDirectory($root)
 Assert-Plain $root $true
 $base = $root
 foreach ($component in @('runtime','conpty')) {
  $base = Join-Path $base $component
  [void][IO.Directory]::CreateDirectory($base)
  Assert-Plain $base $true
 }
 $target = Join-Path $base $inventory.version
 Require (-not (Test-Path -LiteralPath $target)) 'runtime_stage_destination_exists'
 $pending = Join-Path $base ('.staging-' + [guid]::NewGuid().ToString('N'))
 try {
  Copy-Item -LiteralPath $runtimePath -Destination $pending -Recurse
  Assert-Runtime $pending
  [IO.Directory]::Move($pending,$target)
 } finally { if (Test-Path -LiteralPath $pending) { Remove-Item -LiteralPath $pending -Recurse -Force } }
 $licenseTarget = Join-Path $root 'licenses/conpty'
 [void][IO.Directory]::CreateDirectory($licenseTarget)
 foreach ($notice in $inventory.notices) {
  $source = Join-Path $repository ('licenses/conpty/' + $notice.path)
  Require ((Get-Item -LiteralPath $source).Length -eq $notice.size -and (Hash $source) -ceq $notice.sha256) 'runtime_notice_identity'
  Copy-Item -LiteralPath $source -Destination (Join-Path $licenseTarget $notice.path)
 }
 Copy-Item -LiteralPath (Join-Path $repository 'licenses/conpty/runtime-manifest.json') -Destination $licenseTarget
}
Write-Output 'conpty_stage=complete;version=1.25.260930003;members=3;notices=complete'
