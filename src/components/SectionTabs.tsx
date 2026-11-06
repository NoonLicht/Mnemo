import { Activity, MessageSquareText, Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Section } from "../lib/mnemo";

/* Segmented switch between Search, Ask and Activity. Graphite on a quiet
   capsule, the same material as the search field. */

const ITEMS: { id: Section; icon: typeof Search }[] = [
  { id: "search", icon: Search },
  { id: "ask", icon: MessageSquareText },
  { id: "activity", icon: Activity },
];

export function SectionTabs({ value, onChange, compact = false }: { value: Section; onChange: (s: Section) => void; compact?: boolean }) {
  const { t } = useTranslation("mnemo");
  return (
    <div
      role="tablist"
      aria-label={t("sections")}
      className={`flex shrink-0 items-center gap-0.5 rounded-full bg-accent-soft p-0.5 shadow-[inset_0_0_0_1px_var(--border)] ${compact ? "h-8" : "h-9"}`}
    >
      {ITEMS.map(({ id, icon: Icon }) => {
        const active = value === id;
        return (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={active}
            onClick={() => onChange(id)}
            className={`flex ${compact ? "h-7" : "h-8"} items-center gap-1.5 rounded-full px-3 text-[13px] font-medium transition-[background,color,box-shadow] duration-[160ms] ${
              active
                ? "bg-[var(--control-active)] text-fg shadow-[var(--control-active-edge)]"
                : "text-muted hover:text-fg"
            }`}
          >
            <Icon className="h-3.5 w-3.5" strokeWidth={1.75} />
            <span className="hidden sm:inline">{t(`tabs.${id}`)}</span>
          </button>
        );
      })}
    </div>
  );
}
