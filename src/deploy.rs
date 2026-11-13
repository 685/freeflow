use crate::{
    archive,
    files::{self, Problem, Result},
    jobs::{Job, Jobs, Mode},
    mcp::required,
    projects::{Project, Projects},
};
use regex::Regex;
use reqwest::{Method, RequestBuilder, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use tokio::time::{Instant, sleep};
use tokio_util::io::ReaderStream;

const NETLIFY_API: &str = "https://api.netlify.com/api/v1/";
const ZIP_LIMIT: u64 = 128 * 1024 * 1024;
const API_LIMIT: usize = 2 * 1024 * 1024;
static NETLIFY_SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,61}[a-z0-9]$|^[a-z0-9]$").unwrap());
static REMOTE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,100}$").unwrap());
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    team: String,
    name: String,
    token: String,
    site: Option<Site>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Site {
    id: String,
    url: String,
    published_at: Option<String>,
    disabled: bool,
    pending: Option<Pending>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Pending {
    title: String,
    id: Option<String>,
}

fn record_path(projects: &Projects, project: &Project) -> PathBuf {
    projects
        .home
        .join("hosting")
        .join(format!("{}.json", project.id))
}

fn load(projects: &Projects, project: &Project) -> Result<Settings> {
    Ok(files::read_json(&record_path(projects, project))?.unwrap_or_default())
}

fn public(settings: &Settings) -> Value {
    json!({
        "team":settings.team,"name":settings.name,"hasToken":!settings.token.is_empty(),
        "site":settings.site.as_ref().map(|site| json!({
            "url":site.url,"publishedAt":site.published_at,"disabled":site.disabled,
            "pending":site.pending.is_some(),"uncertainUpload":site.pending.as_ref().is_some_and(|pending| pending.id.is_none()),
        })),
    })
}

pub fn state(jobs: &Jobs, project: &Project) -> Result<Value> {
    let mut data = public(&load(&jobs.projects, project)?);
    data["job"] = jobs.active_command(&project.id, "web-publish");
    Ok(data)
}

pub fn configure(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let _guard = SETTINGS_LOCK.lock().unwrap();
    let path = record_path(&jobs.projects, project);
    let mut settings = load(&jobs.projects, project)?;
    let team = required(input, "team")?.trim();
    let name = required(input, "name")?.trim();
    let token = input["token"].as_str().unwrap_or_default().trim();
    if jobs.busy(&project.id) {
        return Err(Problem::new(
            409,
            "Wait for the current project job before changing hosting settings.",
        ));
    }
    if !NETLIFY_SLUG.is_match(team) || (!name.is_empty() && !NETLIFY_SLUG.is_match(name)) {
        return Err(Problem::new(
            400,
            "Use lowercase letters, digits and hyphens for the Netlify team and site name.",
        ));
    }
    if settings.site.is_some() && (team != settings.team || name != settings.name) {
        return Err(Problem::new(
            409,
            "This project is already linked to its Netlify site.",
        ));
    }
    if token.len() > 512 || token.bytes().any(|byte| !byte.is_ascii_graphic()) {
        return Err(Problem::new(400, "Enter a valid Netlify access token."));
    }
    if input["forgetToken"] == true {
        settings.token.clear();
    } else if !token.is_empty() {
        settings.token = token.into();
    }
    if team != settings.team && token.is_empty() {
        settings.token.clear();
    }
    settings.team = team.into();
    settings.name = name.into();
    files::write_json(&path, &settings)?;
    Ok(public(&settings))
}

pub fn operation(jobs: &Arc<Jobs>, project: Project, input: &Value) -> Result<Value> {
    operation_with_api(jobs, project, input, NETLIFY_API)
}

fn operation_with_api(
    jobs: &Arc<Jobs>,
    project: Project,
    input: &Value,
    base: &str,
) -> Result<Value> {
    let action = required(input, "action")?;
    let mode = match action {
        "publish" => Mode::PausePreview,
        "unpublish" | "refresh" | "clear-pending" => Mode::Exclusive,
        _ => return Err(Problem::new(400, "Choose a web publishing action.")),
    };
    let action = action.to_owned();
    let target = project.clone();
    let _guard = SETTINGS_LOCK.lock().unwrap();
    let path = record_path(&jobs.projects, &target);
    let mut settings = load(&jobs.projects, &target)?;
    let api = Netlify::new(base, &settings.token)?;
    jobs.enqueue(
        "web-publish",
        Some(project),
        mode,
        move |jobs, job| async move {
            match action.as_str() {
                "publish" => {
                    if refresh(&api, &path, &mut settings, &job).await? {
                        return Ok(public(&settings));
                    }
                    let parent = jobs.projects.home.join("tmp");
                    fs::create_dir_all(&parent)?;
                    let folder = tempfile::Builder::new()
                        .prefix("publish-")
                        .tempdir_in(parent)?;
                    let output = folder.path().join("public");
                    job.log("Building the Flutter web app for publishing.");
                    jobs.build_site(&job, &target, &output).await?;
                    let zip = folder.path().join("site.zip");
                    let task_path = zip.clone();
                    let cancel = job.cancel.clone();
                    tokio::task::spawn_blocking(move || package(&output, &task_path, &cancel))
                        .await
                        .map_err(Problem::internal)??;
                    job.check()?;
                    publish(&api, &path, &mut settings, &job, &zip).await?;
                }
                "refresh" => {
                    refresh(&api, &path, &mut settings, &job).await?;
                }
                "clear-pending" => {
                    let site = settings
                        .site
                        .as_mut()
                        .ok_or_else(|| Problem::new(404, "This project has no Netlify site."))?;
                    if site
                        .pending
                        .as_ref()
                        .is_none_or(|pending| pending.id.is_some())
                    {
                        return Err(Problem::new(
                            409,
                            "Use Refresh status to check the known deployment.",
                        ));
                    }
                    site.pending = None;
                    files::write_json(&path, &settings)?;
                }
                _ => unpublish(&api, &path, &mut settings, &job).await?,
            }
            Ok(public(&settings))
        },
    )
}

fn package(output: &Path, zip: &Path, cancel: &tokio_util::sync::CancellationToken) -> Result<()> {
    let folder = fs::symlink_metadata(output)?;
    if !folder.is_dir() || files::is_link(&folder) || !output.join("index.html").is_file() {
        return Err(Problem::new(
            422,
            "Flutter did not produce a deployable web build.",
        ));
    }
    let include = |name: &str| {
        !name.starts_with('.')
            && files::visible_name(name)
            && !matches!(
                name.to_ascii_lowercase().as_str(),
                "firebase.json" | "netlify.toml"
            )
    };
    let index = fs::symlink_metadata(output.join("index.html"))?;
    if files::is_link(&index) {
        return Err(Problem::new(
            422,
            "Flutter's index.html must be a regular file.",
        ));
    }
    let redirects = output.join("_redirects");
    if !fs::symlink_metadata(&redirects).is_ok_and(|meta| meta.is_file() && !files::is_link(&meta))
    {
        files::atomic_create(&redirects, b"/* /index.html 200\n")?;
    }
    archive::zip_files(output, zip, include, ZIP_LIMIT, cancel)
}

struct Netlify {
    api: Url,
    client: reqwest::Client,
    token: header::HeaderValue,
}

impl Netlify {
    fn new(base: &str, token: &str) -> Result<Self> {
        if token.is_empty() {
            return Err(Problem::new(
                422,
                "Save a Netlify team and access token first.",
            ));
        }
        let mut token = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Problem::new(400, "Enter a valid Netlify access token."))?;
        token.set_sensitive(true);
        Ok(Self {
            api: Url::parse(base).map_err(Problem::internal)?,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(45))
                .build()
                .map_err(Problem::internal)?,
            token,
        })
    }

    fn request(&self, method: Method, path: &[&str]) -> RequestBuilder {
        let mut url = self.api.clone();
        url.path_segments_mut()
            .expect("Netlify API URL")
            .pop_if_empty()
            .extend(path);
        self.client
            .request(method, url)
            .header(header::AUTHORIZATION, self.token.clone())
            .header(header::ACCEPT, "application/json")
    }

    async fn send(&self, request: RequestBuilder) -> Result<Value> {
        let mut response = request.send().await.map_err(|_| {
            Problem::new(502, "Freeflow could not complete the Netlify request. Use Refresh status to check whether it went through.")
        })?;
        let status = response.status().as_u16();
        if status == 204 {
            return Ok(Value::Null);
        }
        if !response.status().is_success() {
            return Err(match status {
                401 => Problem::new(401, "Netlify rejected the access token."),
                403 => Problem::new(403, "Netlify denied access to the selected team or site."),
                404 => Problem::new(
                    404,
                    "Netlify could not find the selected team, site or deploy.",
                ),
                409 | 422 => Problem::new(
                    422,
                    "Netlify rejected the site name, deploy or site state. Check the site in Netlify.",
                ),
                413 => Problem::new(413, "Netlify rejected the ZIP because it is too large."),
                429 => Problem::new(429, "Netlify rate-limited the request. Try again shortly."),
                _ => Problem::new(502, format!("Netlify returned HTTP {status}.")),
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Problem::new(502, "The Netlify response was interrupted."))?
        {
            if body.len() + chunk.len() > API_LIMIT {
                return Err(Problem::new(
                    502,
                    "Netlify returned a response that is too large.",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| Problem::new(502, "Netlify returned an invalid response."))
    }

    async fn site(&self, id: &str) -> Result<Value> {
        self.send(self.request(Method::GET, &["sites", id])).await
    }

    async fn deploy(&self, site: &str, id: &str) -> Result<Value> {
        self.send(self.request(Method::GET, &["sites", site, "deploys", id]))
            .await
    }
}

fn remote_id(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|id| REMOTE_ID.is_match(id))
        .ok_or_else(|| Problem::new(502, "Netlify returned an invalid identifier."))
}

fn site_url(value: &Value) -> Result<String> {
    let url = value
        .as_str()
        .and_then(|value| Url::parse(value).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        })
        .ok_or_else(|| Problem::new(502, "Netlify did not return a public HTTPS URL."))?;
    Ok(url.into())
}

async fn ensure_site(api: &Netlify, path: &Path, settings: &mut Settings, job: &Job) -> Result<()> {
    if settings.site.is_none() {
        job.log("Creating a Netlify site in your team.");
        if settings.name.is_empty() {
            settings.name = format!("flutter-{}", files::short_id(16));
            files::write_json(path, settings)?;
        }
        let data = api
            .send(
                api.request(Method::POST, &[&settings.team, "sites"])
                    .json(&json!({"name":settings.name})),
            )
            .await?;
        let id = remote_id(&data["id"])?.to_owned();
        settings.site = Some(Site {
            id,
            url: String::new(),
            published_at: None,
            disabled: false,
            pending: None,
        });
        files::write_json(path, settings)?;
    }
    Ok(())
}

async fn publish(
    api: &Netlify,
    path: &Path,
    settings: &mut Settings,
    job: &Job,
    zip: &Path,
) -> Result<()> {
    let input = tokio::fs::File::open(zip).await?;
    let size = input.metadata().await?.len();
    job.check()?;
    ensure_site(api, path, settings, job).await?;
    job.check()?;
    let site = settings.site.as_mut().unwrap();
    let id = site.id.clone();
    let title = format!("Freeflow {}", files::short_id(16));
    site.pending = Some(Pending {
        title: title.clone(),
        id: None,
    });
    files::write_json(path, settings)?;
    job.log("Uploading the built website to Netlify.");
    let request = api
        .request(Method::POST, &["sites", &id, "deploys"])
        .query(&[("title", title.as_str())])
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, size)
        .timeout(Duration::from_secs(300))
        .body(reqwest::Body::wrap_stream(ReaderStream::new(input)));
    let uploaded = tokio::select! {
        response = api.send(request) => response,
        _ = job.cancel.cancelled() => return job.check(),
    };
    let response = match uploaded {
        Ok(response) => response,
        Err(error) => {
            if error.status.is_client_error() {
                settings.site.as_mut().unwrap().pending = None;
                files::write_json(path, settings)?;
            }
            return Err(error);
        }
    };
    let remote = remote_id(&response["id"])?.to_owned();
    settings.site.as_mut().unwrap().pending.as_mut().unwrap().id = Some(remote);
    files::write_json(path, settings)?;
    refresh(api, path, settings, job).await?;
    Ok(())
}

async fn refresh(api: &Netlify, path: &Path, settings: &mut Settings, job: &Job) -> Result<bool> {
    let Some(site) = &settings.site else {
        return Ok(false);
    };
    let Some(pending) = site.pending.clone() else {
        let mut site = site.clone();
        let remote = api.site(&site.id).await?;
        site.url = site_url(&remote["ssl_url"]).unwrap_or(site.url);
        if let Some(disabled) = remote["disabled"].as_bool() {
            site.disabled = disabled;
        }
        if remote_id(&remote["published_deploy"]["id"]).is_ok() {
            site.published_at = remote["published_deploy"]["published_at"]
                .as_str()
                .map(str::to_owned)
                .or(site.published_at)
                .or_else(|| Some(files::now()));
        } else if remote.get("published_deploy").is_some() {
            site.published_at = None;
        }
        settings.site = Some(site);
        files::write_json(path, settings)?;
        return Ok(false);
    };
    let site_id = site.id.clone();
    let deploy_id = match pending.id {
        Some(id) => id,
        None => {
            let request = api
                .request(Method::GET, &["sites", &site_id, "deploys"])
                .query(&[("per_page", "20")]);
            let data = api.send(request).await?;
            let matching = data
                .as_array()
                .ok_or_else(|| Problem::new(502, "Netlify returned an invalid deploy list."))?
                .iter()
                .find(|entry| entry["title"] == pending.title);
            let Some(matching) = matching else {
                return Err(Problem::new(
                    409,
                    "Netlify has not listed the upload yet. Check the deploys in Netlify before clearing the upload status.",
                ));
            };
            let id = remote_id(&matching["id"])?.to_owned();
            settings.site.as_mut().unwrap().pending.as_mut().unwrap().id = Some(id.clone());
            files::write_json(path, settings)?;
            id
        }
    };
    let deadline = Instant::now() + Duration::from_secs(100);
    job.log("Waiting for Netlify to publish the deploy.");
    loop {
        job.check()?;
        let deploy = api.deploy(&site_id, &deploy_id).await?;
        match deploy["state"].as_str() {
            Some("error" | "rejected" | "canceled" | "cancelled") => {
                settings.site.as_mut().unwrap().pending = None;
                files::write_json(path, settings)?;
                return Err(Problem::new(
                    422,
                    "Netlify could not publish this build. See the deploy log in Netlify.",
                ));
            }
            Some("ready") => {
                let remote = api.site(&site_id).await?;
                if remote["published_deploy"]["id"] == deploy_id {
                    let site = settings.site.as_mut().unwrap();
                    site.url =
                        site_url(&remote["ssl_url"]).or_else(|_| site_url(&deploy["ssl_url"]))?;
                    if remote["disabled"].as_bool().unwrap_or(site.disabled) {
                        job.check()?;
                        api.send(api.request(Method::PUT, &["sites", &site_id, "enable"]))
                            .await?;
                    }
                    site.disabled = false;
                    site.published_at = Some(
                        deploy["published_at"]
                            .as_str()
                            .map_or_else(files::now, str::to_owned),
                    );
                    site.pending = None;
                    files::write_json(path, settings)?;
                    job.log("Netlify confirmed the new public build.");
                    return Ok(true);
                }
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(Problem::new(
                504,
                "Netlify is still processing this deploy. Use Refresh status to check it again.",
            ));
        }
        tokio::select! {
            _ = sleep(Duration::from_secs(2)) => {},
            _ = job.cancel.cancelled() => job.check()?,
        }
    }
}

async fn unpublish(api: &Netlify, path: &Path, settings: &mut Settings, job: &Job) -> Result<()> {
    let site = settings
        .site
        .as_mut()
        .ok_or_else(|| Problem::new(404, "This project has no Netlify site."))?;
    job.check()?;
    let request = api
        .request(Method::PUT, &["sites", &site.id, "disable"])
        .query(&[("reason", "Unpublished from Freeflow")]);
    api.send(request).await?;
    site.disabled = true;
    site.pending = None;
    files::write_json(path, settings)?;
    job.log("Netlify disabled the public site.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flutter::Flutter;
    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        extract::{Request, State},
        response::{IntoResponse, Response},
    };
    use std::io::{Cursor, Read};
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Hosting {
        names: Vec<String>,
        release: usize,
        title: String,
        disabled: bool,
        fail_upload: bool,
    }

    async fn provider(State(state): State<Arc<Mutex<Hosting>>>, request: Request) -> Response {
        assert_eq!(
            request.headers()["authorization"],
            "Bearer local-test-access"
        );
        let (parts, body) = request.into_parts();
        let body = to_bytes(body, 1024 * 1024).await.unwrap();
        let mut remote = state.lock().unwrap();
        let prefix = "/api/v1/";
        let path = parts.uri.path().strip_prefix(prefix).unwrap();
        let deploy = format!("deploy-{}", remote.release);
        let ssl_url = "https://my-flutter-site.netlify.app/";
        match (parts.method.as_str(), path) {
            ("POST", "my-team/sites") => {
                let input: Value = serde_json::from_slice(&body).unwrap();
                remote.names.push(input["name"].as_str().unwrap().to_owned());
                Json(json!({"id":"site-1"})).into_response()
            }
            ("POST", "sites/site-1/deploys") => {
                assert_eq!(parts.headers["content-type"], "application/zip");
                assert_eq!(parts.headers["content-length"], body.len().to_string());
                let mut zip = zip::ZipArchive::new(Cursor::new(body)).unwrap();
                let files: Vec<_> = zip.file_names().collect();
                assert_eq!(files.len(), 3);
                assert!(files.contains(&"main.dart.js"));
                assert!(files.contains(&"index.html"));
                let mut redirect = String::new();
                zip.by_name("_redirects").unwrap().read_to_string(&mut redirect).unwrap();
                assert_eq!(redirect, "/* /index.html 200\n");
                remote.title = Url::parse(&format!("https://example.test{}", parts.uri))
                    .unwrap().query_pairs().find(|(key, _)| key == "title").unwrap().1.into();
                remote.release += 1;
                if remote.fail_upload {
                    remote.fail_upload = false;
                    return (axum::http::StatusCode::BAD_GATEWAY, Body::empty()).into_response();
                }
                Json(json!({"id":format!("deploy-{}", remote.release)})).into_response()
            }
            ("GET", "sites/site-1") => {
                Json(json!({"ssl_url":ssl_url, "disabled":remote.disabled,
                    "published_deploy": {"id":deploy,"published_at":"2026-01-01T00:00:00Z"}})).into_response()
            }
            ("GET", "sites/site-1/deploys") => {
                Json(json!([{"id":deploy,"title":remote.title}])).into_response()
            }
            ("GET", route) if route == format!("sites/site-1/deploys/{deploy}") => {
                Json(json!({"id":deploy,"state":"ready","ssl_url":ssl_url,"published_at":"2026-01-01T00:00:00Z"})).into_response()
            }
            ("PUT", "sites/site-1/disable") => {
                remote.disabled = true;
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
            ("PUT", "sites/site-1/enable") => {
                remote.disabled = false;
                axum::http::StatusCode::NO_CONTENT.into_response()
            }
            _ => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    #[cfg(unix)]
    fn fake_flutter(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(
            path,
            r#"#!/bin/sh
if [ "$3" = pub ]; then exit 0; fi
while [ "$1" != "--output" ]; do [ "$#" -gt 0 ] || exit 2; shift; done
mkdir -p "$2"
printf '<html>Flutter</html>' > "$2/index.html"
printf 'compiled' > "$2/main.dart.js"
printf 'excluded' > "$2/.env"
printf 'excluded' > "$2/private.pem"
"#,
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(windows)]
    fn fake_flutter(path: &Path) {
        let lines = [
            "@echo off",
            "if \"%~3\"==\"pub\" exit /b 0",
            ":args",
            "if \"%~1\"==\"\" exit /b 2",
            "if \"%~1\"==\"--output\" goto build",
            "shift",
            "goto args",
            ":build",
            "mkdir \"%~2\"",
            "echo Flutter>\"%~2\\index.html\"",
            "echo compiled>\"%~2\\main.dart.js\"",
            "echo excluded>\"%~2\\.env\"",
            "echo excluded>\"%~2\\private.pem\"",
            "exit /b 0",
        ];
        fs::write(path, lines.join("\r\n")).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn builds_uploads_recovers_and_unpublishes_on_the_same_site() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("flutter project");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("pubspec.yaml"),
            "name: test_app\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();
        let sdk = temp.path().join(if cfg!(windows) {
            "flutter.cmd"
        } else {
            "flutter"
        });
        fake_flutter(&sdk);
        files::write_json(
            &home.join("settings.json"),
            &json!({"flutterPath":sdk,"managedAutoUpdate":false}),
        )
        .unwrap();
        let projects = Arc::new(Projects::new(home.clone()));
        let project = projects.import_folder(root.to_str().unwrap()).unwrap();
        let jobs = Jobs::new(projects, Arc::new(Flutter::new(home).unwrap()), None);
        let config = configure(
            &jobs,
            &project,
            &json!({
                "team":"my-team","name":"my-flutter-site","token":"local-test-access",
            }),
        )
        .unwrap();
        assert_eq!(config["hasToken"], true);
        assert!(!config.to_string().contains("local-test-access"));
        let remote = Arc::new(Mutex::new(Hosting::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v1/", listener.local_addr().unwrap());
        let router = Router::new().fallback(provider).with_state(remote.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let run = async |action| {
            let job = operation_with_api(&jobs, project.clone(), &json!({"action":action}), &base)
                .unwrap();
            let id = job["id"].as_str().unwrap();
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let state = jobs.get(id, 0).unwrap();
                    if matches!(state["status"].as_str(), Some("succeeded" | "failed"))
                        && !jobs.busy(&project.id)
                    {
                        return state;
                    }
                    sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap()
        };
        let created = run("publish").await;
        assert_eq!(created["status"], "succeeded", "{created}");
        let url = created["result"]["site"]["url"].clone();
        assert_eq!(url, "https://my-flutter-site.netlify.app/");
        remote.lock().unwrap().fail_upload = true;
        let interrupted = run("publish").await;
        assert_eq!(interrupted["status"], "failed");
        assert_eq!(
            state(&jobs, &project).unwrap()["site"]["uncertainUpload"],
            true
        );
        let resumed = run("refresh").await;
        assert_eq!(resumed["result"]["site"]["url"], url);
        assert_eq!(resumed["result"]["site"]["pending"], false);
        let disabled = run("unpublish").await;
        assert_eq!(disabled["result"]["site"]["disabled"], true);
        let enabled = run("publish").await;
        assert_eq!(enabled["result"]["site"]["url"], url);
        assert_eq!(enabled["result"]["site"]["disabled"], false);
        {
            let remote = remote.lock().unwrap();
            assert_eq!(remote.names, ["my-flutter-site"]);
            assert_eq!(remote.release, 3);
            assert!(!remote.disabled);
        }
        jobs.close().await;
        server.abort();
    }
}
