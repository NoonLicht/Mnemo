import { getVersion } from "@tauri-apps/api/app";
import { DoctorReport } from "../types";

export interface ReportOptions {
  title?: string;
  whatHappened?: string;
  steps?: string;
  report?: DoctorReport | null;
  diagnostics?: string;
}

export function detectPlatform(): "Linux" | "macOS (Intel)" | "macOS (Apple Silicon)" | "Windows" {
  const ua = navigator.userAgent;
  if (/Windows/i.test(ua)) return "Windows";
  if (/Macintosh|Mac OS/i.test(ua)) {
    // Default to Apple Silicon on modern macOS if indeterminate, or Intel if specifically x86_64
    return /Intel/i.test(ua) ? "macOS (Intel)" : "macOS (Apple Silicon)";
  }
  return "Linux";
}

/**
 * Builds and opens a pre-filled GitHub Bug Report issue in the user's default browser,
 * while copying full diagnostics to the clipboard so nothing is lost.
 */
export async function openGitHubBugReport(options: ReportOptions = {}): Promise<{ copied: boolean }> {
  let version = "1.0.14";
  try {
    version = await getVersion();
  } catch {
    // Fallback to static version
  }

  const platform = detectPlatform();

  // Determine issue title
  let title = options.title ?? "[bug] ";
  if (!options.title && options.report) {
    const failedChecks = options.report.checks.filter((c) => c.status === "fail");
    const warnChecks = options.report.checks.filter((c) => c.status === "warn");
    if (failedChecks.length > 0) {
      title = `[bug] Health check failure: ${failedChecks.map((c) => c.label).join(", ")}`;
    } else if (warnChecks.length > 0) {
      title = `[bug] Health check warning: ${warnChecks.map((c) => c.label).join(", ")}`;
    }
  }

  // Format diagnostics payload
  let diagnosticsJson = options.diagnostics ?? "";
  if (!diagnosticsJson && options.report) {
    diagnosticsJson = JSON.stringify(options.report, null, 2);
  }

  // Format what-happened description
  let whatHappened = options.whatHappened ?? "";
  if (!whatHappened && options.report) {
    const problemChecks = options.report.checks.filter((c) => c.status !== "ok");
    if (problemChecks.length > 0) {
      whatHappened = problemChecks
        .map((c) => `• ${c.label} (${c.status}): ${c.detail}${c.fix ? `\n  Suggested fix: ${c.fix}` : ""}`)
        .join("\n");
    }
  }

  // Copy full diagnostics to clipboard first so user has complete un-truncated data
  let copied = false;
  if (diagnosticsJson) {
    try {
      await navigator.clipboard.writeText(diagnosticsJson);
      copied = true;
    } catch (err) {
      console.warn("Could not copy diagnostics to clipboard:", err);
    }
  }

  // Construct URL query parameters for GitHub's issue form template (.github/ISSUE_TEMPLATE/bug_report.yml)
  const params = new URLSearchParams();
  params.set("template", "bug_report.yml");
  params.set("title", title);
  params.set("version", version);
  params.set("platform", platform);

  if (whatHappened) {
    params.set("what-happened", whatHappened);
  }

  if (options.steps) {
    params.set("steps", options.steps);
  }

  // GitHub / browser URL length is capped (~2000-4000 characters).
  // If the diagnostics JSON is compact enough, pass it directly; otherwise provide a clean summary
  // and remind that full JSON is already copied to clipboard.
  if (diagnosticsJson) {
    if (diagnosticsJson.length < 1800) {
      params.set("diagnostics", diagnosticsJson);
    } else {
      const summaryText = options.report
        ? `Summary: ${options.report.summary.ok} ok, ${options.report.summary.warn} warnings, ${options.report.summary.fail} failures\n\n(Full diagnostic JSON is copied to your clipboard - please paste below!)`
        : `(Full diagnostic JSON is copied to your clipboard - please paste below!)`;
      params.set("diagnostics", summaryText);
    }
  }

  // No external tracker is contacted: the diagnostics are only copied to the clipboard.

  return { copied };
}
