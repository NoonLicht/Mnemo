//! Tauri commands for the Mnemo additions: Ask (grounded answers with citation checks) and the
//! opt-in activity timeline. Retrieval reuses the upstream search; nothing here talks to the
//! network except the user-configured LLM endpoint (plain HTTP, loopback unless allowed).

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::AppState;
use crate::activity::service::{ActivityService, now_secs};
use crate::activity::{summarize, totals_by_app};
use crate::ask::llm::{LlmConfig, chat_stream};
use crate::ask::rag::{Source, build_messages, verify};
use crate::mnemo_settings::MnemoSettings;

static ACTIVITY: Mutex<Option<Arc<ActivityService>>> = Mutex::new(None);

/// Starts (or restarts) activity tracking according to the saved settings.
pub fn restart_activity() {
    let mut slot = ACTIVITY.lock().unwrap();
    if let Some(old) = slot.take() {
        old.shutdown();
    }
    let settings = MnemoSettings::load();
    match ActivityService::start(&settings) {
        Ok(svc) => *slot = svc,
        Err(e) => crate::crashlog::info(&format!("activity disabled: {e}")),
    }
}

pub fn shutdown_activity() {
    if let Some(svc) = ACTIVITY.lock().unwrap().take() {
        svc.shutdown();
    }
}

/// Whether the running process is actually encrypting (the setting may differ until restart).
#[tauri::command]
pub fn encryption_active() -> bool {
    crate::crypt::enabled()
}

#[tauri::command]
pub fn get_mnemo_settings() -> MnemoSettings {
    MnemoSettings::load()
}

#[tauri::command]
pub fn save_mnemo_settings(settings: MnemoSettings) -> Result<(), String> {
    settings.save()?;
    restart_activity();
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AskEvent {
    Source {
        n: usize,
        title: String,
        path: String,
    },
    Token {
        text: String,
    },
    Done {
        text: String,
        cited: Vec<usize>,
        invalid: Vec<usize>,
        unverified: Vec<String>,
    },
    Error {
        message: String,
    },
}

const MAX_SOURCES: usize = 6;

/// Answers a question from the user's indexed files. Progress is streamed as `mnemo-ask`
/// events; the command itself resolves when the answer is complete.
#[tauri::command]
pub async fn ask(
    question: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let emit = {
        let app = app.clone();
        move |ev: AskEvent| {
            let _ = app.emit("mnemo-ask", ev);
        }
    };
    let question = question.trim().to_owned();
    if question.is_empty() {
        return Ok(());
    }

    let found = crate::commands::search(question.clone(), None, None, None, state.clone()).await?;
    let hits: Vec<_> = found
        .results
        .into_iter()
        .filter(|r| !r.is_dir)
        .take(MAX_SOURCES)
        .collect();
    let paths: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
    let q = question.clone();
    let snippets =
        tauri::async_runtime::spawn_blocking(move || crate::commands::compute_snippets(paths, &q))
            .await
            .map_err(|e| e.to_string())?;

    let mut sources = Vec::new();
    for hit in &hits {
        let text = hit
            .snippet
            .clone()
            .or_else(|| {
                snippets
                    .iter()
                    .find(|s| s.path == hit.path)
                    .and_then(|s| s.snippet.clone())
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }
        let n = sources.len() + 1;
        emit(AskEvent::Source {
            n,
            title: hit.filename.clone(),
            path: hit.path.clone(),
        });
        sources.push(Source {
            n,
            title: hit.filename.clone(),
            path: hit.path.clone(),
            text,
        });
    }
    if sources.is_empty() {
        emit(AskEvent::Done {
            text: "I found nothing in your indexed files that relates to this question.".into(),
            cited: vec![],
            invalid: vec![],
            unverified: vec![],
        });
        return Ok(());
    }

    let settings = MnemoSettings::load();
    if settings.llm_endpoint.trim().is_empty() {
        emit(AskEvent::Done {
            text: "No language model is configured, so here are the best sources. Set an endpoint in Settings to get an answer.".into(),
            cited: vec![],
            invalid: vec![],
            unverified: vec![],
        });
        return Ok(());
    }
    let llm = LlmConfig {
        endpoint: settings.llm_endpoint.clone(),
        model: settings.llm_model.clone(),
        api_key: std::env::var("MNEMO_LLM_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        allow_remote: settings.llm_allow_remote,
    };
    let messages = build_messages(&question, &sources);
    let n_sources = sources.len();
    let emit2 = emit.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        chat_stream(&llm, &messages, |t| {
            emit2(AskEvent::Token { text: t.to_owned() })
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    match result {
        Ok(text) => {
            let v = verify(&text, n_sources);
            emit(AskEvent::Done {
                text,
                cited: v.cited,
                invalid: v.invalid,
                unverified: v.unverified,
            });
        }
        Err(e) => emit(AskEvent::Error {
            message: e.to_string(),
        }),
    }
    Ok(())
}

#[derive(Serialize)]
pub struct SessionDto {
    app: String,
    start: i64,
    end: i64,
    secs: i64,
    titles: Vec<String>,
}

#[derive(Serialize)]
pub struct AppTotal {
    app: String,
    secs: i64,
}

#[derive(Serialize)]
pub struct WebVisit {
    ts: i64,
    app: String,
    title: String,
    url: String,
}

#[derive(Serialize)]
pub struct ActivityReport {
    /// Whether any activity source is switched on.
    enabled: bool,
    sessions: Vec<SessionDto>,
    totals: Vec<AppTotal>,
    web: Vec<WebVisit>,
}

#[tauri::command]
pub fn get_activity(hours: Option<u32>) -> Result<ActivityReport, String> {
    let hours = i64::from(hours.unwrap_or(24).clamp(1, 24 * 30));
    let guard = ACTIVITY.lock().unwrap();
    let Some(svc) = guard.as_ref() else {
        return Ok(ActivityReport {
            enabled: false,
            sessions: vec![],
            totals: vec![],
            web: vec![],
        });
    };
    let now = now_secs();
    let events = svc
        .between(now - hours * 3600, now)
        .map_err(|e| e.to_string())?;
    let mut sessions: Vec<SessionDto> = summarize(&events)
        .into_iter()
        .map(|s| SessionDto {
            secs: s.secs(),
            app: s.app,
            start: s.start,
            end: s.end,
            titles: s.titles,
        })
        .collect();
    sessions.reverse();
    let totals = totals_by_app(&events)
        .into_iter()
        .map(|(app, secs)| AppTotal { app, secs })
        .collect();
    let mut web: Vec<WebVisit> = events
        .iter()
        .filter(|e| e.kind == "web")
        .filter_map(|e| {
            e.url.clone().map(|url| WebVisit {
                ts: e.ts_start,
                app: e.app.clone(),
                title: e.title.clone(),
                url,
            })
        })
        .collect();
    web.sort_by_key(|w| std::cmp::Reverse(w.ts));
    web.truncate(200);
    Ok(ActivityReport {
        enabled: true,
        sessions,
        totals,
        web,
    })
}

/// Deletes recorded activity from the last `minutes` minutes (0 = everything).
#[tauri::command]
pub fn forget_activity(minutes: u32) -> Result<usize, String> {
    let guard = ACTIVITY.lock().unwrap();
    let Some(svc) = guard.as_ref() else {
        return Ok(0);
    };
    let now = now_secs();
    let from = if minutes == 0 {
        0
    } else {
        now - i64::from(minutes) * 60
    };
    svc.forget(from, now + 1).map_err(|e| e.to_string())
}
