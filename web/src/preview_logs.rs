use crate::{dom, state};
use gloo_timers::callback::Timeout;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::VecDeque};
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, PartialEq)]
struct Scope {
    project: String,
    generation: u64,
    job: String,
    epoch: u64,
}

#[derive(Default)]
struct Queue {
    scope: Option<Scope>,
    lines: VecDeque<String>,
    bytes: usize,
    dropped: usize,
    timer: Option<Timeout>,
    sending: bool,
}

thread_local! { static LOGS: RefCell<Queue> = RefCell::default(); }

pub fn reset() {
    LOGS.with_borrow_mut(|queue| *queue = Queue::default());
}

// The inspector bridge verifies the iframe's window, origin and preview version.
pub fn enqueue(message: &Value) {
    let Some(line) = message["message"].as_str().filter(|line| !line.is_empty()) else {
        return;
    };
    let scope = state::read(|state| Scope {
        project: state.project_id().into(),
        generation: state.generation,
        job: dom::string(&state.preview["id"]).into(),
        epoch: state.preview["result"]["epoch"]
            .as_u64()
            .unwrap_or_default(),
    });
    let line: String = line.chars().take(4000).collect();
    LOGS.with_borrow_mut(|queue| {
        if queue.scope.as_ref() != Some(&scope) {
            *queue = Queue {
                scope: Some(scope),
                ..Queue::default()
            };
        }
        queue.bytes += line.len();
        queue.lines.push_back(line);
        while queue.lines.len() > 200 || queue.bytes > 256 * 1024 {
            if let Some(line) = queue.lines.pop_front() {
                queue.bytes -= line.len();
                queue.dropped += 1;
            }
        }
    });
    schedule();
}

fn schedule() {
    LOGS.with_borrow_mut(|queue| {
        if queue.timer.is_none() && !queue.sending && !queue.lines.is_empty() {
            queue.timer = Some(Timeout::new(250, || spawn_local(flush())));
        }
    });
}

async fn flush() {
    let batch = LOGS.with_borrow_mut(|queue| {
        queue.timer.take();
        let scope = queue.scope.clone()?;
        let mut lines = vec![];
        let mut bytes = 0;
        if queue.dropped > 0 {
            lines.push(format!(
                "[Freeflow] {} preview log lines omitted during a burst.",
                queue.dropped
            ));
            queue.dropped = 0;
        }
        while lines.len() < 16
            && queue
                .lines
                .front()
                .is_some_and(|line| bytes + line.len() <= 48 * 1024)
        {
            let line = queue.lines.pop_front().unwrap();
            bytes += line.len();
            queue.bytes -= line.len();
            lines.push(line);
        }
        queue.sending = true;
        Some((scope, lines))
    });
    let Some((scope, lines)) = batch else {
        return;
    };
    let current = state::read(|state| {
        state.project_id() == scope.project
            && state.generation == scope.generation
            && state.preview["id"] == scope.job
            && state.preview["result"]["epoch"] == scope.epoch
    });
    if current {
        let _ = dom::project_post(
            &scope.project,
            "preview-logs",
            json!({"jobId":scope.job,"epoch":scope.epoch,"lines":lines}),
        )
        .await;
    }
    LOGS.with_borrow_mut(|queue| {
        if queue.scope.as_ref() == Some(&scope) {
            queue.sending = false;
        }
    });
    schedule();
}
