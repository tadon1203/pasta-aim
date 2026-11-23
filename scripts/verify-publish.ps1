param(
    [string]$Publish = 'Pstm/bin/Release/net10.0-windows/win-x64/publish'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$folder = Join-Path $root $Publish
$failures = [System.Collections.Generic.List[string]]::new()

function Test-Item([bool]$Condition, [string]$Message) {
    if ($Condition) {
        Write-Host "ok   $Message"
    } else {
        Write-Host "FAIL $Message"
        $script:failures.Add($Message)
    }
}

$exe = Join-Path $folder 'pstm.exe'
Test-Item (Test-Path -LiteralPath $exe) 'pstm.exe exists'
if (-not (Test-Path -LiteralPath $exe)) {
    exit 1
}

$extra = Get-ChildItem -LiteralPath $folder -File |
    Where-Object { $_.Name -ne 'pstm.exe' } |
    Select-Object -ExpandProperty Name
Test-Item ($extra.Count -eq 0) "pstm.exe is the only file (found: $($extra -join ', '))"

$version = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($exe)
Test-Item ($version.CompanyName -eq 'pstm') 'CompanyName is pstm'
Test-Item ($version.ProductName -eq 'pstm') 'ProductName is pstm'
Test-Item ($version.FileDescription -eq 'pstm') 'FileDescription is pstm'
Test-Item ($version.ProductVersion -notmatch '\+') 'ProductVersion has no source revision'

$stream = [IO.File]::OpenRead($exe)
try {
    $header = New-Object byte[] 4096
    $count = $stream.Read($header, 0, $header.Length)
    $pe = [BitConverter]::ToInt32($header, 0x3C)
    $subsystem = [BitConverter]::ToUInt16($header, $pe + 24 + 68)
} finally {
    $stream.Dispose()
}

Test-Item ($subsystem -eq 2) 'the image uses the GUI subsystem'

$forbidden = @(
    'PastaAim',
    'NiceAimassist',
    'nice-aimassist',
    'nosas',
    'tadon1203',
    'raw.githubusercontent',
    'C:\Users',
    'MAKCU',
    'Makcu',
    'AimTracker',
    'aimassist'
)

$found = @{}
foreach ($name in $forbidden) {
    $found[$name] = $false
}

$stream = [IO.File]::OpenRead($exe)
$buffer = New-Object byte[] (8MB)
$carry = ''
try {
    while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
        $text = $carry + [Text.Encoding]::GetEncoding(28591).GetString($buffer, 0, $read)
        foreach ($name in $forbidden) {
            if (-not $found[$name] -and $text.Contains($name)) {
                $found[$name] = $true
            }
        }

        $carry = $text.Substring([Math]::Max(0, $text.Length - 64))
    }
} finally {
    $stream.Dispose()
}

foreach ($name in $forbidden) {
    Test-Item (-not $found[$name]) "the image does not contain '$name'"
}

if ($failures.Count -eq 0) {
    Write-Host 'the publish output is clean'
    exit 0
}

Write-Host "$($failures.Count) checks failed"
exit 1
