# Exact additional obligations embedded in source, outside the package's declared
# licence expression or leading Rust comments. Line ranges include the full licence,
# short notice and upstream attribution; hashes cover UTF-8 joined with LF, no final LF.
# A new dependency version or changed excerpt requires reviewing the upstream notice.
function Get-NoticeTextSha256([string]$Text) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [System.BitConverter]::ToString(
            $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Text))
        ).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
}

function Read-SourceNoticeRequirements([string]$Path) {
    $parsed = [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json
    $entries = @($parsed)
    if ($entries.Count -eq 0) { throw 'The embedded-source notice inventory is empty.' }
    $seen = @{}
    foreach ($entry in $entries) {
        if ($entry.crate -notmatch '^[A-Za-z0-9_-]+$' -or
            $entry.version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$' -or
            $entry.path -notmatch '^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+$' -or
            @($entry.path -split '/') -contains '..' -or
            @($entry.path -split '/') -contains '.' -or
            ($entry.first_line -isnot [int] -and $entry.first_line -isnot [long]) -or
            $entry.first_line -lt 1 -or $entry.first_line -gt [int]::MaxValue -or
            ($entry.last_line -isnot [int] -and $entry.last_line -isnot [long]) -or
            $entry.last_line -lt $entry.first_line -or $entry.last_line -gt [int]::MaxValue -or
            $entry.license -notin @('W3C-20150513', 'ISC') -or
            $entry.sha256 -notmatch '^[0-9a-f]{64}$') {
            throw 'Malformed or unreviewed embedded-source notice requirement.'
        }
        $key = "$($entry.crate) $($entry.version) $($entry.path)"
        if ($seen.ContainsKey($key)) { throw "Duplicate source notice: $key" }
        $seen[$key] = $true
    }
    return $entries
}

function Get-SourceNoticeRequirements($Entries, [string]$Crate, [string]$Version) {
    $forCrate = @($Entries | Where-Object { $_.crate -ceq $Crate })
    $forVersion = @($forCrate | Where-Object { $_.version -ceq $Version })
    if ($forCrate.Count -gt 0 -and $forVersion.Count -eq 0) {
        throw "Re-review embedded source notices for $Crate $Version before distributing it."
    }
    return $forVersion
}

function Get-SourceNoticeText($Entry, [string]$PackageDir) {
    $path = Join-Path $PackageDir $Entry.path
    $lines = [System.IO.File]::ReadAllLines($path)
    if ($lines.Count -lt $Entry.last_line) { throw "Source notice was truncated: $path" }
    $text = $lines[($Entry.first_line - 1)..($Entry.last_line - 1)] -join "`n"
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = [System.BitConverter]::ToString(
            $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($text))
        ).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha.Dispose()
    }
    if ($hash -cne $Entry.sha256) {
        throw "Embedded source notice changed in $path; review it before updating source-notices.json."
    }
    return $text
}
