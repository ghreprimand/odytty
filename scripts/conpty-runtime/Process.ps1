# SPDX-License-Identifier: GPL-3.0-only
function Require([bool]$Condition,[string]$Reason) { if (-not $Condition) { throw $Reason } }
function Hash([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
# Every launched process is owned. Drain both streams concurrently in fixed
# chunks; retain at most the explicit bound, then kill rather than buffer more.
function Is-ProcessReason([string]$Reason) {
 return $Reason -cmatch '\Aowned_(configure|start|stdout_begin|stderr_begin|poll|stdout_result|stderr_result|stdout_append|stderr_append|timeout_kill|kill_wait|exit_code|cleanup|invoke|other)_(?:win32(?:_(file_not_found|path_not_found|access_denied|invalid_handle|bad_format|invalid_parameter|invalid_name|module_not_found|name_too_long|machine_mismatch|elevation_required|other))?|(?:invalid_operation|argument|io|disposed|not_supported|denied|timeout|method_binding|method_invocation|parameter_binding|property_binding|null_reference|other))\z'
}
function Native-ErrorKind([int]$Code) {
 switch ($Code) {
  2 {return 'file_not_found'}
  3 {return 'path_not_found'}
  5 {return 'access_denied'}
  6 {return 'invalid_handle'}
  11 {return 'bad_format'}
  193 {return 'bad_format'}
  87 {return 'invalid_parameter'}
  123 {return 'invalid_name'}
  126 {return 'module_not_found'}
  206 {return 'name_too_long'}
  216 {return 'machine_mismatch'}
  740 {return 'elevation_required'}
  default {return 'other'}
 }
}
function Select-Application([object[]]$Applications,[string]$ExpectedBase) {
 Require ($null -ne $Applications -and $Applications.Count -gt 0) 'application_resolution_missing'
 Require ($Applications.Count -le 32) 'application_resolution_excessive'
 $first=$Applications[0]
 Require ($first.CommandType -eq [Management.Automation.CommandTypes]::Application) 'application_kind'
 $file=[IO.Path]::GetFullPath([string]$first.Path)
 Require ([IO.Path]::IsPathFullyQualified($file)) 'application_path'
 Require ([IO.Path]::GetFileName($file) -ieq $ExpectedBase) 'application_identity'
 Require ([IO.File]::Exists($file)) 'application_missing'
 $resolution=if ($Applications.Count -eq 1) {'single'} else {'multiple'}
 return @{Path=$file;Resolution=$resolution;LegacySourceExists=[IO.File]::Exists([string]$Applications.Source)}
}
function Resolve-Application([string]$Name,[string]$ExpectedBase) {
 $applications=@(Get-Command -Name $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 33)
 return (Select-Application $applications $ExpectedBase)
}
function Start-Facts([string]$Exe,[string]$Working,[string]$ExpectedBase,[string]$Resolution,[bool]$LegacySourceExists) {
 $resolution=if ($Resolution -cin @('single','multiple')) {$Resolution} else {'other'}
 return ('git_start_inputs resolution={0} legacy_source_exists={1} executable_exists={2} working_directory_exists={3} expected_basename={4} filename_fully_qualified={5}' -f $resolution,$LegacySourceExists.ToString().ToLowerInvariant(),[IO.File]::Exists($Exe).ToString().ToLowerInvariant(),[IO.Directory]::Exists($Working).ToString().ToLowerInvariant(),([IO.Path]::GetFileName($Exe) -ieq $ExpectedBase).ToString().ToLowerInvariant(),[IO.Path]::IsPathFullyQualified($Exe).ToString().ToLowerInvariant())
}
function Process-ErrorReason([string]$Operation,[Exception]$Failure) {
 $allowed=@('configure','start','stdout_begin','stderr_begin','poll','stdout_result','stderr_result','stdout_append','stderr_append','timeout_kill','kill_wait','exit_code','cleanup','invoke','other')
 $op=if ($Operation -cin $allowed) { $Operation } else { 'other' }
 $kind='other'; $native='other'; $current=$Failure
 # PowerShell wraps .NET exceptions. Inspect at most four known type names;
 # never publish a message, path, HResult, stack, native error code or type name.
 for ($depth=0; $depth -lt 4 -and $null -ne $current; $depth++) {
  if ($current -is [Management.Automation.ParameterBindingException]) { $kind='parameter_binding'; break }
  $specific=$true
  switch -Exact ($current.GetType().FullName) {
   'System.ComponentModel.Win32Exception' { $kind='win32'; $native=Native-ErrorKind $current.NativeErrorCode }
   'System.NullReferenceException' { $kind='null_reference' }
   'System.Management.Automation.PropertyNotFoundException' { $kind='property_binding' }
   'System.Management.Automation.GetValueInvocationException' { $kind='property_binding'; $specific=$false }
   'System.Management.Automation.SetValueInvocationException' { $kind='property_binding'; $specific=$false }
   'System.Management.Automation.SetValueException' { $kind='property_binding' }
   'System.InvalidOperationException' { $kind='invalid_operation' }
   'System.ArgumentException' { $kind='argument' }
   'System.ArgumentNullException' { $kind='argument' }
   'System.ArgumentOutOfRangeException' { $kind='argument' }
   'System.IO.IOException' { $kind='io' }
   'System.IO.FileNotFoundException' { $kind='io' }
   'System.IO.DirectoryNotFoundException' { $kind='io' }
   'System.ObjectDisposedException' { $kind='disposed' }
   'System.NotSupportedException' { $kind='not_supported' }
   'System.UnauthorizedAccessException' { $kind='denied' }
   'System.Security.SecurityException' { $kind='denied' }
   'System.TimeoutException' { $kind='timeout' }
   'System.Management.Automation.MethodException' { $kind='method_binding' }
   'System.Management.Automation.PSArgumentException' { $kind='method_binding' }
   'System.Management.Automation.PSInvalidCastException' { $kind='method_binding' }
   'System.Management.Automation.MethodInvocationException' { $kind='method_invocation'; $specific=$false }
   'System.Reflection.TargetInvocationException' { $kind='method_invocation'; $specific=$false }
   default { $specific=$false }
  }
  if ($specific) { break }
  $current=$current.InnerException
 }
 if ($kind -ceq 'win32') { return ('owned_'+$op+'_win32_'+$native) }
 return ('owned_'+$op+'_'+$kind)
}
function Owned-Run([string]$Exe,[string[]]$Arguments,[string]$Working,[int]$Seconds,[int]$Cap,[Text.Encoding]$Encoding=$null) {
 $operation='configure'
 try {
 $info = [Diagnostics.ProcessStartInfo]::new()
 $info.FileName=$Exe; $info.WorkingDirectory=$Working; $info.UseShellExecute=$false
 $info.CreateNoWindow=$true; $info.RedirectStandardOutput=$true; $info.RedirectStandardError=$true
 if ($null -ne $Encoding) { $info.StandardOutputEncoding=$Encoding; $info.StandardErrorEncoding=$Encoding }
 foreach ($arg in $Arguments) { $info.ArgumentList.Add($arg) }
 $process=[Diagnostics.Process]::new(); $process.StartInfo=$info
 $started=$false; $out=[Text.StringBuilder]::new(); $err=[Text.StringBuilder]::new()
 $ob=[char[]]::new(1024); $eb=[char[]]::new(1024)
 $expired=$false; $oversize=$false
 try {
  $operation='start'
  $started=$process.Start(); Require $started 'process_start'
  $operation='stdout_begin'
  $ot=$process.StandardOutput.ReadAsync($ob,0,$ob.Length)
  $operation='stderr_begin'
  $et=$process.StandardError.ReadAsync($eb,0,$eb.Length)
  $oe=$false; $ee=$false; $clock=[Diagnostics.Stopwatch]::StartNew()
  $operation='poll'
  while (-not ($oe -and $ee -and $process.HasExited)) {
   if ($clock.Elapsed.TotalSeconds -ge $Seconds) { $expired=$true; break }
   $operation='stdout_result'
   if (-not $oe -and $ot.IsCompleted) {
    $n=$ot.GetAwaiter().GetResult()
    if ($out.Length+$n -gt $Cap) { $oversize=$true; break }
    if ($n -eq 0) { $oe=$true } else { $operation='stdout_append'; [void]$out.Append($ob,0,$n); $operation='stdout_begin'; $ot=$process.StandardOutput.ReadAsync($ob,0,$ob.Length) }
   }
   $operation='stderr_result'
   if (-not $ee -and $et.IsCompleted) {
    $n=$et.GetAwaiter().GetResult()
    if ($err.Length+$n -gt $Cap) { $oversize=$true; break }
    if ($n -eq 0) { $ee=$true } else { $operation='stderr_append'; [void]$err.Append($eb,0,$n); $operation='stderr_begin'; $et=$process.StandardError.ReadAsync($eb,0,$eb.Length) }
   }
   $operation='poll'
   Start-Sleep -Milliseconds 10
  }
  if ($expired -or $oversize) {
   $operation='timeout_kill'
   if (-not $process.HasExited) { $process.Kill($true) }
   $operation='kill_wait'
   Require ($process.WaitForExit(5000)) 'owned_kill_timeout'
   if ($expired) { throw 'external_watchdog' } else { throw 'output_cap' }
  }
  $operation='exit_code'
  return @{Code=$process.ExitCode; Out=$out.ToString(); Err=$err.ToString()}
 } finally {
  $savedOperation=$operation
  $operation='cleanup'
  if ($started -and -not $process.HasExited) {
   $process.Kill($true)
   if (-not $process.WaitForExit(5000)) { $process.Dispose(); throw 'owned_finally_kill_timeout' }
  }
  $process.Dispose()
  $operation=$savedOperation
 }

 } catch {
  $known=@('process_start','owned_kill_timeout','external_watchdog','output_cap','owned_finally_kill_timeout')
  if ($_.Exception.Message -cin $known) { throw $_.Exception.Message }
  throw (Process-ErrorReason $operation $_.Exception)
 }
}
