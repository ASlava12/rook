param([Parameter(Mandatory)][ValidateSet('inline','fullscreen')][string]$Mode)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/scrollback-probe-'+$Mode+'-root.txt')).Trim()
function Console([string]$Text='',[string]$Key='',[string]$Capture='',[int]$Width=0){
 $taskArgs=@('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'console.ps1'),'-Root',$taskRoot)
 if($Text){$taskArgs+=@('-Text',$Text)};if($Key){$taskArgs+=@('-Key',$Key)};if($Capture){$taskArgs+=@('-Capture',$Capture)};if($Width){$taskArgs+=@('-Width',$Width)}
 $taskOutput=& powershell @taskArgs
 if($LASTEXITCODE-ne0){throw 'Native probe console failed'}
 $taskOutput | ConvertFrom-Json
}
function WaitFrame([string]$Name,[string]$Pattern){
 $taskDeadline=(Get-Date).AddSeconds(30)
 do{$taskFrame=Console -Capture $Name;if($taskFrame.screen-match$Pattern){return $taskFrame};if((Get-Date)-gt$taskDeadline){throw ('Frame missing '+$Pattern)};Start-Sleep -Milliseconds 100}while($true)
}
$taskInitial=WaitFrame 'initial' 'ROW_0079'
Console -Text KEEP_DRAFT | Out-Null
$taskDraft=WaitFrame 'draft' 'KEEP_DRAFT'
$taskRows=$taskDraft.screen -split '\r?\n'
if(!$taskRows[$taskDraft.height-2].Contains('NEXT_MESSAGE')){throw 'Queue did not stay at the lower edge before resize'}
Console -Key ctrl-o | Out-Null
$null=WaitFrame 'overlay' 'OVERLAY_PROBE'
Console -Key escape | Out-Null
$taskRestored=WaitFrame 'restored' 'KEEP_DRAFT'
if(!$taskRestored.screen.Contains('ROW_0079')){throw 'Overlay damaged the existing main-buffer rows'}
Console -Key ctrl-n | Out-Null
$taskAppended=WaitFrame 'appended' 'ROW_0080'
$taskResizeRequest=Console -Width 40 -Capture resize-request
$taskResized=WaitFrame 'resized' 'KEEP_DRAFT'
$taskResizeSupported=$taskResized.width-eq40 -and !$taskResizeRequest.resize_error
$taskQueueRow=-1
$taskRows=$taskResized.screen -split '\r?\n'
for($taskRow=0;$taskRow-lt$taskResized.height;$taskRow++){if($taskRows[$taskRow].Contains('NEXT_MESSAGE')){$taskQueueRow=$taskRow}}
Console -Key ctrl-q | Out-Null
$taskDeadline=(Get-Date).AddSeconds(30)
while(!(Test-Path (Join-Path $taskRoot 'run-result.json')) -or (Get-Item (Join-Path $taskRoot 'run-result.json')).Length-eq0){if((Get-Date)-gt$taskDeadline){throw 'Probe result was not saved'};Start-Sleep -Milliseconds 100}
$taskResult=Get-Content -Raw -Encoding utf8 (Join-Path $taskRoot 'run-result.json') | ConvertFrom-Json
if($taskResult.draft-ne'KEEP_DRAFT' -or $taskResult.emitted-ne81 -or $taskResult.overlays-ne1){throw 'Probe interaction result mismatch'}
$taskProof=@{mode=$Mode;draft_retained=$true;overlay_restored=$true;append=$true;resize_supported=$taskResizeSupported;resize_error=$taskResizeRequest.resize_error;queue_row_after_resize=$taskQueueRow;screen_rows_after_resize=$taskResized.height;native_buffer_rows=$taskInitial.buffer_rows;oldest_row_in_native_buffer=$taskInitial.history.Contains('ROW_0000');host_scrollback_is_not_win32_buffer=($taskInitial.buffer_rows-eq$taskInitial.height)}
[IO.File]::WriteAllText((Join-Path $taskRoot 'proof.json'),($taskProof|ConvertTo-Json))
Write-Output ($taskProof|ConvertTo-Json)
