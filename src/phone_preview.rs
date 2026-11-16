use crate::{
    files::{self, Problem, Result},
    flutter::Sdk,
    jobs::{Job, Jobs},
    preview,
    projects::Project,
    server::stream,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path as RoutePath, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{QrCode, render::svg};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const PAGE: &str = include_str!("../ui/phone-preview.html");
const WATCH: &str = include_str!("../ui/phone-preview.js");

fn local(address: Ipv4Addr) -> bool {
    address.is_private() || address.is_link_local()
}

pub fn suggested_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A UDP connect selects an interface without sending a packet.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) if local(*address.ip()) => Some(*address.ip()),
        _ => None,
    }
}

pub fn address(input: &Value) -> Result<Ipv4Addr> {
    let value = input["address"].as_str().unwrap_or_default().trim();
    let parsed = if value.is_empty() {
        suggested_address()
    } else {
        value.parse().ok()
    };
    parsed.filter(|address| local(*address)).ok_or_else(|| {
        Problem::new(
            422,
            "Enter this computer’s local IPv4 address, such as 192.168.1.12.",
        )
    })
}

struct Build {
    generation: u64,
    root: PathBuf,
    _folder: tempfile::TempDir,
}

#[derive(Default)]
struct Builds {
    current: Option<Arc<Build>>,
    previous: Option<Arc<Build>>,
    building: bool,
    failed: bool,
}

struct Site {
    host: String,
    key: String,
    builds: RwLock<Builds>,
    stop: CancellationToken,
}

impl Site {
    fn new(listener: &TcpListener, stop: CancellationToken) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            host: listener.local_addr()?.to_string(),
            key: files::random_token(16)?,
            builds: RwLock::new(Builds {
                building: true,
                ..Default::default()
            }),
            stop,
        }))
    }

    fn url(&self) -> String {
        format!("http://{}/{}/", self.host, self.key)
    }

    fn base(&self, generation: u64) -> String {
        format!("/{}/app/{generation}/", self.key)
    }

    fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(
                "/{key}/",
                get(|| async { content(PAGE, "text/html; charset=utf-8") }),
            )
            .route(
                "/{key}/watch.js",
                get(|| async { content(WATCH, "text/javascript; charset=utf-8") }),
            )
            .route("/{key}/status.json", get(status))
            .route("/{key}/app/{generation}/", get(index))
            .route("/{key}/app/{generation}/{*file}", get(asset))
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(self.clone(), access))
    }
}

fn content(data: &'static str, kind: &'static str) -> Response {
    ([(header::CONTENT_TYPE, kind)], data).into_response()
}

// This listener has its own opaque share link and never routes to the workbench.
async fn access(
    State(site): State<Arc<Site>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let uri = request.uri();
    let key = uri
        .path()
        .strip_prefix('/')
        .and_then(|path| path.split_once('/'))
        .map(|(key, _)| key);
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if site.stop.is_cancelled()
        || !matches!(peer, SocketAddr::V4(address) if local(*address.ip()) || address.ip().is_loopback())
        || host != Some(site.host.as_str())
        || uri.scheme().is_some()
        || !matches!(*request.method(), Method::GET | Method::HEAD)
        || !key.is_some_and(|key| bool::from(key.as_bytes().ct_eq(site.key.as_bytes())))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response = next.run(request).await;
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

async fn status(State(site): State<Arc<Site>>) -> Json<Value> {
    let builds = site.builds.read().unwrap();
    Json(json!({
        "generation": builds.current.as_ref().map(|build| build.generation),
        "building": builds.building,
        "failed": builds.failed,
    }))
}

async fn index(
    State(site): State<Arc<Site>>,
    RoutePath((_, generation)): RoutePath<(String, u64)>,
) -> Result<Response> {
    serve(&site, generation, "index.html", false).await
}

async fn asset(
    State(site): State<Arc<Site>>,
    RoutePath((_, generation, file)): RoutePath<(String, u64, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let navigation = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    serve(&site, generation, &file, navigation).await
}

async fn serve(site: &Site, generation: u64, relative: &str, navigation: bool) -> Result<Response> {
    let missing = || Problem::new(404, "Preview file not found.");
    let build = {
        let builds = site.builds.read().unwrap();
        builds
            .current
            .iter()
            .chain(builds.previous.iter())
            .find(|build| build.generation == generation)
            .cloned()
    }
    .ok_or_else(missing)?;
    let parts = files::relative_parts(relative).map_err(|_| missing())?;
    let name = parts.last().unwrap().to_ascii_lowercase();
    if parts.iter().any(|part| part.starts_with('.'))
        || matches!(
            name.as_str(),
            "_headers" | "_redirects" | "flutter_service_worker.js"
        )
        || name.ends_with(".dart")
        || name.ends_with(".map")
    {
        return Err(missing());
    }
    let file = match files::project_file(&build.root, relative, false) {
        Ok(file) => file,
        Err(error)
            if error.status == StatusCode::NOT_FOUND && navigation && !name.contains('.') =>
        {
            files::project_file(&build.root, "index.html", false).map_err(|_| missing())?
        }
        Err(_) => return Err(missing()),
    };
    stream(&file, None).await
}

pub async fn run(jobs: &Jobs, job: &Job, project: &Project, address: Ipv4Addr) -> Result<()> {
    let sdk = jobs.flutter.require()?;
    let listener = TcpListener::bind((address, 0)).await.map_err(|_| {
        Problem::new(
            422,
            format!("Cannot listen on {address}. Choose a local address for this computer."),
        )
    })?;
    let site = Site::new(&listener, job.cancel.child_token())?;
    let url = site.url();
    let qr = QrCode::new(url.as_bytes())
        .map_err(Problem::internal)?
        .render::<svg::Color>()
        .min_dimensions(208, 208)
        .build();
    job.report(json!({
        "url": url, "qr": format!("data:image/svg+xml;base64,{}", STANDARD.encode(qr)),
        "reloading": true,
    }));
    job.log("Building a Flutter web preview for your phone.");
    let server = axum::serve(
        listener,
        site.router()
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(site.stop.clone().cancelled_owned());
    let parent = jobs.projects.home.join("tmp");
    fs::create_dir_all(&parent)?;
    let mut serving = tokio::spawn(async move { server.await });
    let result = build_loop(jobs, job, project, &sdk, &parent, &site).await;
    site.stop.cancel();
    let _ = std::mem::take(&mut *site.builds.write().unwrap());
    if tokio::time::timeout(Duration::from_secs(1), &mut serving)
        .await
        .is_err()
    {
        serving.abort();
    }
    result
}

async fn build_loop(
    jobs: &Jobs,
    job: &Job,
    project: &Project,
    sdk: &Sdk,
    parent: &Path,
    site: &Site,
) -> Result<()> {
    let mut last: Option<preview::Stamps> = None;
    let mut watch = preview::watch();
    let mut generation = 0;
    let mut dependencies_ready = false;
    let url = site.url();
    loop {
        let manual = if last.is_none() {
            true
        } else {
            tokio::select! {
                _ = job.cancel.cancelled() => return Ok(()),
                _ = job.reload.notified() => true,
                _ = watch.tick() => false,
            }
        };
        if !manual && jobs.busy(&project.id) {
            continue;
        }
        let next = preview::stamps(&project.root)?;
        if !manual && last.as_ref() == Some(&next) {
            continue;
        }
        let refresh_dependencies = !dependencies_ready
            || last
                .as_ref()
                .is_some_and(|last| last.project != next.project);
        last = Some(next);
        generation += 1;
        job.refreshing();
        {
            let mut builds = site.builds.write().unwrap();
            builds.building = true;
            builds.failed = false;
        }
        let build = async {
            if refresh_dependencies {
                dependencies_ready = false;
                jobs.pub_command(job, project, sdk, &["pub", "get"]).await?;
                dependencies_ready = true;
            }
            let folder = tempfile::Builder::new()
                .prefix("phone-preview-")
                .tempdir_in(parent)?;
            let output = folder.path().join("web");
            let base = site.base(generation);
            let root = preview::compile(jobs, job, project, sdk, &output, Some(&base)).await?;
            files::project_file(&root, "index.html", false)?;
            Ok::<_, Problem>(Arc::new(Build {
                generation,
                root,
                _folder: folder,
            }))
        }
        .await;
        if job.cancel.is_cancelled() {
            return Ok(());
        }
        let mut builds = site.builds.write().unwrap();
        builds.building = false;
        match build {
            Ok(build) => {
                let old = builds.current.replace(build);
                let retired = std::mem::replace(&mut builds.previous, old);
                drop(builds);
                job.ready(&url, generation, false);
                job.log("Phone preview updated.");
                drop(retired);
            }
            Err(error) => {
                builds.failed = true;
                job.compile_error(error.message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(generation: u64, text: &str) -> Arc<Build> {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().to_owned();
        fs::write(root.join("index.html"), text).unwrap();
        fs::write(root.join("main.dart.js"), "compiled Flutter").unwrap();
        fs::write(root.join(".env"), "local configuration").unwrap();
        fs::write(root.join("main.dart"), "Flutter source").unwrap();
        fs::write(root.join("flutter_service_worker.js"), "old worker").unwrap();
        Arc::new(Build {
            generation,
            root,
            _folder: folder,
        })
    }

    #[tokio::test]
    async fn phone_link_serves_versioned_web_builds_until_revoked() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let site = Site::new(&listener, CancellationToken::new()).unwrap();
        let url = site.url();
        let origin = format!("http://{}", site.host);
        let router = site
            .router()
            .into_make_service_with_connect_info::<SocketAddr>();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let get = |relative| client.get(format!("{url}{relative}"));

        let shell = get("").send().await.unwrap();
        assert!(shell.status().is_success());
        assert_eq!(shell.headers()["referrer-policy"], "no-referrer");
        assert_eq!(shell.headers()["cache-control"], "no-store");
        assert!(
            shell
                .text()
                .await
                .unwrap()
                .contains("Flutter phone preview")
        );
        let state: Value = get("status.json")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            state,
            json!({"generation":null,"building":true,"failed":false})
        );

        {
            let mut builds = site.builds.write().unwrap();
            builds.current = Some(sample(2, "second Flutter build"));
            builds.previous = Some(sample(1, "first Flutter build"));
            builds.building = false;
        }
        assert_eq!(
            get("app/2/").send().await.unwrap().text().await.unwrap(),
            "second Flutter build"
        );
        assert_eq!(
            get("app/1/").send().await.unwrap().text().await.unwrap(),
            "first Flutter build"
        );
        assert_eq!(
            get("app/2/main.dart.js")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "compiled Flutter"
        );
        let route = get("app/2/settings/account")
            .header(header::ACCEPT, "text/html")
            .send()
            .await
            .unwrap();
        assert_eq!(route.text().await.unwrap(), "second Flutter build");
        for relative in [
            "api/state",
            "app/0/",
            "app/2/.env",
            "app/2/main.dart",
            "app/2/flutter_service_worker.js",
        ] {
            assert_eq!(
                get(relative).send().await.unwrap().status(),
                StatusCode::NOT_FOUND
            );
        }
        let wrong_link = format!("{origin}/{}/", "0".repeat(32));
        assert_eq!(
            client.get(&wrong_link).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get("")
                .header(header::HOST, "other.test")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        site.stop.cancel();
        assert_eq!(
            get("").send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        server.abort();
    }
}
