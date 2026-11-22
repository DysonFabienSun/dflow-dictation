param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')]
    [string] $Tag,

    [string] $SourceUrl
)

$root = Split-Path -Parent $PSScriptRoot
$readme = Get-Content -LiteralPath (Join-Path $root 'README.zh-CN.md') -Raw -Encoding utf8 -ErrorAction Stop
$changelog = Get-Content -LiteralPath (Join-Path $root 'CHANGELOG.zh-CN.md') -Raw -Encoding utf8 -ErrorAction Stop
$install = '安装或升级'
if ($readme -cnotmatch "(?m)^## $install\r?$") {
    throw "README.zh-CN.md must contain the '## $install' heading."
}

$anchor = "release-$Tag"
$pattern = '(?ms)^<a name="{0}"></a>\r?\n\r?\n## {1}\r?\n(?<notes>.*?)(?=^(?:<a name="release-v|##[ \t])|\z)' -f
    [regex]::Escape($anchor), [regex]::Escape($Tag)
$entries = [regex]::Matches($changelog, $pattern)
$anchorPattern = '(?m)^<a name="{0}"></a>\r?$' -f [regex]::Escape($anchor)
$anchors = [regex]::Matches($changelog, $anchorPattern)
if ($entries.Count -ne 1 -or $anchors.Count -ne 1 -or $entries[0].Groups['notes'].Value -notmatch '(?m)^- \S') {
    throw "CHANGELOG.zh-CN.md must contain one entry and anchor for $Tag with release notes."
}

if ($SourceUrl) {
    $source = $SourceUrl.TrimEnd('/')
    $installAnchor = [uri]::EscapeDataString($install)
    "[更新日志]($source/CHANGELOG.zh-CN.md#$anchor) · [$install]($source/README.zh-CN.md#$installAnchor)"
}
