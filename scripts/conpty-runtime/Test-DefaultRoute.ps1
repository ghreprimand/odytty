# SPDX-License-Identifier: GPL-3.0-only
# Compare the same new test through the frozen and candidate public spawn route.
# Source restoration is unconditional; no diagnostic override is compiled in.
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
$repository=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$source=Join-Path $repository 'src/pty/windows.rs'
$candidate=[IO.File]::ReadAllBytes($source)
$git=Resolve-Application 'git' 'git.exe'
$cargo=Resolve-Application 'cargo' 'cargo.exe'
$baseline=Owned-Run $git.Path @('show','1ca3d120b972d6d7424333ccafcdeec01262c7bd:src/pty/windows.rs') $repository 15 131072 ([Text.Encoding]::Latin1)
Require ($baseline.Code -eq 0 -and $baseline.Err.Length -eq 0) 'baseline_source'
$baselineBytes=[Text.Encoding]::Latin1.GetBytes($baseline.Out)
Require ([Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($baselineBytes)).ToLowerInvariant() -ceq 'b22bcac159e4c4a6fadaea9f308a8d1369d0a203ddda586693f87572df6c1154') 'baseline_source_hash'
try {
 [IO.File]::WriteAllBytes($source,$baselineBytes)
 $arguments=@('test','--locked','--test','conpty_graphics_transport','default_public_spawn_delivers_fixed_sixel_and_core_state','--','--exact','--show-output','--test-threads=1')
 $result=Owned-Run $cargo.Path $arguments $repository 900 65536
 Require ($result.Code -ne 0 -and ($result.Out+$result.Err).Contains('exact fixed DCS before Terminal')) 'baseline_default_route_failure'
 Write-Output 'conpty_default_route_before=fail;cause=missing_exact_fixed_dcs;tests=1;source=1ca3d120'
} finally { [IO.File]::WriteAllBytes($source,$candidate) }
$result=Owned-Run $cargo.Path $arguments $repository 900 65536
Require ($result.Code -eq 0 -and ($result.Out+$result.Err).Contains('1 passed; 0 failed')) 'candidate_default_route_pass'
Write-Output 'conpty_default_route_after=pass;tests=1;exact_dcs=true;core_image=true;core_cursor=true;eof=true'
