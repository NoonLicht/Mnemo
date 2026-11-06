import React from "react";
import { useTranslation } from "react-i18next";

/* Shared primitives for the Settings pages and elsewhere: 1px-bordered
   cards, field rows, switches, badges. Tokens only. */

/** Tauri rejections are usually plain strings; extract a readable reason. */
export function reasonFrom(err: unknown, fallback = "unknown error"): string {
  if (typeof err === "string" && err) return err;
  if (err instanceof Error && err.message) return err.message;
  try {
    const text = JSON.stringify(err);
    if (text && text !== "{}" && text !== "null") return text;
  } catch {
    /* non-serializable — fall through */
  }
  return fallback;
}

export function Field({
  label,
  description,
  hint,
  children,
}: {
  label: string;
  description?: string;
  /** Quiet sub-line under the description, shown only when relevant. */
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-[52px] items-center justify-between gap-6 border-b border-line px-4 py-2.5 last:border-b-0">
      <div className="min-w-0 space-y-0.5">
        <p className="text-[13px] font-medium text-fg">{label}</p>
        {description && <p className="line-clamp-2 text-[12px] leading-[18px] text-muted">{description}</p>}
        {hint && <p className="line-clamp-2 text-[12px] leading-[18px] text-subtle">{hint}</p>}
      </div>
      {children}
    </div>
  );
}

export function Card({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="mb-5">
      {title && <p className="t-meta px-1 pb-2.5 text-subtle">{title}</p>}
      <div className="surface overflow-hidden">{children}</div>
    </section>
  );
}

export function SectionHeader({ title, description }: { title: string; description: string }) {
  return (
    <div className="pb-5">
      <h2 className="text-fg" style={{ fontSize: 24, lineHeight: "30px", fontWeight: 600, letterSpacing: "-0.03em" }}>
        {title}
      </h2>
      <p className="t-lead pt-1.5 text-muted">{description}</p>
    </div>
  );
}

export function Badge({ children }: { children: React.ReactNode }) {
  return <span className="t-meta rounded-small bg-accent-soft px-2 py-1 text-muted">{children}</span>;
}

export function Switch({
  checked,
  disabled,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-[22px] w-[38px] shrink-0 rounded-full transition-colors duration-200 ${
        checked ? "bg-fg" : "bg-accent-soft shadow-[inset_0_0_0_1px_var(--border-strong)]"
      } ${disabled ? "cursor-not-allowed opacity-40" : ""}`}
    >
      <span
        className={`absolute top-[2px] h-[18px] w-[18px] rounded-full shadow-[0_1px_3px_rgb(0_0_0/0.25)] transition-all duration-300 [transition-timing-function:var(--ease)] ${
          checked ? "left-[18px] bg-bg" : "left-[2px] bg-white"
        }`}
      />
    </button>
  );
}

export function Stepper({
  value,
  min,
  max,
  step,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
}) {
  const { t } = useTranslation("common");
  const clamp = (v: number) => Math.min(max, Math.max(min, v));
  return (
    <div className="flex h-8 items-center rounded-control bg-accent-soft shadow-[inset_0_0_0_1px_var(--border)]">
      <button type="button" onClick={() => onChange(clamp(value - step))} className="flex h-full w-8 items-center justify-center text-muted transition-colors hover:text-fg" aria-label={t("stepper.decrease")}>
        −
      </button>
      <span className="t-meta w-12 text-center text-fg">{value}</span>
      <button type="button" onClick={() => onChange(clamp(value + step))} className="flex h-full w-8 items-center justify-center text-muted transition-colors hover:text-fg" aria-label={t("stepper.increase")}>
        +
      </button>
    </div>
  );
}
