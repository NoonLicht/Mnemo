import React from "react";
import { classify } from "../lib/categories";
import { HighlightedName } from "./EmptyState";

/* Typographic tile. Files without a picture are shown as type, not as an
   icon: the extension set large in the bottom-left, and the real snippet
   (when the search found one) set as body text above it. Folders show a
   single large slash. One ink, no per-type colors. */

export function extOf(filename: string, isDir: boolean): string {
  if (isDir) return "/";
  const i = filename.lastIndexOf(".");
  return i > 0 ? filename.slice(i + 1).toLowerCase() : "file";
}

export const TypeTile: React.FC<{
  filename: string;
  isDir: boolean;
  snippet?: string | null;
  query?: string;
  compact?: boolean;
}> = ({ filename, isDir, snippet, query = "", compact = false }) => {
  const ext = extOf(filename, isDir);
  const isCode = classify({ is_dir: isDir, filename }) === "code";
  const label = isDir ? "/" : `.${ext}`;
  const size = compact ? 15 : label.length > 5 ? 20 : 24;

  return (
    <span className="absolute inset-0 flex flex-col justify-between p-3">
      {snippet && !compact ? (
        <span
          className={`line-clamp-4 text-muted ${
            isCode ? "font-mono text-[11px] leading-[16px]" : "text-[12px] leading-[17px]"
          }`}
        >
          <HighlightedName text={snippet} query={query} />
        </span>
      ) : (
        <span />
      )}
      <span
        className="font-medium text-fg"
        style={{ fontSize: size, lineHeight: 1, letterSpacing: "-0.045em" }}
      >
        {label}
      </span>
    </span>
  );
};
