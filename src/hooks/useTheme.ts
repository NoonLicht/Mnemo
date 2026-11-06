import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type Theme = "light" | "dark";

const STORAGE_KEY = "mnemo.theme";

/** Older builds stored "paper" and "void". Map them so nobody loses their pick. */
export function normalizeTheme(raw: string | null | undefined): Theme {
  if (raw === "light" || raw === "paper") return "light";
  if (raw === "dark" || raw === "void") return "dark";
  return "dark";
}

/** The OS appearance: what a first run should look like. */
function systemTheme(): Theme {
  try {
    return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  } catch {
    return "dark";
  }
}

function hasStored(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== null;
  } catch {
    return false;
  }
}

function readStored(): Theme {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored === null ? systemTheme() : normalizeTheme(stored);
  } catch {
    /* storage unavailable (e.g. webview restrictions) */
    return systemTheme();
  }
}

function apply(t: Theme) {
  document.documentElement.dataset.theme = t;
}

/**
 * Theme control. Two finishes: light and dark. localStorage holds the
 * pre-paint pick, but the backend config is the durable source of truth —
 * the launcher destroys its webview on hide, so localStorage (which may not
 * survive that) must never be trusted alone. On mount the persisted config
 * wins, unless the user has already picked in this session.
 */
let lastLocalChange = 0;

export function useTheme() {
  const [theme, setThemeState] = useState<Theme>(readStored);

  useEffect(() => {
    apply(theme);
  }, [theme]);

  useEffect(() => {
    const startedAt = Date.now();
    let cancelled = false;
    const firstPaintFromSystem = !hasStored();
    invoke<{ theme?: string; is_setup_complete?: boolean }>("get_config")
      .then((cfg) => {
        if (cancelled || lastLocalChange > startedAt) return;
        // First run (setup not finished, no pick yet): match the OS instead
        // of the config's built-in "dark", and persist it so the choice is
        // stable from then on. Existing installs keep their saved theme.
        if (firstPaintFromSystem && cfg?.is_setup_complete !== true) {
          const os = systemTheme();
          setThemeState(os);
          try {
            localStorage.setItem(STORAGE_KEY, os);
          } catch {
            /* ignore */
          }
          invoke("set_theme", { theme: os }).catch(() => undefined);
          return;
        }
        const persisted = normalizeTheme(cfg?.theme);
        setThemeState((current) => {
          if (persisted === current) return current;
          try {
            localStorage.setItem(STORAGE_KEY, persisted);
          } catch {
            /* ignore */
          }
          return persisted;
        });
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const setTheme = useCallback((t: Theme) => {
    lastLocalChange = Date.now();
    setThemeState(t);
    try {
      localStorage.setItem(STORAGE_KEY, t);
    } catch {
      /* ignore */
    }
    invoke("set_theme", { theme: t }).catch((err) =>
      console.warn("set_theme not available yet:", err),
    );
  }, []);

  const toggle = useCallback(() => {
    setTheme(theme === "light" ? "dark" : "light");
  }, [theme, setTheme]);

  return { theme, setTheme, toggle };
}
