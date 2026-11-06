import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import React from "react";
import { useTranslation, Trans } from "react-i18next";
import type { TFunction } from "i18next";
import { Check, Plus, ArrowRight, ArrowLeft, LoaderCircle, Sun, Moon, Download, X, FolderOpen, Minus } from "lucide-react";
import { useTheme } from "../hooks/useTheme";
import { WindowModePicker } from "./WindowModePicker";
import type { IndexingStatus, MnemoConfig, OcrEngine, OcrStatus, ModelsStatus } from "../types";

/* First run: three screens, one decision each.
   1 Welcome → 2 Pick folders → 3 Exclusions + start. */

const DEFAULT_EXCLUSIONS = [
  "node_modules", ".git", "target", "dist", "build", "out",
  ".next", ".nuxt", ".output", ".cache", ".venv", "venv",
  "__pycache__", "vendor", ".gradle", ".idea", ".vscode",
  ".Trash", "lost+found",
];

const PHASE_KEYS = {
  verifying: "scan.phases.verifying",
  walking: "scan.phases.walking",
  extracting: "scan.phases.extracting",
  embedding: "scan.phases.embedding",
  committing: "scan.phases.committing",
  paused: "scan.phases.paused",
  idle: "scan.phases.idle",
  starting: "scan.phases.starting",
} as const;


function formatEta(seconds: number, t: TFunction<"setup">): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "";
  if (seconds < 60)
    return t("scan.eta.seconds", { seconds: Math.max(1, Math.round(seconds)) });
  const mins = Math.floor(seconds / 60);
  const secs = Math.round(seconds % 60);
  if (mins < 60) return t("scan.eta.minutes", { minutes: mins, seconds: secs });
  const hours = Math.floor(mins / 60);
  return t("scan.eta.hours", { hours, minutes: mins % 60 });
}

/* Picks the engine the UI should preselect. Mirrors the backend's startup
   decision so the radio shows the engine that will actually run: the config
   choice (`engine`), or PP-OCRv6 when tesseract is missing and the accurate
   pack is compiled in. `null`/undefined fields degrade optimistically. */
function pickEngine(status: OcrStatus): OcrEngine {
  const accurateCompilable = status.accurate_compiled !== false;
  if (status.engine === "indra" && accurateCompilable) return "indra";
  if (status.engine === "accurate" && accurateCompilable) return "accurate";
  if (!status.engine_present && accurateCompilable) return "accurate";
  return "tesseract";
}

export function SetupModal({ onSetupCompleted }: { onSetupCompleted: () => void }) {
  const { t } = useTranslation("setup");
  const [step, setStep] = useState<0 | 1 | 2>(0);
  // Start at sign-in is on by default, so the app is one shortcut away after a restart.
  const startAtLogin = true;
  const [suggestedDirs, setSuggestedDirs] = useState<string[]>([]);
  const [selectedDirs, setSelectedDirs] = useState<string[]>([]);
  const [customDirInput, setCustomDirInput] = useState("");
  const [dirError, setDirError] = useState<string | null>(null);
  const [dirBusy, setDirBusy] = useState(false);
  const excludedDirs = DEFAULT_EXCLUSIONS;
  const [ocrStatus, setOcrStatus] = useState<OcrStatus | null>(null);
  const [ocrPackStatus, setOcrPackStatus] = useState<ModelsStatus | null>(null);
  const [ocrBusy, setOcrBusy] = useState(false);
  const [ocrPackBusy, setOcrPackBusy] = useState(false);
  const [indraPackStatus, setIndraPackStatus] = useState<ModelsStatus | null>(null);
  const [indraPackBusy, setIndraPackBusy] = useState(false);
  const [selectedOcrEngine, setSelectedOcrEngine] = useState<OcrEngine | null>(null);
  const [packError, setPackError] = useState<string | null>(null);
  const [shortcut, setShortcut] = useState("Alt+Space");
  // New installs default to launcher mode: one shortcut away, always.
  const [appMode, setAppMode] = useState<"app" | "launcher">("launcher");
  /* Set once the user picks a mode, so the config lookup below cannot
     overwrite a click that races the invoke. */
  const appModeTouched = useRef(false);
  const chooseAppMode = (mode: "app" | "launcher") => {
    appModeTouched.current = true;
    setAppMode(mode);
  };
  const [capturing, setCapturing] = useState(false);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [scanStatus, setScanStatus] = useState<IndexingStatus | null>(null);
  const [done, setDone] = useState(false);
  const etaRef = useRef<{ phase: string; done: number; at: number; total: number }>({
    phase: "",
    done: 0,
    at: 0,
    total: 0,
  });
  const { theme, toggle } = useTheme();

  useEffect(() => {
    invoke<string[]>("get_suggested_directories")
      .then((dirs) => {
        setSuggestedDirs(dirs);
        // Suggested folders are shown for convenience, but nothing is selected
        // by default: the user must explicitly choose what to index.
      })
      .catch((err) => console.error("Failed to load suggested dirs:", err));

    // Resume an interrupted first scan: if setup is already configured but the
    // first scan never finished, skip straight to the scanning state.
    invoke<boolean>("is_setup_configured")
      .then((configured) => {
        if (configured) setScanning(true);
      })
      .catch(() => {});

    // Running setup again keeps the existing mode; only a genuinely fresh
    // install starts on the app default. Watched folders are saved by
    // `complete_setup`, so a non-empty list means this config predates the
    // wizard run.
    invoke<MnemoConfig>("get_config")
      .then((cfg) => {
        if (appModeTouched.current) return;
        if ((cfg.watched_dirs?.length ?? 0) > 0) {
          setAppMode(cfg.app_mode === "launcher" ? "launcher" : "app");
        }
      })
      .catch(() => {});
  }, []);

  // Capture a global shortcut by pressing the key combination.
  useEffect(() => {
    if (!capturing) return;
    // Modifiers accumulate across keydowns, so tapping Ctrl then K works as
    // well as holding Ctrl+K.
    const pressed = new Set<string>();
    const normalizeModifier = (key: string) =>
      key === "Meta" ? "Super" : key;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      if (e.key === "Escape") {
        setCapturing(false);
        return;
      }
      if (["Control", "Super", "Meta", "Alt", "Shift"].includes(e.key)) {
        pressed.add(normalizeModifier(e.key));
        return;
      }
      if (e.ctrlKey) pressed.add("Control");
      if (e.metaKey) pressed.add("Super");
      if (e.altKey) pressed.add("Alt");
      if (e.shiftKey) pressed.add("Shift");
      const order = ["Control", "Super", "Alt", "Shift"];
      const parts = order.filter((m) => pressed.has(m));
      const key = e.key === " " ? "Space" : e.key.length === 1 ? e.key.toUpperCase() : e.key;
      parts.push(key);
      setShortcut(parts.join("+"));
      setCapturing(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [capturing]);

  // Load OCR state when the final screen opens. The selection mirrors the
  // engine the backend actually runs — its startup probe may already have
  // switched to PP-OCRv6 because tesseract is not installed — instead of
  // re-persisting a dead default over the fallback on Finish. An explicit
  // user choice (selection ≠ null) is never overwritten by re-entry.
  useEffect(() => {
    if (step !== 1) return;
    invoke<OcrStatus>("get_ocr_status")
      .then((s) => {
        setOcrStatus(s);
        setSelectedOcrEngine((prev) => prev ?? pickEngine(s));
      })
      .catch(() => {});
    invoke<ModelsStatus>("get_ocr_pack_status").then(setOcrPackStatus).catch(() => {});
    invoke<ModelsStatus>("get_indra_pack_status").then(setIndraPackStatus).catch(() => {});
  }, [step]);

  // Poll while the Indra pack download is running.
  useEffect(() => {
    if (step !== 1 || !indraPackStatus?.downloading) return;
    const id = window.setInterval(() => {
      invoke<ModelsStatus>("get_indra_pack_status").then(setIndraPackStatus).catch(() => {});
    }, 600);
    return () => window.clearInterval(id);
  }, [step, indraPackStatus?.downloading]);

  // Poll while an OCR pack download is running.
  useEffect(() => {
    if (step !== 1 || !ocrPackStatus?.downloading) return;
    const id = window.setInterval(() => {
      invoke<ModelsStatus>("get_ocr_pack_status").then(setOcrPackStatus).catch(() => {});
    }, 600);
    return () => window.clearInterval(id);
  }, [step, ocrPackStatus?.downloading]);

  // Poll indexing status while scanning; finish when the crawl completes.
  useEffect(() => {
    if (!scanning || done) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const st = await invoke<IndexingStatus>("get_indexing_status");
        if (cancelled) return;
        setScanStatus(st);
        const isDone = await invoke<boolean>("is_setup_complete");
        if (isDone) {
          if (!cancelled) {
            setScanStatus(st);
            setDone(true);
          }
        }
      } catch (err) {
        console.error("Scan status error:", err);
      }
    };
    tick();
    const interval = setInterval(tick, 800);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [scanning, done, onSetupCompleted]);

  const addDirs = (paths: string[]) =>
    setSelectedDirs((prev) => [...prev, ...paths.filter((p) => !prev.includes(p))]);

  const toggleDir = (dir: string) =>
    setSelectedDirs((prev) =>
      prev.includes(dir) ? prev.filter((d) => d !== dir) : [...prev, dir],
    );

  /* Native folder picker. The plugin module loads lazily so a plain-browser
     dev preview (mockTauri) only pays for it when the picker is used. */
  const browseForFolder = async () => {
    setDirError(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        directory: true,
        multiple: true,
        title: t("folders.browseTitle"),
      });
      if (!picked) return;
      addDirs(Array.isArray(picked) ? picked : [picked]);
    } catch (err) {
      setDirError(t("folders.pickerUnavailable", { error: String(err) }));
    }
  };

  /* Typed entry: accept several paths at once (comma/semicolon separated),
     expand `~`, strip quotes, verify every folder exists. Bad paths surface
     as a single inline error; good ones are added in canonical form. */
  const addManualDirs = async () => {
    const raw = customDirInput.trim();
    if (!raw || dirBusy) return;
    setDirBusy(true);
    setDirError(null);
    const ok: string[] = [];
    const failed: string[] = [];
    for (const part of raw.split(/[,;]/).map((s) => s.trim()).filter(Boolean)) {
      try {
        ok.push(await invoke<string>("validate_directory", { path: part }));
      } catch (err) {
        failed.push(String(err));
      }
    }
    if (ok.length) addDirs(ok);
    setCustomDirInput("");
    if (failed.length) setDirError(failed.join("  ·  "));
    setDirBusy(false);
  };

  const handleFinish = async () => {
    if (selectedDirs.length === 0) return;
    setIsSubmitting(true);
    try {
      await invoke("complete_setup", {
        watchedDirs: selectedDirs,
        excludedDirs: excludedDirs,
        shortcut: shortcut,
        // Keep the backend's engine choice when the user did not touch the
        // radios (e.g. status never loaded) instead of resetting to a name.
        ocrEngine: selectedOcrEngine ?? (ocrStatus ? pickEngine(ocrStatus) : "tesseract"),
        appMode,
      });
      // The default text model downloads without asking. Failure is not fatal:
      // keyword search works without it, and Settings can retry.
      invoke("download_models").catch((err) => console.warn("Model download did not start:", err));
      // Start at sign-in is on by default (see `startAtLogin`).
      if (startAtLogin) {
        await invoke("set_autostart", { enabled: true }).catch((err) =>
          console.warn("Could not enable start at sign-in:", err),
        );
      }
      setScanning(true);
    } catch (err) {
      console.error("Setup failed:", err);
      alert(t("setupFailed", { error: String(err) }));
    } finally {
      setIsSubmitting(false);
    }
  };

  /* Setup can index for a long time, so while it is up the window behaves
     like a normal app window that can be minimized and found again in the
     taskbar. When the wizard closes, the backend applies the mode the user
     chose (app or launcher); restoring launcher behavior unconditionally
     would override an app-mode choice. */
  useEffect(() => {
    const win = getCurrentWindow();
    win.setSkipTaskbar(false).catch(() => {});
    win.setAlwaysOnTop(false).catch(() => {});
    return () => {
      invoke("apply_window_mode").catch(() => undefined);
    };
  }, []);

  const minimizeButton = (
    <button
      type="button"
      title={t("minimize")}
      aria-label={t("minimize")}
      onClick={() => getCurrentWindow().minimize().catch((err) => console.warn("Could not minimize:", err))}
      className="btn btn-ghost btn-icon h-8 !rounded-full"
    >
      <Minus className="h-4 w-4" strokeWidth={1.75} />
    </button>
  );

  const themeButton = (
    <button type="button" title={t("theme")} onClick={toggle} className="btn btn-ghost btn-icon h-8 !rounded-full">
      {theme === "light" ? <Moon className="h-4 w-4" strokeWidth={1.75} /> : <Sun className="h-4 w-4" strokeWidth={1.75} />}
    </button>
  );

  if (done) {
    const files = scanStatus?.files_indexed ?? 0;
    const content = scanStatus?.content_indexed ?? 0;
    return (
      <Shell mark={t("readyMark")} trailing={<>{themeButton}{minimizeButton}</>} brand={<BrandMoment word={t("brand.done.word")} gloss={t("brand.done.gloss")} />}>
        <div className="flex flex-1 flex-col justify-center gap-10">
          <div className="space-y-3">
            <h1 className="t-title rise-in text-fg">{t("done.title")}</h1>
            <p className="t-lead rise-in max-w-sm text-muted" style={{ ["--stagger-delay" as string]: "60ms" }}>
              {appMode === "app" ? (
                t("done.openFromTaskbar")
              ) : (
                <Trans
                  i18nKey="done.pressShortcut"
                  ns="setup"
                  values={{ shortcut }}
                  components={{ kbd: <span className="kbd align-middle" /> }}
                />
              )}
            </p>
            <p className="rise-in max-w-sm text-[12px] leading-[18px] text-blue" style={{ ["--stagger-delay" as string]: "90ms" }}>
              {appMode === "app" ? t("done.quitNote") : t("done.closingNote")}
            </p>
          </div>
          <dl className="rise-in grid grid-cols-2 gap-8" style={{ ["--stagger-delay" as string]: "120ms" }}>
            <Stat value={files} label={t("done.files")} />
            <Stat value={content} label={t("done.readInside")} />
          </dl>
        </div>
        <Footer>
          <span />
          <button type="button" onClick={onSetupCompleted} className="btn btn-primary h-10 px-5">
            {t("done.start")} <ArrowRight className="h-4 w-4" strokeWidth={1.75} />
          </button>
        </Footer>
      </Shell>
    );
  }

  if (scanning) {
    const st = scanStatus;
    const files = st?.files_indexed ?? 0;
    const phase = (st?.phase === "walking" && (st?.files_indexed ?? 0) === 0) ? "verifying" : (st?.phase ?? "walking");
    const extractTotal = st?.extract_total ?? 0;
    const extracting = phase === "extracting" && extractTotal > 0;
    const doneCount = extracting ? (st?.extract_done ?? 0) : files;
    const total = extracting ? extractTotal : (st?.total_files ?? 0);
    let eta = "";
    let pct = 0;
    if (total > 0) {
      pct = Math.min(100, Math.round((doneCount / total) * 100));
      if (etaRef.current.phase !== phase || etaRef.current.total !== total) {
        etaRef.current = { phase, done: 0, at: Date.now(), total };
      }
      const elapsed = Math.max(0, (Date.now() - etaRef.current.at) / 1000);
      if (doneCount > 0 && total > doneCount && elapsed >= 1) {
        eta = formatEta((elapsed * (total - doneCount)) / doneCount, t);
      }
    }
    const phaseKey = PHASE_KEYS[phase as keyof typeof PHASE_KEYS];

    return (
      <Shell mark={t("indexingMark")} trailing={<>{themeButton}{minimizeButton}</>} brand={<BrandMoment word={t("brand.scan.word")} gloss={t("brand.scan.gloss")} busy />}>
        <div className="flex flex-1 flex-col justify-center gap-8">
          <div className="space-y-2">
            <p className="t-meta text-subtle">
              {phaseKey ? t(phaseKey) : st?.current_status ?? t("scan.phases.starting")}
            </p>
            <p className="font-medium tabular-nums text-fg" style={{ fontSize: 64, lineHeight: 1, letterSpacing: "-0.05em" }}>
              {doneCount.toLocaleString("ru-RU")}
            </p>
            <p className="t-lead text-muted">
              {total > 0 ? t("scan.ofFiles", { total: total.toLocaleString("ru-RU") }) : t("scan.filesSoFar")}
            </p>
          </div>
          <div className="max-w-sm space-y-2">
            <div className="h-px w-full overflow-hidden bg-line">
              <div className="h-full bg-ink transition-[width] duration-500 [transition-timing-function:var(--ease)]" style={{ width: `${Math.max(2, pct)}%` }} />
            </div>
            <div className="t-meta flex justify-between text-subtle">
              <span>{pct}%</span>
              <span>{eta || (pct >= 100 ? t("scan.finishing") : t("scan.estimating"))}</span>
            </div>
          </div>
        </div>
        <Footer>
          <p className="text-[12px] text-subtle">{t("scan.onlyFirstTime")}</p>
          <span />
        </Footer>
      </Shell>
    );
  }

  const effectiveOcrEngine = selectedOcrEngine ?? (ocrStatus ? pickEngine(ocrStatus) : "tesseract");
  const ocrReady = (() => {
    if (effectiveOcrEngine === "tesseract") return Boolean(ocrStatus?.available);
    if (effectiveOcrEngine === "accurate")
      return Boolean(ocrStatus?.accurate_available || ocrPackStatus?.ready);
    if (effectiveOcrEngine === "indra")
      return Boolean(ocrStatus?.indra_available || indraPackStatus?.ready);
    return false;
  })();

  return (
    <Shell
      mark={`0${step + 1} / 03`}
      trailing={<>{themeButton}{minimizeButton}</>}
      brand={<BrandMoment word={t(`brand.step${Math.min(step, 2)}.word` as never)} gloss={t(`brand.step${Math.min(step, 2)}.gloss` as never)} />}
    >
      {step === 0 && (
        <div key="s0" className="section-enter flex min-h-0 flex-1 flex-col gap-8 overflow-y-auto pt-2">
          <div className="space-y-2">
            <h1 className="t-title text-fg">{t("folders.title")}</h1>
            <p className="t-lead text-muted">{t("folders.lead")}</p>
          </div>
          <div className="flex flex-col gap-2">
            {[...suggestedDirs, ...selectedDirs.filter((d) => !suggestedDirs.includes(d))].map((dir, i) => {
              const selected = selectedDirs.includes(dir);
              const name = dir.split(/[\\/]/).filter(Boolean).pop() || dir;
              return (
                <button
                  key={dir}
                  type="button"
                  onClick={() => toggleDir(dir)}
                  aria-pressed={selected}
                  style={{ ["--stagger-delay" as string]: `${i * 40}ms` }}
                  className={`surface rise-in group flex h-14 items-center gap-4 px-4 text-left transition-[box-shadow,transform] duration-200 active:scale-[0.99] ${
                    selected ? "![box-shadow:inset_0_0_0_1.5px_color-mix(in_srgb,var(--ink)_45%,transparent)]" : "hover:[box-shadow:var(--panel-edge),var(--shadow-float)]"
                  }`}
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[14px] font-medium text-fg">{name}</span>
                    <span className="block truncate text-[12px] text-subtle">{dir.replace(/^[/\\]+Users[/\\][^/\\]+/, "~")}</span>
                  </span>
                  <span
                    className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-full transition-colors duration-200 ${
                      selected ? "bg-ink text-bg" : "shadow-[inset_0_0_0_1.5px_var(--border-strong)]"
                    }`}
                  >
                    {selected && <Check className="h-3 w-3" strokeWidth={2.5} />}
                  </span>
                </button>
              );
            })}
          </div>

          <div className="flex flex-col gap-2">
            <button
              type="button"
              onClick={browseForFolder}
              className="btn btn-secondary h-11 w-full"
            >
              <FolderOpen className="h-4 w-4" strokeWidth={1.75} />
              {t("folders.browse")}
              <span className="text-[11px] text-subtle">{t("folders.pickMany")}</span>
            </button>
            <div className="flex gap-2">
              <input
                type="text"
                value={customDirInput}
                placeholder={t("folders.pathPlaceholder")}
                onChange={(e) => setCustomDirInput(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && addManualDirs()}
                spellCheck={false}
                className="field h-10 min-w-0 flex-1"
              />
              <button
                type="button"
                onClick={addManualDirs}
                disabled={!customDirInput.trim() || dirBusy}
                className="btn btn-secondary btn-icon h-10 w-10"
                aria-label={t("folders.addFolder")}
              >
                {dirBusy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <Plus className="h-4 w-4" strokeWidth={1.75} />}
              </button>
            </div>
            {dirError && (
              <p className="flex items-start gap-2 break-words rounded-control bg-danger/10 px-3 py-2 text-[12px] text-danger">
                <X className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {dirError}
              </p>
            )}
          </div>
        </div>
      )}

      {step === 1 && (
        <div key="s1" className="section-enter flex min-h-0 flex-1 flex-col gap-10 overflow-y-auto pt-2">
          <div className="space-y-2">
            <h1 className="t-title text-fg">{t("ocr.title")}</h1>
            <p className="t-lead text-muted">{t("ocr.lead")}</p>
          </div>

          <div className="flex flex-col gap-2">
            <EngineOption
              selected={selectedOcrEngine === "tesseract"}
              onSelect={() => setSelectedOcrEngine("tesseract")}
              name="Tesseract"
              note={t("ocr.tesseractNote")}
              ready={Boolean(ocrStatus?.available)}
              busy={ocrBusy}
              disabled={ocrBusy || !ocrStatus?.engine_present}
              installHint={!ocrStatus || ocrStatus.engine_present ? null : t("ocr.notInstalled")}
              onDownload={async () => {
                setPackError(null);
                setOcrBusy(true);
                try {
                  setOcrStatus(await invoke<OcrStatus>("download_ocr_language"));
                } catch (err) {
                  setPackError(t("ocr.downloadFailed", { error: String(err) }));
                } finally {
                  setOcrBusy(false);
                }
              }}
            />
            <EngineOption
              selected={selectedOcrEngine === "accurate"}
              onSelect={() => setSelectedOcrEngine("accurate")}
              name="PP-OCRv6"
              note={t("ocr.ppocrNote")}
              ready={Boolean(ocrPackStatus?.ready)}
              busy={Boolean(ocrPackStatus?.downloading || ocrPackBusy)}
              disabled={Boolean(ocrPackStatus?.downloading) || !ocrStatus?.accurate_compiled}
              installHint={!ocrStatus?.accurate_compiled ? t("ocr.notInBuild") : null}
              busyLabel={
                ocrPackStatus?.downloading && ocrPackStatus.progress
                  ? ocrPackStatus.progress.phase === "verifying"
                    ? t("ocr.verifying", { file: ocrPackStatus.progress.file })
                    : t("ocr.downloadingPercent", {
                        percent: Math.round(
                          ((ocrPackStatus.progress.downloaded ?? 0) /
                            Math.max(1, ocrPackStatus.progress.total ?? 1)) *
                            100,
                        ),
                      })
                  : undefined
              }
              onDownload={async () => {
                setPackError(null);
                setOcrPackBusy(true);
                try {
                  setOcrPackStatus(await invoke<ModelsStatus>("download_ocr_pack"));
                } catch (err) {
                  setPackError(t("ocr.downloadFailed", { error: String(err) }));
                } finally {
                  setOcrPackBusy(false);
                }
              }}
            />
            <EngineOption
              selected={selectedOcrEngine === "indra"}
              onSelect={() => setSelectedOcrEngine("indra")}
              name="Indra OCR"
              note={t("ocr.indraNote")}
              ready={Boolean(indraPackStatus?.ready)}
              busy={Boolean(indraPackStatus?.downloading || indraPackBusy)}
              disabled={Boolean(indraPackStatus?.downloading) || !ocrStatus?.accurate_compiled}
              installHint={!ocrStatus?.accurate_compiled ? t("ocr.notInBuild") : null}
              busyLabel={
                indraPackStatus?.downloading && indraPackStatus.progress
                  ? indraPackStatus.progress.phase === "verifying"
                    ? t("ocr.verifying", { file: indraPackStatus.progress.file })
                    : t("ocr.downloadingPercent", {
                        percent: Math.round(
                          ((indraPackStatus.progress.downloaded ?? 0) /
                            Math.max(1, indraPackStatus.progress.total ?? 1)) *
                            100,
                        ),
                      })
                  : undefined
              }
              onDownload={async () => {
                setPackError(null);
                setIndraPackBusy(true);
                try {
                  setIndraPackStatus(await invoke<ModelsStatus>("download_indra_pack"));
                } catch (err) {
                  setPackError(t("ocr.downloadFailed", { error: String(err) }));
                } finally {
                  setIndraPackBusy(false);
                }
              }}
            />
            {packError && (
              <p className="flex items-start gap-2 break-words rounded-control bg-danger/10 px-3 py-2 text-[12px] text-danger">
                <X className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {packError}
              </p>
            )}
            {/* Background downloads (startup auto-fallback) report failures
                through the pack status, not through an invoke rejection —
                surface them the same way. */}
            {ocrPackStatus?.error && !ocrPackStatus.downloading && (
              <p className="flex items-start gap-2 break-words rounded-control bg-danger/10 px-3 py-2 text-[12px] text-danger">
                <X className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {ocrPackStatus.error}
              </p>
            )}
          </div>

        </div>
      )}

      {step === 2 && (
        <div key="s2" className="section-enter flex min-h-0 flex-1 flex-col gap-7 overflow-y-auto pt-2">
          <div className="space-y-2">
            <h1 className="t-title text-fg">{t("mode.title")}</h1>
            <p className="t-lead text-muted">{t("mode.lead")}</p>
          </div>

          <div className="space-y-3">
            <WindowModePicker
              value={appMode}
              onChange={chooseAppMode}
              shortcut={shortcut}
              labels={{
                app: t("mode.app"),
                launcher: t("mode.launcher"),
                appDescription: t("mode.appCard"),
                launcherDescription: t("mode.launcherCard"),
                ariaLabel: t("mode.aria"),
              }}
            />
            <p className="px-1 text-[12px] leading-[18px] text-subtle">
              {appMode === "app" ? t("mode.appNote") : t("mode.launcherNote")}
            </p>
          </div>

          {appMode === "launcher" && (
            <div className="space-y-3">
              <p className="text-[12px] text-muted">{t("mode.escHides")}</p>
              <button
                type="button"
                onClick={() => setCapturing(true)}
                className={`surface flex h-14 w-full items-center justify-between px-4 text-left transition-shadow ${capturing ? "![box-shadow:inset_0_0_0_1.5px_color-mix(in_srgb,var(--ink)_45%,transparent)]" : ""}`}
              >
                <span className="flex items-center gap-1.5">
                  {capturing ? (
                    <span className="text-[13px] text-fg">{t("mode.pressShortcut")}</span>
                  ) : (
                    shortcut.split("+").map((k, i) => (
                      <span key={i} className="kbd !h-7 !min-w-7 !px-2 !text-[12px] text-fg">{k}</span>
                    ))
                  )}
                </span>
                <span className="text-[12px] text-subtle">{capturing ? t("mode.escCancel") : t("mode.change")}</span>
              </button>
              <p className="text-[12px] text-subtle">
                {t("mode.wayland")}
              </p>
            </div>
          )}
        </div>
      )}

      <Footer>
        <p className="text-[12px] text-subtle">
          {step === 0
            ? t("footer.folders", { count: selectedDirs.length })
            : ocrReady
              ? t("footer.staysLocal")
              : t("footer.downloadToContinue", {
                  engine:
                    effectiveOcrEngine === "indra"
                      ? "Indra OCR"
                      : effectiveOcrEngine === "accurate"
                        ? "PP-OCRv6"
                        : "Tesseract",
                })}
        </p>
        <div className="flex items-center gap-2">
          {step > 0 && (
            <button type="button" onClick={() => setStep((s) => (s > 0 ? ((s - 1) as 0 | 1 | 2) : 0))} className="btn btn-ghost h-10">
              <ArrowLeft className="h-4 w-4" strokeWidth={1.75} /> {t("footer.back")}
            </button>
          )}
          {step === 0 && (
            <button type="button" onClick={() => setStep(1)} disabled={selectedDirs.length === 0} className="btn btn-primary h-10 px-5">
              {t("footer.continue")} <ArrowRight className="h-4 w-4" strokeWidth={1.75} />
            </button>
          )}
          {step === 1 && (
            <button
              type="button"
              onClick={() => setStep(2)}
              disabled={selectedDirs.length === 0 || !ocrReady}
              className="btn btn-primary h-10 px-5"
            >
              {t("footer.continue")} <ArrowRight className="h-4 w-4" strokeWidth={1.75} />
            </button>
          )}
          {step === 2 && (
            <button
              type="button"
              onClick={handleFinish}
              disabled={isSubmitting || selectedDirs.length === 0}
              className="btn btn-primary h-10 px-5"
            >
              {isSubmitting && <LoaderCircle className="h-4 w-4 animate-spin" />}
              {t("footer.startIndexing")}
              {!isSubmitting && <ArrowRight className="h-4 w-4" strokeWidth={1.75} />}
            </button>
          )}
        </div>
      </Footer>
    </Shell>
  );
}

/* ---------------- layout pieces ---------------- */

function Shell({
  brand,
  mark,
  trailing,
  children,
}: {
  brand: React.ReactNode;
  mark: string;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="absolute inset-0 z-50 flex bg-bg">
      <aside data-tauri-drag-region className="relative z-10 hidden w-[40%] min-w-[300px] flex-col justify-between p-8 md:flex">
        <span className="text-[14px] font-semibold tracking-[-0.02em] text-fg">Mnemo</span>
        {brand}
        <span className="t-meta text-subtle">{mark}</span>
      </aside>
      <section className="relative z-10 flex min-w-0 flex-1 flex-col">
        <div className="surface m-2 flex min-h-0 flex-1 flex-col rounded-sheet px-7 pt-3 md:ml-0">
          <div data-tauri-drag-region className="flex h-10 shrink-0 items-center justify-end gap-1">{trailing}</div>
          <div className="mx-auto flex min-h-0 w-full max-w-[460px] flex-1 flex-col">{children}</div>
        </div>
      </section>
    </div>
  );
}

function BrandMoment({ word, gloss, busy = false }: { word: string; gloss: string; busy?: boolean }) {
  return (
    <div className="space-y-5">
      <p
        className={`rise-in whitespace-pre-line text-fg ${busy ? "dot-pulse" : ""}`}
        style={{
          fontFamily: "var(--font-serif)",
          fontWeight: 600,
          fontSize: "clamp(34px, 4.6vw, 64px)",
          lineHeight: 1.04,
          letterSpacing: "-0.035em",
        }}
      >
        {word}
      </p>
      <p className="t-moment rise-in max-w-[340px] text-muted" style={{ ["--stagger-delay" as string]: "90ms" }}>
        {gloss}
      </p>
    </div>
  );
}

function Footer({ children }: { children: React.ReactNode }) {
  return <div className="mx-auto flex h-20 w-full max-w-[460px] shrink-0 items-center justify-between gap-4">{children}</div>;
}

function Stat({ value, label }: { value: number; label: string }) {
  return (
    <div className="space-y-1">
      <dd className="font-medium tabular-nums text-fg" style={{ fontSize: 34, lineHeight: 1, letterSpacing: "-0.04em" }}>
        {value.toLocaleString("ru-RU")}
      </dd>
      <dt className="t-meta text-subtle">{label}</dt>
    </div>
  );
}

function EngineOption({
  selected,
  onSelect,
  name,
  note,
  ready,
  busy,
  disabled,
  installHint,
  busyLabel,
  onDownload,
}: {
  selected: boolean;
  onSelect: () => void;
  name: string;
  note: string;
  ready: boolean;
  busy: boolean;
  disabled: boolean;
  /** Replaces the Download button with a plain reason when set. */
  installHint?: string | null;
  busyLabel?: string;
  onDownload: () => void;
}) {
  const { t } = useTranslation("setup");
  return (
    <div
      role="radio"
      aria-checked={selected}
      aria-disabled={disabled || undefined}
      tabIndex={0}
      onClick={() => !disabled && onSelect()}
      onKeyDown={(e) => {
        if (disabled) return;
        (e.key === "Enter" || e.key === " ") && onSelect();
      }}
      className={`surface flex min-h-16 cursor-default items-center gap-4 px-4 py-3 transition-shadow duration-200 ${
        selected ? "![box-shadow:inset_0_0_0_1.5px_color-mix(in_srgb,var(--ink)_45%,transparent)]" : "hover:[box-shadow:var(--panel-edge),var(--shadow-float)]"
      } ${disabled ? "opacity-60" : ""}`}
    >
      <span className={`flex h-5 w-5 shrink-0 items-center justify-center rounded-full transition-colors ${selected ? "bg-ink" : "shadow-[inset_0_0_0_1.5px_var(--border-strong)]"}`}>
        {selected && <span className="h-1.5 w-1.5 rounded-full bg-bg" />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-[14px] font-medium text-fg">{name}</span>
        <span className="block text-[12px] text-muted">{note}</span>
      </span>
      {ready ? (
        <span className="t-meta flex items-center gap-1.5 text-success">
          <span className="h-1.5 w-1.5 rounded-full bg-success" /> {t("ocr.ready")}
        </span>
      ) : installHint ? (
        <span className="text-[12px] text-subtle">{installHint}</span>
      ) : (
        <button
          type="button"
          disabled={disabled}
          onClick={(e) => {
            e.stopPropagation();
            onDownload();
          }}
          className="btn btn-secondary h-8 px-3 text-[12px]"
        >
          {busy ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" /> : <Download className="h-3.5 w-3.5" strokeWidth={1.75} />}
          {busy ? busyLabel ?? t("ocr.downloading") : t("ocr.download")}
        </button>
      )}
    </div>
  );
}
