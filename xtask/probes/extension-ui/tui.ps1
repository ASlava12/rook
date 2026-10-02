param([Parameter(Mandatory)][ValidateSet('local','shared')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/extension-live-'+$Mode+'-root.txt')).Trim()
function Console([string]$Text='', [string]$Key='', [string]$Capture='') {
 $taskArgs=@('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot '../phase-routing/console.ps1'),'-Mode',$Mode,'-RootPath',$taskRoot)
 if($Text){$taskArgs+=@('-Text',$Text)};if($Key){$taskArgs+=@('-Key',$Key)};if($Capture){$taskArgs+=@('-Capture',$Capture)}
 & powershell @taskArgs | Out-Null
 if($LASTEXITCODE-ne0){throw 'Native console driver failed'}
}
function WaitFrame([string]$Name,[string]$Pattern) {
 $taskDeadline=(Get-Date).AddSeconds(30)
 do{Console -Capture $Name;$taskFrame=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot ($Name+'.txt'));if($taskFrame-match$Pattern){return $taskFrame};if((Get-Date)-gt$taskDeadline){throw ('Frame never contained '+$Pattern)};Start-Sleep -Milliseconds 100}while($true)
}
Console -Text ($Mode.ToUpperInvariant()+'_EXTENSION') -Key enter
$taskFirst=WaitFrame ($Mode+'-form-1') 'question 1 of 4'
if(!$taskFirst.Contains('Typed extension setup') -or !$taskFirst.Contains('Extension hook prompt #1') -or !$taskFirst.Contains('Name')){throw 'Source, title or first field was clipped'}
Console -Text PRIVATE_TUI_INPUT -Key enter
$null=WaitFrame ($Mode+'-form-2') 'Target \(choose listed options\)'
Console -Text 2 -Key enter
$null=WaitFrame ($Mode+'-form-3') 'Continue \(Yes or No\)'
Console -Text 2 -Key enter
$null=WaitFrame ($Mode+'-form-4') 'Count \(1\.\.=10\)'
Console -Text 7 -Key enter
$null=WaitFrame ($Mode+'-form-finished') 'IMPLEMENTATION_SEED'
$taskAnswerPath=Join-Path $taskRoot 'workspace/hook-answer.json'
if((Get-Item $taskAnswerPath).Length-gt8192){throw 'Fixture answer exceeds admitted limit'}
$taskAnswer=Get-Content -Raw -Encoding utf8 $taskAnswerPath | ConvertFrom-Json
if($taskAnswer.form_answer.status-ne'answered' -or $taskAnswer.form_answer.values.name-ne'PRIVATE_TUI_INPUT' -or $taskAnswer.form_answer.values.target-ne'remote' -or $taskAnswer.form_answer.values.confirm-ne$false -or $taskAnswer.form_answer.values.count-ne7){throw 'Typed native answer mismatch'}
Console -Text /context -Key enter
$taskFound=$false
for($taskPage=0;$taskPage-lt24;$taskPage++){
 Console -Capture ($Mode+'-form-context-'+$taskPage)
 $taskFrame=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot ($Mode+'-form-context-'+$taskPage+'.txt'))
 if($taskFrame.Contains('Typed extension setup') -and $taskFrame.Contains('answered')){$taskFound=$true;break}
 Console -Key page-down
}
if(!$taskFound){throw 'Saved form status missing from actual Context pane'}
foreach($taskRequest in Get-ChildItem -LiteralPath $taskRoot -Filter 'request-*.json'){
 if($taskRequest.Length-gt4194304){throw 'Fixture request exceeds its admitted limit'}
 if((Get-Content -Raw -Encoding utf8 $taskRequest.FullName).Contains('PRIVATE_TUI_INPUT')){throw 'Form values leaked to model request'}
}
Console -Key escape
if($Mode-eq'local'){Console -Text /quit -Key enter}else{Console -Key escape}
$taskDeadline=(Get-Date).AddSeconds(30)
while(@(Get-CimInstance Win32_Process | Where-Object {$_.Name-eq'rook.exe' -and $_.CommandLine.Contains($taskRoot) -and $_.CommandLine.Contains('tui')}).Count){
 if((Get-Date)-gt$taskDeadline){throw 'Owned TUI did not exit'}
 Start-Sleep -Milliseconds 100
}
[IO.File]::WriteAllText((Join-Path $taskRoot ($Mode+'-proof.json')),(@{mode=$Mode;typed_answer=$true;source_visible=$true;context=$true;model_exclusion=$true}|ConvertTo-Json))
Write-Output ('Actual '+$Mode+' TUI forms and typed answers verified')
