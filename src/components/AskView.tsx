import { invoke } from "@tauri-apps/api/core";
import { ArrowUp, ChevronDown, FileText, Loader2, TriangleAlert } from "lucide-react";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  askQuestion,
  getMnemoSettings,
  onAskEvent,
  saveMnemoSettings,
  type MnemoSettings,
} from "../lib/mnemo";

/* Ask. One field, one answer. Every claim carries a numbered citation that
   opens the file; unsupported sentences and bad citations are flagged, never
   hidden. The model is whatever local server the user points it at. */

interface SourceItem {
  n: number;
  title: string;
  path: string;
}
interface Verdict {
  cited: number[];
  invalid: number[];
  unverified: string[];
}

function Answer({
  text,
  sources,
  onCite,
}: {
  text: string;
  sources: SourceItem[];
  onCite: (n: number) => void;
}) {
  const parts = text.split(/(\[\d+\])/g);
  return (
    <p className="t-lead whitespace-pre-wrap text-fg">
      {parts.map((part, i) => {
        const m = /^\[(\d+)\]$/.exec(part);
        if (!m) return <Fragment key={i}>{part}</Fragment>;
        const n = Number(m[1]);
        const known = sources.some((s) => s.n === n);
        return (
          <button
            key={i}
            type="button"
            disabled={!known}
            onClick={() => onCite(n)}
            className={`mx-0.5 inline-flex h-[18px] min-w-[18px] translate-y-[-1px] items-center justify-center rounded-full px-1 align-middle text-[11px] font-medium leading-none transition-colors ${
              known
                ? "bg-accent-soft text-fg shadow-[inset_0_0_0_1px_var(--border-strong)] hover:bg-[var(--ink)] hover:text-[var(--accent-fg)]"
                : "bg-[color-mix(in_srgb,var(--danger)_14%,transparent)] text-danger"
            }`}
          >
            {n}
          </button>
        );
      })}
    </p>
  );
}

function ModelPanel({ settings, onChange }: { settings: MnemoSettings; onChange: (s: MnemoSettings) => void }) {
  const { t } = useTranslation("mnemo");
  const [draft, setDraft] = useState(settings);
  const [saved, setSaved] = useState(false);
  useEffect(() => setDraft(settings), [settings]);
  const field =
    "h-9 w-full rounded-[var(--r-control)] bg-bg-sunken px-3 text-[13px] text-fg outline-none shadow-[inset_0_0_0_1px_var(--border)] focus:shadow-[inset_0_0_0_1px_var(--border-strong)]";
  return (
    <div className="surface fade-in mt-3 grid gap-3 p-4">
      <p className="text-[13px] text-muted">{t("ask.modelHelp")}</p>
      <label className="grid gap-1">
        <span className="t-meta text-subtle">{t("ask.endpoint")}</span>
        <input
          className={field}
          placeholder="http://127.0.0.1:11434/v1"
          value={draft.llm_endpoint}
          onChange={(e) => setDraft({ ...draft, llm_endpoint: e.target.value })}
          spellCheck={false}
        />
      </label>
      <label className="grid gap-1">
        <span className="t-meta text-subtle">{t("ask.model")}</span>
        <input
          className={field}
          placeholder="llama3.2"
          value={draft.llm_model}
          onChange={(e) => setDraft({ ...draft, llm_model: e.target.value })}
          spellCheck={false}
        />
      </label>
      <label className="flex items-start gap-2 text-[13px] text-muted">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={draft.llm_allow_remote}
          onChange={(e) => setDraft({ ...draft, llm_allow_remote: e.target.checked })}
        />
        <span>{t("ask.allowRemote")}</span>
      </label>
      <div className="flex items-center gap-3">
        <button
          type="button"
          className="btn btn-primary h-8 px-4"
          onClick={async () => {
            await saveMnemoSettings(draft);
            onChange(draft);
            setSaved(true);
            setTimeout(() => setSaved(false), 1500);
          }}
        >
          {t("save")}
        </button>
        {saved && <span className="fade-in t-meta text-subtle">{t("saved")}</span>}
      </div>
    </div>
  );
}

export function AskView() {
  const { t } = useTranslation("mnemo");
  const [question, setQuestion] = useState("");
  const [asked, setAsked] = useState("");
  const [busy, setBusy] = useState(false);
  const [text, setText] = useState("");
  const [sources, setSources] = useState<SourceItem[]>([]);
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [settings, setSettings] = useState<MnemoSettings | null>(null);
  const [showModel, setShowModel] = useState(false);
  const [flash, setFlash] = useState<number | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    getMnemoSettings().then(setSettings).catch(() => undefined);
    inputRef.current?.focus();
  }, []);

  useEffect(
    () =>
      onAskEvent((e) => {
        if (e.kind === "source") setSources((s) => (s.some((x) => x.n === e.n) ? s : [...s, e]));
        else if (e.kind === "token") setText((s) => s + e.text);
        else if (e.kind === "done") {
          setText(e.text);
          setVerdict({ cited: e.cited, invalid: e.invalid, unverified: e.unverified });
          setBusy(false);
        } else if (e.kind === "error") {
          setError(e.message);
          setBusy(false);
        }
      }),
    [],
  );

  const submit = useCallback(async () => {
    const q = question.trim();
    if (!q || busy) return;
    setAsked(q);
    setText("");
    setSources([]);
    setVerdict(null);
    setError(null);
    setBusy(true);
    try {
      await askQuestion(q);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }, [question, busy]);

  const cite = (n: number) => {
    setFlash(n);
    document.getElementById(`source-${n}`)?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    setTimeout(() => setFlash(null), 1200);
  };

  const noModel = settings !== null && settings.llm_endpoint.trim() === "";
  const empty = !asked;

  return (
    <div className="mx-auto flex h-full w-full max-w-[760px] flex-col px-4 pb-6 pt-4">
      <div className={`flex min-h-0 flex-1 flex-col overflow-y-auto ${empty ? "justify-center" : ""}`}>
        {empty && (
          <div className="fade-in mb-8 text-center">
            <h1 className="font-serif text-[34px] font-semibold leading-[40px] tracking-[-0.03em] text-fg">{t("ask.title")}</h1>
            <p className="mx-auto mt-3 max-w-[460px] text-[13px] text-muted">{t("ask.subtitle")}</p>
          </div>
        )}

        {!empty && (
          <div className="fade-in grid gap-5 pb-4">
            <p className="t-meta text-subtle">{asked}</p>

            {busy && !text && (
              <div className="flex items-center gap-2 text-[13px] text-muted">
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                {sources.length ? t("ask.writing") : t("ask.searching")}
              </div>
            )}

            {text && <Answer text={text} sources={sources} onCite={cite} />}

            {error && (
              <div className="surface flex items-start gap-2 p-3 text-[13px] text-danger">
                <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
                <span className="min-w-0 break-words">{error}</span>
              </div>
            )}

            {verdict && (verdict.invalid.length > 0 || verdict.unverified.length > 0) && (
              <div className="surface grid gap-1.5 p-3 text-[13px] text-warning">
                <div className="flex items-center gap-2 font-medium">
                  <TriangleAlert className="h-4 w-4" />
                  {t("ask.checkTitle")}
                </div>
                {verdict.invalid.length > 0 && (
                  <p className="text-muted">{t("ask.invalid", { list: verdict.invalid.join(", ") })}</p>
                )}
                {verdict.unverified.slice(0, 4).map((s, i) => (
                  <p key={i} className="text-muted">
                    {t("ask.unverified")} <span className="text-fg">{s.trim()}</span>
                  </p>
                ))}
              </div>
            )}

            {sources.length > 0 && (
              <div className="grid gap-2">
                <span className="t-meta text-subtle">{t("ask.sources")}</span>
                {sources.map((s) => (
                  <button
                    key={s.n}
                    id={`source-${s.n}`}
                    type="button"
                    onClick={() => invoke("open_file", { path: s.path }).catch(() => undefined)}
                    title={s.path}
                    className={`surface card-in flex min-w-0 items-center gap-3 p-3 text-left transition-[box-shadow,transform] duration-[160ms] hover:-translate-y-px ${
                      flash === s.n ? "[box-shadow:var(--panel-edge),0_0_0_2px_var(--blue)]" : ""
                    }`}
                  >
                    <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-accent-soft text-[11px] font-medium">
                      {s.n}
                    </span>
                    <FileText className="h-4 w-4 shrink-0 text-subtle" strokeWidth={1.75} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] font-medium text-fg">{s.title}</span>
                      <span className="t-meta block truncate text-subtle">{s.path}</span>
                    </span>
                    {verdict?.cited.includes(s.n) && (
                      <span className="t-meta shrink-0 text-success">{t("ask.cited")}</span>
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>

      <div className="shrink-0">
        <div className="surface flex items-center gap-2 py-2 pl-4 pr-2 shadow-[var(--panel-edge),var(--control-shadow)]">
          <input
            ref={inputRef}
            value={question}
            onChange={(e) => setQuestion(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
            }}
            placeholder={t("ask.placeholder")}
            className="min-w-0 flex-1 bg-transparent text-[15px] text-fg outline-none placeholder:text-subtle"
          />
          <button
            type="button"
            onClick={submit}
            disabled={busy || !question.trim()}
            aria-label={t("ask.send")}
            className="btn btn-primary h-8 w-8 !rounded-full !p-0"
          >
            {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : <ArrowUp className="h-4 w-4" strokeWidth={2} />}
          </button>
        </div>

        <div className="mt-2 flex items-center justify-between px-1">
          <button
            type="button"
            onClick={() => setShowModel((v) => !v)}
            className="flex items-center gap-1 text-[12px] text-subtle transition-colors hover:text-fg"
          >
            <ChevronDown className={`h-3 w-3 transition-transform ${showModel ? "rotate-180" : ""}`} />
            {noModel ? t("ask.noModel") : t("ask.modelLine", { model: settings?.llm_model || "default" })}
          </button>
          <span className="t-meta text-subtle">{t("ask.privacy")}</span>
        </div>
        {(showModel || noModel) && settings && (
          <ModelPanel
            settings={settings}
            onChange={(s) => {
              setSettings(s);
              if (s.llm_endpoint.trim()) setShowModel(false);
            }}
          />
        )}
      </div>
    </div>
  );
}
