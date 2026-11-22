# Authenticode signing for the Windows build (docs/signing.md, Windows).
#
#   windows-sign.ps1 setup artifact   Azure Artifact Signing (formerly Trusted
#                                      Signing): fetch the signing client, write
#                                      its metadata and the Tauri config.
#   windows-sign.ps1 setup test       A throwaway self-signed certificate, so CI
#                                      can prove every file Tauri builds goes
#                                      through the sign command without real
#                                      credentials.
#   windows-sign.ps1 sign <file>      Sign one file. Tauri runs this for the app
#                                      exe, the NSIS installer, its uninstaller
#                                      and plugins (bundle.windows.signCommand).
#   windows-sign.ps1 verify           Check the built files are signed (and, for
#                                      Artifact Signing, trusted and timestamped).
#
# `setup` writes environment variables to $GITHUB_ENV for the later steps,
# including TAURI_SIGN_CONFIG: the file to pass to `tauri build --config`.
param(
  [Parameter(Mandatory = $true, Position = 0)]
  [ValidateSet("setup", "sign", "verify")]
  [string]$Action,
  [Parameter(Position = 1)]
  [string]$Target
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# Pinned: the client is fetched at build time. Its DLL must carry a valid
# Microsoft Authenticode signature (checked below) before signtool loads it.
$ClientVersion = "1.0.151"
$TimestampUrl = "http://timestamp.acs.microsoft.com"

function Add-GitHubEnv([string]$Name, [string]$Value) {
  Set-Item -Path "Env:$Name" -Value $Value
  if ($env:GITHUB_ENV) { Add-Content -Path $env:GITHUB_ENV -Value "$Name=$Value" -Encoding utf8 }
}

function Find-SignTool {
  # The Artifact Signing client needs SignTool 10.0.22621 or later and does
  # not work with the 20348 SDK; take the newest x64 one installed.
  $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
  $tool = Get-ChildItem -Path $kits -Directory -Filter "10.*" -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -notlike "10.0.20348*" -and (Test-Path (Join-Path $_.FullName "x64\signtool.exe")) } |
    Sort-Object { [version]$_.Name } -Descending |
    Select-Object -First 1
  if (-not $tool) { throw "signtool.exe (x64, Windows SDK 10.0.22621 or later) not found under $kits" }
  Join-Path $tool.FullName "x64\signtool.exe"
}

function Setup-Artifact {
  foreach ($name in "ARTIFACT_SIGNING_ENDPOINT", "ARTIFACT_SIGNING_ACCOUNT", "ARTIFACT_SIGNING_PROFILE", "AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET") {
    if (-not [Environment]::GetEnvironmentVariable($name)) { throw "$name is not set (docs/signing.md, Windows)" }
  }
  $endpoint = $env:ARTIFACT_SIGNING_ENDPOINT.TrimEnd("/")
  if ($endpoint -notmatch '^https://[a-z0-9]+\.codesigning\.azure\.net$') {
    throw "ARTIFACT_SIGNING_ENDPOINT must look like https://weu.codesigning.azure.net (the account's region), got $endpoint"
  }

  $dir = Join-Path $env:RUNNER_TEMP "artifact-signing"
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $package = Join-Path $dir "client.zip"
  Invoke-WebRequest -UseBasicParsing -Uri "https://www.nuget.org/api/v2/package/Microsoft.ArtifactSigning.Client/$ClientVersion" -OutFile $package
  Expand-Archive -Path $package -DestinationPath (Join-Path $dir "client") -Force
  $dlib = Join-Path $dir "client\bin\x64\Azure.CodeSigning.Dlib.dll"
  if (-not (Test-Path $dlib)) { throw "Azure.CodeSigning.Dlib.dll not found in Microsoft.ArtifactSigning.Client $ClientVersion" }
  $sig = Get-AuthenticodeSignature -FilePath $dlib
  $signer = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { "(none)" }
  if ($sig.Status -ne "Valid" -or $signer -notmatch "O=Microsoft Corporation") {
    throw "Azure.CodeSigning.Dlib.dll is not signed by Microsoft ($($sig.Status), $signer)"
  }

  # Only the environment credential (AZURE_TENANT_ID, AZURE_CLIENT_ID,
  # AZURE_CLIENT_SECRET): nothing else on the runner is tried.
  $metadata = Join-Path $dir "metadata.json"
  [ordered]@{
    Endpoint               = $endpoint
    CodeSigningAccountName = $env:ARTIFACT_SIGNING_ACCOUNT
    CertificateProfileName = $env:ARTIFACT_SIGNING_PROFILE
    CorrelationId          = "$env:GITHUB_REPOSITORY $env:GITHUB_RUN_ID"
    ExcludeCredentials     = @(
      "ManagedIdentityCredential", "WorkloadIdentityCredential", "SharedTokenCacheCredential",
      "VisualStudioCredential", "VisualStudioCodeCredential", "AzureCliCredential",
      "AzurePowerShellCredential", "AzureDeveloperCliCredential", "InteractiveBrowserCredential"
    )
  } | ConvertTo-Json | Set-Content -Path $metadata -Encoding utf8

  Add-GitHubEnv "ANIXOPS_SIGN_MODE" "artifact"
  Add-GitHubEnv "ANIXOPS_SIGN_DLIB" $dlib
  Add-GitHubEnv "ANIXOPS_SIGN_METADATA" $metadata
  Write-TauriConfig
}

function Setup-Test {
  $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=AnixOps SSH CI test signature" `
    -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddDays(2)
  $password = [guid]::NewGuid().ToString()
  $pfx = Join-Path $env:RUNNER_TEMP "ci-test-signing.pfx"
  Export-PfxCertificate -Cert $cert -FilePath $pfx -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null
  Remove-Item -Path "Cert:\CurrentUser\My\$($cert.Thumbprint)"
  if ($env:GITHUB_ACTIONS) { Write-Output "::add-mask::$password" }
  Add-GitHubEnv "ANIXOPS_SIGN_MODE" "test"
  Add-GitHubEnv "ANIXOPS_SIGN_PFX" $pfx
  Add-GitHubEnv "ANIXOPS_SIGN_PFX_PASSWORD" $password
  Write-TauriConfig
}

function Write-TauriConfig {
  $log = Join-Path $env:RUNNER_TEMP "signed-files.txt"
  Set-Content -Path $log -Value $null
  Add-GitHubEnv "ANIXOPS_SIGN_LOG" $log
  $config = Join-Path $env:RUNNER_TEMP "tauri-windows-sign.json"
  @{
    bundle = @{
      windows = @{
        signCommand = @{
          cmd  = "pwsh"
          args = @("-NoProfile", "-NonInteractive", "-File", $PSCommandPath, "sign", "%1")
        }
      }
    }
  } | ConvertTo-Json -Depth 6 | Set-Content -Path $config -Encoding utf8
  Add-GitHubEnv "TAURI_SIGN_CONFIG" $config
  Get-Content $config
}

function Sign-File([string]$File) {
  if (-not (Test-Path $File)) { throw "no such file: $File" }
  $signtool = Find-SignTool
  switch ($env:ANIXOPS_SIGN_MODE) {
    "artifact" {
      $arguments = @("sign", "/v", "/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256",
        "/dlib", $env:ANIXOPS_SIGN_DLIB, "/dmdf", $env:ANIXOPS_SIGN_METADATA, $File)
    }
    "test" {
      $arguments = @("sign", "/fd", "SHA256", "/f", $env:ANIXOPS_SIGN_PFX, "/p", $env:ANIXOPS_SIGN_PFX_PASSWORD, $File)
    }
    default { throw "ANIXOPS_SIGN_MODE is not set: run 'windows-sign.ps1 setup' first" }
  }
  # The signing service and the timestamp server have the odd transient
  # failure; a release should not fail on one.
  for ($attempt = 1; ; $attempt++) {
    & $signtool @arguments
    if ($LASTEXITCODE -eq 0) { break }
    if ($attempt -ge 4) { throw "signtool failed $attempt times for $File" }
    Write-Warning "signtool failed (exit $LASTEXITCODE), retrying in $($attempt * 10)s"
    Start-Sleep -Seconds ($attempt * 10)
  }
  if ($env:ANIXOPS_SIGN_LOG) { Add-Content -Path $env:ANIXOPS_SIGN_LOG -Value $File }
}

function Test-Signed([string]$File) {
  $sig = Get-AuthenticodeSignature -FilePath $File
  $subject = if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { "(none)" }
  Write-Output "$File`n  status: $($sig.Status)`n  signer: $subject`n  timestamp: $(if ($sig.TimeStamperCertificate) { $sig.TimeStamperCertificate.Subject } else { '(none)' })"
  if ($env:ANIXOPS_SIGN_MODE -eq "artifact") {
    if ($sig.Status -ne "Valid") { throw "$File is not validly signed: $($sig.Status) $($sig.StatusMessage)" }
    if (-not $sig.TimeStamperCertificate) { throw "$File has no timestamp (Artifact Signing certificates last three days)" }
  } else {
    # The test certificate is not trusted, so only check it is the signer.
    if ($subject -notmatch "AnixOps SSH CI test signature") { throw "$File is not signed with the test certificate: $($sig.Status)" }
  }
}

function Verify-Build {
  # target\release\anixops-ssh.exe itself is not checked: Tauri signs it,
  # packs it, and afterwards puts back the unpatched build. The copy inside
  # the installer (below) is the one users run.
  $files = @(Get-ChildItem "target\release\bundle\nsis\*.exe")
  if ($files.Count -lt 1) { throw "no NSIS installer under target\release\bundle\nsis" }
  foreach ($file in $files) { Test-Signed $file.FullName }

  # The copy of the app inside the installer is the one users run.
  $unpacked = Join-Path $env:RUNNER_TEMP "nsis-unpacked"
  $installer = (Get-ChildItem "target\release\bundle\nsis\*.exe" | Select-Object -First 1).FullName
  & 7z x -y "-o$unpacked" $installer | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "7z could not unpack $installer" }
  $inner = Get-ChildItem -Path $unpacked -Recurse -Filter "anixops-ssh.exe" | Select-Object -First 1
  if (-not $inner) { throw "anixops-ssh.exe not found inside $installer" }
  Test-Signed $inner.FullName

  Write-Output "Files passed to the sign command:"
  $signed = @(Get-Content $env:ANIXOPS_SIGN_LOG | Where-Object { $_ })
  $signed | ForEach-Object { Write-Output "  $_" }
  # At least the app exe, the uninstaller (signed from a temporary path by
  # NSIS's !uninstfinalize) and the installer.
  if (@($signed | Sort-Object -Unique).Count -lt 3) { throw "expected the app, the uninstaller and the installer to be signed" }
  if ($env:GITHUB_STEP_SUMMARY) {
    @("### Windows signing ($env:ANIXOPS_SIGN_MODE)", "") + ($signed | ForEach-Object { "- ``$(Split-Path $_ -Leaf)``" }) |
      Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8
  }
}

switch ($Action) {
  "setup" {
    switch ($Target) {
      "artifact" { Setup-Artifact }
      "test" { Setup-Test }
      default { throw "usage: windows-sign.ps1 setup artifact|test" }
    }
  }
  "sign" { if (-not $Target) { throw "usage: windows-sign.ps1 sign <file>" }; Sign-File $Target }
  "verify" { Verify-Build }
}
