# SPDX-License-Identifier: GPL-3.0-only
# Pure contracts and bounded owned-process checks, also runnable on non-Windows.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
foreach ($name in @('Process.ps1','Verify.ps1','Prepare.ps1','Stage.ps1','Package.ps1','Test-Package.ps1','Test-DefaultRoute.ps1')) {
 $tokens=$null; $errors=$null
 [void][Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $name),[ref]$tokens,[ref]$errors)
 if ($errors.Count -ne 0) { throw 'conpty_tools_parse' }
}
. (Join-Path $PSScriptRoot 'Process.ps1')
. (Join-Path $PSScriptRoot 'Verify.ps1')
$pin=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'pin.json') -Raw | ConvertFrom-Json
[xml]$config=New-NugetConfig $pin.microsoft_author_certificates
Require ($config.configuration.config.add.value -ceq 'require') 'test_signature_require'
Require ($config.SelectNodes('/configuration/trustedSigners/author/certificate').Count -eq 2) 'test_author_count'
foreach ($certificate in $config.SelectNodes('/configuration/trustedSigners/author/certificate')) {
 Require ($certificate.allowUntrustedRoot -ceq 'false' -and $certificate.hashAlgorithm -ceq 'SHA256') 'test_author_chain'
}
foreach ($pins in @(@(),@('bad','pin'),@(('A'*64),('A'*64)),@(('A'*64),('B'*64),('C'*64)))) {
 $rejected=$false
 try { [void](New-NugetConfig $pins) } catch { $rejected=$true }
 Require $rejected 'test_invalid_author_pins'
}
Require (@(Get-NugetDiagnostics 'NU3034 NU3018 NU3034').Count -eq 2) 'test_diagnostic_codes'
$exe=(Get-Process -Id $PID).Path
$result=Owned-Run $exe @('-NoLogo','-NoProfile','-NonInteractive','-Command','[Console]::Out.Write("owned"); [Console]::Error.Write("error"); exit 7') $PSScriptRoot 15 1024
Require ($result.Code -eq 7 -and $result.Out -ceq 'owned' -and $result.Err -ceq 'error') 'test_owned_streams'
foreach ($script in @('[Console]::Write("x"*2048)','Start-Sleep -Seconds 10')) {
 $rejected=$false
 try { [void](Owned-Run $exe @('-NoLogo','-NoProfile','-Command',$script) $PSScriptRoot 1 1024) } catch { $rejected=$_.Exception.Message -cin @('output_cap','external_watchdog') }
 Require $rejected 'test_owned_bounds'
}
$savedEncoding=[Console]::OutputEncoding
try {
 [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
 $payload=[byte[]]@(0,65,195,169,228,184,173,128,255,192,175,10)
 $script='$bytes=[byte[]]@(0,65,195,169,228,184,173,128,255,192,175,10); $stream=[Console]::OpenStandardOutput(); $stream.Write($bytes,0,$bytes.Length); $stream.Flush()'
 $result=Owned-Run $exe @('-NoLogo','-NoProfile','-NonInteractive','-Command',$script) $PSScriptRoot 15 1024 ([Text.Encoding]::Latin1)
 Require ($result.Code -eq 0) 'test_binary_child'
 $actual=[Text.Encoding]::Latin1.GetBytes($result.Out)
 Require ([Convert]::ToHexString($actual) -ceq [Convert]::ToHexString($payload)) 'test_lossless_binary_roundtrip'
 $path=Join-Path ([IO.Path]::GetTempPath()) ('conpty-byte-roundtrip-'+[guid]::NewGuid().ToString('N'))
 try {
  [IO.File]::WriteAllBytes($path,$actual)
  Require ([Convert]::ToHexString([IO.File]::ReadAllBytes($path)) -ceq [Convert]::ToHexString($payload)) 'test_lossless_file_roundtrip'
 } finally { if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force } }
 $baseline=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Test-DefaultRoute.ps1') -Raw
 Require ($baseline.Contains('131072 ([Text.Encoding]::Latin1)') -and $baseline.Contains('[Text.Encoding]::Latin1.GetBytes($baseline.Out)') -and $baseline.Contains('[IO.File]::WriteAllBytes($source,$baselineBytes)')) 'test_baseline_lossless_contract'
 Write-Output 'conpty_lossless_baseline=pass;non_ascii=true;invalid_utf8=true;nul=true;file_bytes=true'
} finally { [Console]::OutputEncoding=$savedEncoding }
Write-Output 'conpty_tools=pass;parse=7;author_policy=true;streams=true;bounds=true'
