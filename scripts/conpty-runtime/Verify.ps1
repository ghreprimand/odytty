# SPDX-License-Identifier: GPL-3.0-only
# Loaded by the bounded runner. Never executes package content.
function Get-NugetDiagnostics([string]$Output) {
 return @([regex]::Matches($Output,'\bNU30\d\d\b') | ForEach-Object { $_.Value } | Sort-Object -Unique)
}
# Require-mode trust and CLI fingerprints are independent verifier inputs.
# Only the manifest-bound published author certificates identify this supplier.
function New-NugetConfig([string[]]$Fingerprints) {
 Require ($null -ne $Fingerprints -and $Fingerprints.Count -eq 2) 'runtime_nuget_signature'
 Require ($Fingerprints[0] -cne $Fingerprints[1]) 'runtime_nuget_signature'
 $certificates=foreach ($fingerprint in $Fingerprints) {
  Require ($fingerprint -cmatch '\A[A-F0-9]{64}\z') 'runtime_nuget_signature'
  '<certificate fingerprint="'+$fingerprint+'" hashAlgorithm="SHA256" allowUntrustedRoot="false" />'
 }
 return '<configuration><packageSources><clear /></packageSources><config><add key="signatureValidationMode" value="require" /></config><trustedSigners><author name="microsoft">'+[string]::Join('',$certificates)+'</author></trustedSigners></configuration>'
}
function Get-NugetTrustFailure([string]$Output) {
 if ($Output -notmatch '\bNU3034\b') { return 'unclassified' }
 if ($Output -match 'no trusted signers were specified') { return 'missing_trusted_signers' }
 if ($Output -match 'not match' -and $Output -match 'fingerprint') { return 'signer_mismatch' }
 return 'unclassified'
}
function Prepare-Runtime([string]$Destination,[string]$Work,[string]$Dotnet) {
 Require ($IsWindows) 'runtime_requires_windows'
 $pin=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'pin.json') -Raw | ConvertFrom-Json
 $archive=Join-Path $Work 'runtime.nupkg'
 $client=[Net.Http.HttpClient]::new(); $client.Timeout=[TimeSpan]::FromSeconds(45)
 try {
  $response=$client.GetAsync($pin.url,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
  try {
   Require $response.IsSuccessStatusCode 'runtime_download_status'
   $inputStream=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
   $outputStream=[IO.File]::Create($archive)
   try {
    $buffer=[byte[]]::new(16384); $total=0; $clock=[Diagnostics.Stopwatch]::StartNew()
    while (($n=$inputStream.ReadAsync($buffer,0,$buffer.Length).WaitAsync([TimeSpan]::FromSeconds(10)).GetAwaiter().GetResult()) -ne 0) {
     $total+=$n
     Require ($total -le $pin.size -and $clock.Elapsed.TotalSeconds -lt 45) 'runtime_download_bound'
     $outputStream.Write($buffer,0,$n)
    }
    Require ($total -eq $pin.size) 'runtime_download_size'
   } finally { $outputStream.Dispose(); $inputStream.Dispose() }
  } finally { $response.Dispose() }
 } finally { $client.Dispose() }
 $digest=[Convert]::ToBase64String([Security.Cryptography.SHA512]::HashData([IO.File]::ReadAllBytes($archive)))
 Require ($digest -ceq $pin.sha512_base64) 'runtime_archive_hash'
 $config=Join-Path $Work 'NuGet.Config'
 [IO.File]::WriteAllText($config,(New-NugetConfig $pin.microsoft_author_certificates))
 # Supported NuGet verifier validates ZIP integrity, signatures, chains and timestamps.
 # Fingerprints come from Microsoft's published certificate transition notice.
 $arguments=@('nuget','verify',$archive,'--all','--verbosity','detailed','--configfile',$config)
 foreach ($fingerprint in $pin.microsoft_author_certificates) { $arguments+=@('--certificate-fingerprint',$fingerprint) }
 $env:NUGET_CERT_REVOCATION_MODE='online'
 $verify=Owned-Run $Dotnet $arguments $Work 120 65536
 [IO.File]::WriteAllText((Join-Path $Work 'nuget-verification.log'),$verify.Out+$verify.Err)
 foreach ($code in @(Get-NugetDiagnostics ($verify.Out+$verify.Err))) { Write-Output ('runtime_nuget_diagnostic='+$code) }
 if (($verify.Out+$verify.Err) -match '\bNU3034\b') { Write-Output ('runtime_nuget_trust_failure='+(Get-NugetTrustFailure ($verify.Out+$verify.Err))) }
 Require ($verify.Code -eq 0 -and $verify.Out -notmatch '\bNU30\d\d\b' -and $verify.Err -notmatch '\bNU30\d\d\b') 'runtime_nuget_signature'
 Write-Output 'runtime_nuget_signature=verified;revocation=online;publisher=published_microsoft_fingerprint'
 $zip=[IO.Compression.ZipFile]::OpenRead($archive)
 try {
  Require ($zip.Entries.Count -eq $pin.members.Count -and $zip.Entries.Count -le 32) 'runtime_member_count'
  $seen=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
  $expanded=0
  foreach ($entry in $zip.Entries) {
   Require ($seen.Add($entry.FullName)) 'runtime_duplicate_member'
   $known=@($pin.members | Where-Object { $_.name -ceq $entry.FullName })
   Require ($known.Count -eq 1 -and $entry.Length -eq $known[0].size) 'runtime_member_identity'
   $expanded+=$entry.Length; Require ($expanded -le 20000000) 'runtime_expanded_cap'
   $stream=$entry.Open(); $memory=[IO.MemoryStream]::new()
   try { $bytes=[byte[]]::new([int]$entry.Length); $stream.ReadExactly($bytes,0,$bytes.Length); Require ($stream.ReadByte() -eq -1) 'runtime_member_bound' } finally { $memory.Dispose(); $stream.Dispose() }
   Require ([Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant() -ceq $known[0].sha256) 'runtime_member_hash'
   $relative=switch -CaseSensitive ($entry.FullName) {
    'runtimes/win-x64/native/conpty.dll' {'conpty.dll'}
    'build/native/runtimes/x64/OpenConsole.exe' {'x64/OpenConsole.exe'}
    'build/native/runtimes/arm64/OpenConsole.exe' {'arm64/OpenConsole.exe'}
    default {$null}
   }
   if ($null -ne $relative) {
    $path=Join-Path $Destination $relative
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))
    [IO.File]::WriteAllBytes($path,$bytes)
    Require ($bytes.Length -ge 256 -and $bytes[0] -eq 77 -and $bytes[1] -eq 90) 'runtime_dos_header'
    $pe=[BitConverter]::ToInt32($bytes,60)
    Require ($pe -ge 64 -and $pe -le $bytes.Length-26 -and [BitConverter]::ToUInt32($bytes,$pe) -eq 17744) 'runtime_pe_header'
    $machine=[BitConverter]::ToUInt16($bytes,$pe+4)
    $expected=if ($relative -ceq 'arm64/OpenConsole.exe') {0xaa64} else {0x8664}
    Require ($machine -eq $expected) 'runtime_pe_architecture'
    $signature=Get-AuthenticodeSignature -LiteralPath $path
    Require ($signature.Status -eq 'Valid' -and $null -ne $signature.SignerCertificate -and $null -ne $signature.TimeStamperCertificate) 'runtime_authenticode'
    Require ($signature.SignerCertificate.Subject -match '(?:^|,\s*)O=Microsoft Corporation(?:,|$)') 'runtime_pe_publisher'
    foreach ($pair in @(@($signature.SignerCertificate,'1.3.6.1.5.5.7.3.3'),@($signature.TimeStamperCertificate,'1.3.6.1.5.5.7.3.8'))) {
     $chain=[Security.Cryptography.X509Certificates.X509Chain]::new()
     try {
      $chain.ChainPolicy.RevocationMode='Online'; $chain.ChainPolicy.RevocationFlag='ExcludeRoot'
      $chain.ChainPolicy.UrlRetrievalTimeout=[TimeSpan]::FromSeconds(15)
      [void]$chain.ChainPolicy.ApplicationPolicy.Add([Security.Cryptography.Oid]::new($pair[1]))
      Require ($chain.Build($pair[0])) 'runtime_online_chain'
     } finally { $chain.Dispose() }
    }
    $certHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($signature.SignerCertificate.RawData)).ToLowerInvariant()
    $timestampHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($signature.TimeStamperCertificate.RawData)).ToLowerInvariant()
    $version=[Diagnostics.FileVersionInfo]::GetVersionInfo($path)
    Write-Output ('runtime_member='+$relative+';sha256='+$known[0].sha256+';authenticode=valid;online_chain=valid;signer_sha256='+$certHash+';timestamp_signer_sha256='+$timestampHash)
    Write-Output ('runtime_file_version={0}.{1}.{2}.{3}' -f $version.FileMajorPart,$version.FileMinorPart,$version.FileBuildPart,$version.FilePrivatePart)
   }
  }
 } finally { $zip.Dispose() }
 Write-Output 'runtime_verification=complete;candidate_execution=not_started'
}
