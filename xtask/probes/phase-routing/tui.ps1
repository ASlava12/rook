param([Parameter(Mandatory)][ValidateSet('local','shared')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/phase-live-'+$Mode+'-root.txt')).Trim()
$taskTag=$Mode.ToUpperInvariant()+'_PHASE'
function Console([string]$Text='', [string]$Key='', [string]$Capture='') {
  $taskArgs=@('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'console.ps1'),'-Mode',$Mode)
  if($Text){$taskArgs+=@('-Text',$Text)}
  if($Key){$taskArgs+=@('-Key',$Key)}
  if($Capture){$taskArgs+=@('-Capture',$Capture)}
  & powershell @taskArgs | Out-Null
  if($LASTEXITCODE-ne0){throw 'Console helper failed'}
}
function WaitFrame([string]$Name,[string]$Pattern) {
  $taskDeadline=(Get-Date).AddSeconds(30)
  do {
    Console -Capture $Name
    $taskFrame=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot ($Name+'.txt'))
    if($taskFrame-match$Pattern){return $taskFrame}
    if((Get-Date)-gt$taskDeadline){throw ('Frame never contained '+$Pattern)}
    Start-Sleep -Milliseconds 100
  }while($true)
}
Console -Text $taskTag -Key enter
$taskDeadline=(Get-Date).AddSeconds(30)
while(!(Test-Path (Join-Path $taskRoot ('started-'+$taskTag+'-2')))){if((Get-Date)-gt$taskDeadline){throw 'Implementation request did not start'};Start-Sleep -Milliseconds 100}
Console -Text /context -Key enter
$taskLive=WaitFrame ($Mode+'-live-home') 'Selected: analysis . phase analysis'
if(!$taskLive.Contains('Window 32768') -or !$taskLive.Contains('Dispatched: analysis / analysis-model')){throw 'Live inspector lost its effective window or historical source'}
Console -Key page-down
$taskCost=WaitFrame ($Mode+'-live-cost') '2 started . 1 completed'
if(!$taskCost.Contains('1 pending')){throw 'Current physical attempt was hidden'}
[IO.File]::WriteAllText((Join-Path $taskRoot ('release-'+$taskTag)),'1')
$taskDeadline=(Get-Date).AddSeconds(30)
do {
  Console -Key r
  $taskComplete=WaitFrame ($Mode+'-complete-home') 'Selected: analysis . phase implementation'
  Console -Key page-down
  Console -Capture ($Mode+'-complete-cost')
  $taskCost=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot ($Mode+'-complete-cost.txt'))
  if($taskCost.Contains('Known subtotal: USD 0.00007000') -and $taskCost-match'3 started . 3 completed'){break}
  if((Get-Date)-gt$taskDeadline){throw 'Completion never retained all three model receipts'}
  Start-Sleep -Milliseconds 100
}while($true)
if(!$taskCost.Contains('Dispatched: implementation / implementation-model')){throw 'Final inspector lost actual dispatch'}
Console -Key escape
Console -Capture ($Mode+'-chat-completed')
$taskChat=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot ($Mode+'-chat-completed.txt'))
if($taskChat-notmatch'[0-9A-HJKMNP-TV-Z]{26}'){throw 'Session ID is missing from the terminal header'}
[IO.File]::WriteAllText((Join-Path $taskRoot 'session-id'),$Matches[0])
Console -Key escape
Write-Output ('Live and final TUI phase checks passed: '+$Mode+' '+$taskRoot)
