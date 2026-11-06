import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Settings as SettingsIcon, Square, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Section } from "../lib/mnemo";
import { SectionTabs } from "./SectionTabs";

/* Window chrome for app mode. Replaces the stock Windows title bar: the Mnemo mark and name on the
   left, the three sections in the middle, settings and window controls on the right. The whole
   bar drags the window; a double click maximizes it. */

function Mark({ className = "" }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="var(--ink)" />
      <path
        d="M9 22V10.5L16 17.5 23 10.5V22"
        fill="none"
        stroke="var(--accent-fg)"
        strokeWidth="2.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="16" cy="8.6" r="1.5" fill="var(--blue)" />
    </svg>
  );
}

function win() {
  try {
    return getCurrentWindow();
  } catch {
    return null;
  }
}

export function TitleBar({
  section,
  onSection,
  onSettings,
  minimal = false,
}: {
  minimal?: boolean;
  section: Section;
  onSection: (s: Section) => void;
  onSettings: () => void;
}) {
  const { t } = useTranslation("mnemo");
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const w = win();
    if (!w) return;
    let off: (() => void) | undefined;
    const sync = () => w.isMaximized().then(setMaximized).catch(() => undefined);
    sync();
    w.onResized(sync).then((f) => (off = f)).catch(() => undefined);
    return () => off?.();
  }, []);

  const ctl =
    "flex h-full w-[46px] items-center justify-center text-muted transition-colors duration-[120ms] hover:bg-accent-soft hover:text-fg";

  return (
    <header
      data-tauri-drag-region
      className="relative z-[60] flex h-10 shrink-0 items-stretch justify-between border-b border-[var(--border)] bg-bg pl-3"
    >
      <div data-tauri-drag-region className="flex min-w-0 flex-1 items-center gap-2.5">
        <Mark className="pointer-events-none h-[22px] w-[22px]" />
        <span data-tauri-drag-region className="pointer-events-none text-[13px] font-semibold tracking-[-0.02em] text-fg">
          Mnemo
        </span>
      </div>

      <div className="flex items-center">
        {!minimal && <SectionTabs value={section} onChange={onSection} compact />}
      </div>

      <div data-tauri-drag-region className="flex min-w-0 flex-1 items-stretch justify-end">
        {!minimal && (
          <button type="button" title={t("settings")} onClick={onSettings} className={ctl}>
            <SettingsIcon className="h-[15px] w-[15px]" strokeWidth={1.75} />
          </button>
        )}
        <button type="button" title={t("window.minimize")} onClick={() => win()?.minimize()} className={ctl}>
          <Minus className="h-[15px] w-[15px]" strokeWidth={1.75} />
        </button>
        <button
          type="button"
          title={maximized ? t("window.restore") : t("window.maximize")}
          onClick={() => win()?.toggleMaximize()}
          className={ctl}
        >
          {maximized ? (
            <Copy className="h-[13px] w-[13px] -scale-x-100" strokeWidth={1.75} />
          ) : (
            <Square className="h-[12px] w-[12px]" strokeWidth={1.75} />
          )}
        </button>
        <button
          type="button"
          title={t("window.close")}
          onClick={() => win()?.close()}
          className="flex h-full w-[46px] items-center justify-center text-muted transition-colors duration-[120ms] hover:bg-[#e81123] hover:text-white"
        >
          <X className="h-[15px] w-[15px]" strokeWidth={1.75} />
        </button>
      </div>
    </header>
  );
}
