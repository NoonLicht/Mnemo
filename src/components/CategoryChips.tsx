import React from "react";
import { useTranslation } from "react-i18next";
import { CATEGORY_CHIPS, categoryCounts } from "../lib/categories";
import { Facets, SearchResult } from "../types";
import { formatComma } from "../lib/format";

/* Category segments in one floating capsule. Click toggles include;
   Alt-click toggles exclude. OR within the row, AND with the query.
   Empty categories step back instead of disappearing, so nothing reflows. */

interface CategoryChipsProps {
  results: SearchResult[];
  facets?: Facets;
  selected: Set<string>;
  excluded: Set<string>;
  onToggle: (id: string, alt: boolean) => void;
}

export const CategoryChips: React.FC<CategoryChipsProps> = ({ results, facets, selected, excluded, onToggle }) => {
  const { t } = useTranslation("search");
  const counts = facets ? { all: facets.all, recent: facets.recent, ...facets.types } : categoryCounts(results);
  const empty = selected.size === 0 && excluded.size === 0;
  const hasAny = results.length > 0;
  return (
    <div className="min-w-0 flex-1">
      <div className="control no-scrollbar inline-flex max-w-full items-center gap-0.5 overflow-x-auto rounded-control p-1">
        {CATEGORY_CHIPS.map((chip) => {
          const isExcluded = excluded.has(chip.id);
          const isSelected = chip.id === "all" ? empty : selected.has(chip.id);
          const count = counts[chip.id];
          const dim = hasAny && chip.id !== "all" && !count && !isSelected;
          return (
            <button
              key={chip.id}
              type="button"
              onClick={(e) => onToggle(chip.id, e.altKey)}
              title={isExcluded ? t("chips.excluded") : t("chips.includeHint")}
              data-active={isSelected && !isExcluded}
              className={`seg !px-3 ${isExcluded ? "text-danger line-through" : ""} ${dim ? "opacity-40" : ""}`}
            >
              <span>{t(`categories.${chip.id}`)}</span>
              {hasAny && count != null && count > 0 && (
                <span className="t-meta text-[10px] text-subtle">{formatComma(count)}</span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
};
