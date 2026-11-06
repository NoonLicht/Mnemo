import React, { useEffect, useRef, useState } from "react";
import { X, Terminal, Search } from "lucide-react";
import { useTranslation } from "react-i18next";

/* Search capsule. One floating control, Spotlight-style: glyph, 22px query,
   result count, clear, settings. Solid surface,
   14px corners. A hairline runs along its base while a search is live. */

interface SearchBarProps {
  query: string;
  setQuery: (q: string) => void;
  inputRef: React.RefObject<HTMLInputElement | null>;
  onFocusInput: () => void;
  resultCount: number;
  /** Total matches before the display limit, when known. */
  total?: number;
  /** Icon buttons rendered at the right end of the capsule (settings). */
  trailing?: React.ReactNode;
}

export function SearchBar({
  query,
  setQuery,
  inputRef,
  onFocusInput,
  resultCount,
  total,
  trailing,
}: SearchBarProps) {
  const [phIdx, setPhIdx] = useState(0);
  const rotateRef = useRef(false);

  const { t } = useTranslation("search");
  const { t: tc } = useTranslation("common");

  const placeholders = [
    t("placeholders.everything"),
    t("placeholders.invoice"),
    t("placeholders.pdf"),
    t("placeholders.chart"),
  ];

  useEffect(() => {
    rotateRef.current = query === "";
    if (!query) {
      const id = window.setInterval(() => {
        if (rotateRef.current) setPhIdx((i) => (i + 1) % placeholders.length);
      }, 5000);
      return () => window.clearInterval(id);
    }
  }, [query, placeholders.length]);

  const isCommandMode = query.startsWith(">") || query.startsWith("/");
  const count = total && total > resultCount ? total : resultCount;
  const Glyph = isCommandMode ? Terminal : Search;

  return (
    <div data-tauri-drag-region className="px-3.5 pb-1.5 pt-2.5">
      <div className="relative flex h-11 items-center gap-2.5 pl-3.5 pr-1.5">
        <Glyph className="h-5 w-5 shrink-0 text-muted" strokeWidth={1.75} />
        <input
          ref={inputRef}
          type="text"
          value={query}
          spellCheck={false}
          autoComplete="off"
          autoFocus
          onChange={(e) => setQuery(e.target.value)}
          placeholder={isCommandMode ? t("command") : placeholders[phIdx % placeholders.length]}
          onFocus={onFocusInput}
          aria-label={tc("actions.search")}
          className="min-w-0 flex-1 bg-transparent text-[19px] font-medium leading-none tracking-[-0.022em] text-fg outline-none placeholder:font-normal placeholder:text-subtle"
          style={{ caretColor: "var(--blue)" }}
        />

        {query && !isCommandMode && (
          <span key={count} className="t-meta fade-in shrink-0 text-subtle">
            {count === 0 ? t("empty.noResults") : t("resultCount", { count })}
          </span>
        )}

        {query && (
          <button
            type="button"
            title={t("clearInput")}
            onClick={() => {
              setQuery("");
              inputRef.current?.focus();
            }}
            className="btn btn-ghost btn-icon h-9 w-9 shrink-0"
          >
            <X className="h-4 w-4" strokeWidth={1.75} />
          </button>
        )}

        {trailing && <div className="flex shrink-0 items-center">{trailing}</div>}
      </div>
    </div>
  );
}
