param([Parameter(Mandatory)][ValidateSet('local','shared')][string]$Mode,[switch]$Prepare)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/phase-live-'+$Mode+'-root.txt')).Trim()
$env:ROOK_HOME=Join-Path $taskRoot 'home'
$env:ROOK_LOG='error'
if($Prepare) {
  $taskParent=(Get-Content (Join-Path $taskRoot 'session-id')).Trim()
  $taskHistory=& target/debug/rook.exe --workspace (Join-Path $taskRoot 'workspace') --json session show $taskParent
  if($LASTEXITCODE-ne0){throw 'Saved history read failed'}
  $taskRows=$taskHistory | ConvertFrom-Json
  $taskWrite=@($taskRows | Where-Object { $_.kind-eq'tool-result' -and $_.label-eq'write_file' })
  if($taskWrite.Count-ne1 -or $taskRows.Count-ne26){throw 'Unexpected fixture history; do not guess fork boundaries'}
  $taskBefore=[int]$taskWrite[0].seq
  $taskAfter=1+[int]($taskRows | Measure-Object seq -Maximum).Maximum
  $taskIds=@{parent=$taskParent;before_at=$taskBefore;after_at=$taskAfter}
  foreach($taskCase in @(@('before',$taskBefore),@('after',$taskAfter))) {
    $taskOutput=& target/debug/rook.exe --workspace (Join-Path $taskRoot 'workspace') session fork $taskParent --at $taskCase[1]
    if($LASTEXITCODE-ne0 -or ($taskOutput-join"`n")-notmatch'[0-9A-HJKMNP-TV-Z]{26}'){throw 'Fork failed'}
    $taskIds[$taskCase[0]]=$Matches[0]
  }
  [IO.File]::WriteAllText((Join-Path $taskRoot 'recovery-ids.json'),($taskIds | ConvertTo-Json))
  Write-Output ('Prepared saved-prefix forks: '+$Mode)
  exit 0
}
$taskIds=Get-Content -Raw (Join-Path $taskRoot 'recovery-ids.json') | ConvertFrom-Json
function Console([string]$Text='',[string]$Key='',[string]$Capture='') {
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
foreach($taskCase in @('before','after','parent')) {
  $taskId=$taskIds.$taskCase
  Console -Text ('/session '+$taskId) -Key enter
  $null=WaitFrame ($Mode+'-reopen-'+$taskCase+'-chat') ('workspace . '+$taskId)
  Console -Text /context -Key enter
  $taskPhase=if($taskCase-eq'before'){'analysis'}else{'implementation'}
  $taskWindow=if($taskCase-eq'before'){65536}else{32768}
  $taskCost=if($taskCase-eq'before'){'0.00003800'}else{'0.00007000'}
  $taskHome=WaitFrame ($Mode+'-reopen-'+$taskCase+'-home') ('Selected: analysis . phase '+$taskPhase)
  if(!$taskHome.Contains('Window '+$taskWindow)){throw 'Fork inspector used the wrong next-request window'}
  if(!$taskHome.Contains('Dispatched: '+$taskPhase+' / '+$taskPhase+'-model')){throw 'Fork inspector used another branch receipt'}
  Console -Key page-down
  $null=WaitFrame ($Mode+'-reopen-'+$taskCase+'-cost') ('Known subtotal: USD '+[regex]::Escape($taskCost))
  Console -Key escape
}
if(@(Get-ChildItem -LiteralPath $taskRoot -Filter 'request-*.json').Count-ne3){throw 'Inspection generated another model call'}
if((Get-Content -Raw (Join-Path $taskRoot 'workspace/untouched.txt'))-ne'untouched phase workspace'){throw 'Inspection changed workspace contents'}
Console -Key escape
Write-Output ('Actual TUI reopen and before/after fork checks passed: '+$Mode)
