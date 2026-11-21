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
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
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

fn refresh_error(message: &Value) -> Option<String> {
    if message["error"].is_null()
        && !message["result"]["code"]
            .as_i64()
            .is_some_and(|code| code != 0)
    {
        return None;
    }
    Some(
        message["error"]
            .as_str()
            .or(message["error"]["message"].as_str())
            .or(message["result"]["message"].as_str())
            .unwrap_or("Flutter could not apply this change.")
            .to_owned(),
    )
}

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

#[derive(Clone, PartialEq)]
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

pub(crate) fn missing_dart_sources(
    previous: &Stamps,
    next: &Stamps,
    missing: &mut HashSet<String>,
) -> bool {
    let present: HashSet<_> = next.code.iter().map(|(path, _)| path).collect();
    missing.extend(
        previous
            .code
            .iter()
            .filter(|(path, _)| path.ends_with(".dart") && !present.contains(path))
            .map(|(path, _)| path.clone()),
    );
    missing.retain(|path| !present.contains(path));
    !missing.is_empty()
}

pub(crate) fn prepare_hot_reload(
    project: &Project,
    previous: &Stamps,
    next: Stamps,
) -> Result<Stamps> {
    let before: HashMap<_, _> = previous
        .code
        .iter()
        .map(|(path, stamp)| (path, stamp))
        .collect();
    // Dart's file stat can truncate subsecond precision, while Flutter records a
    // fractional compilation time. Rapid atomic saves must still invalidate the
    // compiler.
    let modified = UNIX_EPOCH
        + Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(Problem::internal)?
                .as_secs()
                + 1,
        );
    for (path, stamp) in &next.code {
        if !path.ends_with(".dart") || before.get(path).copied() == Some(stamp) {
            continue;
        }
        let path = files::project_file(&project.root, path, false)?;
        let file = fs::OpenOptions::new().write(true).open(path)?;
        if file.metadata()?.modified()? < modified {
            file.set_modified(modified)?;
        }
    }
    // Do not mistake our own timestamp updates for another source edit.
    stamps(&project.root)
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
        sdk.web_hot_reload(),
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
    hot_reload: bool,
) -> Result<DebugEnd> {
    let mut last = stamps(&project.root)?;
    let mut applied = last.clone();
    let mut watch = watch();
    let start_deadline = Instant::now() + Duration::from_secs(90);
    let mut app_id: Option<String> = None;
    let mut ready = false;
    let mut request_id = 0;
    let mut pending: Option<(u64, Instant, bool)> = None;
    let mut startup_error = None;
    let mut missing_sources = HashSet::new();
    job.preview_reload_support(hot_reload);

    loop {
        let deadline = pending.map_or(start_deadline, |(_, deadline, _)| deadline);
        let full_restart = tokio::select! {
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
                                if pending.is_some_and(|(id, _, _)| message["id"].as_u64() == Some(id)) {
                                    let (_, _, reloading) = pending.take().unwrap();
                                    if let Some(error) = refresh_error(&message) {
                                        if reloading {
                                            job.compile_error(format!("{error} Fix the code and reload, or use Hot restart to reset the app."));
                                        } else if !message["error"].is_null() || RESTART_REQUIRED.is_match(&error) {
                                            job.log(format!("{error} Restarting Flutter."));
                                            return Ok(DebugEnd::Restart(epoch + 1));
                                        } else {
                                            job.compile_error(error);
                                        }
                                    } else if reloading {
                                        applied = last.clone();
                                        job.hot_reloaded();
                                        job.log("Hot reload complete. App state preserved.");
                                    } else {
                                        applied = last.clone();
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
                continue;
            }
            _ = time::sleep_until(deadline), if !ready || pending.is_some() => {
                if ready {
                    if pending.is_some_and(|(_, _, reloading)| reloading) {
                        pending = None;
                        job.compile_error("Hot reload timed out. The running app was kept. Try Hot restart to reset it.".into());
                        continue;
                    }
                    job.log("Flutter restart timed out. Restarting Flutter.");
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                return Err(Problem::new(422, "Flutter did not start a preview within 90 seconds. See the console."));
            }
            _ = job.restart_app.notified(), if ready && pending.is_none() => true,
            _ = job.reload.notified(), if ready && pending.is_none() => !hot_reload,
            _ = watch.tick(), if ready && pending.is_none() => {
                if jobs.busy(&project.id) { continue; }
                let next = stamps(&project.root)?;
                if next.project != last.project {
                    job.log("Project files changed. Restarting Flutter.");
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                let changed = next.code != last.code;
                if !changed { continue; }
                !hot_reload
            }
        };
        {
            let Some(app_id) = &app_id else {
                job.log("Dart source changed. Restarting Flutter.");
                return Ok(DebugEnd::Restart(epoch + 1));
            };
            let next = stamps(&project.root)?;
            if next.project != last.project {
                job.log("Project files changed. Restarting Flutter.");
                return Ok(DebugEnd::Restart(epoch + 1));
            }
            // The incremental compiler can retain a deleted library. Keep the
            // running app until the user restarts or restores the missing files.
            if missing_dart_sources(&last, &next, &mut missing_sources) {
                last = next;
                if full_restart {
                    job.log("Dart files were removed. Restarting Flutter to rebuild the preview.");
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                job.compile_error("A Dart file was removed. Use Hot restart to rebuild the preview, or restore the file to continue Hot reload. The running app was kept.".into());
                continue;
            }
            match prepare_hot_reload(project, &applied, next) {
                Ok(next) => last = next,
                Err(error) if full_restart => {
                    job.log(format!(
                        "{} Restarting Flutter to read the changed source.",
                        error.message
                    ));
                    return Ok(DebugEnd::Restart(epoch + 1));
                }
                Err(error) => {
                    last = stamps(&project.root)?;
                    job.compile_error(format!(
                        "{} Use Hot restart to apply this change.",
                        error.message
                    ));
                    continue;
                }
            }
            request_id += 1;
            job.refreshing();
            job.log(if full_restart {
                "Hot restarting the preview."
            } else {
                "Hot reloading the preview."
            });
            let command = json!([{"id":request_id,"method":"app.restart","params":{
                "appId":app_id,"fullRestart":full_restart,"pause":false}}]);
            if process.write(&format!("{command}\n")).await.is_err() {
                return Ok(DebugEnd::Restart(epoch + 1));
            }
            pending = Some((
                request_id,
                Instant::now() + Duration::from_secs(65),
                !full_restart,
            ));
        }
    }
}

pub(crate) async fn compile(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    sdk: &Sdk,
    output: &Path,
    base: Option<&str>,
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
    if let Some(base) = base {
        args.push(format!("--base-href={base}").into());
    }
    args.extend(
        backends::options(&jobs.projects.home, project)?
            .into_iter()
            .map(OsString::from),
    );
    let _build = jobs.flutter_build_slot(job, project).await?;
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
    job.preview_reload_support(false);
    let parent = jobs.projects.home.join("tmp");
    fs::create_dir_all(&parent)?;
    let folder = tempfile::Builder::new()
        .prefix("preview-")
        .tempdir_in(&parent)?;
    let mut last = stamps(&project.root)?;
    let mut generation = 0;
    let output = compile(jobs, job, project, sdk, &folder.path().join("web-0"), None).await?;
    let site = Site::start(output).await?;
    job.ready(&site.url, epoch, false);
    job.log("Preview ready. Flutter recompiles when source files change.");
    let mut watch = watch();
    while !job.cancel.is_cancelled() {
        let manual = tokio::select! {
            _ = job.cancel.cancelled() => break,
            _ = job.reload.notified() => true,
            _ = job.restart_app.notified() => true,
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
            compile(jobs, job, project, sdk, &next, None).await
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
