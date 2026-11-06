import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SystemStats, emptyStats } from "../types";

interface UseStatsResult {
  stats: SystemStats;
  /** True when `get_stats` exists and returned live data at least once. */
  live: boolean;
  refresh: () => void;
}

/**
 * Polls `get_stats` at a given interval (ms) while mounted. The command may
 * not exist yet (backend lands it concurrently) — every failure falls back
 * to a zeroed placeholder so callers can render gracefully. Defaults to
 * polling; pass intervalMs={null} for a one-shot fetch.
 */
export function useStats(intervalMs: number | null = 2000): UseStatsResult {
  const [stats, setStats] = useState<SystemStats>(emptyStats);
  const [live, setLive] = useState(false);
  const mounted = useRef(false);

  const fetch = useCallback(async () => {
    try {
      const res = await invoke<SystemStats>("get_stats");
      // Merge defensively — partial fields fall back to zeroes.
      const base = emptyStats();
      setStats({
        queries: { ...base.queries, ...res.queries },
        index: { ...base.index, ...res.index },
        ingestion: { ...base.ingestion, ...res.ingestion },
        runtime: { ...base.runtime, ...res.runtime },
      });
      setLive(true);
    } catch (err) {
      console.warn("get_stats not available:", err);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    fetch();
    if (intervalMs !== null) {
      const id = window.setInterval(fetch, intervalMs);
      return () => window.clearInterval(id);
    }
  }, [fetch, intervalMs]);

  return { stats, live, refresh: fetch };
}
