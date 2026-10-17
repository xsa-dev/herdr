# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=jcode
# HERDR_INTEGRATION_VERSION=1

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:HERDR_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_PANE_ID)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:HERDR_SOCKET_PATH)) { exit 0 }

$rawPayload = $env:JCODE_HOOK_PAYLOAD
try {
    $payload = if ([string]::IsNullOrWhiteSpace($rawPayload)) { $null } else { $rawPayload | ConvertFrom-Json }
} catch {
    $payload = $null
}

$event = if (-not [string]::IsNullOrWhiteSpace($env:JCODE_HOOK_EVENT)) {
    $env:JCODE_HOOK_EVENT
} elseif ($null -ne $payload -and $payload.event -is [string]) {
    $payload.event
} else {
    $null
}
if ($null -ne $event -and $event -notin @("", "session_start", "SessionStart")) { exit 0 }

$sessionId = if (-not [string]::IsNullOrWhiteSpace($env:JCODE_HOOK_SESSION_ID)) {
    $env:JCODE_HOOK_SESSION_ID
} elseif ($null -ne $payload -and $payload.session_id -is [string]) {
    $payload.session_id
} else {
    $null
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$rawSource = if (-not [string]::IsNullOrWhiteSpace($env:JCODE_HOOK_SOURCE)) {
    $env:JCODE_HOOK_SOURCE
} elseif ($null -ne $payload -and $payload.source -is [string]) {
    $payload.source
} else {
    $null
}
$sessionStartSource = if ($rawSource -in @("create", "attach", "startup")) {
    "startup"
} elseif ($rawSource -eq "resume") {
    "resume"
} else {
    $null
}

$herdr = if ([string]::IsNullOrWhiteSpace($env:HERDR_BIN_PATH)) { "herdr" } else { $env:HERDR_BIN_PATH }
$commandArgs = @(
    "pane", "report-agent-session", $env:HERDR_PANE_ID,
    "--source", "herdr:jcode", "--agent", "jcode",
    "--agent-session-id", [string]$sessionId,
    "--seq", [string][DateTime]::UtcNow.Ticks
)
if (-not [string]::IsNullOrWhiteSpace($sessionStartSource)) {
    $commandArgs += @("--session-start-source", [string]$sessionStartSource)
}
$job = $null
try {
    $job = Start-Job -ScriptBlock {
        param($Executable, [object[]]$Arguments)
        & $Executable @Arguments *> $null
    } -ArgumentList $herdr, (,$commandArgs)
    if ($null -eq (Wait-Job -Job $job -Timeout 1)) {
        Stop-Job -Job $job
    }
} catch {
    $null = $_
} finally {
    if ($null -ne $job) {
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}
