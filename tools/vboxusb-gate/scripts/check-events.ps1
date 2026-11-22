# System event-log check between runs: errors/warnings since -Since, plus every crash marker.
param([datetime]$Since = (Get-Date).AddMinutes(-10))
$ignore = 'DCOM|DistributedCOM|Dnscache|Time-Service'
$ev = Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $Since; Level = 1, 2, 3 } -ErrorAction SilentlyContinue |
    Where-Object { $_.ProviderName -notmatch $ignore }
$crash = Get-WinEvent -FilterHashtable @{ LogName = 'System'; StartTime = $Since; Id = 41, 1001, 6008 } -ErrorAction SilentlyContinue
"events since $($Since.ToString('s')): $(@($ev).Count) error/warning (DCOM/DNS/Time ignored), $(@($crash).Count) crash markers (41/1001/6008)"
$ev | Select-Object -First 15 | ForEach-Object { "  $($_.TimeCreated.ToString('s')) $($_.LevelDisplayName) $($_.ProviderName) $($_.Id): $(($_.Message -split "`n")[0])" }
