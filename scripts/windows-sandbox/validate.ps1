param([string]$Path)
$tokens = $null
$errors = $null
[System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors) | Out-Null
$errors | Select-Object Message, Extent | Format-List
if ($errors.Count) { exit 1 }
Write-Output 'powershell_51_parse=passed'
