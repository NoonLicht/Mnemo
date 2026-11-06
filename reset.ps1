# ============================================================
#  Mnemo — Windows data reset script
#  Usage:
#    .\reset.ps1              # reset everything (config + index + models)
#    .\reset.ps1 -IndexOnly   # reset only the index, keep settings
#    .\reset.ps1 -DryRun      # show what would be deleted without deleting
# ============================================================

param(
    [switch]$IndexOnly,
    [switch]$DryRun
)

# --- Resolve the data directory (matches Tauri's app_data_dir on Windows) ---
$dataDir = Join-Path $env:APPDATA "mnemo"

# --- Helper: coloured output ---
function Write-Header([string]$msg) { Write-Host "`n$msg" -ForegroundColor Cyan }
function Write-Ok([string]$msg)     { Write-Host "  OK  $msg" -ForegroundColor Green }
function Write-Warn([string]$msg)   { Write-Host "  >>  $msg" -ForegroundColor Yellow }
function Write-Err([string]$msg)    { Write-Host "  !!  $msg" -ForegroundColor Red }

# --- Banner ---
Write-Host ""
Write-Host "  ██╗  ██╗███████╗███╗   ██╗███████╗ █████╗ ██╗  ██╗██╗   ██╗" -ForegroundColor Magenta
Write-Host "  ██║ ██╔╝██╔════╝████╗  ██║██╔════╝██╔══██╗██║ ██╔╝██║   ██║" -ForegroundColor Magenta
Write-Host "  █████╔╝ █████╗  ██╔██╗ ██║███████╗███████║█████╔╝ ██║   ██║" -ForegroundColor Magenta
Write-Host "  ██╔═██╗ ██╔══╝  ██║╚██╗██║╚════██║██╔══██║██╔═██╗ ██║   ██║" -ForegroundColor Magenta
Write-Host "  ██║  ██╗███████╗██║ ╚████║███████║██║  ██║██║  ██╗╚██████╔╝" -ForegroundColor Magenta
Write-Host "  ╚═╝  ╚═╝╚══════╝╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ " -ForegroundColor Magenta
Write-Host "  Reset Tool" -ForegroundColor DarkMagenta
Write-Host ""

Write-Host "  Data directory : $dataDir" -ForegroundColor Gray

if (-not (Test-Path $dataDir)) {
    Write-Warn "No Mnemo data found at $dataDir"
    Write-Host "  Nothing to reset — Mnemo will start fresh on first launch.`n" -ForegroundColor Gray
    exit 0
}

if ($DryRun) {
    Write-Warn "DRY RUN — no files will be deleted."
}

# ----------------------------------------------------------------
# Mode: -IndexOnly  ->  remove index + thumbnails, keep config.json
# ----------------------------------------------------------------
if ($IndexOnly) {
    Write-Header "Resetting index (settings will be preserved)..."

    $targets = @(
        (Join-Path $dataDir "index"),
        (Join-Path $dataDir "thumbs"),
        (Join-Path $dataDir "metadata.db"),
        (Join-Path $dataDir "metadata.db-wal"),
        (Join-Path $dataDir "metadata.db-shm")
    )

    foreach ($t in $targets) {
        if (Test-Path $t) {
            if ($DryRun) {
                Write-Warn "Would delete: $t"
            } else {
                Remove-Item $t -Recurse -Force -ErrorAction SilentlyContinue
                Write-Ok "Deleted: $t"
            }
        }
    }

    $cfg = Join-Path $dataDir "config.json"
    if (Test-Path $cfg) {
        Write-Ok "Settings preserved: $cfg"
    }

    Write-Host ""
    if ($DryRun) {
        Write-Host "  Dry run complete. Run without -DryRun to apply.`n" -ForegroundColor Yellow
    } else {
        Write-Host "  Index reset complete. Settings kept. Mnemo will re-index on next launch.`n" -ForegroundColor Green
    }
    exit 0
}

# ----------------------------------------------------------------
# Mode: full reset  ->  delete the entire mnemo data folder
# ----------------------------------------------------------------
Write-Header "Full reset — this will delete ALL Mnemo data:"
Write-Host ""
Write-Host "   Config, index, thumbnails, models, crash logs" -ForegroundColor Yellow
Write-Host "   Location: $dataDir" -ForegroundColor Yellow
Write-Host ""

if (-not $DryRun) {
    $confirm = Read-Host "  Are you sure? Type YES to confirm"
    if ($confirm -ne "YES") {
        Write-Warn "Cancelled. Nothing was deleted."
        Write-Host ""
        exit 0
    }
}

if ($DryRun) {
    Write-Warn "Would delete entire directory: $dataDir"
} else {
    Remove-Item $dataDir -Recurse -Force -ErrorAction Stop
    Write-Ok "Deleted: $dataDir"
    Write-Host ""
    Write-Host "  Full reset complete. Mnemo will start fresh on next launch.`n" -ForegroundColor Green
}
