import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { formatBytes, formatRelativeTime } from "../lib/format";
import type { SearchResult } from "../types";

/* Hover info. A slim bar rises from the card's bottom edge on hover: the
   modified date, the size, and image dimensions when known. It fades in
   after a short pause and never covers the thumbnail. Dimensions are read
   on hover only, never during search. Right-click keeps the full menu. */

interface Details {
  width?: number | null;
  height?: number | null;
}

export function HoverDetails({ result }: { result: SearchResult }) {
  const { t } = useTranslation("results");
  const { t: tCommon } = useTranslation("common");
  const [details, setDetails] = useState<Details>({});

  useEffect(() => {
    if (result.is_dir) return;
    let cancelled = false;
    invoke<Details>("get_file_details", { path: result.path })
      .then((d) => {
        if (!cancelled && d) setDetails(d);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [result.path, result.is_dir]);

  const parts = [
    result.is_dir ? t("hover.folder") : formatBytes(result.file_size),
    details.width && details.height ? `${details.width}×${details.height}` : null,
    formatRelativeTime(result.modified, tCommon),
  ].filter(Boolean) as string[];

  return (
    <span
      aria-hidden="true"
      className="hover-reveal pointer-events-none absolute bottom-2 left-2 z-20 max-w-[calc(100%-16px)] rounded-control bg-[var(--panel)] px-2 py-1 shadow-[var(--panel-edge)] backdrop-blur-xl"
    >
      <span className="t-meta block truncate text-muted">{parts.join(" · ")}</span>
    </span>
  );
}
