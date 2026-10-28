# SPDX-License-Identifier: GPL-3.0-only
# Shared release/development/CI package assembly. Destination must be fresh.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Binary, [Parameter(Mandatory)][string]$Runtime,
 [Parameter(Mandatory)][string]$Destination)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$root = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $root) { throw 'package_destination_must_be_new' }
[void][IO.Directory]::CreateDirectory($root)
Copy-Item -LiteralPath $Binary -Destination (Join-Path $root 'odytty.exe')
foreach ($file in @('README.md','LICENSE','NOTICE','THIRD-PARTY-LICENSES')) {
 Copy-Item -LiteralPath (Join-Path $repository $file) -Destination $root
}
foreach ($path in Get-Content -LiteralPath (Join-Path $repository 'dist/font-licenses.txt')) {
 if ($path.Trim() -eq '') { continue }
 $relative = $path.Substring('assets/fonts/'.Length)
 $target = Join-Path $root ('licenses/fonts/' + $relative)
 [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target))
 Copy-Item -LiteralPath (Join-Path $repository $path) -Destination $target
}
& (Join-Path $PSScriptRoot 'Stage.ps1') -Runtime $Runtime -Destination @($root)
