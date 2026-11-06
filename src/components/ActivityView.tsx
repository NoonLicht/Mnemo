import { openUrl } from "@tauri-apps/plugin-opener";
import { Globe, Monitor, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  forgetActivity,
  formatClock,
  formatDuration,
  getActivity,
  encryptionActive,
  getMnemoSettings,
  saveMnemoSettings,
  type ActivityReport,
  type MnemoSettings,
} from "../lib/mnemo";

/* Activity. A quiet day timeline: where the time went, then what you did in
   order. Everything is opt-in, stored on this machine, and one click away
   from deletion. */

const RANGES = [
  { hours: 1, key: "range.hour" },
  { hours: 24, key: "range.day" },
  { hours: 168, key: "range.week" },
] as const;

function appLabel(app: string): string {
  return app.replace(/\.exe$/i, "");
}

function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  hint: string;
}) {
  return (
    <label className="flex cursor-pointer items-start justify-between gap-6 py-2">
      <span className="min-w-0">
        <span className="block text-[13px] font-medium text-fg">{label}</span>
        <span className="block text-[12px] text-muted">{hint}</span>
      </span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`relative mt-0.5 h-[22px] w-[38px] shrink-0 rounded-full transition-colors duration-[160ms] ${
          checked ? "bg-[var(--ink)]" : "bg-[var(--border-strong)]"
        }`}
      >
        <span
          className={`absolute top-[2px] h-[18px] w-[18px] rounded-full bg-[var(--bg-elevated)] shadow transition-[left] duration-[160ms] ${
            checked ? "left-[18px]" : "left-[2px]"
          }`}
        />
      </button>
    </label>
  );
}

export function ActivityView() {
  const { t } = useTranslation("mnemo");
  const [hours, setHours] = useState(24);
  const [report, setReport] = useState<ActivityReport | null>(null);
  const [settings, setSettings] = useState<MnemoSettings | null>(null);
  const [confirm, setConfirm] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [encActive, setEncActive] = useState<boolean | null>(null);

  const load = useCallback(() => {
    getActivity(hours).then(setReport).catch(() => setReport(null));
  }, [hours]);

  useEffect(() => {
    getMnemoSettings().then(setSettings).catch(() => undefined);
    encryptionActive().then(setEncActive).catch(() => undefined);
  }, []);
  useEffect(() => {
    load();
    const id = setInterval(load, 30_000);
    return () => clearInterval(id);
  }, [load]);

  const update = async (patch: Partial<MnemoSettings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    await saveMnemoSettings(next);
    load();
  };

  const forget = async (minutes: number) => {
    const n = await forgetActivity(minutes);
    setConfirm(null);
    setNotice(t("activity.forgotten", { count: n }));
    setTimeout(() => setNotice(null), 2500);
    load();
  };

  const total = useMemo(() => (report?.totals ?? []).reduce((a, b) => a + b.secs, 0), [report]);
  const top = report?.totals[0]?.secs ?? 1;
  const off = settings !== null && !settings.activity_enabled && !settings.browser_history_enabled;

  return (
    <div className="mx-auto h-full w-full max-w-[760px] overflow-y-auto px-4 pb-10 pt-4">
      <div className="mb-5 flex items-end justify-between gap-4">
        <div>
          <h1 className="t-title text-fg">{t("activity.title")}</h1>
          <p className="mt-1 text-[13px] text-muted">{t("activity.subtitle")}</p>
        </div>
        <div className="flex h-8 shrink-0 items-center gap-0.5 rounded-full bg-accent-soft p-0.5 shadow-[inset_0_0_0_1px_var(--border)]">
          {RANGES.map((r) => (
            <button
              key={r.hours}
              type="button"
              onClick={() => setHours(r.hours)}
              className={`h-7 rounded-full px-3 text-[12px] font-medium transition-colors ${
                hours === r.hours
                  ? "bg-[var(--control-active)] text-fg shadow-[var(--control-active-edge)]"
                  : "text-muted hover:text-fg"
              }`}
            >
              {t(r.key)}
            </button>
          ))}
        </div>
      </div>

      <section className="surface mb-5 px-4 py-2">
        <Toggle
          checked={settings?.activity_enabled ?? false}
          onChange={(v) => update({ activity_enabled: v })}
          label={t("activity.windows")}
          hint={t("activity.windowsHint")}
        />
        <div className="h-px bg-[var(--border)]" />
        <Toggle
          checked={settings?.browser_history_enabled ?? false}
          onChange={(v) => update({ browser_history_enabled: v })}
          label={t("activity.browser")}
          hint={t("activity.browserHint")}
        />
      </section>

      <section className="surface mb-5 px-4 py-2">
        <Toggle
          checked={settings?.encrypt_at_rest ?? false}
          onChange={(v) => update({ encrypt_at_rest: v })}
          label={t("security.encrypt")}
          hint={t("security.encryptHint")}
        />
        {settings && encActive !== null && settings.encrypt_at_rest !== encActive && (
          <p className="fade-in pb-2 text-[12px] text-warning">{t("security.restart")}</p>
        )}
        <p className="pb-2 text-[12px] text-subtle">{t("security.limits")}</p>
      </section>

      {off && (
        <div className="fade-in py-12 text-center">
          <p className="font-serif text-[22px] font-semibold tracking-[-0.02em] text-fg">{t("activity.offTitle")}</p>
          <p className="mx-auto mt-2 max-w-[420px] text-[13px] text-muted">{t("activity.offBody")}</p>
        </div>
      )}

      {!off && report && report.totals.length === 0 && report.web.length === 0 && (
        <p className="fade-in py-12 text-center text-[13px] text-muted">{t("activity.empty")}</p>
      )}

      {report && report.totals.length > 0 && (
        <section className="mb-6">
          <div className="mb-2 flex items-baseline justify-between">
            <span className="t-meta text-subtle">{t("activity.byApp")}</span>
            <span className="t-meta text-subtle">{formatDuration(total)}</span>
          </div>
          <div className="grid gap-2">
            {report.totals.slice(0, 8).map((a, i) => (
              <div key={a.app} className="card-in grid gap-1" style={{ ["--stagger-delay" as string]: `${i * 28}ms` }}>
                <div className="flex items-baseline justify-between text-[13px]">
                  <span className="truncate text-fg">{appLabel(a.app)}</span>
                  <span className="t-meta text-muted">{formatDuration(a.secs)}</span>
                </div>
                <div className="h-1.5 overflow-hidden rounded-full bg-accent-soft">
                  <div
                    className="h-full rounded-full bg-[var(--ink)] opacity-80 transition-[width] duration-[520ms]"
                    style={{ width: `${Math.max(2, (a.secs / top) * 100)}%` }}
                  />
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {report && report.sessions.length > 0 && (
        <section className="mb-6">
          <span className="t-meta mb-2 block text-subtle">{t("activity.timeline")}</span>
          <ol className="grid gap-1">
            {report.sessions.slice(0, 60).map((s, i) => (
              <li key={`${s.start}-${i}`} className="surface flex items-start gap-3 px-3 py-2.5">
                <Monitor className="mt-0.5 h-4 w-4 shrink-0 text-subtle" strokeWidth={1.75} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-baseline justify-between gap-3">
                    <span className="truncate text-[13px] font-medium text-fg">{appLabel(s.app)}</span>
                    <span className="t-meta shrink-0 text-subtle">
                      {formatClock(s.start)} · {formatDuration(s.secs)}
                    </span>
                  </div>
                  {s.titles.length > 0 && (
                    <p className="truncate text-[12px] text-muted">{s.titles.slice(0, 2).join(" · ")}</p>
                  )}
                </div>
              </li>
            ))}
          </ol>
        </section>
      )}

      {report && report.web.length > 0 && (
        <section className="mb-6">
          <span className="t-meta mb-2 block text-subtle">{t("activity.web")}</span>
          <ul className="grid gap-1">
            {report.web.slice(0, 40).map((w, i) => (
              <li key={`${w.url}-${i}`}>
                <button
                  type="button"
                  title={w.url}
                  onClick={() => openUrl(w.url).catch(() => undefined)}
                  className="flex w-full items-center gap-3 rounded-[var(--r-control)] px-3 py-2 text-left transition-colors hover:bg-accent-soft"
                >
                  <Globe className="h-4 w-4 shrink-0 text-subtle" strokeWidth={1.75} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[13px] text-fg">{w.title || w.url}</span>
                    <span className="t-meta block truncate text-subtle">{w.url}</span>
                  </span>
                  <span className="t-meta shrink-0 text-subtle">{formatClock(w.ts)}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      {!off && (
        <section className="surface p-4">
          <div className="mb-2 text-[13px] font-medium text-fg">{t("activity.forgetTitle")}</div>
          <p className="mb-3 text-[12px] text-muted">{t("activity.forgetHint")}</p>
          <div className="flex flex-wrap items-center gap-2">
            {[
              { m: 60, label: t("activity.lastHour") },
              { m: 1440, label: t("activity.lastDay") },
              { m: 0, label: t("activity.everything") },
            ].map(({ m, label }) =>
              confirm === m ? (
                <button key={m} type="button" className="btn h-8 bg-danger px-3 text-white" onClick={() => forget(m)}>
                  {t("activity.confirm")}
                </button>
              ) : (
                <button key={m} type="button" className="btn btn-secondary h-8 gap-1.5 px-3" onClick={() => setConfirm(m)}>
                  <Trash2 className="h-3.5 w-3.5" strokeWidth={1.75} />
                  {label}
                </button>
              ),
            )}
            {notice && <span className="fade-in t-meta text-subtle">{notice}</span>}
          </div>
        </section>
      )}
    </div>
  );
}
