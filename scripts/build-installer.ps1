param(
    [string]$Bundles = "msi",
    [ValidateSet("nats", "iggy", "none")]
    [string]$BusSidecar = "nats",
    [string]$NatsVersion = "2.12.7",
    [string]$NatsServerPath = "",
    [string]$NatsServerUrl = "",
    [string]$NatsServerSha256 = "",
    [string]$IggyRef = "server-0.7.0",
    [string]$IggyServerPath = "",
    [string]$IggyServerUrl = "",
    [string]$IggyServerSha256 = "",
    [switch]$AllowIggySourceBuild,
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$wixDir = Join-Path $repoRoot ".sidecars\wix"

function New-WixSidecarFragment {
    param(
        [string]$BinaryBaseName,
        [string]$ComponentId,
        [string]$Target
    )

    $source = Join-Path $repoRoot "src-tauri\binaries\$BinaryBaseName-$Target.exe"
    if (-not (Test-Path $source)) {
        throw "Expected sidecar binary at $source before generating WiX fragment."
    }

    New-Item -ItemType Directory -Force -Path $wixDir | Out-Null
    $fragmentPath = Join-Path $wixDir "$BinaryBaseName-sidecar.wxs"
    $escapedSource = [System.Security.SecurityElement]::Escape((Resolve-Path $source).Path)
    $installName = "$BinaryBaseName.exe"

    @"
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Fragment>
    <DirectoryRef Id="INSTALLDIR">
      <Component Id="$ComponentId" Guid="*" Win64="yes">
        <File Id="${ComponentId}File" Source="$escapedSource" Name="$installName" KeyPath="yes" Checksum="yes" />
      </Component>
    </DirectoryRef>
  </Fragment>
</Wix>
"@ | Set-Content -Path $fragmentPath -Encoding UTF8

    return $fragmentPath
}

function Find-WixTool {
    param([string]$Name)

    $candidates = @(
        (Join-Path $env:LOCALAPPDATA "tauri\WixTools314\$Name"),
        (Join-Path $env:LOCALAPPDATA "tauri\WixTools\$Name")
    )

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return (Resolve-Path $candidate).Path
        }
    }

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        return $command.Source
    }

    throw "Could not find $Name. Run the Tauri MSI build once so WiX tools are installed."
}

function Add-SidecarToGeneratedWix {
    param(
        [string]$BinaryBaseName,
        [string]$ComponentId,
        [string]$Target
    )

    $wixSource = Join-Path $repoRoot "src-tauri\target\release\wix\x64\main.wxs"
    if (-not (Test-Path $wixSource)) {
        throw "Expected generated WiX source at $wixSource."
    }

    $sidecarSource = Join-Path $repoRoot "src-tauri\binaries\$BinaryBaseName-$Target.exe"
    if (-not (Test-Path $sidecarSource)) {
        throw "Expected sidecar binary at $sidecarSource."
    }

    $content = Get-Content -Path $wixSource -Raw
    if ($content.Contains("Component Id=`"$ComponentId`"") -and $content.Contains("ComponentRef Id=`"$ComponentId`"")) {
        return $wixSource
    }

    $escapedSource = [System.Security.SecurityElement]::Escape((Resolve-Path $sidecarSource).Path)
    $installName = "$BinaryBaseName.exe"
    $component = @"
            <Component Id="$ComponentId" Guid="*" Win64="`$(var.Win64)">
                <File Id="${ComponentId}File" Source="$escapedSource" Name="$installName" KeyPath="yes" Checksum="yes"/>
            </Component>
"@

    if (-not $content.Contains("Component Id=`"$ComponentId`"")) {
        $installDirClose = "        </DirectoryRef>`r`n`r`n        <DirectoryRef Id=`"ApplicationProgramsFolder`">"
        if (-not $content.Contains($installDirClose)) {
            throw "Could not find INSTALLDIR DirectoryRef insertion point in generated WiX source."
        }
        $content = $content.Replace($installDirClose, "$component`r`n$installDirClose")
    }

    if (-not $content.Contains("ComponentRef Id=`"$ComponentId`"")) {
        $externalFeaturePattern = '(<Feature Id="External" AllowAdvertise="no" Absent="disallow">\s*)(</Feature>)'
        $componentRef = '$1            <ComponentRef Id="' + $ComponentId + '"/>' + "`r`n" + '$2'
        $updated = [System.Text.RegularExpressions.Regex]::Replace(
            $content,
            $externalFeaturePattern,
            $componentRef,
            [System.Text.RegularExpressions.RegexOptions]::Singleline
        )
        if ($updated -eq $content) {
            throw "Could not find External Feature insertion point in generated WiX source."
        }
        $content = $updated
    }

    if (-not $content.Contains("CustomAction Id=`"SeedOrionEnrollmentFromBundle`"")) {
        $seedScript = "`$d=Split-Path '[OriginalDatabase]';`$s=`$d+'\config.json';if(Test-Path `$s){`$t=`$env:APPDATA+'\OrionII';md `$t -Force;cp `$s (`$t+'\config.json') -Force;cp (`$d+'\deployment.json') (`$t+'\deployment.json') -Force -EA 0}"
        $seedCommand = "powershell.exe -NoP -W Hidden -C `"$seedScript`""
        $escapedSeedCommand = [System.Security.SecurityElement]::Escape($seedCommand)
        $customAction = @"
        <CustomAction Id="SeedOrionEnrollmentFromBundle"
                      Directory="INSTALLDIR"
                      Execute="immediate"
                      Impersonate="yes"
                      Return="ignore"
                      ExeCommand="$escapedSeedCommand" />

        <InstallExecuteSequence>
          <Custom Action="SeedOrionEnrollmentFromBundle" After="InstallFiles">NOT REMOVE</Custom>
        </InstallExecuteSequence>

"@
        $webViewMarker = "        <!-- WebView2 -->"
        if (-not $content.Contains($webViewMarker)) {
            throw "Could not find WebView2 insertion point in generated WiX source."
        }
        $content = $content.Replace($webViewMarker, "$customAction$webViewMarker")
    }

    Set-Content -Path $wixSource -Value $content -Encoding UTF8
    return $wixSource
}

function Rebuild-MsiFromGeneratedWix {
    param(
        [string]$WixSource
    )

    $wixDir = Split-Path -Parent $WixSource
    $wixObj = Join-Path $wixDir "main.wixobj"
    $locale = Join-Path $wixDir "locale.wxl"
    $msi = Get-ChildItem -Path (Join-Path $repoRoot "src-tauri\target\release\bundle\msi") -Filter "*.msi" |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if ($null -eq $msi) {
        throw "Could not find generated MSI to relink."
    }

    $candle = Find-WixTool "candle.exe"
    $light = Find-WixTool "light.exe"

    & $candle -nologo -arch x64 -ext WixUIExtension -out $wixObj $WixSource | Write-Host
    if ($LASTEXITCODE -ne 0) {
        throw "candle.exe failed with exit code $LASTEXITCODE."
    }

    Remove-Item -LiteralPath $msi.FullName -Force
    & $light -nologo -ext WixUIExtension -cultures:en-US -loc $locale -out $msi.FullName $wixObj | Write-Host
    if ($LASTEXITCODE -ne 0) {
        throw "light.exe failed with exit code $LASTEXITCODE."
    }

    return $msi.FullName
}

if ($BusSidecar -eq "nats") {
    & (Join-Path $PSScriptRoot "prepare-nats-sidecar.ps1") `
        -NatsVersion $NatsVersion `
        -PrebuiltPath $NatsServerPath `
        -PrebuiltUrl $NatsServerUrl `
        -ExpectedSha256 $NatsServerSha256 `
        -Target $Target
}
elseif ($BusSidecar -eq "iggy") {
    & (Join-Path $PSScriptRoot "prepare-iggy-sidecar.ps1") `
        -IggyRef $IggyRef `
        -PrebuiltPath $IggyServerPath `
        -PrebuiltUrl $IggyServerUrl `
        -ExpectedSha256 $IggyServerSha256 `
        -AllowSourceBuild:$AllowIggySourceBuild `
        -Target $Target
}

$previousTauriConfig = $env:TAURI_CONFIG
$externalBin = @()
$wixFragmentPath = ""
$wixComponentRef = ""
if ($BusSidecar -eq "nats") {
    $externalBin = @("binaries/nats-server")
    $wixFragmentPath = New-WixSidecarFragment `
        -BinaryBaseName "nats-server" `
        -ComponentId "NatsServerSidecar" `
        -Target $Target
    $wixComponentRef = "NatsServerSidecar"
}
elseif ($BusSidecar -eq "iggy") {
    $externalBin = @("binaries/iggy-server")
    $wixFragmentPath = New-WixSidecarFragment `
        -BinaryBaseName "iggy-server" `
        -ComponentId "IggyServerSidecar" `
        -Target $Target
    $wixComponentRef = "IggyServerSidecar"
}

$overlay = if ($externalBin.Count -gt 0) {
    $bundle = @{
        externalBin = $externalBin
    }

    if (-not [string]::IsNullOrWhiteSpace($wixFragmentPath)) {
        $bundle.windows = @{
            wix = @{
                fragmentPaths = @($wixFragmentPath)
                componentRefs = @($wixComponentRef)
            }
        }
    }

    @{ bundle = $bundle } | ConvertTo-Json -Depth 8 -Compress
}
else {
    ""
}

Push-Location $repoRoot
try {
    if (-not [string]::IsNullOrWhiteSpace($overlay)) {
        $env:TAURI_CONFIG = $overlay
    }
    npm run tauri -- build --bundles $Bundles

    if ($externalBin.Count -gt 0 -and ($Bundles -eq "all" -or $Bundles -match "(^|,)msi($|,)")) {
        if ($BusSidecar -eq "nats") {
            $wixSource = Add-SidecarToGeneratedWix `
                -BinaryBaseName "nats-server" `
                -ComponentId "NatsServerSidecar" `
                -Target $Target
            $rebuiltMsi = Rebuild-MsiFromGeneratedWix -WixSource $wixSource
            Write-Host "Rebuilt MSI with bundled NATS sidecar: $rebuiltMsi"
        }
        elseif ($BusSidecar -eq "iggy") {
            $wixSource = Add-SidecarToGeneratedWix `
                -BinaryBaseName "iggy-server" `
                -ComponentId "IggyServerSidecar" `
                -Target $Target
            $rebuiltMsi = Rebuild-MsiFromGeneratedWix -WixSource $wixSource
            Write-Host "Rebuilt MSI with bundled Iggy sidecar: $rebuiltMsi"
        }
    }
} finally {
    if ($null -eq $previousTauriConfig) {
        Remove-Item Env:\TAURI_CONFIG -ErrorAction SilentlyContinue
    } else {
        $env:TAURI_CONFIG = $previousTauriConfig
    }
    Pop-Location
}
