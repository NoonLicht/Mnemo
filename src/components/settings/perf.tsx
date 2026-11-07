import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Check, Copy, RefreshCw, Trash2 } from "lucide-react";
import { Card } from "./kit";

/* Speed report: stage timings of the last scan (per drive, with NVMe/SATA/HDD
   type) and of recent searches, as Markdown ready to paste into an issue.
   Built from local records only; query text is never recorded. */
export function PerfReportCard() {
  const { t } = useTranslation("settings");
  const [report, setReport] = useState("");
  const [copied, setCopied] = useState(false);

  const load = useCallback(() => {
    invoke<string>("get_perf_report")
      .then(setReport)
      .catch((e) => setReport(String(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(report);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      /* clipboard unavailable: the text stays selectable in the box */
    }
  };

  const clear = async () => {
    await invoke("clear_perf_report").catch(() => {});
    load();
  };

  return (
    <Card title={t("perf.title")}>
      <p className="px-4 pt-3 text-[11px] text-subtle">{t("perf.description")}</p>
      <pre className="mx-4 my-3 max-h-80 overflow-auto whitespace-pre rounded-control border border-line p-3 font-mono text-[11px] leading-5 text-muted select-text">
        {report || t("perf.empty")}
      </pre>
      <div className="flex flex-wrap gap-1.5 border-t border-line p-3">
        <button type="button" onClick={copy} className="btn btn-secondary h-8 px-3 text-[12px]">
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
          {copied ? t("perf.copied") : t("perf.copy")}
        </button>
        <button type="button" onClick={load} className="btn btn-secondary h-8 px-3 text-[12px]">
          <RefreshCw className="h-3.5 w-3.5" />
          {t("perf.refresh")}
        </button>
        <button type="button" onClick={clear} className="btn btn-secondary h-8 px-3 text-[12px]">
          <Trash2 className="h-3.5 w-3.5" />
          {t("perf.clear")}
        </button>
      </div>
    </Card>
  );
}
