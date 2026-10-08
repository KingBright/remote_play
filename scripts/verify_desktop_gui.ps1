param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$Version,
    [string]$Receipt
)
$ErrorActionPreference='Stop'
# Uses only the compiled metadata command, before any profile/window/runtime.
# No Python installation or Windows application alias changes are required.
$resolved=(Resolve-Path -LiteralPath $Binary).Path
if(-not (Test-Path -LiteralPath $resolved -PathType Leaf)){throw 'Product binary is not a file'}
if($Receipt -and (Test-Path -LiteralPath $Receipt)){throw 'Do not overwrite a GUI receipt'}
$before=(Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLower()
$start=New-Object System.Diagnostics.ProcessStartInfo
$start.FileName=$resolved
$start.Arguments='--product-info-json'
$start.UseShellExecute=$false
$start.CreateNoWindow=$true
$start.RedirectStandardOutput=$true
$start.RedirectStandardError=$true
$start.EnvironmentVariables.Remove('REMOTE_PLAY_LEGACY_MAC_GUI')
$process=New-Object System.Diagnostics.Process
$process.StartInfo=$start
try {
    if(-not $process.Start()){throw 'Cannot inspect compiled product identity'}
    $stdout=$process.StandardOutput.ReadToEndAsync()
    $stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit(15000)){
        $process.Kill();$process.WaitForExit()
        throw 'Product identity command timed out; no package created'
    }
    $text=$stdout.GetAwaiter().GetResult()
    $errorText=$stderr.GetAwaiter().GetResult()
    if($process.ExitCode -ne 0 -or $text.Length -gt 16384){throw 'Product identity command failed or returned oversized data'}
    $info=$text | ConvertFrom-Json
    $expected=@{product='RemotePlay';version=$Version;platform='windows';default_gui='restored-original-gpui'}
    foreach($key in $expected.Keys){
        if($info.$key -isnot [string] -or $info.$key -cne $expected[$key]){throw ('Compiled GUI metadata mismatch: '+$key)}
    }
    if($info.schema -isnot [int] -or $info.schema -ne 1){throw 'Unsupported GUI metadata schema'}
    foreach($key in @('original_gui_compiled','native_video_compiled')){
        if($info.$key -isnot [bool] -or $info.$key -ne $true){throw ('Missing compiled product requirement: '+$key)}
    }
    if($info.architecture -notin @('x86_64','aarch64')){throw 'Unsupported compiled architecture'}
    if((Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLower() -ne $before){throw 'Product binary changed during inspection'}
    $result=[ordered]@{binary_sha256=$before;product_info=$info;gui_identity_verified=$true;visual_acceptance='not_evaluated';functional_acceptance='not_evaluated';network_started=$false}
    $json=$result|ConvertTo-Json -Depth 5
    if($Receipt){
        $utf8=New-Object Text.UTF8Encoding($false)
        $stream=New-Object IO.FileStream($Receipt,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try{$bytes=$utf8.GetBytes($json+"`n");$stream.Write($bytes,0,$bytes.Length)}finally{$stream.Dispose()}
    }
    Write-Output $json
} finally { $process.Dispose() }
