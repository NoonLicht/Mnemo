import React from "react";
import type { IndexingStatus, DoctorReport } from "../types";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { formatComma } from "../lib/format";
import { Stethoscope, ArrowUpCircle } from "lucide-react";

/* Status line. Index health on the left, the few keys worth knowing on the
   right. Quiet by default: it only takes the ink while indexing. */

const HINTS: {
  keys: string;
  labelKey: "actions.open" | "actions.preview" | "actions.reveal" | "actions.details";
}[] = [
  { keys: "↵", labelKey: "actions.open" },
  { keys: "Space", labelKey: "actions.preview" },
  { keys: "⇧↵", labelKey: "actions.reveal" },
  { keys: "⌘I", labelKey: "actions.details" },
];

function statusText(
  s: IndexingStatus | null | undefined,
  t: TFunction<"common">,
): { text: string; busy: boolean } {
  if (!s) return { text: "", busy: false };
  if (s.paused) return { text: t("status.paused"), busy: false };
  if (s.is_indexing || (s.phase && s.phase !== "idle")) {
    if (s.phase === "rebuilding") return { text: t("status.rebuilding"), busy: true };
    /* Background semantic embedding runs after the crawl. Showing it as
       "Indexing N of N" (the finished crawl's counts) looked stuck. */
    if (s.phase === "embedding" && !s.is_indexing)
      return { text: t("status.embedding", { count: formatComma(s.files_indexed) }), busy: true };
    const done = s.extract_done ?? s.files_indexed;
    const total = s.extract_total ?? s.total_files;
    return {
      text: total
        ? t("status.indexingProgress", { done: formatComma(done), total: formatComma(total) })
        : t("status.indexingFiles", { count: formatComma(done) }),
      busy: true,
    };
  }
  return { text: t("status.filesCount", { count: formatComma(s.files_indexed) }), busy: false };
}

export const KeyHintStrip: React.FC<{
  status?: IndexingStatus | null;
  doctor?: DoctorReport | null;
  updateAvailable?: string | null;
  onDoctor?: () => void;
  onUpdate?: () => void;
}> = ({ status, doctor, updateAvailable, onDoctor, onUpdate }) => {
  const { t } = useTranslation("common");
  const issues = doctor ? doctor.summary.warn + doctor.summary.fail : 0;
  const { text, busy } = statusText(status, t);
  return (
    <div className="relative z-10 flex h-7 shrink-0 items-center justify-between gap-4 px-3.5 text-[11px] text-subtle">
      <span className="flex items-center gap-2">
        {text && (
          <>
            <span className={`h-1.5 w-1.5 rounded-full ${busy ? "dot-pulse bg-ink" : "bg-success/70"}`} />
            <span className="t-meta">{text}</span>
          </>
        )}
        {doctor && (
          <button
            type="button"
            onClick={onDoctor}
            title={issues ? t("health.issue", { count: issues }) : t("health.ok")}
            className={`flex items-center gap-1 rounded-full px-1.5 py-0.5 transition-colors hover:bg-surface-2 ${issues ? "text-warning" : "text-subtle"}`}
          >
            <Stethoscope className="h-3.5 w-3.5" />
            {issues > 0 && <span className="t-meta">{t("health.issuesShort", { count: issues })}</span>}
          </button>
        )}
        {updateAvailable && (
          <button
            type="button"
            onClick={onUpdate}
            title={t("update.available", { version: updateAvailable })}
            className="flex items-center gap-1 rounded-full px-1.5 py-0.5 text-accent transition-colors hover:bg-surface-2"
          >
            <ArrowUpCircle className="h-3.5 w-3.5" />
            <span className="t-meta">{t("update.label")}</span>
          </button>
        )}
      </span>
      <span className="hidden items-center gap-4 sm:flex">
        {HINTS.map((h) => (
          <span key={h.labelKey} className="flex items-center gap-1.5">
            <span className="kbd">{h.keys}</span>
            <span>{t(h.labelKey)}</span>
          </span>
        ))}
      </span>
    </div>
  );
};
