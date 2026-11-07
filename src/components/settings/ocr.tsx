import React, { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronDown,
  Copy,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";
import { GpuOcrStatus, MnemoConfig, ModelsStatus, OcrEngine, OcrStatus } from "../../types";
import { openGitHubBugReport } from "../../lib/githubIssue";
import { formatBytes } from "../../lib/format";
import { Switch } from "./kit";

/* OCR row — state-aware. Calls `get_ocr_status` on mount; every outcome
   is honest: when OCR cannot work the switch is disabled next to the
   reason, never left looking functional. The hint line height is always
   reserved so resolving the status never shifts the row. */

const OCR_DESCRIPTION_KEY = "ocr.description";

/* Engines that run from a downloaded model pack inside Mnemo. */
const PACKS = {
  accurate: {
    name: "PP-OCRv6",
    statusCommand: "get_ocr_pack_status",
    downloadCommand: "download_ocr_pack",
    blurbKey: "ocr.ppocrBlurb",
  },
  indra: {
    name: "Indra OCR",
    statusCommand: "get_indra_pack_status",
    downloadCommand: "download_indra_pack",
    blurbKey: "ocr.indraBlurb",
  },
} as const;

const ENGINE_NAMES: Record<OcrEngine, string> = {
  tesseract: "Tesseract",
  accurate: "PP-OCRv6",
  indra: "Indra OCR",
};

const INSTALL_COMMANDS = [
  {
    id: "linux" as const,
    label: "Ubuntu / Debian",
    command: "sudo apt install tesseract-ocr tesseract-ocr-eng",
  },
  {
    id: "arch" as const,
    label: "Arch Linux",
    command: "sudo pacman -S tesseract tesseract-data-eng",
  },
  {
    id: "fedora" as const,
    label: "Fedora",
    command: "sudo dnf install tesseract tesseract-langpack-eng",
  },
  { id: "mac" as const, label: "macOS (Homebrew)", command: "brew install tesseract" },
  {
    id: "windows" as const,
    label: "Windows (winget)",
    command: "winget install UB-Mannheim.TesseractOCR",
  },
];

function pickPlatform(): (typeof INSTALL_COMMANDS)[number] {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return INSTALL_COMMANDS[4];
  if (/Macintosh|Mac OS/i.test(ua)) return INSTALL_COMMANDS[3];
  /* Linux webviews and anything unmatched land here — the honest default. */
  return INSTALL_COMMANDS[0];
}

/* Hybrid OCR: CPU plus an NVIDIA GPU in a helper process. Everything the user
   needs to decide is on the row: what is missing, one download button, and a
   live count once the helper runs. */
function GpuOcrRow({
  config,
  setConfig,
}: {
  config: MnemoConfig;
  setConfig: React.Dispatch<React.SetStateAction<MnemoConfig | null>>;
}) {
  const { t } = useTranslation("settings");
  const [gpu, setGpu] = useState<GpuOcrStatus | null>(null);

  const refresh = useCallback(async () => {
    try {
      setGpu(await invoke<GpuOcrStatus>("get_gpu_ocr_status"));
    } catch {
      setGpu(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const mode = config.ocr_compute ?? "cpu";
  const live = !!gpu && (gpu.downloading || gpu.helper_starting || gpu.mode === "hybrid");
  useEffect(() => {
    if (!live) return;
    const id = window.setInterval(refresh, gpu?.downloading ? 1000 : 3000);
    return () => window.clearInterval(id);
  }, [live, gpu?.downloading, refresh]);

  const download = async () => {
    try {
      setGpu(await invoke<GpuOcrStatus>("download_gpu_runtime"));
    } catch (err) {
      console.warn("download_gpu_runtime failed:", err);
    }
  };

  if (!gpu?.supported) return null;

  const percent = Math.round((gpu.downloaded / Math.max(1, gpu.total)) * 100);
  const runtimeLine = gpu.downloading
    ? gpu.phase === "verifying"
      ? t("ocr.gpuVerifying")
      : gpu.phase === "extracting"
        ? t("ocr.gpuExtracting")
        : t("ocr.gpuDownloading", { percent })
    : gpu.runtime_present
      ? t("ocr.gpuRuntimeReady")
      : (gpu.download_error ?? t("ocr.gpuRuntimeMissing", { size: formatBytes(gpu.runtime_bytes) }));

  const status = gpu.helper_ready
    ? t("ocr.gpuReady", { gpu: gpu.gpu_images, cpu: gpu.cpu_fallbacks })
    : gpu.helper_starting
      ? t("ocr.gpuStarting")
      : gpu.mode === "hybrid"
        ? (gpu.problem ?? t("ocr.gpuIdle"))
        : null;
  const statusIsProblem = !gpu.helper_ready && !gpu.helper_starting && !!gpu.problem && gpu.mode === "hybrid";

  return (
    <div className="mt-3 border-t border-line pt-3">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="text-[11px] font-medium text-fg">{t("ocr.gpuTitle")}</p>
          <p className="text-[12px] leading-[18px] text-muted">{t("ocr.gpuDescription")}</p>
        </div>
        <select
          aria-label={t("ocr.gpuAria")}
          value={mode}
          onChange={(e) =>
            setConfig((c) =>
              c ? { ...c, ocr_compute: e.target.value as "cpu" | "hybrid" } : c,
            )
          }
          className="h-8 shrink-0 rounded-control border border-line bg-[var(--panel)] px-2 text-[11px] text-fg"
        >
          <option value="cpu">{t("ocr.gpuCpuOption")}</option>
          <option value="hybrid">{t("ocr.gpuHybridOption")}</option>
        </select>
      </div>

      <div className="mt-2 flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="text-[11px] text-fg">{t("ocr.gpuRuntime")}</p>
          <p className="font-mono text-[11px] text-subtle">{runtimeLine}</p>
        </div>
        {!gpu.runtime_present && (
          <button
            type="button"
            disabled={gpu.downloading}
            onClick={download}
            className="btn btn-secondary h-8 shrink-0 px-3 text-[12px]"
          >
            <RefreshCw className={`h-3 w-3 ${gpu.downloading ? "animate-spin" : ""}`} />
            {t("ocr.gpuDownload")}
          </button>
        )}
      </div>

      {!gpu.cuda_found && (
        <p className="mt-2 flex items-center gap-1.5 text-[11px] leading-[18px] text-warning">
          <TriangleAlert className="h-3 w-3 shrink-0" />
          {t("ocr.gpuNoCuda")}
        </p>
      )}
      {gpu.overridden && (
        <p className="mt-2 text-[11px] leading-[18px] text-muted">
          {t("ocr.gpuOverride", { mode: gpu.mode })}
        </p>
      )}
      {!gpu.overridden && mode !== gpu.mode && (
        <p className="mt-2 text-[11px] leading-[18px] text-muted">{t("ocr.gpuSaveToApply")}</p>
      )}
      {status && (
        <p
          className={`mt-2 flex items-center gap-1.5 font-mono text-[11px] leading-[18px] ${
            statusIsProblem ? "text-warning" : "text-muted"
          }`}
        >
          {statusIsProblem && <TriangleAlert className="h-3 w-3 shrink-0" />}
          {status}
        </p>
      )}
    </div>
  );
}

export function OcrField({
  config,
  setConfig,
}: {
  config: MnemoConfig;
  setConfig: React.Dispatch<React.SetStateAction<MnemoConfig | null>>;
}) {
  const { t } = useTranslation("settings");
  /* null = status unknown (missing command or re-check pending) → the
     switch stays enabled, optimistic, with no hint line. */
  const [status, setStatus] = useState<OcrStatus | null>(null);
  const [pack, setPack] = useState<ModelsStatus | null>(null);
  const [packBusy, setPackBusy] = useState(false);
  const [rechecking, setRechecking] = useState(false);
  const [allPlatforms, setAllPlatforms] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const platform = pickPlatform();

  const engine: OcrEngine = config.ocr_engine ?? "tesseract";
  const packInfo = engine === "tesseract" ? null : PACKS[engine];

  const check = useCallback(async () => {
    try {
      setStatus(await invoke<OcrStatus>("get_ocr_status"));
    } catch (err) {
      console.warn("get_ocr_status not available:", err);
      setStatus(null);
    }
    if (!packInfo) return;
    try {
      setPack(await invoke<ModelsStatus>(packInfo.statusCommand));
    } catch {
      setPack(null);
    }
  }, [packInfo]);

  useEffect(() => {
    check();
  }, [check]);

  /* While the pack downloads, poll so progress moves and the status flips to
     ready (self-test included) the moment it lands. */
  const downloading = !!pack?.downloading;
  useEffect(() => {
    if (!downloading) return;
    const id = window.setInterval(check, 1000);
    return () => {
      window.clearInterval(id);
      void check();
    };
  }, [downloading, check]);

  const downloadPack = async () => {
    if (!packInfo) return;
    setPackBusy(true);
    try {
      setPack(await invoke<ModelsStatus>(packInfo.downloadCommand));
    } catch (err) {
      console.warn(`${packInfo.downloadCommand} failed:`, err);
    } finally {
      setPackBusy(false);
    }
  };

  const recheck = async () => {
    setRechecking(true);
    await check();
    setRechecking(false);
  };

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(text);
      window.setTimeout(() => setCopied((c) => (c === text ? null : c)), 1500);
    } catch {
      /* clipboard unavailable — the text stays selectable */
    }
  };

  const usingPpocr = packInfo !== null;
  /* Ready means the engine read its test image correctly, not that files exist. */
  const ppocrReady = engine === "indra" ? !!status?.indra_available : !!status?.accurate_available;
  const packProblem = engine === "indra" ? status?.indra_problem : status?.accurate_problem;
  const fallbackEngine =
    usingPpocr && !ppocrReady && status?.active_engine && status.active_engine !== "none"
      ? ENGINE_NAMES[status.active_engine]
      : null;
  /* A pack engine's own controls (download, problem) live in its section, so
     the switch stays usable while it gets ready. */
  const unavailable = usingPpocr ? false : status !== null && !status.available;
  const engineMissing = !usingPpocr && status !== null && !status.engine_present;
  const noLanguages = !usingPpocr && !engineMissing && status !== null && !status.available;

  const hint: React.ReactNode | null = usingPpocr ? (
    ppocrReady ? (
      <span className="font-mono text-muted">
        {t("ocr.readyVerified", { name: packInfo.name })}
      </span>
    ) : downloading ? (
      <span className="font-mono text-muted">{t("ocr.downloadingName", { name: packInfo.name })}</span>
    ) : status === null ? null : (
      <>
        <TriangleAlert className="h-3 w-3 shrink-0 text-warning" />
        <span className="text-warning">
          {packProblem ?? t("ocr.cannotRun", { name: packInfo.name })}
          {fallbackEngine ? t("ocr.usingFallback", { engine: fallbackEngine }) : ""}
        </span>
        <button
          type="button"
          onClick={() =>
            openGitHubBugReport({
              title: `[bug] OCR ${packInfo.name} issue: ${packProblem ?? "cannot run"}`,
              whatHappened: `OCR engine ${packInfo.name} failed with: ${packProblem ?? "cannot run"}`,
            })
          }
          className="ml-1 text-[11px] text-accent underline hover:opacity-80"
        >
          {t("ocr.reportIssue")}
        </button>
      </>
    )
  ) : engineMissing ? (
    <>
      <TriangleAlert className="h-3 w-3 shrink-0 text-warning" />
      <span className="text-warning">{t("ocr.notFound")}</span>
    </>
  ) : noLanguages ? (
    <>
      <TriangleAlert className="h-3 w-3 shrink-0 text-warning" />
      <span className="text-warning">{t("ocr.noLanguages")}</span>
    </>
  ) : status?.available ? (
    <span className="font-mono text-muted">
      {t("ocr.engineReady", { languages: status.languages.join(", ") })}
    </span>
  ) : null; /* unknown — slot reserved but empty */

  return (
    <div className={unavailable ? "border-b border-line px-4 py-3" : "px-4 py-3"}>
      <div className="flex items-center justify-between gap-6">
        <div className="min-w-0 space-y-0.5">
          <p className="text-[12px] font-medium text-fg">{t("ocr.title")}</p>
          <p className="text-[12px] leading-[18px] text-muted">{t(OCR_DESCRIPTION_KEY)}</p>
          {/* Reserved line: same height whether or not a hint resolves into it. */}
          <p className="flex min-h-[18px] items-center gap-1.5 text-[11px] leading-[18px]">
            {hint}
          </p>
        </div>
        <Switch
          checked={config.ocr_enabled ?? true}
          disabled={unavailable}
          onChange={(v) => setConfig((c) => (c ? { ...c, ocr_enabled: v } : c))}
        />
      </div>

      <div className="mt-3 flex items-center justify-between gap-4 border-t border-line pt-3">
        <div>
          <p className="text-[11px] font-medium text-fg">{t("ocr.engine")}</p>
          <p className="text-[12px] text-muted">{t("ocr.engineDescription")}</p>
        </div>
        <select
          aria-label={t("ocr.engineAria")}
          value={engine}
          onChange={(e) =>
            setConfig((c) =>
              c ? { ...c, ocr_engine: e.target.value as OcrEngine } : c,
            )
          }
          className="h-8 rounded-control border border-line bg-[var(--panel)] px-2 text-[11px] text-fg"
        >
          <option value="tesseract">{t("ocr.tesseractOption")}</option>
          <option value="accurate" disabled={!status?.accurate_compiled}>
            {t("ocr.ppocrOption")}
          </option>
          <option value="indra" disabled={!status?.accurate_compiled}>
            {t("ocr.indraOption")}
          </option>
        </select>
      </div>

      {usingPpocr && (status?.accurate_compiled ? (
        <div className="mt-3 border-t border-line pt-3">
          <div className="flex items-center justify-between gap-4">
            <div className="min-w-0">
              <p className="text-[11px] font-medium text-fg">
                {t("ocr.pack", { name: packInfo.name })}
              </p>
              <p className="text-[12px] leading-[18px] text-muted">{t(packInfo.blurbKey)}</p>
              <p className="mt-0.5 font-mono text-[11px] text-subtle">
                {pack?.downloading
                  ? pack.progress?.phase === "verifying"
                    ? t("ocr.verifying", { file: pack.progress.file })
                    : t("ocr.downloadingPercent", {
                        percent: Math.round(
                          ((pack.progress?.downloaded ?? 0) / Math.max(1, pack.progress?.total ?? 1)) * 100,
                        ),
                      })
                  : pack?.ready
                    ? ppocrReady
                      ? t("ocr.installedVerified")
                      : packProblem ?? t("ocr.installed")
                    : pack?.error
                      ? pack.error
                      : t("ocr.notInstalled")}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              {!pack?.ready && (
                <button
                  type="button"
                  disabled={packBusy || pack?.downloading}
                  onClick={downloadPack}
                  className="btn btn-secondary h-8 px-3 text-[12px]"
                >
                  <RefreshCw
                    className={`h-3 w-3 ${packBusy || pack?.downloading ? "animate-spin" : ""}`}
                  />
                  {t("ocr.download")}
                </button>
              )}
            </div>
          </div>
          {pack && (
            <div className="mt-3 space-y-1 border-t border-line pt-2">
              <p className="truncate font-mono text-[11px] text-subtle">{pack.dir}</p>
              {pack.artifacts.map((a) => (
                <div key={a.name + a.url} className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-2">
                    <span
                      className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                        a.present ? "bg-success" : "bg-subtle"
                      }`}
                    />
                    <code className="font-mono text-[11px] text-fg">{a.name}</code>
                  </div>
                  <span className="font-mono text-[11px] text-subtle">
                    {formatBytes(a.present ? a.bytes_on_disk || a.bytes_expected : a.bytes_expected)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className="mt-3 border-t border-line pt-3">
          <p className="text-[12px] leading-[18px] text-muted">
            {t("ocr.notCompiled", { name: packInfo?.name })}
          </p>
        </div>
      ))}

      {engine === "accurate" && status?.accurate_compiled && (
        <GpuOcrRow config={config} setConfig={setConfig} />
      )}

      {!unavailable ? null : engineMissing ? (
        /* tesseract absent — show the install command for this platform,
           keep the rest one chevron away, let the user re-check. */
        <div className="space-y-2 bg-accent-soft px-4 py-3">
          {status?.error && (
            <p className="break-words font-mono text-[11px] leading-4 text-muted">
              {status.error}
            </p>
          )}
          <div className="flex items-center justify-between gap-2">
            <div className="flex min-w-0 flex-1 items-center gap-2">
              <span className="shrink-0 font-mono text-[11px] text-subtle">
                {platform.label}
              </span>
              <code className="no-scrollbar min-w-0 flex-1 select-text overflow-x-auto whitespace-nowrap font-mono text-[11px] leading-5 text-fg">
                {platform.command}
              </code>
            </div>
            <button
              type="button"
              title={t("ocr.copyInstall", { command: platform.command })}
              onClick={() => copy(platform.command)}
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-elevated hover:text-fg"
            >
              {copied === platform.command ? (
                <Check className="h-3.5 w-3.5 text-accent" />
              ) : (
                <Copy className="h-3.5 w-3.5" />
              )}
            </button>
          </div>

          {allPlatforms && (
            <div className="space-y-0.5 rounded-control border border-line bg-[var(--panel)] p-1.5">
              {INSTALL_COMMANDS.map((p) => (
                <div key={p.id} className="flex min-w-0 items-center gap-2 px-1.5 py-1">
                  <span className="w-32 shrink-0 truncate text-[11px] text-subtle">
                    {p.label}
                  </span>
                  <code className="no-scrollbar min-w-0 flex-1 select-text overflow-x-auto whitespace-nowrap font-mono text-[11px] leading-4 text-fg">
                    {p.command}
                  </code>
                  <button
                    type="button"
                    title={t("ocr.copyInstall", { command: p.command })}
                    onClick={() => copy(p.command)}
                    className="flex h-7 w-7 shrink-0 items-center justify-center rounded text-subtle transition-colors hover:bg-accent-soft hover:text-fg"
                  >
                    {copied === p.command ? (
                      <Check className="h-3 w-3 text-accent" />
                    ) : (
                      <Copy className="h-3 w-3" />
                    )}
                  </button>
                </div>
              ))}
            </div>
          )}

          <div className="flex items-center justify-between gap-2 pt-0.5">
            <button
              type="button"
              aria-expanded={allPlatforms}
              onClick={() => setAllPlatforms((v) => !v)}
              className="flex h-7 items-center gap-1 pl-1 pr-2 text-[12px] text-muted transition-colors hover:text-fg"
            >
              <ChevronDown
                className={`h-3 w-3 transition-transform duration-150 ${
                  allPlatforms ? "rotate-180" : ""
                }`}
              />
              {t("ocr.otherPlatforms")}
            </button>
            <button
              type="button"
              disabled={rechecking}
              onClick={recheck}
              className="btn btn-secondary h-8 px-3 text-[12px]"
            >
              <RefreshCw
                className={`h-3 w-3 ${rechecking ? "animate-spin" : ""}`}
                aria-hidden={rechecking}
              />
              {t("ocr.recheck")}
            </button>
          </div>
        </div>
      ) : (
        /* Engine present, no language data — setup owns the download. */
        <div className="space-y-2.5 bg-accent-soft px-4 py-3">
          {status?.error && (
            <p className="break-words font-mono text-[11px] leading-4 text-muted">
              {status.error}
            </p>
          )}
          <p className="text-[12px] leading-[18px] text-muted">
            {t("ocr.languageData")}
          </p>
          <button
            type="button"
            disabled={rechecking}
            onClick={recheck}
            className="btn btn-secondary h-8 px-3 text-[12px]"
          >
            <RefreshCw className={`h-3 w-3 ${rechecking ? "animate-spin" : ""}`} aria-hidden={rechecking} />
            {t("ocr.recheck")}
          </button>
        </div>
      )}
    </div>
  );
}
