param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('Prepare', 'ValidateTag')]
    [string] $Mode,
    [string] $OutputFile,
    [string] $SummaryFile,
    [string] $Tag
)

$ErrorActionPreference = 'Stop'
$script:CalVerPattern = '^\d{4}\.(?:[1-9]|1[0-2])\.(?:0|[1-9]\d*)$'
$script:TagPattern = '^v(?<version>\d{4}\.(?:[1-9]|1[0-2])\.(?:0|[1-9]\d*))$'

function Invoke-Git([string[]] $Arguments) {
    $result = & git @Arguments
    if ($LASTEXITCODE -ne 0) { throw "git $($Arguments -join ' ') failed ($LASTEXITCODE)." }
    return $result
}

function Set-GitIdentity {
    Invoke-Git @('config', 'user.name', 'github-actions[bot]') | Out-Null
    Invoke-Git @('config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com') | Out-Null
}

function Get-ReleaseTags {
    $tags = @(Invoke-Git @('tag', '--list', 'v*'))
    $valid = @()
    foreach ($tagName in $tags) {
        if ($tagName -cmatch $script:TagPattern) {
            $valid += [pscustomobject]@{ Name = $tagName; Version = $Matches.version; Year = [int]$Matches.version.Substring(0, 4); Month = [int]($Matches.version.Split('.')[1]); Patch = [int]($Matches.version.Split('.')[2]) }
        }
    }
    return $valid
}

function Get-NextVersion {
    $now = [datetime]::UtcNow
    $year = $now.ToString('yyyy', [Globalization.CultureInfo]::InvariantCulture)
    $month = $now.Month
    $matching = @(Get-ReleaseTags | Where-Object { $_.Year -eq [int]$year -and $_.Month -eq $month })
    $patch = 0
    if ($matching.Count -gt 0) { $patch = ($matching | Measure-Object -Property Patch -Maximum).Maximum + 1 }
    return "$year.$month.$patch"
}

function Get-CargoVersion([string] $Path) {
    $text = Get-Content -LiteralPath $Path -Raw
    if ($Path -eq 'Cargo.toml') {
        $matches = [regex]::Matches($text, '(?ms)^\[package\]\s*(?<body>.*?)(?=^\[|\z)')
        if ($matches.Count -ne 1) { throw 'Cargo.toml must contain exactly one [package] block.' }
        $versions = [regex]::Matches($matches[0].Groups['body'].Value, '(?m)^version\s*=\s*"(?<version>[^"]+)"\s*(?:#.*)?$')
    } else {
        $matches = [regex]::Matches($text, '(?ms)^\[\[package\]\]\s*(?<body>.*?)(?=^\[|\z)')
        $versions = @()
        foreach ($block in $matches) {
            if ($block.Groups['body'].Value -match '(?m)^name\s*=\s*"wslim"\s*$') {
                $versions += [regex]::Matches($block.Groups['body'].Value, '(?m)^version\s*=\s*"(?<version>[^"]+)"\s*$')
            }
        }
    }
    if ($versions.Count -ne 1) { throw "$Path must contain exactly one wslim package version." }
    return $versions[0].Groups['version'].Value
}

function Set-CargoVersion([string] $Path, [string] $Version) {
    $text = Get-Content -LiteralPath $Path -Raw
    if ($Path -eq 'Cargo.toml') {
        $blockPattern = '(?ms)^\[package\]\s*(?<body>.*?)(?=^\[|\z)'
        $blocks = [regex]::Matches($text, $blockPattern)
        if ($blocks.Count -ne 1) { throw 'Cargo.toml must contain exactly one [package] block.' }
        $versionPattern = '(?m)^version\s*=\s*"[^"]+"'
    } else {
        $blockPattern = '(?ms)^\[\[package\]\]\s*(?<body>.*?)(?=^\[|\z)'
        $blocks = [regex]::Matches($text, $blockPattern)
        $target = @($blocks | Where-Object { $_.Groups['body'].Value -match '(?m)^name\s*=\s*"wslim"\s*$' })
        if ($target.Count -ne 1) { throw 'Cargo.lock must contain exactly one wslim package block.' }
        $blocks = $target
        $versionPattern = '(?m)^version\s*=\s*"[^"]+"'
    }
    $block = $blocks[0]
    if ($Path -eq 'Cargo.toml') { $block = $blocks[0] }
    $body = $block.Groups['body'].Value
    $versions = [regex]::Matches($body, $versionPattern)
    if ($versions.Count -ne 1) { throw "$Path target block must contain exactly one version." }
    $replacer = [System.Text.RegularExpressions.MatchEvaluator] { param($match) ($match.Value.Substring(0, $match.Value.IndexOf('"') + 1) + $Version + '"') }
    $newBody = [regex]::new($versionPattern).Replace($body, $replacer, 1)
    $bodyIndex = $block.Groups['body'].Index
    $updated = $text.Substring(0, $bodyIndex) + $newBody + $text.Substring($bodyIndex + $body.Length)
    Set-Content -LiteralPath $Path -Value $updated -NoNewline
    if ((Get-CargoVersion $Path) -ne $Version) { throw "Failed to verify updated version in $Path." }
}

function Set-Output([string] $Key, [string] $Value) {
    if ($OutputFile) { Add-Content -LiteralPath $OutputFile -Value "$Key=$Value" }
}

switch ($Mode) {
    'Prepare' {
        if (-not $env:DEFAULT_BRANCH -or -not $env:GITHUB_REF -or -not $env:GITHUB_SHA -or -not $env:GITHUB_EVENT_NAME) { throw 'DEFAULT_BRANCH, GITHUB_REF, GITHUB_SHA, and GITHUB_EVENT_NAME are required.' }
        if ($env:GITHUB_EVENT_NAME -ne 'workflow_dispatch') { throw 'Prepare only accepts workflow_dispatch events.' }
        if ($env:GITHUB_REF -ne "refs/heads/$($env:DEFAULT_BRANCH)") { throw 'Manual release must run from the default branch.' }
        Invoke-Git @('fetch', '--no-tags', 'origin', "refs/heads/$($env:DEFAULT_BRANCH):refs/remotes/origin/$($env:DEFAULT_BRANCH)") | Out-Null
        Invoke-Git @('fetch', '--tags', 'origin') | Out-Null
        $headValue = Invoke-Git @('rev-parse', 'HEAD')
        $head = $headValue.Trim()
        $originValue = Invoke-Git @('rev-parse', "refs/remotes/origin/$($env:DEFAULT_BRANCH)")
        $origin = $originValue.Trim()
        if ($head -ne $env:GITHUB_SHA -or $head -ne $origin) { throw 'Checkout SHA is not current remote default branch head.' }
        $status = @(Invoke-Git @('status', '--porcelain'))
        if ($status.Count -ne 0) { throw 'Release preparation requires a clean working tree.' }
        $manifestVersion = Get-CargoVersion 'Cargo.toml'
        $lockVersion = Get-CargoVersion 'Cargo.lock'
        if ($manifestVersion -eq '0.1.0') {
            if ($lockVersion -ne '0.1.0') { throw 'Only matching 0.1.0 package versions are valid bootstrap.' }
        } elseif ($manifestVersion -cnotmatch $script:CalVerPattern -or $lockVersion -cne $manifestVersion) {
            throw 'Manifest and lockfile must match CalVer, or both be 0.1.0 bootstrap.'
        }
        Set-GitIdentity
        $version = Get-NextVersion
        $tag = "v$version"
        if (@(Invoke-Git @('tag', '--list', $tag)).Count -ne 0) { throw "Tag $tag already exists." }
        Set-CargoVersion 'Cargo.toml' $version
        Set-CargoVersion 'Cargo.lock' $version
        Invoke-Git @('add', '--', 'Cargo.toml', 'Cargo.lock') | Out-Null
        Invoke-Git @('commit', '-m', "chore: release v$version") | Out-Null
        $commitValue = Invoke-Git @('rev-parse', 'HEAD')
        $commit = $commitValue.Trim()
        Invoke-Git @('tag', '-a', $tag, '-m', "Release v$version") | Out-Null
        Set-Output 'base' $head
        Set-Output 'commit' $commit
        Set-Output 'tag' $tag
        Set-Output 'version' $version
        $summary = "Prepared Release v$version`n`n- Base: ``$head```n- Commit: ``$commit```n- Tag: ``$tag```n"
        if ($SummaryFile) { Add-Content -LiteralPath $SummaryFile -Value $summary }
        Write-Output "version=$version`ntag=$tag`ncommit=$commit`nbase=$head"
    }
    'ValidateTag' {
        if ($env:GITHUB_EVENT_NAME -ne 'push' -or $env:GITHUB_REF -cne "refs/tags/$Tag") { throw 'ValidateTag only accepts matching tag push events.' }
        if (-not $Tag -or $Tag -cnotmatch $script:TagPattern) { throw 'Tag must use vYYYY.M.patch format.' }
        $version = $Matches.version
        if ((Get-CargoVersion 'Cargo.toml') -cne $version -or (Get-CargoVersion 'Cargo.lock') -cne $version) { throw 'Manifest and lockfile versions do not match tag.' }
        $tagCommitValue = Invoke-Git @('rev-parse', "$Tag^{commit}")
        $tagCommit = $tagCommitValue.Trim()
        $headValue = Invoke-Git @('rev-parse', 'HEAD')
        $head = $headValue.Trim()
        if ($tagCommit -ne $head -or $head -ne $env:GITHUB_SHA) { throw 'Tag, checkout, and event SHA must resolve to the same commit.' }
        $remote = @(Invoke-Git @('ls-remote', 'origin', "refs/tags/$Tag", "refs/tags/$Tag^{}"))
        $remoteCommit = $null
        foreach ($line in $remote) { if ($line -match '^([0-9a-f]+)\s+refs/tags/.+\^\{\}$') { $remoteCommit = $Matches[1] } }
        if (-not $remoteCommit) { foreach ($line in $remote) { if ($line -match '^([0-9a-f]+)\s+refs/tags/') { $remoteCommit = $Matches[1] } } }
        if ($remoteCommit -ne $head) { throw 'Remote release tag does not resolve to checked-out commit.' }
        Set-Output 'base' $head
        Set-Output 'commit' $head
        Set-Output 'tag' $Tag
        Set-Output 'version' $version
        if ($SummaryFile) { Add-Content -LiteralPath $SummaryFile -Value "Validated Release v$version`n`n- Commit: ``$head```n- Tag: ``$Tag```n" }
    }
}
