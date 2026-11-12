use crate::{
    backends,
    files::{self, Problem, Result, Session},
    flutter::Flutter,
    jobs::Jobs,
    mcp::{self, required, revision},
    projects::{self, Project, Projects},
    signing, visual,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

const HTML: &str = include_str!("../ui/index.html");
const STYLE: &str = include_str!("../ui/styles.css");
const LOADER: &str = "import init from '/freeflow_web.js';\nawait init({module_or_path: '/freeflow_web_bg.wasm'});\n";
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' blob: data:; frame-src http://127.0.0.1:*; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";
const BODY_LIMIT: usize = 15 * 1024 * 1024;

struct Download {
    path: PathBuf,
    name: String,
    expires: Instant,
}

struct Workbench {
    host: String,
    origin: String,
    token: String,
    assets: PathBuf,
    bin: PathBuf,
    jobs: Arc<Jobs>,
    downloads: Mutex<HashMap<String, Download>>,
}

pub async fn serve(
    listener: TcpListener,
    home: PathBuf,
    assets: PathBuf,
    bin: PathBuf,
    no_open: bool,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    for file in ["freeflow_web.js", "freeflow_web_bg.wasm"] {
        anyhow::ensure!(
            assets.join(file).is_file(),
            "Build the Freeflow browser interface with cargo run -p freeflow-build."
        );
    }
    std::fs::create_dir_all(&home)?;
    let host = listener.local_addr()?.to_string();
    let origin = format!("http://{host}");
    let token = files::random_token(32)?;
    let projects = Arc::new(Projects::new(home.clone()));
    let flutter = Arc::new(Flutter::new(home.clone())?);
    let jobs = Jobs::new(projects, flutter);
    let state = Arc::new(Workbench {
        host,
        origin: origin.clone(),
        token: token.clone(),
        assets,
        bin,
        jobs: jobs.clone(),
        downloads: Mutex::new(HashMap::new()),
    });
    let session = home.join("session.json");
    files::write_json(
        &session,
        &Session {
            origin: origin.clone(),
            token: token.clone(),
            pid: std::process::id(),
        },
    )?;
    let update = jobs.clone();
    let timer = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
        loop {
            interval.tick().await;
            let _ = update.auto_update().await;
        }
    });
    let router = Router::new()
        .fallback(dispatch)
        .layer(middleware::from_fn(headers))
        .with_state(state);
    println!(
        "Freeflow {} is running at {origin}",
        env!("CARGO_PKG_VERSION")
    );
    if !no_open {
        if let Err(error) = open::that_detached(&origin) {
            eprintln!("Open {origin} in your browser: {error}");
        }
    }
    let shutdown_jobs = jobs.clone();
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            shutdown_jobs.close().await;
        })
        .await;
    timer.abort();
    jobs.close().await;
    if files::read_json::<Session>(&session)
        .ok()
        .flatten()
        .is_some_and(|old| old.token == token)
    {
        let _ = std::fs::remove_file(session);
    }
    Ok(result?)
}

pub async fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await
}

async fn headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (key, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, CSP),
    ] {
        headers.insert(key, HeaderValue::from_static(value));
    }
    response
}

fn response(status: u16, value: impl serde::Serialize) -> Response {
    (
        StatusCode::from_u16(status).expect("valid status"),
        Json(value),
    )
        .into_response()
}

fn text(content: impl Into<Body>, kind: &'static str) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .body(content.into())
        .unwrap()
}

async fn stream(path: &Path, name: Option<&str>) -> Result<Response> {
    let file = tokio::fs::File::open(path).await.map_err(|error| {
        if files::missing(&error) {
            Problem::new(404, "File not found.")
        } else {
            error.into()
        }
    })?;
    let stat = file.metadata().await?;
    if !stat.is_file() {
        return Err(Problem::new(404, "File not found."));
    }
    let mut builder = Response::builder()
        .header(
            header::CONTENT_TYPE,
            mime_guess::from_path(path).first_or_octet_stream().as_ref(),
        )
        .header(header::CONTENT_LENGTH, stat.len());
    if let Some(name) = name {
        builder = builder.header(
            header::CONTENT_DISPOSITION,
            format!(r#"attachment; filename="{name}""#),
        );
    }
    builder
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(Problem::internal)
}

fn get_header<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    headers.get(key).and_then(|value| value.to_str().ok())
}

async fn body(request: Request) -> Result<Value> {
    if get_header(request.headers(), "content-type").and_then(|value| value.split(';').next())
        != Some("application/json")
    {
        return Err(Problem::new(415, "Use application/json."));
    }
    let bytes = to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| Problem::new(413, "This request is too large."))?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| Problem::new(400, "Send a JSON object."))
}

pub fn read_widgets(projects: &Projects, project: &Project, relative: &str) -> Result<Value> {
    if !relative.ends_with(".dart") {
        return Err(Problem::new(400, "Select a Dart file."));
    }
    let file = projects.file(project, relative)?;
    let mut outline = visual::outline(&file.content)?;
    outline["path"] = json!(file.path);
    outline["revision"] = json!(file.revision);
    Ok(outline)
}

pub fn write_widgets(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let relative = required(input, "path")?;
    if !relative.ends_with(".dart") {
        return Err(Problem::new(400, "Select a Dart file."));
    }
    let file = projects.update(project, relative, revision(input)?, |source| {
        let source = source.ok_or_else(|| Problem::new(404, "Dart file not found."))?;
        visual::edit(&source.content, &input["command"])
    })?;
    Ok(json!(file))
}

fn quote(value: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn ai(state: &Workbench, project: Option<Project>) -> Value {
    let bin = state.bin.to_string_lossy();
    let mut args = vec![
        "mcp".to_owned(),
        "--home".into(),
        state.jobs.projects.home.to_string_lossy().into_owned(),
    ];
    let mut name = String::from("freeflow");
    if let Some(project) = &project {
        name.push('-');
        name.push_str(&project.id);
        args.extend(["--project".into(), project.id.clone()]);
    }
    let command = std::iter::once(bin.as_ref())
        .chain(args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ");
    let desktop = serde_json::to_string_pretty(
        &json!({"mcpServers":{name.clone():{"command":bin,"args":args}}}),
    )
    .unwrap();
    let context = project.map_or(String::new(), |project| {
        format!(r#" "{}" ({})"#, project.name, project.id)
    });
    json!({"codex":format!("codex mcp add {name} -- {command}"),
        "claude":format!("claude mcp add --transport stdio --scope user {name} -- {command}"),
        "desktop":desktop, "prompt":format!("Use the Freeflow MCP tools to inspect the Flutter project{context}. Read source revisions before edits, run analysis, and refresh the local preview after changes.")})
}

fn query(url: &reqwest::Url, key: &str) -> String {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

async fn dispatch(State(state): State<Arc<Workbench>>, request: Request) -> Result<Response> {
    let headers = request.headers();
    let uri = request.uri();
    let path = uri.path();
    if get_header(headers, "host") != Some(&state.host)
        || get_header(headers, "origin").is_some_and(|origin| origin != state.origin)
        || get_header(headers, "sec-fetch-site") == Some("cross-site")
        || uri.scheme().is_some()
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Err(Problem::new(403, "Use the local Freeflow address."));
    }
    let method = request.method().clone();
    if method == Method::GET {
        match path {
            "/" => {
                return Ok(text(
                    HTML.replace("FREEFLOW_SESSION_TOKEN", &state.token),
                    "text/html; charset=utf-8",
                ));
            }
            "/styles.css" => return Ok(text(STYLE, "text/css; charset=utf-8")),
            "/app.mjs" => return Ok(text(LOADER, "text/javascript; charset=utf-8")),
            "/freeflow_web.js" | "/freeflow_web_bg.wasm" => {
                return stream(&state.assets.join(&path[1..]), None).await;
            }
            _ => {}
        }
        if let Some(key) = path.strip_prefix("/download/") {
            let ticket = state
                .downloads
                .lock()
                .unwrap()
                .remove(key)
                .filter(|ticket| ticket.expires > Instant::now())
                .ok_or_else(|| {
                    Problem::new(
                        404,
                        "This download link expired. Open Export to download it again.",
                    )
                })?;
            return stream(&ticket.path, Some(&ticket.name)).await;
        }
    }
    if !path.starts_with("/api/") {
        return Err(Problem::new(404, "Page not found."));
    }
    let token = get_header(headers, "x-freeflow-token").unwrap_or_default();
    if token.as_bytes().ct_eq(state.token.as_bytes()).unwrap_u8() != 1 {
        return Err(Problem::new(401, "Reload the workbench to reconnect."));
    }
    let url = reqwest::Url::parse(&format!(
        "{}{}",
        state.origin,
        uri.path_and_query().unwrap().as_str()
    ))
    .map_err(|_| Problem::new(400, "Invalid request URL."))?;
    let path = path.to_owned();
    let jobs = &state.jobs;
    let projects = &jobs.projects;
    match (method.as_str(), path.as_str()) {
        ("GET", "/api/state") => {
            return Ok(response(
                200,
                json!({
                    "appVersion":env!("CARGO_PKG_VERSION"), "runtime":"rust", "projects":projects.list()?,
                    "templates":projects::templates(), "flutter":jobs.flutter.status()?
                }),
            ));
        }
        ("POST", "/api/projects/new") => {
            let input = body(request).await?;
            return Ok(response(
                202,
                jobs.scaffold(
                    required(&input, "name")?,
                    required(&input, "template")?,
                    input["organization"].as_str(),
                )?,
            ));
        }
        ("POST", "/api/projects/import") => {
            let input = body(request).await?;
            return Ok(response(
                201,
                projects.import_folder(required(&input, "folder")?)?,
            ));
        }
        ("POST", "/api/mcp") => {
            let input = body(request).await?;
            return Ok(response(
                200,
                mcp::call(
                    jobs,
                    required(&input, "name")?,
                    &input["arguments"],
                    input["scope"].as_str(),
                )?,
            ));
        }
        ("GET", "/api/ai") => {
            let id = query(&url, "project");
            return Ok(response(
                200,
                ai(
                    &state,
                    (!id.is_empty()).then(|| projects.get(&id)).transpose()?,
                ),
            ));
        }
        ("POST", "/api/settings") => {
            let result = jobs.flutter.save_settings(&body(request).await?)?;
            let update = jobs.clone();
            tokio::spawn(async move {
                let _ = update.auto_update().await;
            });
            return Ok(response(200, result));
        }
        ("POST", "/api/sdk/check") => {
            jobs.flutter.latest(true).await?;
            return Ok(response(200, jobs.flutter.status()?));
        }
        ("POST", "/api/sdk/install") => return Ok(response(202, jobs.sdk_install(false)?)),
        ("GET", "/api/sdk/job") => return Ok(response(200, jobs.sdk_status())),
        ("POST", "/api/sdk/doctor") => return Ok(response(200, jobs.flutter.diagnostics().await?)),
        _ => {}
    }
    if let Some(path) = path.strip_prefix("/api/jobs/") {
        if let Some(id) = path.strip_suffix("/stop").filter(|id| !id.contains('/')) {
            if method == Method::POST {
                return Ok(response(200, jobs.cancel(id)?));
            }
        } else if !path.contains('/') && method == Method::GET {
            return Ok(response(
                200,
                jobs.get(path, query(&url, "after").parse().unwrap_or(0))?,
            ));
        }
    }
    if let Some((id, part)) = path
        .strip_prefix("/api/projects/")
        .and_then(|path| path.split_once('/'))
    {
        if valid_id(id)
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        {
            let project = projects.get(id)?;
            return project_route(&state, request, &method, &url, project, part).await;
        }
    }
    Err(Problem::new(404, "Freeflow route not found."))
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 8
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn project_route(
    state: &Workbench,
    request: Request,
    method: &Method,
    url: &reqwest::Url,
    project: Project,
    part: &str,
) -> Result<Response> {
    let jobs = &state.jobs;
    let projects = &jobs.projects;
    let file = query(url, "path");
    if *method == Method::GET {
        let data = match part {
            "tree" => json!(projects.tree(&project)?),
            "file" => json!(projects.file(&project, &file)?),
            "plan" => json!(projects.plan(&project)?),
            "widgets" => read_widgets(projects, &project, &file)?,
            "history" => json!(projects.history(&project, &file)?),
            "history-version" => projects.history_version(&project, &file, &query(url, "id"))?,
            "preview" => jobs.preview(&project.id),
            "jobs" => json!(jobs.list(&project.id)),
            "artifacts" => json!(jobs.artifacts(&project)?),
            "backends" => backends::state(projects, &project)?,
            "signing" => signing::status(projects, &project),
            "asset" => {
                if !files::is_image(&file) {
                    return Err(Problem::new(415, "Select a supported image file."));
                }
                let path = files::project_file(&project.root, &file, false)?;
                if tokio::fs::metadata(&path).await?.len() > 20 * 1024 * 1024 {
                    return Err(Problem::new(413, "Asset previews are limited to 20 MiB."));
                }
                return stream(&path, None).await;
            }
            _ => return Err(Problem::new(404, "Freeflow route not found.")),
        };
        return Ok(response(200, data));
    }
    if *method == Method::PUT && part == "file" {
        let input = body(request).await?;
        return Ok(response(
            200,
            projects.save(
                &project,
                required(&input, "path")?,
                required(&input, "content")?.into(),
                revision(&input)?,
            )?,
        ));
    }
    if *method == Method::POST {
        let input = body(request).await?;
        let (code, value) = match part {
            "widgets" => (200, write_widgets(projects, &project, &input)?),
            "preview" => match input["action"].as_str() {
                Some("start") => (202, jobs.start_preview(project)?),
                Some("reload") => (202, jobs.reload_preview(project)?),
                Some("stop") => (200, jobs.stop_preview(&project.id).await),
                _ => return Err(Problem::new(400, "Choose a preview action.")),
            },
            "command" => match input["action"].as_str() {
                Some(action @ ("analyze" | "test")) => (202, jobs.analyze(project, action)?),
                Some("format") => (
                    202,
                    jobs.format(project, required(&input, "path")?, revision(&input)?)?,
                ),
                _ => return Err(Problem::new(400, "Choose a project command.")),
            },
            "build" => (
                202,
                jobs.build(project, required(&input, "target")?, &input)?,
            ),
            "backends" => (
                202,
                jobs.integrate(project, required(&input, "provider")?, &input["config"])?,
            ),
            "signing" => (202, jobs.sign_android(project, input)?),
            "asset" => (
                201,
                projects.asset(
                    &project,
                    required(&input, "name")?,
                    required(&input, "data")?,
                )?,
            ),
            "forget" => {
                jobs.stop_preview(&project.id).await;
                if jobs.busy(&project.id) {
                    return Err(Problem::new(409, "Wait for the project command to finish."));
                }
                projects.forget(&project.id)?;
                (200, json!({"removed":true}))
            }
            "download" => {
                let (path, artifact) = jobs.artifact(&project, required(&input, "id")?)?;
                let key = files::random_token(24)?;
                let now = Instant::now();
                let mut tickets = state.downloads.lock().unwrap();
                tickets.retain(|_, ticket| ticket.expires > now);
                if tickets.len() >= 100 {
                    if let Some(old) = tickets
                        .iter()
                        .min_by_key(|(_, ticket)| ticket.expires)
                        .map(|(key, _)| key.clone())
                    {
                        tickets.remove(&old);
                    }
                }
                tickets.insert(
                    key.clone(),
                    Download {
                        path,
                        name: artifact.name,
                        expires: now + Duration::from_secs(120),
                    },
                );
                (200, json!({"url":format!("/download/{key}")}))
            }
            _ => return Err(Problem::new(404, "Freeflow route not found.")),
        };
        return Ok(response(code, value));
    }
    Err(Problem::new(404, "Freeflow route not found."))
}
