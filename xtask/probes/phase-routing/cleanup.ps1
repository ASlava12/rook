param([ValidateSet('local','shared','browser')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/phase-live-'+$Mode+'-root.txt')).Trim()
if($Mode-eq'browser') { & node (Join-Path $PSScriptRoot 'close-browser.mjs'); if($LASTEXITCODE-ne0){throw 'Browser close failed'} }
foreach($taskName in @('daemon','model')) {
  $taskPidFile=Join-Path $taskRoot ($taskName+'.pid')
  if(!(Test-Path -LiteralPath $taskPidFile)){continue}
  $taskPid=[int](Get-Content -LiteralPath $taskPidFile)
  $taskProcess=Get-CimInstance Win32_Process -Filter ('ProcessId = '+$taskPid)
  if($taskProcess){
    if(!$taskProcess.CommandLine.Contains($taskRoot)){throw ('PID no longer belongs to fixture: '+$taskPid)}
    Stop-Process -Id $taskPid -ErrorAction Stop
    Wait-Process -Id $taskPid -Timeout 30 -ErrorAction SilentlyContinue
  }
}
$taskRemaining=@(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($taskRoot) })
if($taskRemaining.Count-ne0){throw 'Fixture still has live processes'}
Write-Output ('Owned fixture processes stopped: '+$taskRoot)
