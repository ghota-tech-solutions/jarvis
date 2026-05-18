//! Server-Sent Events for the task page. One axum handler tails the ledger,
//! gates events by ancestor-chain membership of the subscribed root, and
//! formats each surviving event with `render_event_for_sse`.

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::stream::Stream;
use jarvis_core::TaskId;
use jarvis_ledger::{EventRecord, Ledger};
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::str::FromStr;

use super::render::render_event_blocks;
use super::util::AppError;
use super::WebState;

#[derive(Deserialize)]
pub(super) struct SseQuery {
    task: String,
    #[serde(default)]
    since: i64,
}

pub(super) async fn api_events_sse(
    State(s): State<WebState>,
    Query(q): Query<SseQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, AppError> {
    let subscribed_root =
        TaskId::from_str(&q.task).map_err(|_| AppError::BadRequest("invalid task id".into()))?;
    let chain = s.ledger.walk_ancestors(subscribed_root).await.unwrap_or_default();
    let backfill = s
        .ledger
        .query_events_multi(&chain.iter().map(|t| t.id).collect::<Vec<_>>(), q.since, 0)
        .await
        .unwrap_or_default();
    let mut live = s.ledger.subscribe();

    // Per-event cache of "this task's chain root" so we don't re-walk on every
    // event for the same task. Children dynamically attach to the chain — that's
    // why we walk ancestors instead of capturing chain_ids statically.
    let mut root_cache: HashMap<TaskId, TaskId> = HashMap::new();
    for t in &chain {
        root_cache.insert(t.id, subscribed_root);
    }
    let ledger = s.ledger.clone();

    let stream = async_stream::stream! {
        for ev in backfill {
            if let Some(sse) = render_event_for_sse(&ev) {
                yield Ok::<_, Infallible>(sse);
            }
        }
        loop {
            match live.recv().await {
                Ok(ev) => {
                    if descends_from(&ledger, &mut root_cache, ev.task_id, subscribed_root).await
                        && let Some(sse) = render_event_for_sse(&ev)
                    {
                        yield Ok(sse);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Decide how to surface a ledger event on the SSE stream. Returns None when
/// the event should be silenced (heartbeat, attempt) or transformed into a
/// chunk-typed event the client JS handles separately.
fn render_event_for_sse(ev: &EventRecord) -> Option<SseEvent> {
    match ev.kind.to_string().as_str() {
        // Live token deltas — sent as a dedicated event type the page listens to.
        "llm_chunk" => {
            let delta = ev.payload.get("delta").and_then(|v| v.as_str())?;
            Some(
                SseEvent::default()
                    .event("chunk")
                    .id(ev.id.0.to_string())
                    .data(delta),
            )
        }
        // Decision arriving means the streamed text is now complete — signal
        // the front-end to clear the live composing buffer, then send the full
        // rendered block.
        "decision" => {
            let html = render_event_blocks(std::slice::from_ref(ev));
            Some(
                SseEvent::default()
                    .event("decision")
                    .id(ev.id.0.to_string())
                    .data(html),
            )
        }
        // Noise we don't want on the page at all. `tool_call` is silenced
        // because the matching `tool_result` carries `args` (since M6.7) and
        // renders the full action card on its own.
        "heartbeat" | "attempt" | "tool_call" => None,
        _ => {
            let html = render_event_blocks(std::slice::from_ref(ev));
            Some(
                SseEvent::default()
                    .event("event")
                    .id(ev.id.0.to_string())
                    .data(html),
            )
        }
    }
}

/// Returns true if `task_id` is `root` or any ancestor of `task_id` is `root`.
/// Caches results keyed by task_id so we don't re-walk for streaming events
/// that come from the same task in bursts.
async fn descends_from(
    ledger: &Ledger,
    cache: &mut HashMap<TaskId, TaskId>,
    task_id: TaskId,
    root: TaskId,
) -> bool {
    if let Some(known_root) = cache.get(&task_id) {
        return *known_root == root;
    }
    let mut current = Some(task_id);
    let mut walked = Vec::new();
    while let Some(id) = current {
        walked.push(id);
        if id == root {
            for w in &walked {
                cache.insert(*w, root);
            }
            return true;
        }
        if let Some(known_root) = cache.get(&id).copied() {
            let same = known_root == root;
            for w in &walked {
                cache.insert(*w, known_root);
            }
            return same;
        }
        match ledger.get_task(id).await {
            Ok(t) => current = t.parent,
            Err(_) => return false,
        }
    }
    false
}
