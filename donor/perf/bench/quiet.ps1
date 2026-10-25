# Чужая нагрузка машины за ~1 с (perf/bench/route.mjs): процессор всех процессов, кроме своих (Chrome стенда с меткой
# в командной строке, сам оркестратор и этот скрипт), в % всей машины; 3D-движок видеокарты чужими процессами (как
# perf/foreign.ps1), % ; три самых занятых чужих процесса по имени. Вывод — одна строка JSON.
param([string]$Mark = "", [int]$Own = 0, [int]$Ms = 1000)
$ErrorActionPreference = "SilentlyContinue"
$skip = @{}; $skip[$PID] = 1; if ($Own) { $skip[$Own] = 1 }
if ($Mark) {
  Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -like "*$Mark*" } | ForEach-Object { $skip[[int]$_.ProcessId] = 1 }
}
$a = @{}
foreach ($p in Get-Process) { try { $a[$p.Id] = $p.TotalProcessorTime.TotalMilliseconds } catch {} }
$t0 = [Diagnostics.Stopwatch]::StartNew()
$gs = (Get-Counter '\GPU Engine(*engtype_3D)\Utilization Percentage' -SampleInterval 1 -MaxSamples 1).CounterSamples
$rest = $Ms - $t0.ElapsedMilliseconds; if ($rest -gt 0) { Start-Sleep -Milliseconds $rest }
$el = [math]::Max(1, $t0.ElapsedMilliseconds)
$by = @{}
foreach ($p in Get-Process) {
  if ($skip.ContainsKey($p.Id) -or -not $a.ContainsKey($p.Id) -or $p.Id -eq 0) { continue }
  try { $d = $p.TotalProcessorTime.TotalMilliseconds - $a[$p.Id] } catch { continue }
  if ($d -gt 0) { $by[$p.ProcessName] = [double]$by[$p.ProcessName] + $d }
}
$n = [Environment]::ProcessorCount
$cpu = 0; foreach ($v in $by.Values) { $cpu += $v }
$top = $by.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 3 | ForEach-Object { '["' + $_.Key + '",' + [math]::Round(100 * $_.Value / $el / $n, 1) + ']' }
# Только дискретная видеокарта (адаптер с наибольшей выделенной памятью): встроенная видеокарта ноутбука замер не делит.
$luid = ((Get-Counter '\GPU Adapter Memory(*)\Dedicated Usage').CounterSamples | Sort-Object CookedValue -Descending | Select-Object -First 1).InstanceName -replace '_phys.*$', ''
$g = 0
foreach ($x in $gs) { if ($x.InstanceName -match 'pid_(\d+)' -and (-not $luid -or $x.InstanceName -like "*$luid*")) { if (-not $skip.ContainsKey([int]$matches[1])) { $g += $x.CookedValue } } }
'{"cpu":' + [math]::Round(100 * $cpu / $el / $n, 1) + ',"gpu":' + [math]::Round($g, 1) + ',"top":[' + ($top -join ',') + ']}'
