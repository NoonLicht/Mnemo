import { useRef } from "react";
import { Search } from "lucide-react";

/* Two picture cards for mode selection — Apple-style.
   The preview shows the window behaviour: a docked app with a taskbar for
   "Desktop app", a floating Spotlight-style palette for "Launcher".
   Shared by the setup wizard and Settings so the mental model is consistent. */

export type WindowMode = "app" | "launcher";

const MODES: WindowMode[] = ["app", "launcher"];

export function WindowModePicker({
  value,
  onChange,
  labels,
  shortcut = "Alt+Space",
  compact = false,
}: {
  value: WindowMode;
  onChange: (mode: WindowMode) => void;
  labels: {
    app: string;
    launcher: string;
    appDescription: string;
    launcherDescription: string;
    ariaLabel: string;
  };
  /** Shortcut rendered in the launcher preview. */
  shortcut?: string;
  compact?: boolean;
}) {
  const refs = useRef<Record<WindowMode, HTMLButtonElement | null>>({
    app: null,
    launcher: null,
  });

  const move = (event: React.KeyboardEvent, mode: WindowMode) => {
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const delta = event.key === "ArrowRight" || event.key === "ArrowDown" ? 1 : -1;
    const next = MODES[(MODES.indexOf(mode) + delta + MODES.length) % MODES.length];
    onChange(next);
    refs.current[next]?.focus();
  };

  const card = (mode: WindowMode) => {
    const active = value === mode;
    const label = labels[mode];
    const description = labels[mode === "app" ? "appDescription" : "launcherDescription"];
    return (
      <button
        key={mode}
        ref={(el) => { refs.current[mode] = el; }}
        type="button"
        role="radio"
        aria-checked={active}
        tabIndex={active ? 0 : -1}
        onClick={() => onChange(mode)}
        onKeyDown={(event) => move(event, mode)}
        className={`group flex flex-1 flex-col gap-3 rounded-panel p-2 text-left outline-none transition-[background-color,box-shadow] duration-200 [transition-timing-function:var(--ease)] ${
          active
            ? "bg-accent-soft shadow-[inset_0_0_0_1.5px_color-mix(in_srgb,var(--ink)_35%,transparent)]"
            : "shadow-[inset_0_0_0_1px_var(--border)] hover:bg-accent-soft/50 hover:shadow-[inset_0_0_0_1px_var(--border-strong)]"
        }`}
      >
        {/* Preview illustration */}
        {mode === "app" ? (
          <AppPreview compact={compact} />
        ) : (
          <LauncherPreview compact={compact} shortcut={shortcut} />
        )}

        {/* Label row */}
        <span className="flex w-full items-center justify-between gap-2 px-0.5 pb-1">
          <span className="min-w-0 flex-1">
            <span className="flex items-center gap-1.5">
              <span className="text-[13px] font-semibold leading-none text-fg">{label}</span>
            </span>
            <span className="mt-1 block text-[12px] leading-[17px] text-muted">
              {description}
            </span>
          </span>

          {/* Apple-style selection indicator: empty circle → filled blue dot */}
          <span
            aria-hidden
            className={`flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full transition-all duration-150 ${
              active
                ? "bg-blue shadow-[0_0_0_2px_var(--bg),0_0_0_3.5px_var(--blue)]"
                : "shadow-[inset_0_0_0_1.5px_var(--border-strong)] group-hover:shadow-[inset_0_0_0_1.5px_var(--fg-muted)]"
            }`}
          >
            {active && (
              <span className="h-[6px] w-[6px] rounded-full bg-white" />
            )}
          </span>
        </span>
      </button>
    );
  };

  return (
    <div
      role="radiogroup"
      aria-label={labels.ariaLabel}
      className="grid grid-cols-1 gap-2.5 sm:grid-cols-2"
    >
      {MODES.map(card)}
    </div>
  );
}

/* Desktop-app preview: a mini window with coloured traffic lights and a dock. */
function AppPreview({ compact }: { compact: boolean }) {
  return (
    <span
      aria-hidden
      className={`relative block w-full overflow-hidden rounded-control bg-sunken ${
        compact ? "h-24" : "h-28"
      }`}
    >
      {/* Floating mini-window */}
      <span
        className={`absolute left-1/2 flex -translate-x-1/2 flex-col overflow-hidden rounded-control border border-line-strong bg-elevated shadow-[var(--shadow-float)] transition-transform duration-200 [transition-timing-function:var(--ease)] group-hover:-translate-y-0.5 ${
          compact ? "bottom-7 top-2.5 w-[84%]" : "bottom-8 top-3 w-[82%]"
        }`}
      >
        {/* Title-bar with vivid traffic-light buttons */}
        <span className="flex h-5 shrink-0 items-center gap-[5px] border-b border-line px-2.5">
          <span className="h-[9px] w-[9px] rounded-full" style={{ background: "var(--traffic-close)" }} />
          <span className="h-[9px] w-[9px] rounded-full" style={{ background: "var(--traffic-minimize)" }} />
          <span className="h-[9px] w-[9px] rounded-full" style={{ background: "var(--traffic-zoom)" }} />
          <span className="ml-auto h-1 w-8 rounded-full bg-line" />
        </span>

        {/* Content area */}
        <span className="flex flex-1 flex-col gap-1.5 p-1.5">
          <span className="flex h-4 shrink-0 items-center gap-1 rounded-small bg-accent-soft px-1.5">
            <Search className="h-[7px] w-[7px] text-subtle" strokeWidth={2.5} />
            <span className="h-[3px] w-10 rounded-full bg-line-strong" />
          </span>
          <span className="grid flex-1 grid-cols-3 gap-1">
            <span className="flex flex-col justify-end rounded-small bg-accent-soft p-1">
              <span className="h-[3px] w-4 rounded-full" style={{ background: "var(--blue)" }} />
            </span>
            <span className="flex flex-col justify-end rounded-small bg-accent-soft p-1">
              <span className="h-[3px] w-5 rounded-full" style={{ background: "var(--pink)" }} />
            </span>
            <span className="flex flex-col justify-end rounded-small bg-accent-soft p-1">
              <span className="h-[3px] w-3 rounded-full" style={{ background: "var(--success)" }} />
            </span>
          </span>
        </span>
      </span>

      {/* Apple-style dock: rounded-square icon shapes in vivid colours */}
      <span className="absolute bottom-1.5 left-1/2 flex h-[22px] -translate-x-1/2 items-center gap-1 rounded-full border border-line bg-elevated/90 px-2 shadow-[var(--shadow-float)]">
        <span className="h-[11px] w-[11px] rounded-[3px]" style={{ background: "var(--blue)" }} />
        <span className="h-[11px] w-[11px] rounded-[3px]" style={{ background: "var(--pink)" }} />
        <span className="h-[11px] w-[11px] rounded-[3px]" style={{ background: "var(--traffic-zoom)" }} />
        <span className="h-[11px] w-[11px] rounded-[3px] bg-line-strong" />
      </span>
    </span>
  );
}

/* Launcher preview: a floating Spotlight-style palette with summon keys. */
function LauncherPreview({
  compact,
  shortcut,
}: {
  compact: boolean;
  shortcut: string;
}) {
  const keys = shortcut.split("+").filter(Boolean).slice(0, 3);
  return (
    <span
      aria-hidden
      className={`relative block w-full overflow-hidden rounded-control bg-sunken ${
        compact ? "h-24" : "h-28"
      }`}
    >
      {/* Floating palette */}
      <span
        className={`absolute left-1/2 flex w-[78%] -translate-x-1/2 flex-col overflow-hidden rounded-panel border border-line-strong bg-elevated shadow-[var(--shadow-float)] transition-transform duration-200 [transition-timing-function:var(--ease)] group-hover:-translate-y-0.5 ${
          compact ? "bottom-7 top-2.5" : "bottom-8 top-3"
        }`}
      >
        {/* Search bar */}
        <span className="flex shrink-0 items-center gap-1.5 border-b border-line px-2 py-[5px]">
          <Search className="h-[9px] w-[9px] text-muted" strokeWidth={2.5} />
          <span className="h-[3px] w-12 rounded-full bg-line-strong" />
          <span className="ml-auto h-[9px] w-px" style={{ background: "var(--blue)" }} />
        </span>

        {/* Results list */}
        <span className="flex flex-1 flex-col justify-center gap-1 p-1.5">
          <span className="flex items-center gap-1.5 rounded-small bg-accent-soft px-1.5 py-[5px]">
            <span className="h-[10px] w-[10px] shrink-0 rounded-[3px]" style={{ background: "var(--blue)", opacity: 0.6 }} />
            <span className="h-[3px] w-14 rounded-full bg-line-strong" />
          </span>
          {!compact && (
            <span className="flex items-center gap-1.5 rounded-small px-1.5 py-[5px]">
              <span className="h-[10px] w-[10px] shrink-0 rounded-[3px]" style={{ background: "var(--pink)", opacity: 0.5 }} />
              <span className="h-[3px] w-10 rounded-full bg-line" />
            </span>
          )}
        </span>
      </span>

      {/* Shortcut key hints below the palette */}
      <span className="absolute bottom-1.5 left-1/2 flex -translate-x-1/2 items-center gap-1">
        {keys.map((key, index) => (
          <span
            key={index}
            className="kbd !h-4 !min-w-4 !px-1 !text-[9px] text-muted"
          >
            {key}
          </span>
        ))}
      </span>
    </span>
  );
}
