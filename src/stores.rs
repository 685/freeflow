use crate::{
    files::{self, Problem, Result},
    jobs::{Job, Jobs, Mode},
    mcp::required,
    processes::{self, ProcessCommand},
    projects::{Project, Projects},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use regex::Regex;
use reqwest::{Client, Method, RequestBuilder, Url, header};
use ring::{rand::SystemRandom, signature};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio_util::io::ReaderStream;

const PLAY_ORIGIN: &str = "https://androidpublisher.googleapis.com/";
const GOOGLE_OAUTH: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPE: &str = "https://www.googleapis.com/auth/androidpublisher";
const MAX_KEY_BYTES: usize = 64 * 1024;
static PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9_]*(?:\.[a-zA-Z][a-zA-Z0-9_]*)+$").unwrap());
static TRACK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,79}$").unwrap());
static EDIT_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,100}$").unwrap());
static APPLE_KEY_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z0-9]{10}$").unwrap());

fn history_path(projects: &Projects, project: &Project, target: &str) -> PathBuf {
    projects
        .home
        .join("store-uploads")
        .join(&project.id)
        .join(format!("{target}.json"))
}

pub fn state(projects: &Projects, project: &Project) -> Result<Value> {
    Ok(json!({
        "android":files::read_json::<Value>(&history_path(projects,project,"android"))?,
        "ios":files::read_json::<Value>(&history_path(projects,project,"ios"))?,
    }))
}

pub fn submit(jobs: &Arc<Jobs>, project: Project, input: &Value) -> Result<Value> {
    let target = required(input, "target")?;
    if !matches!(target, "android" | "ios") {
        return Err(Problem::new(
            400,
            "Choose Google Play or App Store Connect.",
        ));
    }
    let (bundle, artifact) = jobs.artifact(&project, required(input, "artifact")?)?;
    let extension = if target == "android" { "aab" } else { "ipa" };
    if artifact.target != target
        || artifact.debug_signed
        || bundle.extension().is_none_or(|value| value != extension)
    {
        return Err(Problem::new(
            400,
            "Select a release export signed for this store.",
        ));
    }
    let key = required(input, "key")?;
    if key.len() > MAX_KEY_BYTES {
        return Err(Problem::new(413, "The store key exceeds 64 KiB."));
    }
    let upload = match target {
        "android" => Upload::Play(PlayRelease::parse(input, key)?),
        _ => Upload::Apple(AppleUpload::parse(input, key)?),
    };
    let target = target.to_owned();
    let context = project.clone();
    jobs.enqueue(
        format!("upload-{target}"),
        Some(project),
        Mode::Exclusive,
        move |jobs, job| async move {
            let result = match upload {
                Upload::Play(release) => release.run(&job, &bundle).await?,
                Upload::Apple(upload) => upload.run(&job, &bundle).await?,
            };
            let mut receipt = result;
            receipt["artifact"] = json!(artifact.id);
            receipt["name"] = json!(artifact.name);
            receipt["date"] = json!(files::now());
            if files::write_json(&history_path(&jobs.projects, &context, &target), &receipt)
                .is_err()
            {
                job.log(
                    "The store accepted the upload. Freeflow could not save its local receipt.",
                );
            }
            Ok(receipt)
        },
    )
}

enum Upload {
    Play(PlayRelease),
    Apple(AppleUpload),
}

struct PlayRelease {
    account: Value,
    package: String,
    track: String,
    draft: bool,
    hold_review: bool,
}

fn pem(text: &str) -> Result<Vec<u8>> {
    let data = text
        .trim()
        .strip_prefix("-----BEGIN PRIVATE KEY-----")
        .and_then(|text| text.strip_suffix("-----END PRIVATE KEY-----"))
        .ok_or_else(|| Problem::new(400, "Select the private key file issued by the store."))?;
    let data: String = data
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    STANDARD
        .decode(data)
        .map_err(|_| Problem::new(400, "The private key is invalid."))
}

impl PlayRelease {
    fn parse(input: &Value, key: &str) -> Result<Self> {
        let account: Value = serde_json::from_str(key.trim_start_matches('\u{feff}'))
            .map_err(|_| Problem::new(400, "Select a Google service account JSON key."))?;
        let package = required(input, "package")?.trim();
        let track = required(input, "track")?.trim();
        if package.len() > 200 || !PACKAGE.is_match(package) || !TRACK.is_match(track) {
            return Err(Problem::new(
                400,
                "Enter the Play Console package name and release track.",
            ));
        }
        if account["type"] != "service_account"
            || account["client_email"]
                .as_str()
                .is_none_or(|email| !email.ends_with(".gserviceaccount.com"))
        {
            return Err(Problem::new(
                400,
                "Select a Google service account JSON key.",
            ));
        }
        Ok(Self {
            account,
            package: package.into(),
            track: track.into(),
            draft: input["draft"] == true,
            hold_review: input["holdReview"] == true,
        })
    }

    fn assertion(&self) -> Result<String> {
        let key = pem(required(&self.account, "private_key")?)?;
        let key = signature::RsaKeyPair::from_pkcs8(&key)
            .map_err(|_| Problem::new(400, "The Google service account RSA key is invalid."))?;
        let now = chrono::Utc::now().timestamp();
        let mut header = json!({"alg":"RS256","typ":"JWT"});
        if let Some(id) = self.account["private_key_id"]
            .as_str()
            .filter(|id| !id.is_empty())
        {
            header["kid"] = json!(id);
        }
        let claims = json!({"iss":self.account["client_email"],"scope":GOOGLE_SCOPE,"aud":GOOGLE_OAUTH,
            "iat":now,"exp":now+900});
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut output = vec![0; key.public().modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            message.as_bytes(),
            &mut output,
        )
        .map_err(|_| Problem::new(400, "Could not sign with this service account key."))?;
        Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(output)))
    }

    async fn run(self, job: &Job, bundle: &Path) -> Result<Value> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(12))
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(Problem::internal)?;
        let assertion = self.assertion()?;
        let token = play_json(client.post(GOOGLE_OAUTH).form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &assertion),
        ]))
        .await?;
        let token = token["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| Problem::new(502, "Google did not return a Play API access token."))?;
        let api = PlayApi::new(client, PLAY_ORIGIN, token)?;
        self.release(job, &api, bundle).await
    }

    async fn release(&self, job: &Job, api: &PlayApi, bundle: &Path) -> Result<Value> {
        job.check()?;
        let collection = &["applications", &self.package, "edits"];
        let opened = play_json(
            api.request(Method::POST, false, collection)
                .json(&json!({})),
        )
        .await?;
        let edit = opened["id"]
            .as_str()
            .filter(|id| EDIT_ID.is_match(id))
            .ok_or_else(|| Problem::new(502, "Google Play did not return an edit ID."))?;
        let scope = &["applications", self.package.as_str(), "edits", edit];
        let track = &[
            "applications",
            &self.package,
            "edits",
            edit,
            "tracks",
            &self.track,
        ];
        let prepared = async {
            let current = play_json(api.request(Method::GET, false, track)).await?;
            check_track(&current, self.draft)?;
            let file = tokio::fs::File::open(bundle).await?;
            let size = file.metadata().await?.len();
            job.log(format!("Uploading the signed Android bundle to {}.", self.package));
            let upload = api.request(Method::POST, true, &["applications", &self.package, "edits", edit, "bundles"])
                .query(&[("uploadType","media")]).header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::CONTENT_LENGTH,size).timeout(Duration::from_secs(300))
                .body(reqwest::Body::wrap_stream(ReaderStream::new(file)));
            let uploaded = tokio::select! {
                result = play_json(upload) => result?,
                _ = job.cancel.cancelled() => return Err(Problem::new(409, "Google Play upload canceled.")),
            };
            let version = uploaded["versionCode"].as_i64().filter(|version| *version > 0)
                .ok_or_else(|| Problem::new(502, "Google Play did not return an uploaded version code."))?;
            let release = track_release(&current, &self.track, version, self.draft)?;
            play_json(api.request(Method::PUT, false, track).json(&release)).await?;
            job.check()?;
            Ok::<i64,Problem>(version)
        }.await;
        let version = match prepared {
            Ok(version) => version,
            Err(error) => {
                let _ = api
                    .request(Method::DELETE, false, scope)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await;
                return Err(error);
            }
        };
        let commit = format!("{edit}:commit");
        let query = [
            ("changesNotSentForReview", self.hold_review.to_string()),
            ("changesInReviewBehavior", "ERROR_IF_IN_REVIEW".into()),
        ];
        let response = play_json(
            api.request(
                Method::POST,
                false,
                &["applications", &self.package, "edits", &commit],
            )
            .query(&query),
        )
        .await;
        if let Err(error) = response {
            if error.status.is_client_error() {
                let _ = api
                    .request(Method::DELETE, false, scope)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await;
                return Err(error);
            }
            return Err(Problem::new(
                502,
                "Google Play did not confirm the release. Check Play Console before retrying.",
            ));
        }
        job.log(format!(
            "Google Play accepted version {version} on the {} track.",
            self.track
        ));
        Ok(json!({"package":self.package,"track":self.track,"version":version,"draft":self.draft}))
    }
}

fn track_release(current: &Value, track: &str, version: i64, draft: bool) -> Result<Value> {
    let mut releases = check_track(current, draft)?.to_vec();
    let next = json!({"versionCodes":[version.to_string()],"status":if draft {"draft"} else {"completed"}});
    if draft {
        releases.push(next);
    } else {
        releases = vec![next];
    }
    Ok(json!({"track":track,"releases":releases}))
}

fn check_track(current: &Value, draft: bool) -> Result<&[Value]> {
    let releases = current["releases"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if releases.iter().any(|release| release["status"] == "draft") {
        return Err(Problem::new(
            409,
            "This track already has a draft. Finish or discard it in Play Console first.",
        ));
    }
    if !draft
        && releases
            .iter()
            .any(|release| matches!(release["status"].as_str(), Some("inProgress" | "halted")))
    {
        return Err(Problem::new(
            409,
            "This track has a staged rollout. Finish it in Play Console before replacing it.",
        ));
    }
    Ok(releases)
}

struct PlayApi {
    client: Client,
    origin: Url,
    token: header::HeaderValue,
}

impl PlayApi {
    fn new(client: Client, origin: &str, token: &str) -> Result<Self> {
        let mut token = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Problem::new(502, "Google returned an invalid access token."))?;
        token.set_sensitive(true);
        Ok(Self {
            client,
            origin: Url::parse(origin).map_err(Problem::internal)?,
            token,
        })
    }

    fn request(&self, method: Method, upload: bool, path: &[&str]) -> RequestBuilder {
        let mut url = self.origin.clone();
        let mut segments = url.path_segments_mut().expect("Play API URL");
        segments.pop_if_empty();
        if upload {
            segments.push("upload");
        }
        segments.extend(["androidpublisher", "v3"]).extend(path);
        drop(segments);
        self.client
            .request(method, url)
            .header(header::AUTHORIZATION, self.token.clone())
    }
}

async fn play_json(request: RequestBuilder) -> Result<Value> {
    let mut response = request
        .send()
        .await
        .map_err(|_| Problem::new(502, "Could not reach Google Play."))?;
    let status = response.status();
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Problem::new(502, "The Google Play response was interrupted."))?
    {
        if data.len() + chunk.len() > 512 * 1024 {
            return Err(Problem::new(
                502,
                "Google Play returned an oversized response.",
            ));
        }
        data.extend_from_slice(&chunk);
    }
    let json = serde_json::from_slice::<Value>(&data).ok();
    if status.is_success() {
        return json.ok_or_else(|| Problem::new(502, "Google Play returned an invalid response."));
    }
    let detail = json.as_ref().and_then(|json| {
        json["error"]["message"]
            .as_str()
            .or(json["error_description"].as_str())
    });
    let detail = detail.map(|message| {
        message
            .chars()
            .filter(|ch| !ch.is_control())
            .take(650)
            .collect::<String>()
    });
    Err(Problem::new(
        if status.is_client_error() {
            status.as_u16()
        } else {
            502
        },
        detail.unwrap_or_else(|| format!("Google Play returned HTTP {status}.")),
    ))
}

struct AppleUpload {
    key: String,
    id: String,
    issuer: String,
}

impl AppleUpload {
    fn parse(input: &Value, key: &str) -> Result<Self> {
        if !cfg!(target_os = "macos") {
            return Err(Problem::new(
                422,
                "Upload a signed IPA on a Mac with Xcode installed.",
            ));
        }
        let id = required(input, "keyId")?.trim();
        let issuer = required(input, "issuer")?.trim();
        if !APPLE_KEY_ID.is_match(id) || uuid::Uuid::parse_str(issuer).is_err() {
            return Err(Problem::new(
                400,
                "Enter the App Store Connect key ID and issuer ID.",
            ));
        }
        pem(key)?;
        Ok(Self {
            key: key.into(),
            id: id.into(),
            issuer: issuer.into(),
        })
    }

    async fn run(self, job: &Job, bundle: &Path) -> Result<Value> {
        let xcrun = processes::executable("xcrun")
            .ok_or_else(|| Problem::new(422, "Install Xcode to upload this IPA."))?;
        let key_dir = tempfile::Builder::new().prefix("freeflow-asc-").tempdir()?;
        let keys = key_dir.path().join("private_keys");
        fs::create_dir(&keys)?;
        files::atomic_write(
            &keys.join(format!("AuthKey_{}.p8", self.id)),
            &self.key,
            0o600,
        )?;
        let mut command = ProcessCommand::new(
            xcrun,
            [
                OsString::from("altool"),
                "--upload-app".into(),
                "-t".into(),
                "ios".into(),
                "-f".into(),
                bundle.as_os_str().to_owned(),
                "--apiKey".into(),
                self.id.into(),
                "--apiIssuer".into(),
                self.issuer.into(),
            ],
        );
        command
            .env
            .push(("API_PRIVATE_KEYS_DIR".into(), keys.into_os_string()));
        job.log("Uploading the signed IPA to App Store Connect.");
        job.exec(&command, key_dir.path()).await?;
        job.log("App Store Connect accepted the upload for processing.");
        Ok(json!({"processing":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flutter::Flutter;
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        http::StatusCode,
        response::{IntoResponse, Response},
    };
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Play {
        calls: Vec<String>,
        reject_commit: bool,
    }

    async fn google(State(state): State<Arc<Mutex<Play>>>, request: Request) -> Response {
        let (head, body) = request.into_parts();
        assert_eq!(head.headers["authorization"], "Bearer testing");
        let bytes = to_bytes(body, 2048).await.unwrap();
        let mut state = state.lock().unwrap();
        let path = head.uri.path();
        let path = path
            .strip_prefix("/upload")
            .unwrap_or(path)
            .strip_prefix("/androidpublisher/v3/applications/com.example.app/")
            .unwrap();
        state.calls.push(format!("{} {path}", head.method));
        match (head.method.as_str(), path) {
            ("POST", "edits") => Json(json!({"id":"edit-1"})).into_response(),
            ("GET", "edits/edit-1/tracks/internal") => Json(json!({"track":"internal","releases":[
                {"versionCodes":["10"],"status":"completed"},
            ]}))
            .into_response(),
            ("POST", "edits/edit-1/bundles") => {
                assert_eq!(head.uri.query(), Some("uploadType=media"));
                assert_eq!(head.headers["content-type"], "application/octet-stream");
                assert_eq!(bytes.as_ref(), b"test android bundle");
                Json(json!({"versionCode":11})).into_response()
            }
            ("PUT", "edits/edit-1/tracks/internal") => {
                let data: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(data["releases"][0]["versionCodes"][0], "10");
                assert_eq!(
                    data["releases"][1],
                    json!({"versionCodes":["11"],"status":"draft"})
                );
                Json(data).into_response()
            }
            ("POST", "edits/edit-1:commit") => {
                let query = head.uri.query().unwrap();
                assert!(query.contains("changesNotSentForReview=true"));
                assert!(query.contains("changesInReviewBehavior=ERROR_IF_IN_REVIEW"));
                if state.reject_commit {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":{"message":"Changes already in review"}})),
                    )
                        .into_response();
                }
                Json(json!({"id":"edit-1"})).into_response()
            }
            ("DELETE", "edits/edit-1") => StatusCode::NO_CONTENT.into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    #[tokio::test]
    async fn google_play_preserves_active_release_and_cleans_failed_edits() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("test.aab");
        fs::write(&bundle, b"test android bundle").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let remote = Arc::new(Mutex::new(Play::default()));
        let router = Router::new().fallback(google).with_state(remote.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let jobs = Jobs::new(
            Arc::new(Projects::new(temp.path().into())),
            Arc::new(Flutter::new(temp.path().into()).unwrap()),
            None,
        );
        let run = async || {
            let bundle = bundle.clone();
            let api = PlayApi::new(Client::new(), &origin, "testing").unwrap();
            let release = PlayRelease {
                account: Value::Null,
                package: "com.example.app".into(),
                track: "internal".into(),
                draft: true,
                hold_review: true,
            };
            let job = jobs
                .enqueue(
                    "test-upload",
                    None,
                    Mode::Global,
                    move |_, job| async move { release.release(&job, &api, &bundle).await },
                )
                .unwrap();
            let id = job["id"].as_str().unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let data = jobs.get(id, 0).unwrap();
                    if matches!(data["status"].as_str(), Some("succeeded" | "failed")) {
                        return data;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap()
        };
        let ok = run().await;
        assert_eq!(ok["result"]["version"], 11, "{ok}");
        assert_eq!(ok["result"]["draft"], true);
        {
            let mut state = remote.lock().unwrap();
            assert_eq!(state.calls.len(), 5);
            state.calls.clear();
            state.reject_commit = true;
        }
        let failed = run().await;
        assert_eq!(failed["status"], "failed", "{failed}");
        assert_eq!(failed["error"], "Changes already in review");
        assert_eq!(
            remote.lock().unwrap().calls.last().unwrap(),
            "DELETE edits/edit-1"
        );
        jobs.close().await;
        server.abort();
    }
}
