import type { LucideIcon } from "lucide-react";

export interface SlashCommand {
  id: string;
  name: string;
  label: string;
  description: string;
  icon: LucideIcon;
  run: () => void;
}

/* The `/` menu — discoverable commands and filters typed inline in the
   search box. Replaces the old ⌘K command palette. */

export function SlashMenu({
  items,
  activeIndex,
  onPick,
}: {
  items: SlashCommand[];
  activeIndex: number;
  onPick: (item: SlashCommand) => void;
}) {
  if (items.length === 0) return null;
  return (
    <div className="elevated menu-pop relative z-20 mx-8 mb-2 mt-3 shrink-0 overflow-hidden p-1.5">
      {items.map((item, i) => {
        const Icon = item.icon;
        const active = i === activeIndex;
        return (
          <button
            key={item.id}
            type="button"
            onMouseDown={(e) => {
              e.preventDefault();
              onPick(item);
            }}
            className={`flex w-full items-center gap-3 rounded-control px-3 py-2 text-left transition-colors ${
              active ? "bg-accent-soft" : "hover:bg-accent-soft"
            }`}
          >
            <Icon className={`h-3.5 w-3.5 shrink-0 ${active ? "text-ink" : "text-subtle"}`} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[13px] font-medium text-fg">
                /{item.name}
              </span>
              <span className="block truncate text-[12px] text-muted">{item.description}</span>
            </span>
            <span className="shrink-0 t-meta text-subtle">{item.label}</span>
          </button>
        );
      })}
    </div>
  );
}