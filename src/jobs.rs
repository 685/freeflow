use crate::{
    archive, backends,
    files::{self, Problem, Result},
    flutter::{Flutter, Sdk},
    preview,
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
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::Notify;
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
        }
    }

    fn view(&self, after: u64) -> Value {
        let state = self.state.lock().unwrap();
        json!({ "id":self.id,"kind":self.kind,"projectId":self.project_id,"status":state.status,
            "startedAt":self.started,"endedAt":state.ended,"error":state.error,"result":state.result,
            "log":state.lines.iter().filter(|line| line.id > after).collect::<Vec<_>>() })
    }

    pub fn log(&self, line: impl AsRef<str>) {
        let clean = ANSI.replace_all(line.as_ref(), "");
        if clean.is_empty() {
            return;
        }
        let message = clean.chars().take(6000).collect();
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let id = state.sequence;
        state.lines.push_back(Line { id, message });
        if state.lines.len() > 300 {
            state.lines.pop_front();
        }
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
        if self.kind == "preview" && !state.result["url"].is_null() {
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

    pub fn ready(&self, url: &str, epoch: u64) {
        let mut state = self.state.lock().unwrap();
        state.status = "ready";
        state.result = json!({"url":url,"epoch":epoch,"reloading":false,"compileError":null});
    }

    pub fn refreshing(&self) {
        let mut state = self.state.lock().unwrap();
        state.result["reloading"] = json!(true);
        state.result["compileError"] = Value::Null;
    }

    pub fn is_ready(&self) -> bool {
        self.state.lock().unwrap().status == "ready"
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
enum Mode {
    Exclusive,
    PausePreview,
    Preview,
    Sdk,
}

#[derive(Default)]
struct Queue {
    jobs: HashMap<String, Arc<Job>>,
    order: VecDeque<String>,
    previews: HashMap<String, Arc<Job>>,
    busy: HashMap<String, String>,
    sdk: Option<Arc<Job>>,
    closing: bool,
}

pub struct Jobs {
    pub projects: Arc<Projects>,
    pub flutter: Arc<Flutter>,
    queue: Mutex<Queue>,
}

impl Jobs {
    pub fn new(projects: Arc<Projects>, flutter: Arc<Flutter>) -> Arc<Self> {
        Arc::new(Self {
            projects,
            flutter,
            queue: Mutex::new(Queue::default()),
        })
    }

    fn enqueue<F>(
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
            matches!(mode, Mode::PausePreview)
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
            Mode::Sdk => queue.sdk = Some(job.clone()),
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

    pub async fn pub_command(
        &self,
        job: &Job,
        project: &Project,
        sdk: &Sdk,
        args: &[&str],
    ) -> Result<()> {
        let mut arguments = args.to_vec();
        if self.flutter.settings()?.offline {
            arguments.push("--offline");
        }
        let command = self.flutter.command(sdk, arguments)?;
        job.exec(&command, &project.root).await
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
            let mut debug = false;
            if target != Target::Source {
                let sdk = this.flutter.require()?;
                debug = target == Target::Android && signing::status(&this.projects, &project)["debugFallback"] == true;
                if debug { job.log("This project uses the default debug key. Configure an upload key before submitting to Google Play."); }
                this.pub_command(&job, &project, &sdk, &["pub", "get"]).await?;
                let mut args = vec!["build".into()];
                args.extend(flags);
                args.push("--no-pub".into());
                args.extend(backends::options(&this.projects.home, &project)?);
                job.exec(&this.flutter.command(&sdk, args)?, &project.root).await?;
            }
            let artifact = tokio::task::spawn_blocking(move || this.make_artifact(&job, &project, target, debug))
                .await.map_err(Problem::internal)??;
            Ok(json!({"artifact":artifact}))
        })
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
            _ => {
                let (directory, extension) = match target {
                    Target::Android => ("build/app/outputs/bundle", "aab"),
                    Target::Apk => ("build/app/outputs/flutter-apk", "apk"),
                    Target::Ios => ("build/ios/ipa", "ipa"),
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
        Ok(result)
    }
}

fn find_output(root: &Path, extension: &str) -> Result<PathBuf> {
    let missing = || {
        Problem::new(
            422,
            format!(
                "Flutter did not produce the requested {} output.",
                extension.to_uppercase()
            ),
        )
    };
    if !root.is_dir() {
        return Err(missing());
    }
    let tree = files::walk(root, |_| true, false, 2500)?;
    tree.entries
        .into_iter()
        .filter_map(|entry| {
            let path = root.join(entry.path);
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
            {
                return None;
            }
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
        .ok_or_else(missing)
}
