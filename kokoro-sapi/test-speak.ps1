# M0 acceptance test: select the Kokoro voice through SAPI and speak.
# Run with 32-bit PowerShell so it uses the same SAPI view Kindle does:
#   C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File .\test-speak.ps1
$v = New-Object -ComObject SAPI.SpVoice
$kokoro = $v.GetVoices() | Where-Object { $_.GetAttribute("Name") -eq "Kokoro (SAPI5)" } | Select-Object -First 1
if (-not $kokoro) { Write-Host "Kokoro voice not found"; exit 1 }
$v.Voice = $kokoro
Write-Host "Selected voice: $($v.Voice.GetAttribute('Name'))"
Write-Host "Speaking (you should hear the M0 placeholder tone)..."
[void]$v.Speak("Kokoro engine milestone zero test.")   # synchronous
Write-Host "Speak() returned cleanly."
