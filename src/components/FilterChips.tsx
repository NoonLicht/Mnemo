import React from "react";
import { useTranslation } from "react-i18next";
import { ActiveFilter } from "../types";
import { X, Folder } from "lucide-react";
import { hintToken } from "../lib/hints";

/* Visible chips for every active backend filter — click to remove. Plus
   suggestion hints from the backend (click to apply to the query). */

interface FilterChipsProps {
  filters: ActiveFilter[];
  hints: string[];
  onRemoveFilter: (filter: ActiveFilter) => void;
  onApplyHint: (hint: string) => void;
}

export const FilterChips: React.FC<FilterChipsProps> = ({
  filters,
  hints,
  onRemoveFilter,
  onApplyHint,
}) => {
  const { t } = useTranslation("search");
  if (filters.length === 0 && hints.length === 0) return null;

  return (
    <div className="no-scrollbar flex shrink-0 items-center gap-1.5 overflow-x-auto px-4 pb-2">
      {filters.map((f, i) => (
        <button
          key={f.raw + i}
          type="button"
          title={t("removeFilter", { filter: f.raw })}
          onClick={(e) => {
            e.stopPropagation();
            onRemoveFilter(f);
          }}
          className="group flex h-6 shrink-0 items-center gap-1 rounded-full bg-sunken pl-2.5 pr-1.5 text-[11px] font-mono text-muted transition-colors duration-150 hover:text-fg"
        >
          <span className={f.negated ? "line-through decoration-danger" : ""}>{f.raw}</span>
          <X className="h-3 w-3 opacity-50 transition-opacity group-hover:opacity-100" />
        </button>
      ))}

      {hints.map((h, i) => {
        const { label, isDir } = hintToken(h);
        return (
          <button
            key={h + i}
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onApplyHint(h);
            }}
            className="flex h-6 shrink-0 items-center gap-1.5 rounded-full border border-line px-2.5 text-[11px] text-muted transition-colors duration-150 hover:bg-sunken hover:text-fg"
          >
            {isDir && <Folder className="h-3 w-3 text-accent" />}
            <span className="font-medium">{label}</span>
          </button>
        );
      })}
    </div>
  );
};
