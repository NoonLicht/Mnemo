//! Turns raw focus events into readable sessions and per-app totals (SPEC §5.3).

use std::collections::HashMap;

use super::store::Event;

/// Events of the same app separated by less than this many seconds belong to one session.
pub const MERGE_GAP_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub app: String,
    pub start: i64,
    pub end: i64,
    /// Window titles seen during the session, longest-shown first (at most five).
    pub titles: Vec<String>,
}

impl Session {
    pub fn secs(&self) -> i64 {
        self.end - self.start
    }
}

/// Merges consecutive focus events of one app into sessions. Web visits are not sessions.
pub fn summarize(events: &[Event]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    let mut title_time: Vec<HashMap<String, i64>> = Vec::new();
    for e in events.iter().filter(|e| e.kind == "focus") {
        let mergeable = sessions
            .last()
            .is_some_and(|s| s.app == e.app && e.ts_start - s.end <= MERGE_GAP_SECS);
        if !mergeable {
            sessions.push(Session {
                app: e.app.clone(),
                start: e.ts_start,
                end: e.ts_end,
                titles: Vec::new(),
            });
            title_time.push(HashMap::new());
        }
        let s = sessions.last_mut().unwrap();
        s.end = s.end.max(e.ts_end);
        if !e.title.is_empty() {
            *title_time
                .last_mut()
                .unwrap()
                .entry(e.title.clone())
                .or_default() += (e.ts_end - e.ts_start).max(1);
        }
    }
    for (s, times) in sessions.iter_mut().zip(title_time) {
        let mut by_time: Vec<(String, i64)> = times.into_iter().collect();
        by_time.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        s.titles = by_time.into_iter().take(5).map(|(t, _)| t).collect();
    }
    sessions
}

/// Seconds of focus per app, largest first. Time between sessions is not counted.
pub fn totals_by_app(events: &[Event]) -> Vec<(String, i64)> {
    let mut totals: HashMap<&str, i64> = HashMap::new();
    for e in events.iter().filter(|e| e.kind == "focus") {
        *totals.entry(e.app.as_str()).or_default() += e.ts_end - e.ts_start;
    }
    let mut out: Vec<(String, i64)> = totals.into_iter().map(|(a, t)| (a.to_owned(), t)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(start: i64, end: i64, app: &str, title: &str) -> Event {
        Event {
            id: 0,
            ts_start: start,
            ts_end: end,
            kind: "focus".into(),
            app: app.into(),
            title: title.into(),
            url: None,
        }
    }

    #[test]
    fn merges_same_app_across_short_gaps_only() {
        let events = [
            ev(0, 100, "code", "a.rs"),
            ev(100, 130, "code", "b.rs"),
            ev(130, 200, "chrome", "Docs"),
            ev(200, 300, "code", "a.rs"),
            ev(1000, 1100, "code", "a.rs"),
        ];
        let s = summarize(&events);
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].app.as_str(), s[0].start, s[0].end), ("code", 0, 130));
        assert_eq!(s[0].titles, ["a.rs", "b.rs"]);
        assert_eq!(s[3].start, 1000, "long gap starts a new session");
    }

    #[test]
    fn totals_sorted_and_ignore_web() {
        let mut web = ev(0, 0, "chrome", "x");
        web.kind = "web".into();
        let t = totals_by_app(&[
            ev(0, 100, "code", ""),
            ev(100, 130, "chrome", ""),
            ev(130, 190, "code", ""),
            web,
        ]);
        assert_eq!(t, [("code".to_string(), 160), ("chrome".to_string(), 30)]);
    }

    #[test]
    fn empty_titles_are_skipped() {
        let s = summarize(&[ev(0, 10, "x", "")]);
        assert!(s[0].titles.is_empty());
    }
}
