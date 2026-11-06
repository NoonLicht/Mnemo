import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { TypeTile } from "./TypeTile";
import { displayParent } from "../lib/format";
import { isThumbImage } from "../lib/thumbnails";
import type { SearchResult } from "../types";

/* Quick look (Space). Shows the file's preview at a readable size: the
   first page of a PDF, a video frame, or the image itself. Plain text and
   code show their first lines. Anything else shows its type icon. Escape or
   a click outside closes it. */

const TEXT_EXTS = new Set([
  "txt", "md", "log", "csv", "json", "toml", "yaml", "yml", "xml", "html", "css",
  "js", "jsx", "ts", "tsx", "rs", "py", "go", "sh", "sql", "conf", "ini", "env",
]);

function ext(name: string): string {
  return name.includes(".") ? (name.split(".").pop() ?? "").toLowerCase() : "";
}

export function PreviewDialog({
  result,
  onOpen,
  onClose,
}: {
  result: SearchResult;
  onOpen: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation("details");
  const [src, setSrc] = useState<string | null>(null);
  const [text, setText] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const previewable = !result.is_dir && (isThumbImage(result.filename));
  const textual = !result.is_dir && TEXT_EXTS.has(ext(result.filename));

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setSrc(null);
    setText(null);
    const work = async () => {
      if (previewable) {
        try {
          const data = await invoke<string | null>("get_thumbnail_data", { path: result.path });
          if (!cancelled) setSrc(data);
        } catch {
          /* falls back to the icon */
        }
      } else if (textual) {
        try {
          const body = await invoke<string>("read_file_content", { path: result.path });
          if (!cancelled) setText(body.split("\n").slice(0, 60).join("\n"));
        } catch {
          /* no text available */
        }
      }
      if (!cancelled) setLoading(false);
    };
    void work();
    return () => {
      cancelled = true;
    };
  }, [result.path, previewable, textual]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" || e.key === " ") {
        e.preventDefault();
        onClose();
      }
      if (e.key === "Enter") {
        e.preventDefault();
        onOpen();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, onOpen]);

  return (
    <div
      className="fade-in fixed inset-0 z-[80] flex items-center justify-center bg-scrim p-10 backdrop-blur-[6px]"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div role="dialog" aria-label={t("preview.title")} className="elevated overlay-in flex max-h-full w-full max-w-3xl flex-col overflow-hidden rounded-sheet">
        <div className="flex items-center justify-between gap-4 px-6 pb-4 pt-5">
          <div className="min-w-0">
            <p className="t-lead truncate font-medium text-fg">{result.filename}</p>
            <p className="truncate text-[12px] text-subtle">{displayParent(result.path)}</p>
          </div>
          <button type="button" onClick={onOpen} className="btn btn-primary h-8 shrink-0 px-4">
            {t("preview.open")}
          </button>
        </div>
        <div className="mx-3 flex min-h-[340px] flex-1 items-center justify-center overflow-auto rounded-panel bg-accent-soft p-6 shadow-[inset_0_0_0_1px_var(--border)]">
          {src ? (
            <img src={src} alt="" className="fade-in max-h-[66vh] max-w-full rounded-control object-contain shadow-[var(--shadow-float)]" />
          ) : text ? (
            <pre className="w-full self-stretch overflow-auto whitespace-pre-wrap font-mono text-[12px] leading-[18px] text-fg">
              {text}
            </pre>
          ) : (
            <div className="flex flex-col items-center gap-3 text-muted">
              <span className="relative block h-40 w-52"><TypeTile filename={result.filename} isDir={result.is_dir} /></span>
              <p className="t-meta text-subtle">{loading ? t("preview.loading") : t("preview.unavailable")}</p>
            </div>
          )}
        </div>
        <div className="flex items-center gap-4 px-6 py-3.5 text-[11px] text-subtle">
          <span className="flex items-center gap-1.5"><span className="kbd">Space</span> {t("preview.close")}</span>
          <span className="flex items-center gap-1.5"><span className="kbd">↵</span> {t("preview.open")}</span>
        </div>
      </div>
    </div>
  );
}
