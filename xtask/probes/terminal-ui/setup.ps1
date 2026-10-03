param([Parameter(Mandatory)][ValidateSet('inline','fullscreen')][string]$Mode)
$ErrorActionPreference='Stop'
$taskPointer='target/scrollback-probe-'+$Mode+'-root.txt'
if(Test-Path $taskPointer){
 $taskOld=(Get-Content $taskPointer).Trim()
 if(@(Get-CimInstance Win32_Process | Where-Object {$_.CommandLine -and $_.CommandLine.Contains($taskOld)}).Count){throw 'Previous owned probe is live'}
}
$taskRoot=Join-Path (Get-Location).Path ('target/scrollback-probe-'+$Mode+'-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $taskRoot | Out-Null
[IO.File]::WriteAllText($taskPointer,$taskRoot)
Write-Output $taskRoot
