param([Parameter(Mandatory=$true)][string]$Installer,[Parameter(Mandatory=$true)][string]$OutDir,[switch]$RemoveExistingWebview)
$ErrorActionPreference='Stop'
$result=[ordered]@{passed=$false;checks=@();error=$null}
New-Item -ItemType Directory -Force -Path $OutDir|Out-Null
function Check([bool]$value,[string]$name){if(!$value){throw "Check failed: $name"};$result.checks+=$name}
try{
    $root=Join-Path $env:ProgramFiles 'Pmman Network'
    $webviewRoots=@((Join-Path $env:ProgramFiles 'Microsoft/EdgeWebView/Application'),(Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft/EdgeWebView/Application'),(Join-Path $env:LOCALAPPDATA 'Microsoft/EdgeWebView/Application'))
    $result.webviewPresentBeforeInstall=@($webviewRoots|Where-Object {Test-Path (Join-Path $_ '*/msedgewebview2.exe')}).Count -gt 0
    $result.wireguardPresentBeforeInstall=Test-Path (Join-Path $env:ProgramFiles 'WireGuard/wg.exe')
    if($RemoveExistingWebview){
        # Test fixture preparation inside the broker's disposable guest only:
        # use Microsoft's own runtime uninstaller, never delete Edge files.
        foreach($runtimeRoot in $webviewRoots){
            foreach($version in @(Get-ChildItem -LiteralPath $runtimeRoot -Directory -ErrorAction SilentlyContinue)){
                $setup=Join-Path $version.FullName 'Installer/setup.exe'
                if(Test-Path -LiteralPath $setup){
                    $arguments=@('--uninstall','--msedgewebview','--force-uninstall')
                    if(!$runtimeRoot.StartsWith($env:LOCALAPPDATA,[StringComparison]::OrdinalIgnoreCase)){$arguments+='--system-level'}
                    $uninstall=Start-Process -FilePath $setup -ArgumentList $arguments -PassThru
                    if(!$uninstall.WaitForExit(90000)){throw 'Guest WebView fixture uninstall timeout'}
                }
            }
        }
        if(@($webviewRoots|Where-Object {Test-Path (Join-Path $_ '*/msedgewebview2.exe')}).Count){
            # Some Windows images protect their built-in runtime from uninstall.
            # In this disposable guest, preserve and hide only the exact
            # WebView runtime files/registration to create the missing fixture.
            # Edge browser files and other EdgeUpdate product IDs stay intact.
            foreach($name in @('edgeupdate','edgeupdatem')){Get-Service -Name $name -ErrorAction SilentlyContinue|Stop-Service -Force -ErrorAction SilentlyContinue}
            $result.webviewFixtureConsumers=@(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe' OR Name='Widgets.exe'"|Select-Object Name,ProcessId,ParentProcessId,ExecutablePath)
            # Widgets can immediately respawn their runtime after its child is
            # stopped. Close those fixture consumers first in this empty VM.
            Get-Process -Name Widgets,WidgetsPlatformRuntime,msedgewebview2 -ErrorAction SilentlyContinue|Stop-Process -Force -ErrorAction SilentlyContinue
            foreach($runtimeRoot in $webviewRoots){
                if(!(Test-Path -LiteralPath $runtimeRoot)){continue}
                $resolved=[IO.Path]::GetFullPath($runtimeRoot)
                if(!$resolved.EndsWith('\Microsoft\EdgeWebView\Application',[StringComparison]::OrdinalIgnoreCase)){throw 'Unsafe WebView fixture target'}
                $preserved=$resolved+'.pmman-fixture-original'
                if(Test-Path -LiteralPath $preserved){throw 'WebView fixture destination already exists'}
                Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'"|Where-Object {$_.ExecutablePath -and $_.ExecutablePath.StartsWith($resolved+'\',[StringComparison]::OrdinalIgnoreCase)}|ForEach-Object {Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue}
                $moveDeadline=[DateTime]::UtcNow.AddSeconds(15)
                do{
                    try{Move-Item -LiteralPath $resolved -Destination $preserved;break}catch{
                        if([DateTime]::UtcNow -ge $moveDeadline){throw}
                        Get-Process -Name Widgets,WidgetsPlatformRuntime,msedgewebview2 -ErrorAction SilentlyContinue|Stop-Process -Force -ErrorAction SilentlyContinue
                        Start-Sleep -Milliseconds 250
                    }
                }while($true)
            }
            $runtimeGuid='{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
            foreach($hive in @('HKLM:/SOFTWARE/Microsoft/EdgeUpdate','HKLM:/SOFTWARE/WOW6432Node/Microsoft/EdgeUpdate','HKCU:/Software/Microsoft/EdgeUpdate')){
                foreach($branch in @('Clients','ClientState','ClientStateMedium')){
                    $key="$hive/$branch/$runtimeGuid"
                    if(Test-Path -LiteralPath $key){Rename-Item -LiteralPath $key -NewName ($runtimeGuid+'.pmman-fixture-original')}
                }
            }
            $result.webviewFixtureMethod='preserved-runtime-files-and-exact-registration'
        }else{$result.webviewFixtureMethod='official-runtime-uninstaller'}
        Check (@($webviewRoots|Where-Object {Test-Path (Join-Path $_ '*/msedgewebview2.exe')}).Count -eq 0) 'missing-webview-fixture-confirmed-before-install'
        $result.webviewRemovedForFixture=$true
    }
    $install=Start-Process -FilePath $Installer -ArgumentList '/S' -PassThru
    if(!$install.WaitForExit(420000)){throw 'Offline installer timeout'}
    $result.installerExitCode=$install.ExitCode;Check ($install.ExitCode -eq 0) 'offline-install-exit-zero'
    Check (Test-Path (Join-Path $env:ProgramFiles 'WireGuard/wg.exe')) 'wireguard-internal-runtime-present'
    $executable=Join-Path $root 'pmman-client.exe'
    $selfCheck=Start-Process -FilePath $executable -ArgumentList '--check-webview2' -PassThru
    if(!$selfCheck.WaitForExit(30000)){throw 'WebView loader self-check timeout'}
    Check ($selfCheck.ExitCode -eq 0) 'actual-webview-loader-success'
    $client=Start-Process -FilePath $executable -PassThru
    $deadline=[DateTime]::UtcNow.AddSeconds(45)
    do{
        Start-Sleep -Milliseconds 250;$client.Refresh()
        $webviews=@(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'"|Where-Object {$_.CommandLine -like '*pmman*' -or $_.ParentProcessId -eq $client.Id})
    }while(!$client.HasExited -and (!$client.MainWindowHandle -or !$webviews.Count) -and [DateTime]::UtcNow -lt $deadline)
    Check (!$client.HasExited -and $client.MainWindowHandle -ne 0) 'client-visible-native-window'
    Check ($webviews.Count -gt 0) 'client-actual-webview-process'
    $pipe=[IO.Pipes.NamedPipeClientStream]::new('.','pmman-network',[IO.Pipes.PipeDirection]::InOut)
    try{
        $pipe.Connect(15000)
        $writer=[IO.StreamWriter]::new($pipe,[Text.UTF8Encoding]::new($false),1024,$true);$writer.AutoFlush=$true
        $reader=[IO.StreamReader]::new($pipe,[Text.UTF8Encoding]::new($false),$false,1024,$true)
        $writer.WriteLine('{"action":"status"}');$task=$reader.ReadLineAsync()
        if(!$task.Wait(10000)){throw 'Offline service IPC timeout'}
        $response=$task.Result|ConvertFrom-Json
        Check ($response.ok -and $response.status.state -eq 'disconnected') 'offline-background-service-ready'
        Check $response.status.p2pEnabled 'p2p-default-enabled-on-clean-machine'
    }finally{$pipe.Dispose()}
    $uninstallRoots=@('HKLM:/SOFTWARE/Microsoft/Windows/CurrentVersion/Uninstall/*','HKLM:/SOFTWARE/WOW6432Node/Microsoft/Windows/CurrentVersion/Uninstall/*')
    $entries=@(Get-ItemProperty $uninstallRoots -ErrorAction SilentlyContinue|Where-Object {$_.DisplayName -like '*PMMan*' -or $_.DisplayName -eq 'WireGuard'})
    $result.userEntries=@($entries|ForEach-Object DisplayName)
    Check (@($entries|Where-Object DisplayName -like '*PMMan*').Count -eq 1 -and @($entries|Where-Object DisplayName -eq 'WireGuard').Count -eq 0) 'only-pmman-product-entry'
    $result.passed=$true
}catch{$result.error=$_.Exception.Message}
finally{$result|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $OutDir 'application-result.json') -Encoding utf8}
if(!$result.passed){exit 1}
