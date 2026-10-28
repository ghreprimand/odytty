# SPDX-License-Identifier: GPL-3.0-only
# Online acquisition is explicit; cargo/build.rs never downloads native code.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Destination, [string]$Dotnet = 'dotnet')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
. (Join-Path $PSScriptRoot 'Verify.ps1')
Require ($PSVersionTable.PSVersion -ge [version]'7.4') 'runtime_requires_powershell_7_4'
Require ($IsWindows) 'runtime_requires_windows'
$destinationPath = [IO.Path]::GetFullPath($Destination)
Require (-not (Test-Path -LiteralPath $destinationPath)) 'runtime_destination_must_be_new'
$parent = [IO.Path]::GetDirectoryName($destinationPath)
[void][IO.Directory]::CreateDirectory($parent)
$work = Join-Path $parent ('.conpty-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($work)
try {
 $runtime = Join-Path $work 'verified'
 [void][IO.Directory]::CreateDirectory($runtime)
 $application = Resolve-Application $Dotnet 'dotnet.exe'
 # Fail instead of silently rolling forward to a different NuGet verifier.
 [IO.File]::WriteAllText((Join-Path $work 'global.json'), '{"sdk":{"version":"10.0.100","rollForward":"disable","allowPrerelease":false}}')
 $sdk = Owned-Run $application.Path @('--version') $work 15 1024
 Require ($sdk.Code -eq 0 -and $sdk.Out.Trim() -ceq '10.0.100') 'runtime_sdk_version'
 Prepare-Runtime $runtime $work $application.Path
 # Publish only after the complete package and all native signatures passed.
 [IO.Directory]::Move($runtime, $destinationPath)
} finally {
 if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
