param([ValidateSet('local','shared','browser')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRepo=(Get-Location).Path
$taskPointer=Join-Path $taskRepo ('target/extension-live-'+$Mode+'-root.txt')
if(Test-Path $taskPointer){$taskOld=(Get-Content $taskPointer).Trim();if(@(Get-CimInstance Win32_Process | Where-Object {$_.CommandLine -and $_.CommandLine.Contains($taskOld)}).Count){throw 'Previous owned fixture is live; clean it before replacing its pointer'}}
$taskRoot=Join-Path $taskRepo ('target/extension-live-'+$Mode+'-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $taskRoot,(Join-Path $taskRoot 'home'),(Join-Path $taskRoot 'workspace'),(Join-Path $taskRoot 'edge') | Out-Null
[IO.File]::WriteAllText($taskPointer,$taskRoot)
$taskModel=Start-Process -FilePath (Get-Command node).Source -ArgumentList @(('"'+(Join-Path $PSScriptRoot '../phase-routing/model.mjs')+'"'),('"'+$taskRoot+'"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $taskRoot 'model.out') -RedirectStandardError (Join-Path $taskRoot 'model.err')
$taskModel.Id | Set-Content (Join-Path $taskRoot 'model.pid')
$taskDeadline=(Get-Date).AddSeconds(30)
while(!(Test-Path (Join-Path $taskRoot 'model-address'))){if((Get-Date)-gt$taskDeadline){throw 'Model did not start'};Start-Sleep -Milliseconds 100}
$taskAddress=(Get-Content (Join-Path $taskRoot 'model-address')).Trim()
$taskCommand='powershell -NoProfile -ExecutionPolicy Bypass -File "'+(Join-Path $PSScriptRoot 'hook.ps1')+'" -Workspace "'+(Join-Path $taskRoot 'workspace')+'"'
$taskCommandJson=$taskCommand | ConvertTo-Json -Compress
$taskConfig=@"
[agent]
model='initial'
install_servers=false
one_script=false
plan_first=false
[models.initial]
api='openai'
model='initial-model'
url='$taskAddress/v1'
context_window=32768
[[hooks]]
event='prompt'
ui=true
ui_stream=true
timeout_secs=5
command=$taskCommandJson
"@
[IO.File]::WriteAllText((Join-Path $taskRoot 'home/config.toml'),$taskConfig)
$env:ROOK_HOME=Join-Path $taskRoot 'home';$env:ROOK_LOG='error'
if($Mode-ne'local'){
 $taskDaemon=Start-Process -FilePath (Join-Path $taskRepo 'target/debug/rookd.exe') -ArgumentList @('--workspace',('"'+(Join-Path $taskRoot 'workspace')+'"'),'--port','0') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $taskRoot 'daemon.out') -RedirectStandardError (Join-Path $taskRoot 'daemon.err')
 $taskDaemon.Id | Set-Content (Join-Path $taskRoot 'daemon.pid')
 $taskDeadline=(Get-Date).AddSeconds(30)
 while(!(Test-Path (Join-Path $taskRoot 'home/rookd.addr'))){if((Get-Date)-gt$taskDeadline){throw 'Daemon did not start'};Start-Sleep -Milliseconds 100}
}
if($Mode-eq'browser'){
 $taskEdge=Start-Process -FilePath 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe' -ArgumentList @('--headless=new','--disable-gpu','--no-first-run','--remote-debugging-port=0',('--user-data-dir="'+(Join-Path $taskRoot 'edge')+'"'),'about:blank') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $taskRoot 'edge.out') -RedirectStandardError (Join-Path $taskRoot 'edge.err')
 $taskEdge.Id | Set-Content (Join-Path $taskRoot 'edge.pid')
}
Write-Output $taskRoot
