use crate::{
    archive, backends,
    files::{self, Problem, Result},
    flutter::{Flutter, Sdk},
    phone_preview, preview,
    processes::{self, ProcessCommand},
    projects::{Project, Projects},
    signing,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    fs,
    future::Future,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, Weak},
    time::SystemTime,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[ -/]*[@-~]").unwrap());
static JOB_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{3}$").unwrap());
static ARTIFACT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9_-]+\.(?:zip|aab|apk|ipa)$").unwrap());
static WEB_BASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/(?:[A-Za-z0-9._~-]+/)*$").unwrap());
static FLAVOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_-]{0,39}$").unwrap());
static EXPORT_SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").unwrap());
static IOS_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+(?:\.[0-9]+){2}$").unwrap());
static IOS_BUILD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+(?:\.[0-9]+){0,2}$").unwrap());

#[derive(Clone, Serialize)]
struct Line {
    id: u64,
    message: String,
}

struct JobState {
    status: &'static str,
    error: Option<String>,
    ended: Option<String>,
    result: Value,
    lines: VecDeque<Line>,
    sequence: u64,
}

pub struct Job {
    id: String,
    project_id: Option<String>,
    kind: String,
    started: String,
    state: Mutex<JobState>,
    pub cancel: CancellationToken,
    done: CancellationToken,
    pub reload: Notify,
    pub restart_device: Notify,
}

impl Job {
    fn new(kind: String, project_id: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string()[..12].into(),
            project_id,
            kind,
            started: files::now(),
            state: Mutex::new(JobState {
                status: "queued",
                error: None,
                ended: None,
                result: json!({}),
                lines: VecDeque::new(),
                sequence: 0,
            }),
            cancel: CancellationToken::new(),
            done: CancellationToken::new(),
            reload: Notify::new(),
            restart_device: Notify::new(),
        }
    }

    fn view(&self, after: u64) -> Value {
        let state = self.state.lock().unwrap();
        json!({ "id":self.id,"kind":self.kind,"projectId":self.project_id,"status":state.status,
            "startedAt":self.started,"endedAt":state.ended,"error":state.error,"result":state.result,
            "log":state.lines.iter().filter(|line| line.id > after).collect::<Vec<_>>() })
    }

    fn app_logs(&self, after: u64, limit: usize) -> Value {
        let state = self.state.lock().unwrap();
        let mut lines = vec![];
        let mut bytes = 0;
        let mut truncated = state
            .lines
            .front()
            .is_some_and(|line| line.id > after.saturating_add(1));
        for line in state
            .lines
            .iter()
            .rev()
            .filter(|line| line.id > after)
            .take(limit)
        {
            let remaining = (64 * 1024usize).saturating_sub(bytes);
            let end = line
                .message
                .char_indices()
                .map(|(index, ch)| index + ch.len_utf8())
                .take_while(|end| *end <= remaining)
                .last()
                .unwrap_or(0);
            if end > 0 {
                lines.push(Line {
                    id: line.id,
                    message: line.message[..end].into(),
                });
                bytes += end;
            }
            if end < line.message.len() {
                truncated = true;
                break;
            }
        }
        lines.reverse();
        truncated |= state.lines.iter().filter(|line| line.id > after).count() > lines.len();
        json!({"job":{"id":self.id,"kind":self.kind,"status":state.status,"startedAt":self.started,
            "endedAt":state.ended,"error":state.error.as_ref().map(|text| text.chars().take(2000).collect::<String>())},
            "nextAfter":lines.last().map_or(after, |line| line.id),"lines":lines,"truncated":truncated})
    }

    pub fn log(&self, line: impl AsRef<str>) {
        Self::append_log(&mut self.state.lock().unwrap(), line.as_ref());
    }

    fn append_log(state: &mut JobState, line: &str) {
        let clean = ANSI.replace_all(line, "");
        if clean.is_empty() {
            return;
        }
        let message = clean.chars().take(6000).collect();
        state.sequence += 1;
        let id = state.sequence;
        state.lines.push_back(Line { id, message });
        if state.lines.len() > 300 {
            state.lines.pop_front();
        }
    }

    pub fn report(&self, value: Value) {
        self.state.lock().unwrap().result = value;
    }

    fn finish(&self, result: Result<Value>) {
        let mut state = self.state.lock().unwrap();
        if self.cancel.is_cancelled() {
            state.status = "stopped";
        } else {
            match result {
                Ok(result) => {
                    state.status = "succeeded";
                    state.result = result;
                }
                Err(error) => {
                    state.status = "failed";
                    state.error = Some(error.message);
                }
            }
        }
        if self.kind == "device" {
            state.result["debugUrl"] = Value::Null;
            state.result["reloading"] = json!(false);
        } else if self.kind == "phone-preview" {
            state.result = json!({});
        } else if self.kind == "preview" && !state.result["url"].is_null() {
            state.result["url"] = Value::Null;
            state.result["reloading"] = json!(false);
        }
        state.ended = Some(files::now());
        let error = state.error.clone();
        drop(state);
        if let Some(error) = error {
            self.log(error);
        }
    }

    pub fn ready(&self, url: &str, epoch: u64, inspector: bool) {
        let mut state = self.state.lock().unwrap();
        state.status = "ready";
        state.result["url"] = json!(url);
        state.result["epoch"] = json!(epoch);
        state.result["inspector"] = json!(inspector);
        state.result["reloading"] = json!(false);
        state.result["compileError"] = Value::Null;
    }

    pub fn refreshing(&self) {
        let mut state = self.state.lock().unwrap();
        state.result["reloading"] = json!(true);
        state.result["compileError"] = Value::Null;
    }

    pub fn is_ready(&self) -> bool {
        self.state.lock().unwrap().status == "ready"
    }

    pub fn device_ready(&self, device: &str) {
        let mut state = self.state.lock().unwrap();
        state.status = "ready";
        state.result["device"] = json!(device);
        state.result["reloading"] = json!(false);
        state.result["compileError"] = Value::Null;
    }

    pub fn device_debug_url(&self, url: String) {
        self.state.lock().unwrap().result["debugUrl"] = json!(url);
    }

    pub fn restart(&self, epoch: u64) {
        let mut state = self.state.lock().unwrap();
        state.status = "running";
        state.result = json!({"url":null,"epoch":epoch,"reloading":false});
    }

    pub fn compile_error(&self, message: String) {
        self.log(&message);
        let mut state = self.state.lock().unwrap();
        state.result["reloading"] = json!(false);
        state.result["compileError"] = json!(message);
    }

    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(Problem::new(409, "Command stopped."))
        } else {
            Ok(())
        }
    }

    pub async fn exec(&self, command: &ProcessCommand, cwd: &Path) -> Result<()> {
        let status =
            processes::run(command, Some(cwd), &self.cancel, |line| self.log(line)).await?;
        self.check()?;
        if !status.success() {
            return Err(Problem::new(
                422,
                format!("The command exited with {status}. See the console for details."),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Exclusive,
    PausePreview,
    Preview,
    PhonePreview,
    Device,
    Sdk,
    Global,
}

#[derive(Default)]
struct Queue {
    jobs: HashMap<String, Arc<Job>>,
    order: VecDeque<String>,
    previews: HashMap<String, Arc<Job>>,
    phone_previews: HashMap<String, Arc<Job>>,
    flutter_builds: HashMap<String, Weak<AsyncMutex<()>>>,
    devices: HashMap<String, Arc<Job>>,
    busy: HashMap<String, String>,
    sdk: Option<Arc<Job>>,
    closing: bool,
}

pub struct Jobs {
    pub projects: Arc<Projects>,
    pub flutter: Arc<Flutter>,
    pub origin: Option<String>,
    queue: Mutex<Queue>,
}

impl Jobs {
    pub fn new(
        projects: Arc<Projects>,
        flutter: Arc<Flutter>,
        origin: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            projects,
            flutter,
            origin,
            queue: Mutex::new(Queue::default()),
        })
    }

    pub(crate) fn enqueue<F>(
        self: &Arc<Self>,
        kind: impl Into<String>,
        project: Option<Project>,
        mode: Mode,
        task: impl FnOnce(Arc<Self>, Arc<Job>) -> F + Send + 'static,
    ) -> Result<Value>
    where
        F: Future<Output = Result<Value>> + Send + 'static,
    {
        let mut queue = self.queue.lock().unwrap();
        if queue.closing {
            return Err(Problem::new(503, "Freeflow is shutting down."));
        }
        let project_id = project.as_ref().map(|project| project.id.clone());
        let current_preview = project_id
            .as_ref()
            .and_then(|id| queue.previews.get(id))
            .cloned();
        if let Mode::PhonePreview = mode {
            if let Some(active) = project_id
                .as_ref()
                .and_then(|id| queue.phone_previews.get(id))
                .filter(|job| !job.done.is_cancelled())
            {
                if active.cancel.is_cancelled() {
                    return Err(Problem::new(409, "The phone preview is stopping."));
                }
                return Ok(active.view(0));
            }
        }
        if matches!(mode, Mode::Preview) {
            if let Some(active) = current_preview
                .as_ref()
                .filter(|job| !job.done.is_cancelled() && !job.cancel.is_cancelled())
            {
                return Ok(active.view(0));
            }
        }
        if matches!(mode, Mode::Sdk) {
            if let Some(active) = queue.sdk.as_ref().filter(|job| !job.done.is_cancelled()) {
                return Ok(active.view(0));
            }
        }
        if matches!(mode, Mode::Device)
            && project_id
                .as_ref()
                .and_then(|id| queue.devices.get(id))
                .is_some_and(|job| !job.done.is_cancelled())
        {
            return Err(Problem::new(
                409,
                "This project already has a device session.",
            ));
        }
        let device_running = project_id
            .as_ref()
            .and_then(|id| queue.devices.get(id))
            .is_some_and(|job| !job.done.is_cancelled());
        if device_running && matches!(mode, Mode::PausePreview | Mode::Preview) {
            return Err(Problem::new(
                409,
                "Stop the device session before running this project command.",
            ));
        }
        if let Some(id) = &project_id {
            if queue.busy.contains_key(id) {
                let message = if matches!(mode, Mode::Preview) {
                    "Wait for the project command to finish before starting preview."
                } else {
                    "This project already has a command running."
                };
                return Err(Problem::new(409, message));
            }
        }

        let job = Arc::new(Job::new(kind.into(), project_id));
        let paused = current_preview.filter(|job| {
            matches!(mode, Mode::PausePreview | Mode::Device)
                && !job.done.is_cancelled()
                && !job.cancel.is_cancelled()
        });
        queue.jobs.insert(job.id.clone(), job.clone());
        queue.order.push_back(job.id.clone());
        match mode {
            Mode::Preview => {
                queue
                    .previews
                    .insert(job.project_id.clone().unwrap(), job.clone());
            }
            Mode::PhonePreview => {
                queue
                    .phone_previews
                    .insert(job.project_id.clone().unwrap(), job.clone());
            }
            Mode::Sdk => queue.sdk = Some(job.clone()),
            Mode::Device => {
                queue
                    .devices
                    .insert(job.project_id.clone().unwrap(), job.clone());
            }
            Mode::Global => {}
            _ => {
                queue
                    .busy
                    .insert(job.project_id.clone().unwrap(), job.id.clone());
            }
        }
        drop(queue);
        let initial = job.view(0);
        let this = self.clone();
        tokio::spawn(async move {
            job.state.lock().unwrap().status = "running";
            if let Some(preview) = &paused {
                preview.cancel.cancel();
                preview.done.cancelled().await;
            }
            let result = match job.check() {
                Ok(()) => task(this.clone(), job.clone()).await,
                Err(error) => Err(error),
            };
            job.finish(result);
            let resume = {
                let mut queue = this.queue.lock().unwrap();
                if let Some(id) = &job.project_id {
                    if queue.busy.get(id) == Some(&job.id) {
                        queue.busy.remove(id);
                    }
                }
                job.done.cancel();
                if queue.jobs.len() > 120 {
                    let kept: HashSet<_> = queue
                        .previews
                        .values()
                        .chain(queue.phone_previews.values())
                        .chain(queue.devices.values())
                        .chain(queue.sdk.iter())
                        .map(|job| job.id.clone())
                        .collect();
                    let removable: Vec<_> = queue
                        .order
                        .iter()
                        .filter(|id| !kept.contains(*id) && queue.jobs[*id].done.is_cancelled())
                        .take(queue.jobs.len() - 100)
                        .cloned()
                        .collect();
                    for id in removable {
                        queue.jobs.remove(&id);
                    }
                    let Queue { order, jobs, .. } = &mut *queue;
                    order.retain(|id| jobs.contains_key(id));
                }
                !queue.closing
                    && matches!(mode, Mode::PausePreview)
                    && paused.as_ref().is_some_and(|preview| {
                        job.project_id
                            .as_ref()
                            .and_then(|id| queue.previews.get(id))
                            .is_some_and(|active| Arc::ptr_eq(active, preview))
                    })
            };
            if resume {
                if let Some(project) = project {
                    let _ = this.start_preview(project);
                }
            }
        });
        Ok(initial)
    }

    fn get_job(&self, id: &str) -> Result<Arc<Job>> {
        self.queue
            .lock()
            .unwrap()
            .jobs
            .get(id)
            .cloned()
            .ok_or_else(|| Problem::new(404, "Command not found."))
    }

    pub fn get(&self, id: &str, after: u64) -> Result<Value> {
        Ok(self.get_job(id)?.view(after))
    }

    pub fn scoped(&self, id: &str, project: &Project, after: u64) -> Result<Value> {
        let job = self.get_job(id)?;
        if job.project_id.as_deref() != Some(&project.id) {
            return Err(Problem::new(
                403,
                "That command belongs to another project.",
            ));
        }
        Ok(job.view(after))
    }

    pub fn app_logs(&self, project: &Project, input: &Value) -> Result<Value> {
        let target = input["target"].as_str().unwrap_or("latest");
        if !matches!(target, "latest" | "preview" | "device" | "build") {
            return Err(Problem::new(
                400,
                "Choose preview, device, build or latest logs.",
            ));
        }
        let limit = input.get("limit").map_or(Ok(200), |value| {
            value
                .as_u64()
                .filter(|limit| (1..=200).contains(limit))
                .map(|limit| limit as usize)
                .ok_or_else(|| Problem::new(400, "Choose between 1 and 200 log lines."))
        })?;
        let after = input.get("after").map_or(Ok(0), |value| {
            value
                .as_u64()
                .ok_or_else(|| Problem::new(400, "Use a log cursor from an earlier result."))
        })?;
        let allowed = |kind: &str| {
            matches!(
                kind,
                "preview"
                    | "phone-preview"
                    | "device"
                    | "analyze"
                    | "test"
                    | "format"
                    | "dependencies"
                    | "create"
            ) || kind.starts_with("export-")
        };
        let job = if let Some(id) = input["jobId"].as_str() {
            let job = self.get_job(id)?;
            if job.project_id.as_deref() != Some(&project.id) {
                return Err(Problem::new(
                    403,
                    "That command belongs to another project.",
                ));
            }
            if !allowed(&job.kind) {
                return Err(Problem::new(
                    400,
                    "Choose an app, build, analysis or test command.",
                ));
            }
            Some(job)
        } else {
            let queue = self.queue.lock().unwrap();
            queue
                .order
                .iter()
                .rev()
                .filter_map(|id| queue.jobs.get(id))
                .find(|job| {
                    job.project_id.as_deref() == Some(&project.id)
                        && allowed(&job.kind)
                        && match target {
                            "preview" => job.kind == "preview",
                            "device" => job.kind == "device",
                            "build" => job.kind.starts_with("export-"),
                            _ => true,
                        }
                })
                .cloned()
        };
        Ok(job.map_or_else(
            || json!({"job":null,"lines":[],"nextAfter":after,"truncated":false}),
            |job| job.app_logs(after, limit),
        ))
    }

    pub fn preview_logs(&self, project: &Project, input: &Value) -> Result<Value> {
        let id = crate::mcp::required(input, "jobId")?;
        let epoch = input["epoch"]
            .as_u64()
            .ok_or_else(|| Problem::new(400, "Use the active preview version."))?;
        let values = input["lines"]
            .as_array()
            .filter(|lines| lines.len() <= 32)
            .ok_or_else(|| Problem::new(400, "Send up to 32 preview log lines."))?;
        let mut size = 0;
        for value in values {
            let text = value
                .as_str()
                .filter(|text| text.len() <= 16 * 1024)
                .ok_or_else(|| Problem::new(413, "Preview log lines are too large."))?;
            size += text.len();
        }
        if size > 64 * 1024 {
            return Err(Problem::new(413, "This preview log batch is too large."));
        }
        let Some(job) = self.preview_job(&project.id).filter(|job| job.id == id) else {
            return Ok(json!({"accepted":0}));
        };
        let mut state = job.state.lock().unwrap();
        if state.status != "ready" || state.result["epoch"] != epoch {
            return Ok(json!({"accepted":0}));
        }
        for text in values {
            Job::append_log(&mut state, text.as_str().unwrap());
        }
        Ok(json!({"accepted":values.len()}))
    }

    pub fn list(&self, id: &str) -> Vec<Value> {
        let queue = self.queue.lock().unwrap();
        queue.order.iter().rev().filter_map(|key| queue.jobs.get(key)).filter(|job| job.project_id.as_deref() == Some(id))
            .take(25).map(|job| {
                let state = job.state.lock().unwrap();
                json!({"id":job.id, "kind":job.kind, "status":state.status, "startedAt":job.started, "error":state.error})
            }).collect()
    }

    pub fn cancel(&self, id: &str) -> Result<Value> {
        let job = self.get_job(id)?;
        if !job.done.is_cancelled() {
            job.cancel.cancel();
        }
        Ok(job.view(0))
    }

    pub fn busy(&self, id: &str) -> bool {
        self.queue.lock().unwrap().busy.contains_key(id)
    }

    pub fn active_command(&self, project: &str, kind: &str) -> Value {
        let queue = self.queue.lock().unwrap();
        queue
            .busy
            .get(project)
            .and_then(|id| queue.jobs.get(id))
            .filter(|job| job.kind == kind)
            .map_or(Value::Null, |job| job.view(0))
    }

    pub async fn pub_command(
        &self,
        job: &Job,
        project: &Project,
        sdk: &Sdk,
        args: &[&str],
    ) -> Result<()> {
        let _build = self.flutter_build_slot(job, project).await?;
        let mut arguments = args.to_vec();
        if self.flutter.settings()?.offline {
            arguments.push("--offline");
        }
        let command = self.flutter.command(sdk, arguments)?;
        job.exec(&command, &project.root).await
    }

    pub async fn flutter_build_slot(
        &self,
        job: &Job,
        project: &Project,
    ) -> Result<OwnedMutexGuard<()>> {
        let slot = {
            let mut queue = self.queue.lock().unwrap();
            queue
                .flutter_builds
                .retain(|_, slot| slot.strong_count() != 0);
            let entry = queue.flutter_builds.entry(project.id.clone()).or_default();
            entry.upgrade().unwrap_or_else(|| {
                let slot = Arc::new(AsyncMutex::new(()));
                *entry = Arc::downgrade(&slot);
                slot
            })
        };
        tokio::select! {
            guard = slot.lock_owned() => Ok(guard),
            _ = job.cancel.cancelled() => Err(Problem::new(409, "Command stopped.")),
        }
    }

    pub fn scaffold(
        self: &Arc<Self>,
        name: &str,
        template: &str,
        org: Option<&str>,
    ) -> Result<Value> {
        let sdk = self.flutter.require()?;
        let project = self.projects.reserve(name, template, org)?;
        self.enqueue(
            "create",
            Some(project.clone()),
            Mode::Exclusive,
            move |this, job| async move {
                job.log(format!("Creating {} with Flutter.", project.name));
                let args = [
                    "create",
                    "--no-pub",
                    "--platforms=android,ios,web",
                    "--org",
                    project.organization.as_deref().unwrap(),
                    "--project-name",
                    project.slug.as_deref().unwrap(),
                    ".",
                ];
                job.exec(&this.flutter.command(&sdk, args)?, &project.root)
                    .await?;
                Ok(json!({"project":this.projects.finish_scaffold(project)?}))
            },
        )
    }

    fn preview_job(&self, id: &str) -> Option<Arc<Job>> {
        self.queue.lock().unwrap().previews.get(id).cloned()
    }

    pub fn preview(&self, id: &str) -> Value {
        self.preview_job(id).map_or(Value::Null, |job| job.view(0))
    }

    pub fn device_session(&self, project: &str) -> Value {
        self.queue
            .lock()
            .unwrap()
            .devices
            .get(project)
            .map_or(Value::Null, |job| job.view(0))
    }

    pub fn device_control(&self, project: &str, action: &str) -> Result<Value> {
        let job = self
            .device_job(project)
            .ok_or_else(|| Problem::new(404, "There is no active device session."))?;
        match action {
            "stop" => job.cancel.cancel(),
            "reload" | "restart" if !job.is_ready() => {
                return Err(Problem::new(
                    409,
                    "Wait for Flutter to start on the device.",
                ));
            }
            "reload" => job.reload.notify_one(),
            "restart" => job.restart_device.notify_one(),
            _ => return Err(Problem::new(400, "Choose a device action.")),
        }
        Ok(job.view(0))
    }

    pub async fn stop_device(&self, project: &str) {
        if let Some(job) = self.device_job(project) {
            job.cancel.cancel();
            job.done.cancelled().await;
        }
    }

    fn device_job(&self, project: &str) -> Option<Arc<Job>> {
        self.queue
            .lock()
            .unwrap()
            .devices
            .get(project)
            .filter(|job| !job.done.is_cancelled())
            .cloned()
    }

    pub fn start_preview(self: &Arc<Self>, project: Project) -> Result<Value> {
        self.enqueue(
            "preview",
            Some(project.clone()),
            Mode::Preview,
            move |this, job| async move {
                preview::run(&this, &job, &project).await?;
                Ok(json!({}))
            },
        )
    }

    pub fn reload_preview(self: &Arc<Self>, project: Project) -> Result<Value> {
        if let Some(job) = self.preview_job(&project.id) {
            let state = job.view(0);
            if state["status"] == "ready" {
                job.reload.notify_one();
                return Ok(state);
            }
        }
        self.start_preview(project)
    }

    pub async fn stop_preview(&self, id: &str) -> Value {
        let Some(job) = self.preview_job(id) else {
            return Value::Null;
        };
        if !job.done.is_cancelled() {
            job.cancel.cancel();
            job.done.cancelled().await;
        }
        job.view(0)
    }

    fn phone_preview_job(&self, id: &str) -> Option<Arc<Job>> {
        self.queue.lock().unwrap().phone_previews.get(id).cloned()
    }

    pub fn phone_preview(&self, id: &str) -> Value {
        self.phone_preview_job(id)
            .map_or(Value::Null, |job| job.view(u64::MAX))
    }

    pub fn start_phone_preview(
        self: &Arc<Self>,
        project: Project,
        address: Ipv4Addr,
    ) -> Result<Value> {
        self.enqueue(
            "phone-preview",
            Some(project.clone()),
            Mode::PhonePreview,
            move |jobs, job| async move {
                phone_preview::run(&jobs, &job, &project, address).await?;
                Ok(json!({}))
            },
        )
    }

    pub fn reload_phone_preview(&self, id: &str) -> Result<Value> {
        let job = self
            .phone_preview_job(id)
            .filter(|job| !job.done.is_cancelled() && !job.cancel.is_cancelled())
            .ok_or_else(|| Problem::new(404, "Start sharing the phone preview first."))?;
        job.reload.notify_one();
        Ok(job.view(u64::MAX))
    }

    pub async fn stop_phone_preview(&self, id: &str) -> Value {
        let Some(job) = self.phone_preview_job(id) else {
            return Value::Null;
        };
        job.cancel.cancel();
        job.done.cancelled().await;
        job.view(u64::MAX)
    }

    pub fn analyze(self: &Arc<Self>, project: Project, kind: &str) -> Result<Value> {
        let kind = match kind {
            "analyze" => "analyze",
            "test" => "test",
            _ => return Err(Problem::new(400, "Choose Analyze or Test.")),
        };
        self.enqueue(
            kind,
            Some(project.clone()),
            Mode::PausePreview,
            move |this, job| async move {
                let sdk = this.flutter.require()?;
                this.pub_command(&job, &project, &sdk, &["pub", "get"])
                    .await?;
                let mut args = vec![kind.to_owned(), "--no-pub".into()];
                if kind == "test" {
                    args.extend(backends::options(&this.projects.home, &project)?);
                }
                job.exec(&this.flutter.command(&sdk, args)?, &project.root)
                    .await?;
                Ok(json!({}))
            },
        )
    }

    pub fn refresh_dependencies(self: &Arc<Self>, project: Project) -> Result<Value> {
        self.enqueue(
            "dependencies",
            Some(project.clone()),
            Mode::PausePreview,
            move |this, job| async move {
                let sdk = this.flutter.require()?;
                this.pub_command(&job, &project, &sdk, &["pub", "get"])
                    .await?;
                Ok(json!({}))
            },
        )
    }

    pub fn integrate(
        self: &Arc<Self>,
        project: Project,
        provider: &str,
        input: &Value,
    ) -> Result<Value> {
        let (provider, input) = (provider.to_owned(), input.clone());
        self.enqueue(
            format!("backend-{provider}"),
            Some(project.clone()),
            Mode::PausePreview,
            move |this, job| async move {
                let change =
                    backends::Change::prepare(&this.projects, &project, &provider, &input)?;
                if !change.dependencies().is_empty() {
                    let sdk = this.flutter.require()?;
                    let mut args = vec!["pub", "add"];
                    args.extend(change.dependencies());
                    this.pub_command(&job, &project, &sdk, &args).await?;
                }
                job.check()?;
                change.apply(&this.projects, &project, |line| job.log(line))
            },
        )
    }

    pub fn sign_android(self: &Arc<Self>, project: Project, input: Value) -> Result<Value> {
        self.enqueue(
            "android-signing",
            Some(project.clone()),
            Mode::Exclusive,
            move |this, job| async move {
                job.log("Configuring the Android release upload key.");
                signing::configure(&this.projects, &project, &input)
            },
        )
    }

    pub fn format(
        self: &Arc<Self>,
        project: Project,
        relative: &str,
        expected: Option<&str>,
    ) -> Result<Value> {
        if !relative.ends_with(".dart") {
            return Err(Problem::new(400, "Open a Dart file to format it."));
        }
        let source = self.projects.file(&project, relative)?;
        if source.revision.as_deref() != expected {
            return Err(Problem::new(
                409,
                "This file changed on disk. Reload it first.",
            ));
        }
        let sdk = self.flutter.require()?;
        let dart = sdk
            .file
            .parent()
            .unwrap_or(Path::new(""))
            .join("cache/dart-sdk/bin")
            .join(if cfg!(windows) { "dart.exe" } else { "dart" });
        if !dart.is_file() {
            return Err(Problem::new(
                409,
                "Run Flutter once to initialize its Dart SDK.",
            ));
        }
        self.enqueue(
            "format",
            Some(project.clone()),
            Mode::Exclusive,
            move |this, job| async move {
                let parent = this.projects.home.join("tmp");
                fs::create_dir_all(&parent)?;
                let folder = tempfile::Builder::new()
                    .prefix("format-")
                    .tempdir_in(&parent)?;
                let file = folder.path().join("source.dart");
                fs::write(&file, source.content)?;
                let command =
                    ProcessCommand::new(dart, [OsString::from("format"), file.as_os_str().into()]);
                job.exec(&command, &project.root).await?;
                let updated = this.projects.save(
                    &project,
                    &source.path,
                    fs::read_to_string(&file)?,
                    source.revision.as_deref(),
                )?;
                Ok(json!({"path":updated.path,"revision":updated.revision}))
            },
        )
    }

    pub fn build(self: &Arc<Self>, project: Project, target: &str, input: &Value) -> Result<Value> {
        let target = Target::parse(target)?;
        if target == Target::Ios && !cfg!(target_os = "macos") {
            return Err(Problem::new(
                422,
                "Build App Store IPAs on macOS with Xcode. Use the Flutter source export to move the project.",
            ));
        }
        let flags = target.flags(input)?;
        let mode = if target == Target::Source {
            Mode::Exclusive
        } else {
            Mode::PausePreview
        };
        self.enqueue(format!("export-{}", target.id()), Some(project.clone()), mode, move |this, job| async move {
            let debug = target == Target::Android && signing::status(&this.projects, &project)["debugFallback"] == true;
            if debug { job.log("This project uses the default debug key. Configure an upload key before submitting to Google Play."); }
            let output = match target {
                Target::Ios => {
                    let directory = project.root.join("build/ios/ipa");
                    let previous = build_outputs(&directory, "ipa")?;
                    this.flutter_build(&job, &project, flags).await?;
                    Some(new_ipa(&directory, &previous)?)
                }
                Target::Source => None,
                _ => {
                    this.flutter_build(&job, &project, flags).await?;
                    None
                }
            };
            let artifact = tokio::task::spawn_blocking(move || this.make_artifact(&job, &project, target, debug, output))
                .await.map_err(Problem::internal)??;
            Ok(json!({"artifact":artifact}))
        })
    }

    async fn flutter_build(&self, job: &Job, project: &Project, flags: Vec<String>) -> Result<()> {
        let sdk = self.flutter.require()?;
        self.pub_command(job, project, &sdk, &["pub", "get"])
            .await?;
        let _build = self.flutter_build_slot(job, project).await?;
        let mut args = vec![OsString::from("build")];
        args.extend(flags.into_iter().map(OsString::from));
        args.push("--no-pub".into());
        args.extend(
            backends::options(&self.projects.home, project)?
                .into_iter()
                .map(OsString::from),
        );
        job.exec(&self.flutter.command(&sdk, args)?, &project.root)
            .await
    }

    pub async fn build_site(&self, job: &Job, project: &Project, output: &Path) -> Result<()> {
        let target = output.to_str().ok_or_else(|| {
            Problem::new(422, "Select a Flutter build directory with a UTF-8 path.")
        })?;
        let mut flags = Target::Web.flags(&Value::Null)?;
        flags.extend(["--output".into(), target.into()]);
        self.flutter_build(job, project, flags).await
    }
    fn export_dir(&self, project: &Project) -> PathBuf {
        self.projects.home.join("exports").join(&project.id)
    }

    fn make_artifact(
        &self,
        job: &Job,
        project: &Project,
        target: Target,
        debug_signed: bool,
        output: Option<PathBuf>,
    ) -> Result<Artifact> {
        job.check()?;
        let folder = self.export_dir(project).join(&job.id);
        fs::create_dir_all(&folder)?;
        let stem = project.slug.clone().unwrap_or_else(|| {
            EXPORT_SLUG
                .replace_all(&project.name.to_lowercase(), "-")
                .chars()
                .take(55)
                .collect::<String>()
        });
        let stem = if stem.is_empty() {
            "flutter-app"
        } else {
            &stem
        };
        let (suffix, label) = target.info();
        let name = format!("{stem}-{suffix}");
        let file = folder.join(&name);
        match target {
            Target::Source => archive::zip_folder(&project.root, &file, true, &job.cancel)?,
            Target::Web => {
                archive::zip_folder(&project.root.join("build/web"), &file, false, &job.cancel)?
            }
            Target::Ios => {
                let ipa =
                    output.ok_or_else(|| Problem::new(422, "Flutter did not produce an IPA."))?;
                archive::copy_artifact(&ipa, &file)?;
            }
            _ => {
                let (directory, extension) = match target {
                    Target::Android => ("build/app/outputs/bundle", "aab"),
                    Target::Apk => ("build/app/outputs/flutter-apk", "apk"),
                    _ => unreachable!(),
                };
                let found = find_output(&project.root.join(directory), extension)?;
                archive::copy_artifact(&found, &file)?;
            }
        }
        job.check()?;
        let artifact = Artifact {
            id: job.id.clone(),
            name,
            target: target.id().into(),
            label: label.into(),
            size: fs::metadata(&file)?.len(),
            date: files::now(),
            debug_signed,
        };
        files::write_json(&folder.join("artifact.json"), &artifact)?;
        job.log(format!("Exported {}.", artifact.name));
        Ok(artifact)
    }

    pub fn artifacts(&self, project: &Project) -> Result<Vec<Artifact>> {
        let root = self.export_dir(project);
        if !root.is_dir() {
            return Ok(vec![]);
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && JOB_ID.is_match(&entry.file_name().to_string_lossy())
            {
                if let Some(record) =
                    files::read_json::<Artifact>(&entry.path().join("artifact.json"))?
                {
                    records.push(record);
                }
            }
        }
        records.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(records)
    }

    pub fn artifact(&self, project: &Project, id: &str) -> Result<(PathBuf, Artifact)> {
        let missing = || Problem::new(404, "Export not found.");
        if !JOB_ID.is_match(id) {
            return Err(missing());
        }
        let root = self.export_dir(project).join(id);
        let record: Artifact =
            files::read_json(&root.join("artifact.json"))?.ok_or_else(missing)?;
        if !ARTIFACT_NAME.is_match(&record.name) {
            return Err(missing());
        }
        let file = root.join(&record.name);
        if !fs::symlink_metadata(&file)
            .is_ok_and(|metadata| metadata.is_file() && !files::is_link(&metadata))
        {
            return Err(Problem::new(404, "The export file is missing."));
        }
        Ok((file, record))
    }

    pub fn sdk_status(&self) -> Value {
        self.queue
            .lock()
            .unwrap()
            .sdk
            .as_ref()
            .map_or(Value::Null, |job| job.view(0))
    }

    pub fn sdk_install(self: &Arc<Self>, automatic: bool) -> Result<Value> {
        self.enqueue(
            "flutter-sdk",
            None,
            Mode::Sdk,
            move |this, job| async move {
                let release = this.flutter.latest(!automatic).await?;
                this.flutter
                    .install(&release, &job.cancel, |line| job.log(line))
                    .await
            },
        )
    }

    pub async fn auto_update(self: &Arc<Self>) -> Result<()> {
        let settings = self.flutter.settings()?;
        if settings.offline {
            return Ok(());
        }
        if let Ok(release) = self.flutter.latest(false).await {
            if settings.managed_auto_update
                && self.flutter.selected()?.is_some_and(|sdk| {
                    sdk.mode == "managed" && sdk.version.as_deref() != Some(&release.version)
                })
            {
                self.sdk_install(true)?;
            }
        }
        Ok(())
    }

    pub async fn close(&self) {
        let jobs = {
            let mut queue = self.queue.lock().unwrap();
            queue.closing = true;
            queue.jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in &jobs {
            job.cancel.cancel();
        }
        for job in jobs {
            job.done.cancelled().await;
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub name: String,
    pub target: String,
    pub label: String,
    pub size: u64,
    pub date: String,
    #[serde(default)]
    pub debug_signed: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Target {
    Web,
    Android,
    Apk,
    Ios,
    Source,
}

impl Target {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "web" => Ok(Self::Web),
            "android" => Ok(Self::Android),
            "apk" => Ok(Self::Apk),
            "ios" => Ok(Self::Ios),
            "source" => Ok(Self::Source),
            _ => Err(Problem::new(400, "Select a supported export target.")),
        }
    }
    fn id(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Android => "android",
            Self::Apk => "apk",
            Self::Ios => "ios",
            Self::Source => "source",
        }
    }
    fn info(self) -> (&'static str, &'static str) {
        match self {
            Self::Web => ("web.zip", "Web"),
            Self::Android => ("android.aab", "Google Play"),
            Self::Apk => ("android-debug.apk", "Android device"),
            Self::Ios => ("ios.ipa", "App Store"),
            Self::Source => ("source.zip", "Flutter source"),
        }
    }
    fn flags(self, input: &Value) -> Result<Vec<String>> {
        if self == Self::Source {
            return Ok(vec![]);
        }
        if self == Self::Web {
            let base = input
                .get("baseHref")
                .filter(|v| !v.is_null())
                .map_or(Some("/"), Value::as_str)
                .filter(|base| {
                    WEB_BASE.is_match(base)
                        && !base.split('/').any(|part| matches!(part, "." | ".."))
                })
                .ok_or_else(|| Problem::new(400, "Web base path must look like / or /my-app/."))?;
            return Ok([
                "web",
                "--release",
                "--no-web-resources-cdn",
                "--base-href",
                base,
            ]
            .map(str::to_owned)
            .into());
        }
        let base = match self {
            Self::Android => &["appbundle", "--release"][..],
            Self::Apk => &["apk", "--debug"][..],
            _ => &["ipa", "--release", "--export-method", "app-store"][..],
        };
        let flavor = input
            .get("flavor")
            .filter(|v| !v.is_null())
            .map_or(Some(""), Value::as_str)
            .filter(|flavor| flavor.is_empty() || FLAVOR.is_match(flavor))
            .ok_or_else(|| Problem::new(400, "Enter a valid Flutter flavor name."))?;
        let mut result: Vec<_> = base.iter().map(|value| (*value).to_owned()).collect();
        if !flavor.is_empty() {
            result.extend(["--flavor".into(), flavor.into()]);
        }
        if matches!(self, Self::Android | Self::Ios) {
            self.release_version_flags(input, &mut result)?;
        }
        Ok(result)
    }

    fn release_version_flags(self, input: &Value, flags: &mut Vec<String>) -> Result<()> {
        let name = input["buildName"].as_str().unwrap_or_default().trim();
        if !name.is_empty() {
            if name.len() > 80 || name.chars().any(char::is_control) {
                return Err(Problem::new(
                    400,
                    "Enter a valid version of at most 80 characters.",
                ));
            }
            if self == Self::Ios && !IOS_VERSION.is_match(name) {
                return Err(Problem::new(400, "Enter an iOS version like 1.2.0."));
            }
            flags.push(format!("--build-name={name}"));
        }
        let number = input["buildNumber"].as_str().unwrap_or_default().trim();
        if number.is_empty() {
            return Ok(());
        }
        match self {
            Self::Android => {
                let number = number
                    .parse::<u32>()
                    .ok()
                    .filter(|value| (1..=2_100_000_000).contains(value))
                    .filter(|_| number.bytes().all(|byte| byte.is_ascii_digit()))
                    .ok_or_else(|| {
                        Problem::new(400, "Enter an Android version code from 1 to 2100000000.")
                    })?;
                flags.push(format!("--build-number={number}"));
            }
            Self::Ios if number.len() <= 32 && IOS_BUILD.is_match(number) => {
                flags.push(format!("--build-number={number}"));
            }
            _ => {
                return Err(Problem::new(
                    400,
                    "Enter an iOS build number like 42 or 1.2.3.",
                ));
            }
        }
        Ok(())
    }
}

fn find_output(root: &Path, extension: &str) -> Result<PathBuf> {
    build_outputs(root, extension)?
        .into_iter()
        .max_by_key(|output| output.modified)
        .map(|output| output.path)
        .ok_or_else(|| {
            Problem::new(
                422,
                format!(
                    "Flutter did not produce the requested {} output.",
                    extension.to_uppercase()
                ),
            )
        })
}

#[derive(PartialEq)]
struct BuildOutput {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

fn build_outputs(root: &Path, extension: &str) -> Result<Vec<BuildOutput>> {
    if !root.is_dir() {
        return Ok(vec![]);
    }
    let files = files::walk(root, |_| true, false, 2500)?;
    Ok(files
        .entries
        .into_iter()
        .map(|entry| root.join(entry.path))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
        })
        .filter_map(|path| {
            let metadata = fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || files::is_link(&metadata) {
                return None;
            }
            Some(BuildOutput {
                path,
                modified: metadata.modified().ok()?,
                size: metadata.len(),
            })
        })
        .collect())
}

fn new_ipa(root: &Path, previous: &[BuildOutput]) -> Result<PathBuf> {
    build_outputs(root, "ipa")?
        .into_iter()
        .filter(|output| !previous.contains(output))
        .max_by_key(|output| output.modified)
        .map(|output| output.path)
        .ok_or_else(|| {
            Problem::new(422, "Flutter did not export a new IPA. Check the Xcode signing and export messages in the console.")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_log_excerpts_keep_the_latest_lines_and_bound_unicode_output() {
        let job = Job::new("preview".into(), Some("app".into()));
        for index in 0..350 {
            job.log(format!("\u{1b}[31mline {index}\u{1b}[0m"));
        }
        let result = job.app_logs(0, 200);
        let lines = result["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 200);
        assert_eq!(lines[0]["message"], "line 150");
        assert_eq!(lines[199]["message"], "line 349");
        assert_eq!(result["nextAfter"], 350);
        assert_eq!(result["truncated"], true);
        assert!(
            job.app_logs(350, 200)["lines"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        job.log("🦀".repeat(6000));
        job.log("🦀".repeat(6000));
        job.log("🦀".repeat(6000));
        let bounded = job.app_logs(350, 200);
        let bytes: usize = bounded["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["message"].as_str().unwrap().len())
            .sum();
        assert!(bytes <= 64 * 1024);
        assert_eq!(bounded["nextAfter"], 353);
        assert_eq!(bounded["truncated"], true);
    }

    #[test]
    fn app_logs_are_project_scoped_and_ignore_stale_preview_batches() {
        let home = tempfile::tempdir().unwrap();
        let projects = Arc::new(Projects::new(home.path().into()));
        let flutter = Arc::new(Flutter::new(home.path().into()).unwrap());
        let jobs = Jobs::new(projects, flutter, None);
        let project: Project = serde_json::from_value(
            json!({"id":"app","name":"App","root":home.path(),"template":"blank"}),
        )
        .unwrap();
        assert!(jobs.app_logs(&project, &json!({})).unwrap()["job"].is_null());
        let preview = Arc::new(Job::new("preview".into(), Some(project.id.clone())));
        {
            let mut state = preview.state.lock().unwrap();
            state.status = "ready";
            state.result = json!({"epoch":4});
        }
        let other = Arc::new(Job::new("device".into(), Some("other".into())));
        let agent = Arc::new(Job::new("agent-send".into(), Some(project.id.clone())));
        {
            let mut queue = jobs.queue.lock().unwrap();
            queue.previews.insert(project.id.clone(), preview.clone());
            for job in [&preview, &other, &agent] {
                queue.order.push_back(job.id.clone());
                queue.jobs.insert(job.id.clone(), job.clone());
            }
        }
        assert_eq!(
            jobs.app_logs(&project, &json!({})).unwrap()["job"]["id"],
            preview.id
        );
        assert_eq!(
            jobs.app_logs(&project, &json!({"jobId":other.id}))
                .unwrap_err()
                .status
                .as_u16(),
            403
        );
        assert_eq!(
            jobs.app_logs(&project, &json!({"jobId":agent.id}))
                .unwrap_err()
                .status
                .as_u16(),
            400
        );
        assert_eq!(
            jobs.preview_logs(
                &project,
                &json!({"jobId":preview.id,"epoch":3,"lines":["old"]})
            )
            .unwrap()["accepted"],
            0
        );
        assert_eq!(
            jobs.preview_logs(
                &project,
                &json!({"jobId":other.id,"epoch":4,"lines":["other"]})
            )
            .unwrap()["accepted"],
            0
        );
        assert_eq!(
            jobs.preview_logs(
                &project,
                &json!({"jobId":preview.id,"epoch":4,"lines":["Counter=1"]})
            )
            .unwrap()["accepted"],
            1
        );
        assert_eq!(
            jobs.app_logs(&project, &json!({"target":"preview"}))
                .unwrap()["lines"][0]["message"],
            "Counter=1"
        );
        assert!(jobs.app_logs(&project, &json!({"limit":201})).is_err());
        assert!(jobs.app_logs(&project, &json!({"after":-1})).is_err());
        assert!(
            jobs.preview_logs(
                &project,
                &json!({"jobId":preview.id,"epoch":4,"lines":["x".repeat(17*1024)]})
            )
            .is_err()
        );
        preview.state.lock().unwrap().status = "stopped";
        assert_eq!(
            jobs.preview_logs(
                &project,
                &json!({"jobId":preview.id,"epoch":4,"lines":["late"]})
            )
            .unwrap()["accepted"],
            0
        );
    }

    #[test]
    fn failed_ipa_export_cannot_reuse_an_older_ipa() {
        let folder = tempfile::tempdir().unwrap();
        let old = folder.path().join("Runner.ipa");
        fs::write(&old, b"old archive").unwrap();
        let previous = build_outputs(folder.path(), "ipa").unwrap();
        assert!(new_ipa(folder.path(), &previous).is_err());
        fs::write(&old, b"fresh archive for upload").unwrap();
        assert_eq!(new_ipa(folder.path(), &previous).unwrap(), old);
    }

    #[test]
    fn release_versions_override_flutter_defaults_for_the_selected_export() {
        assert_eq!(
            Target::Android.flags(&Value::Null).unwrap(),
            ["appbundle", "--release"]
        );
        assert_eq!(
            Target::Android
                .flags(&json!({"buildName":"2.1 beta", "buildNumber":"0042"}))
                .unwrap(),
            [
                "appbundle",
                "--release",
                "--build-name=2.1 beta",
                "--build-number=42"
            ]
        );
        assert_eq!(
            Target::Ios
                .flags(&json!({"flavor":"testing", "buildName":"2.1.0", "buildNumber":"1.2.3"}))
                .unwrap(),
            [
                "ipa",
                "--release",
                "--export-method",
                "app-store",
                "--flavor",
                "testing",
                "--build-name=2.1.0",
                "--build-number=1.2.3"
            ]
        );
        for value in ["0", "1.2", "2100000001", "+2", "--debug"] {
            assert!(
                Target::Android
                    .flags(&json!({"buildNumber":value}))
                    .is_err()
            );
        }
        assert!(Target::Ios.flags(&json!({"buildName":"2.1-beta"})).is_err());
        assert!(Target::Ios.flags(&json!({"buildNumber":"1..2"})).is_err());
    }
}
