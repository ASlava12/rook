param(
    [ValidateSet('default','kitty')][string]$Capability='default',
    [switch]$Alternate,
    [string]$WezTerm='C:/Program Files/WezTerm'
)
$ErrorActionPreference='Stop'
$workspace=(Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$target=(Resolve-Path (Join-Path $workspace 'target')).Path
$binary=Join-Path $target 'debug/examples/terminal_images.exe'
if(!(Test-Path -LiteralPath $binary)){throw 'Build the terminal_images example first'}
$mode=if($Alternate){'alternate'}else{'main'}
$probeRoot=Join-Path $target ('terminal-image-'+$Capability+'-'+$mode+'-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $probeRoot | Out-Null
$configPath=Join-Path $probeRoot 'wezterm.lua'
$socketPath=(Join-Path $target ('wi-'+[Guid]::NewGuid().ToString('N').Substring(0,16)+'.sock')).Replace('\','/')
if($socketPath.Length -ge 104){throw 'Workspace path is too long for an owned Unix socket'}
$kittyOption=if($Capability -eq 'kitty'){'enable_kitty_graphics = true,'}else{''}
$config=@"
return {
  $kittyOption
  automatically_reload_config = false,
  check_for_updates = false,
  scrollback_lines = 32,
  unix_domains = {{ name = 'image-probe', socket_path = '$socketPath' }},
}
"@
[IO.File]::WriteAllText($configPath,$config)
$runnerPath=Join-Path $probeRoot 'run.ps1'
$exitPath=Join-Path $probeRoot 'native-exit.txt'
$quotedBinary=$binary.Replace("'","''")
$quotedRoot=$probeRoot.Replace("'","''")
$quotedExit=$exitPath.Replace("'","''")
$alternateArgument=if($Alternate){'--alternate'}else{''}
$runner=@"
& '$quotedBinary' --root '$quotedRoot' $alternateArgument
`$nativeExit=`$LASTEXITCODE
[IO.File]::WriteAllText('$quotedExit',[string]`$nativeExit)
exit `$nativeExit
"@
[IO.File]::WriteAllText($runnerPath,$runner)
$probeArguments=@('--config-file',('"'+$configPath+'"'),'--cwd',('"'+$probeRoot+'"'),'--','powershell','-NoProfile','-ExecutionPolicy','Bypass','-File',('"'+$runnerPath+'"'))
$server=$null
try {
    $server=Start-Process -FilePath (Join-Path $WezTerm 'wezterm-mux-server.exe') -ArgumentList $probeArguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $probeRoot 'server.stdout') -RedirectStandardError (Join-Path $probeRoot 'server.stderr')
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    while([DateTime]::UtcNow -lt $deadline -and !$server.HasExited -and !(Test-Path -LiteralPath $exitPath)){
        Start-Sleep -Milliseconds 100
        $server.Refresh()
    }
    if(!(Test-Path -LiteralPath $exitPath)){throw 'Owned probe did not report an exit within 20 seconds'}
    if((Get-Item -LiteralPath $exitPath).Length -gt 16){throw 'Oversized exit report'}
    $nativeExit=[int]([IO.File]::ReadAllText($exitPath))
    $proofPath=Join-Path $probeRoot 'image-query.json'
    if(!(Test-Path -LiteralPath $proofPath)){throw ('No query proof; inspect '+$probeRoot)}
    if((Get-Item -LiteralPath $proofPath).Length -gt 16384){throw 'Oversized query proof'}
    $proof=Get-Content -Raw -Encoding utf8 -LiteralPath $proofPath | ConvertFrom-Json
    $summary=[ordered]@{root=$probeRoot; scope='Windows console to headless mux query path; no parser-arrival or rendered-pixel proof'; capability=$Capability; alternate=[bool]$Alternate; native_exit=$nativeExit; server_running_at_completion=(!$server.HasExited); outcome=$proof.outcome; response_bytes=$proof.response_bytes.Count}
    [IO.File]::WriteAllText((Join-Path $probeRoot 'proof.json'),($summary | ConvertTo-Json))
    $summary | ConvertTo-Json
    if($nativeExit -ne 0){throw ('Native probe exit '+$nativeExit)}
} finally {
    if($server){
        $server.Refresh()
        if(!$server.HasExited){
            $ownedServer=Get-CimInstance Win32_Process -Filter ('ProcessId='+$server.Id)
            if($ownedServer -and $ownedServer.Name -eq 'wezterm-mux-server.exe' -and $ownedServer.CommandLine.Contains($configPath)){
                Stop-Process -Id $server.Id
            }else{throw 'Refusing to stop a process without the exact owned config path'}
        }
    }
    if(Test-Path -LiteralPath $socketPath){
        $resolvedSocket=(Resolve-Path -LiteralPath $socketPath).Path
        if([IO.Path]::GetDirectoryName($resolvedSocket) -ne $target){throw 'Socket cleanup escaped target'}
        Remove-Item -LiteralPath $resolvedSocket
    }
}
