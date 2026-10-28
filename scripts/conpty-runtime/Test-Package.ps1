# SPDX-License-Identifier: GPL-3.0-only
param([Parameter(Mandatory)][string]$Binary, [Parameter(Mandatory)][string]$Runtime)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$work=Join-Path ([IO.Path]::GetTempPath()) ('conpty-package-'+[guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($work)
try {
 $package=Join-Path $work 'package'
 & (Join-Path $PSScriptRoot 'Package.ps1') -Binary $Binary -Runtime $Runtime -Destination $package
 $zip=Join-Path $work 'package.zip'
 Compress-Archive -Path (Join-Path $package '*') -DestinationPath $zip
 $fresh=Join-Path $work ('fresh space '+[char]0x03b1)
 Expand-Archive -LiteralPath $zip -DestinationPath $fresh
 $inventory=Get-Content -LiteralPath (Join-Path $fresh 'licenses/conpty/runtime-manifest.json') -Raw | ConvertFrom-Json
 foreach ($member in $inventory.members) {
  $path=Join-Path $fresh ('runtime/conpty/'+$inventory.version+'/'+$member.path)
  Require ((Get-Item -LiteralPath $path).Length -eq $member.size -and (Hash $path) -ceq $member.sha256) 'package_runtime_identity'
 }
 foreach ($notice in $inventory.notices) {
  $path=Join-Path $fresh ('licenses/conpty/'+$notice.path)
  Require ((Get-Item -LiteralPath $path).Length -eq $notice.size -and (Hash $path) -ceq $notice.sha256) 'package_notice_identity'
 }
 # The executable's early CLI remains usable offline without creating a PTY.
 $exe=Join-Path $fresh 'odytty.exe'
 $version=Owned-Run $exe @('--version') $work 15 1024
 Require ($version.Code -eq 0 -and $version.Out -match '^odytty ') 'package_version'
 $launch=Owned-Run $exe @('--dump-command','echo conpty-package-live') $work 25 16384
 Require ($launch.Code -eq 0 -and $launch.Out.Contains('conpty-package-live')) 'package_default_pty'
 Remove-Item -LiteralPath (Join-Path $fresh 'runtime') -Recurse -Force
 $version=Owned-Run $exe @('--version') $work 15 1024
 Require ($version.Code -eq 0) 'package_lazy_runtime'
 $missing=Owned-Run $exe @('--dump-command','echo conpty-package-live') $work 15 4096
 Require ($missing.Code -ne 0 -and ($missing.Out+$missing.Err).Contains('reinstall the complete Windows package folder')) 'package_missing_runtime'
 # Fresh full-folder upgrade: no in-place mutation of loaded native files.
 $replacement=Join-Path $work 'replacement'
 Expand-Archive -LiteralPath $zip -DestinationPath $replacement
 Remove-Item -LiteralPath $fresh -Recurse -Force
 [IO.Directory]::Move($replacement,$fresh)
 $version=Owned-Run (Join-Path $fresh 'odytty.exe') @('--version') $work 15 1024
 Require ($version.Code -eq 0) 'package_full_folder_upgrade'
 $launch=Owned-Run (Join-Path $fresh 'odytty.exe') @('--dump-command','echo conpty-package-live') $work 25 16384
 Require ($launch.Code -eq 0 -and $launch.Out.Contains('conpty-package-live')) 'package_upgraded_default_pty'
 Write-Output 'conpty_package=pass;zip=true;unicode_spaces=true;notices=true;lazy_cli=true;default_pty=true;missing_runtime_rejected=true;full_folder_upgrade=true'
} finally { Remove-Item -LiteralPath $work -Recurse -Force }
