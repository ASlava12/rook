param([ValidateSet('local','shared','browser')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRoot=[IO.Path]::GetFullPath((Get-Content ('target/extension-live-'+$Mode+'-root.txt')).Trim())
if($Mode-eq'browser') { & node (Join-Path $PSScriptRoot '../phase-routing/close-browser.mjs') $taskRoot; if($LASTEXITCODE-ne0){throw 'Owned browser close failed'} }
foreach($taskName in @('daemon','model')) {
 $taskPidFile=Join-Path $taskRoot ($taskName+'.pid')
 if(!(Test-Path $taskPidFile)){continue}
 $taskPid=[int](Get-Content $taskPidFile)
 $taskProcess=Get-CimInstance Win32_Process -Filter ('ProcessId = '+$taskPid)
 if($taskProcess){if(!$taskProcess.CommandLine.Contains($taskRoot)){throw 'PID no longer belongs to fixture'};Stop-Process -Id $taskPid;Wait-Process -Id $taskPid -Timeout 30 -ErrorAction SilentlyContinue}
}
$taskRemaining=@(Get-CimInstance Win32_Process | Where-Object {$_.CommandLine -and $_.CommandLine.Contains($taskRoot)})
if($taskRemaining.Count){throw 'Fixture still has live processes; quit the owned TUI first'}
Write-Output ('Owned fixture processes stopped: '+$taskRoot)
