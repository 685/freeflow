use crate::{
    backends,
    files::{self, Problem, Result},
    flutter::Sdk,
    inspector,
    jobs::{Job, Jobs},
    processes::Process,
    projects::Project,
};
use axum::{
    Router,
    body::Body,
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
    time::{Duration, SystemTime},
};
use tokio::{
    net::TcpListener,
    time::{self, Instant, MissedTickBehavior},
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

static WORKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"serviceWorkerSettings:\s*\{\s*serviceWorkerVersion:\s*["'][^"']+["']\s*,?\s*\}\s*,?"#,
    )
    .unwrap()
});
static RESTART_REQUIRED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)not supported|not available|no connected").unwrap());

pub async fn run(jobs: &Jobs, job: &Job, project: &Project) -> Result<()> {
    let sdk = jobs.flutter.require()?;
    let mut epoch = 0;
    let mut instrument = jobs.origin.is_some();
    loop {
        job.restart(epoch);
        jobs.pub_command(job, project, &sdk, &["pub", "get"])
            .await?;
        let entry = instrument.then(|| inspector::entry(project)).flatten();
        let result = debug(
            jobs,
            job,
            project,
            &sdk,
            epoch,
            entry.as_ref().map(|entry| entry.target.as_str()),
        )
        .await;
        match result {
            Ok(DebugEnd::Stopped) => return Ok(()),
            Ok(DebugEnd::Restart(next_epoch)) => epoch = next_epoch,
            Err(error) if entry.is_some() && !job.is_ready() && !job.cancel.is_cancelled() => {
                job.log(error.message);
                job.log("Trying the Flutter preview without widget picking.");
                instrument = false;
            }
            Err(error) if !job.is_ready() && !job.cancel.is_cancelled() => {
                job.log(error.message);
                job.log("Trying the compiled Flutter preview.");
                return compiled(jobs, job, project, &sdk, epoch).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(PartialEq)]
pub(crate) struct Stamps {
    pub(crate) code: Vec<(String, Option<(SystemTime, u64)>)>,
    pub(crate) project: Vec<(String, Option<(SystemTime, u64)>)>,
}

pub(crate) fn stamps(root: &Path) -> Result<Stamps> {
    fn stamp(root: &Path, relative: String) -> (String, Option<(SystemTime, u64)>) {
        let metadata = fs::symlink_metadata(root.join(&relative))
            .ok()
            .filter(|metadata| !files::is_link(metadata));
        let value = metadata
            .and_then(|metadata| metadata.modified().ok().map(|time| (time, metadata.len())));
        (relative, value)
    }
    let collect = |folder: &str| -> Result<Vec<_>> {
        let base = root.join(folder);
        if !fs::symlink_metadata(&base)
            .is_ok_and(|metadata| metadata.is_dir() && !files::is_link(&metadata))
        {
            return Ok(vec![]);
        }
        Ok(files::walk(&base, files::visible_name, true, 4500)?
            .entries
            .into_iter()
            .map(|entry| stamp(root, format!("{folder}/{}", entry.path)))
            .collect())
    };
    let code = collect("lib")?;
    let mut project = collect("web")?;
    project.extend(collect("assets")?);
    for name in ["pubspec.yaml", "pubspec.lock"] {
        project.push(stamp(root, name.into()));
    }
    Ok(Stamps { code, project })
}

enum DebugEnd {
    Stopped,
    Restart(u64),
}

async fn debug(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    sdk: &Sdk,
    epoch: u64,
    target: Option<&str>,
) -> Result<DebugEnd> {
    let address = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?;
    let port = address.port().to_string();
    let url = format!("http://{address}/");
    let mut args = [
        "run",
        "-d",
        "web-server",
        "--web-hostname",
        "127.0.0.1",
        "--web-port",
        &port,
        "--machine",
        "--no-pub",
        "--no-web-resources-cdn",
    ]
    .map(str::to_owned)
    .to_vec();
    args.extend(backends::options(&jobs.projects.home, project)?);
    if let Some((target, origin)) = target.zip(jobs.origin.as_deref()) {
        args.extend([
            format!("--target={target}"),
            format!("--dart-define=FREEFLOW_WORKBENCH_ORIGIN={origin}"),
            "--track-widget-creation".to_owned(),
        ]);
    }
    let command = jobs.flutter.command(sdk, args)?;
    job.check()?;
    job.log("Starting a Flutter web preview.");
    let mut process = command.spawn(Some(&project.root), true)?;
    let result = debug_session(
        jobs,
        job,
        project,
        &mut process,
        &url,
        epoch,
        target.is_some(),
    )
    .await;
    process.stop().await;
    result
}

pub(crate) fn watch() -> time::Interval {
    let interval = Duration::from_millis(950);
    let mut watch = time::interval_at(Instant::now() + interval, interval);
    watch.set_missed_tick_behavior(MissedTickBehavior::Skip);
    watch
}

async fn debug_session(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    process: &mut Process,
    url: &str,
    mut epoch: u64,
    inspector: bool,
) -> Result<DebugEnd> {
    let mut last = stamps(&project.root)?;
    let mut watch = watch();
    let start_deadline = Instant::now() + Duration::from_secs(90);
    let mut app_id: Option<String> = None;
    let mut ready = false;
    let mut request_id = 0;
    let mut pending: Option<(u64, Instant)> = None;
    let mut startup_error = None;

    loop {
        let deadline = pending.map_or(start_deadline, |(_, deadline)| deadline);
        let reload = tokio::select! {
            _ = job.cancel.cancelled() => return Ok(DebugEnd::Stopped),
            status = process.child.wait() => {
                let status = status?;
                let message = startup_error.take().unwrap_or_else(|| format!("Flutter preview exited with {status}."));
                return Err(Problem::new(422, message));
            }
            line = process.lines.recv(), if !process.lines.is_closed() => {
                if let Some(line) = line {
                    match serde_json::from_str::<Vec<Value>>(&line) {
                        Ok(messages) => {
                            for message in messages {
                                let params = &message["params"];
                                match message["event"].as_str() {
                                    Some("app.start" | "app.started") => {
                                        if let Some(id) = params["appId"].as_str() { app_id = Some(id.into()); }
                                    }
                                    Some("app.stop") => {
                                        if let Some(error) = params["error"].as_str() {
                                            startup_error = Some(error.to_owned());
                                            job.log(error);
                                        }
                                    }
                                    Some("app.log" | "daemon.logMessage" | "app.debugOutput") => {
                                        if let Some(log) = params["log"].as_str().or(params["message"].as_str()) { job.log(log); }
                                    }
                                    _ => {}
                                }
                                // Flutter serves HTML before the initial Dart build finishes.
                                if !ready && matches!(message["event"].as_str(), Some("app.webLaunchUrl" | "app.started")) {
                                    job.ready(url, epoch, inspector);
                                    job.log("Preview ready.");
                                    ready = true;
                                }
                                if pending.is_some_and(|(id, _)| message["id"].as_u64() == Some(id)) {
                                    pending = None;
                                    if !message["error"].is_null() {
                                        job.log("Flutter could not refresh the page. Restarting Flutter.");
                                        return Ok(DebugEnd::Restart(epoch + 1));
                                    }
                                    let result = &message["result"];
                                    if result["code"].as_i64().is_some_and(|code| code != 0) {
                                        let error = result["message"].as_str().unwrap_or("Flutter could not compile this change.");
                                        if RESTART_REQUIRED.is_match(error) {
                                            job.log(format!("{error} Restarting Flutter."));
                                            return Ok(DebugEnd::Restart(epoch + 1));
                                        }
                                        job.compile_error(error.into());
                                    } else {
                                        epoch += 1;
                                        job.ready(url, epoch, inspector);
                                        job.log("Preview updated.");
                                    }
                                }
                            }
                        }
                        Err(_) => job.log(line),
                    }
                }
                false
            }
            _ = time::sleep_until(deadline), if !ready || pending.is_some() => {
                if ready {
                    job.log("Flutter restart timed out. Restarting Flutter.");
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                return Err(Problem::new(422, "Flutter did not start a preview within 90 seconds. See the console."));
            }
            _ = job.reload.notified(), if ready && pending.is_none() => true,
            _ = watch.tick(), if ready && pending.is_none() => {
                if jobs.busy(&project.id) { continue; }
                let next = stamps(&project.root)?;
                if next.project != last.project {
                    job.log("Project files changed. Restarting Flutter.");
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                let changed = next.code != last.code;
                last = next;
                changed
            }
        };
        if reload {
            let Some(app_id) = &app_id else {
                job.log("Dart source changed. Restarting Flutter.");
                return Ok(DebugEnd::Restart(epoch + 1));
            };
            request_id += 1;
            job.refreshing();
            job.log("Refreshing the preview.");
            let command = json!([{"id":request_id,"method":"app.restart","params":{
                "appId":app_id,"fullRestart":true,"pause":false}}]);
            if process.write(&format!("{command}\n")).await.is_err() {
                return Ok(DebugEnd::Restart(epoch + 1));
            }
            pending = Some((request_id, Instant::now() + Duration::from_secs(25)));
        }
    }
}

async fn compile(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    sdk: &Sdk,
    output: &Path,
) -> Result<PathBuf> {
    let mut args: Vec<OsString> = [
        "build",
        "web",
        "--release",
        "--no-pub",
        "--no-web-resources-cdn",
        "--output",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    args.push(output.as_os_str().into());
    args.extend(
        backends::options(&jobs.projects.home, project)?
            .into_iter()
            .map(OsString::from),
    );
    job.exec(&jobs.flutter.command(sdk, args)?, &project.root)
        .await?;
    let bootstrap = output.join("flutter_bootstrap.js");
    if bootstrap.is_file() {
        let script = fs::read_to_string(&bootstrap)?;
        // Each build uses the same origin. Avoid registering a worker with stale assets.
        let script = WORKER.replace_all(&script, "");
        fs::write(&bootstrap, script.as_bytes())?;
    }
    Ok(dunce::canonicalize(output)?)
}

async fn compiled(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    sdk: &Sdk,
    mut epoch: u64,
) -> Result<()> {
    let parent = jobs.projects.home.join("tmp");
    fs::create_dir_all(&parent)?;
    let folder = tempfile::Builder::new()
        .prefix("preview-")
        .tempdir_in(&parent)?;
    let mut last = stamps(&project.root)?;
    let mut generation = 0;
    let output = compile(jobs, job, project, sdk, &folder.path().join("web-0")).await?;
    let site = Site::start(output).await?;
    job.ready(&site.url, epoch, false);
    job.log("Preview ready. Flutter recompiles when source files change.");
    let mut watch = watch();
    while !job.cancel.is_cancelled() {
        let manual = tokio::select! {
            _ = job.cancel.cancelled() => break,
            _ = job.reload.notified() => true,
            _ = watch.tick() => false,
        };
        if jobs.busy(&project.id) {
            continue;
        }
        let next = stamps(&project.root)?;
        let changed_project = next.project != last.project;
        let changed = next != last;
        last = next;
        if !manual && !changed {
            continue;
        }
        job.refreshing();
        generation += 1;
        let next = folder.path().join(format!("web-{generation}"));
        let rebuild = async {
            if changed_project {
                jobs.pub_command(job, project, sdk, &["pub", "get"]).await?;
            }
            compile(jobs, job, project, sdk, &next).await
        };
        match rebuild.await {
            Ok(path) => {
                let old = std::mem::replace(&mut *site.root.write().unwrap(), path);
                if old.parent() == Some(folder.path()) {
                    let _ = fs::remove_dir_all(old);
                }
                epoch += 1;
                job.ready(&site.url, epoch, false);
                job.log("Preview updated.");
            }
            Err(error) if !job.cancel.is_cancelled() => {
                job.compile_error(error.message);
                let _ = fs::remove_dir_all(next);
            }
            Err(_) => break,
        }
    }
    Ok(())
}

#[derive(Clone)]
struct SiteState {
    host: String,
    root: Arc<RwLock<PathBuf>>,
}

struct Site {
    url: String,
    root: Arc<RwLock<PathBuf>>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Site {
    async fn start(root: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let host = listener.local_addr()?.to_string();
        let root = Arc::new(RwLock::new(root));
        let router = Router::new()
            .route(
                "/",
                get(|State(site): State<SiteState>, headers: HeaderMap| {
                    serve_file(site, headers, "index.html".into())
                }),
            )
            .route(
                "/{*file}",
                get(
                    |State(site): State<SiteState>,
                     headers: HeaderMap,
                     RoutePath(file): RoutePath<String>| {
                        serve_file(site, headers, file)
                    },
                ),
            )
            .with_state(SiteState {
                host: host.clone(),
                root: root.clone(),
            });
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(signal.cancelled_owned())
                .await;
        });
        Ok(Self {
            url: format!("http://{host}/"),
            root,
            stop,
            task,
        })
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

async fn serve_file(site: SiteState, headers: HeaderMap, relative: String) -> Response {
    if headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(&site.host)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if relative.contains(['\\', ':', '\0'])
        || !Path::new(&relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let root = site.root.read().unwrap().clone();
    let candidate = root.join(&relative);
    let file = dunce::canonicalize(&candidate).ok().or_else(|| {
        candidate
            .extension()
            .is_none()
            .then(|| root.join("index.html"))
    });
    let Some(file) = file.filter(|file| file.starts_with(&root) && file.is_file()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(source) = tokio::fs::File::open(&file).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(stat) = source.metadata().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            mime_guess::from_path(file).first_or_octet_stream().as_ref(),
        )
        .header(header::CONTENT_LENGTH, stat.len())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from_stream(ReaderStream::new(source)))
        .unwrap()
}
