import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/* Types and helpers for the Mnemo additions: Ask and Activity. */

export type Section = "search" | "ask" | "activity";

export interface MnemoSettings {
  activity_enabled: boolean;
  browser_history_enabled: boolean;
  activity_exclude_apps: string[];
  activity_exclude_urls: string[];
  llm_endpoint: string;
  llm_model: string;
  llm_allow_remote: boolean;
  encrypt_at_rest: boolean;
}

export type AskEvent =
  | { kind: "source"; n: number; title: string; path: string }
  | { kind: "token"; text: string }
  | { kind: "done"; text: string; cited: number[]; invalid: number[]; unverified: string[] }
  | { kind: "error"; message: string };

export interface ActivitySession {
  app: string;
  start: number;
  end: number;
  secs: number;
  titles: string[];
}

export interface ActivityReport {
  enabled: boolean;
  sessions: ActivitySession[];
  totals: { app: string; secs: number }[];
  web: { ts: number; app: string; title: string; url: string }[];
}

export const getMnemoSettings = () => invoke<MnemoSettings>("get_mnemo_settings");
export const saveMnemoSettings = (settings: MnemoSettings) =>
  invoke<void>("save_mnemo_settings", { settings });
export const getActivity = (hours: number) => invoke<ActivityReport>("get_activity", { hours });
export const forgetActivity = (minutes: number) => invoke<number>("forget_activity", { minutes });
export const encryptionActive = () => invoke<boolean>("encryption_active");
export const askQuestion = (question: string) => invoke<void>("ask", { question });

/** Subscribes to streamed Ask events. Also listens for the browser-preview mock. */
export function onAskEvent(cb: (e: AskEvent) => void): () => void {
  let off: (() => void) | null = null;
  let disposed = false;
  listen<AskEvent>("mnemo-ask", (ev) => cb(ev.payload))
    .then((fn) => {
      if (disposed) fn();
      else off = fn;
    })
    .catch(() => undefined);
  const mock = (e: Event) => cb((e as CustomEvent<AskEvent>).detail);
  window.addEventListener("mnemo-ask-mock", mock);
  return () => {
    disposed = true;
    off?.();
    window.removeEventListener("mnemo-ask-mock", mock);
  };
}

export function formatDuration(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))}s`;
  const m = Math.round(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  const rest = m % 60;
  return rest ? `${h}h ${rest}m` : `${h}h`;
}

export function formatClock(ts: number): string {
  return new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
