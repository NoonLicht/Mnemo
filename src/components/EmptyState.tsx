import React from "react";
import { queryTerms, highlightSegments } from "../lib/format";
import { Search, SearchX } from "lucide-react";

/* Empty states: one monochrome mark, one plain line of text. No colored
   illustration, no serif caption. */

type Variant = "empty" | "noresults";

export const EmptyState: React.FC<{ variant: Variant; caption: string; children?: React.ReactNode }> = ({
  variant,
  caption,
  children,
}) => {
  const Icon = variant === "noresults" ? SearchX : Search;
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center select-none">
      <span className="flex h-12 w-12 items-center justify-center rounded-full bg-sunken text-muted">
        <Icon className="h-5 w-5" strokeWidth={1.75} />
      </span>
      <p className="text-[13px] text-muted">{caption}</p>
      {children && <div className="mt-2">{children}</div>}
    </div>
  );
};

/** Subtle match highlight inside a filename, per interaction spec.
    No truncation here — the wrapping parent owns ellipsis/clamping. */
export const HighlightedName: React.FC<{ text: string; query: string }> = ({
  text,
  query,
}) => {
  const segs = highlightSegments(text, queryTerms(query));
  return (
    <span>
      {segs.map((s, i) =>
        s.match ? (
          <mark key={i} className="bg-transparent text-ink">
            {s.text}
          </mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </span>
  );
};
