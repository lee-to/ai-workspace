param(
    [Parameter(Mandatory = $true)]
    [string]$ZipPath,

    [string]$ExpectedEntryName = "ai-workspace.exe",

    [int]$ExpectedMethod = 0
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $ZipPath)) {
    throw "ZIP not found: $ZipPath"
}

$resolved = Resolve-Path -LiteralPath $ZipPath
$bytes = [System.IO.File]::ReadAllBytes($resolved)
if ($bytes.Length -lt 30) {
    throw "ZIP is too small to contain a local file header: $ZipPath"
}

$localSignature = [BitConverter]::ToUInt32($bytes, 0)
if ($localSignature -ne 0x04034b50) {
    throw ("Invalid local file header signature: 0x{0:X8}" -f $localSignature)
}

$localMethod = [BitConverter]::ToUInt16($bytes, 8)
$localNameLength = [BitConverter]::ToUInt16($bytes, 26)
$localNameStart = 30
$localNameEnd = $localNameStart + $localNameLength
if ($localNameEnd -gt $bytes.Length) {
    throw "ZIP local file name extends beyond archive length"
}

$localName = [Text.Encoding]::UTF8.GetString($bytes, $localNameStart, $localNameLength)
if ($localName -ne $ExpectedEntryName) {
    throw "Unexpected ZIP entry '$localName', expected '$ExpectedEntryName'"
}

if ($localMethod -ne $ExpectedMethod) {
    throw "Unexpected ZIP local compression method $localMethod for '$localName', expected $ExpectedMethod"
}

$eocdOffset = -1
for ($i = $bytes.Length - 22; $i -ge 0; $i--) {
    if (
        $bytes[$i] -eq 0x50 -and
        $bytes[$i + 1] -eq 0x4b -and
        $bytes[$i + 2] -eq 0x05 -and
        $bytes[$i + 3] -eq 0x06
    ) {
        $eocdOffset = $i
        break
    }
}

if ($eocdOffset -lt 0) {
    throw "ZIP end-of-central-directory record not found"
}

$centralOffset = [BitConverter]::ToUInt32($bytes, $eocdOffset + 16)
if ($centralOffset -lt 0) {
    throw "ZIP central directory offset is invalid"
}

if ($centralOffset + 46 -gt $bytes.Length) {
    throw "ZIP central directory header extends beyond archive length"
}

$centralSignature = [BitConverter]::ToUInt32($bytes, $centralOffset)
if ($centralSignature -ne 0x02014b50) {
    throw ("Invalid central directory header signature: 0x{0:X8}" -f $centralSignature)
}

$centralMethod = [BitConverter]::ToUInt16($bytes, $centralOffset + 10)
$centralNameLength = [BitConverter]::ToUInt16($bytes, $centralOffset + 28)
$centralNameStart = $centralOffset + 46
$centralNameEnd = $centralNameStart + $centralNameLength
if ($centralNameEnd -gt $bytes.Length) {
    throw "ZIP central directory file name extends beyond archive length"
}

$centralName = [Text.Encoding]::UTF8.GetString($bytes, $centralNameStart, $centralNameLength)
if ($centralName -ne $ExpectedEntryName) {
    throw "Unexpected central directory entry '$centralName', expected '$ExpectedEntryName'"
}

if ($centralMethod -ne $ExpectedMethod) {
    throw "Unexpected ZIP central directory compression method $centralMethod for '$centralName', expected $ExpectedMethod"
}

Write-Host "Verified $ZipPath contains $ExpectedEntryName with ZIP compression method $ExpectedMethod"
