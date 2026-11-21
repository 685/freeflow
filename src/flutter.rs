use crate::{
    files::{self, Problem, Result},
    processes::{self, ProcessCommand},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const MIRROR: &str = "https://storage.googleapis.com/flutter_infra_release/flutter/";
const LAUNCHER: &str = if cfg!(windows) {
    "flutter.bat"
} else {
    "flutter"
};
pub const PLATFORM: &str = if cfg!(windows) {
    "win32"
} else if cfg!(target_os = "macos") {
    "darwin"
} else {
    "linux"
};
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$").unwrap());

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub flutter_path: String,
    pub offline: bool,
    pub managed_auto_update: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            flutter_path: String::new(),
            offline: false,
            managed_auto_update: true,
        }
    }
}

#[derive(Clone)]
pub struct Sdk {
    pub file: PathBuf,
    pub version: Option<String>,
    pub mode: &'static str,
}

impl Sdk {
    pub fn web_hot_reload(&self) -> bool {
        let version = self.version.clone().or_else(|| {
            if !standard(&self.file) {
                return None;
            }
            let info: Value =
                files::read_json(&self.file.parent()?.join("cache/flutter.version.json"))
                    .ok()??;
            info["frameworkVersion"]
                .as_str()
                .or(info["flutterVersion"].as_str())
                .map(str::to_owned)
        });
        let Some(version) = version else {
            return false;
        };
        let mut parts = version.split('.');
        let Some((major, minor)) = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .zip(parts.next().and_then(|value| value.parse::<u32>().ok()))
        else {
            return false;
        };
        (major, minor) >= (3, 38)
    }
}

#[derive(Clone)]
pub struct Release {
    pub version: String,
    sha256: String,
    url: reqwest::Url,
}

pub struct Flutter {
    home: PathBuf,
    manifest: Mutex<Option<(Instant, Release)>>,
    refresh: tokio::sync::Mutex<()>,
    client: reqwest::Client,
}

fn locate(input: &Path) -> Option<PathBuf> {
    if input.as_os_str().is_empty() {
        return None;
    }
    let path = std::path::absolute(input).ok()?;
    if path.is_file() {
        return Some(path);
    }
    let executable = path.join("bin").join(LAUNCHER);
    executable.is_file().then_some(executable)
}

impl Flutter {
    pub fn new(home: PathBuf) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Problem::internal)?;
        Ok(Self {
            home,
            manifest: Mutex::new(None),
            refresh: tokio::sync::Mutex::new(()),
            client,
        })
    }

    fn config(&self) -> PathBuf {
        self.home.join("settings.json")
    }

    pub fn settings(&self) -> Result<Settings> {
        Ok(files::read_json(&self.config())?.unwrap_or_default())
    }

    pub fn save_settings(&self, input: &Value) -> Result<Settings> {
        let settings: Settings = serde_json::from_value(input.clone())
            .map_err(|_| Problem::new(400, "Enter valid Flutter settings."))?;
        if !settings.flutter_path.is_empty() {
            let path = Path::new(&settings.flutter_path);
            if !path.is_absolute() {
                return Err(Problem::new(
                    400,
                    "Enter an absolute Flutter SDK or executable path.",
                ));
            }
            if locate(path).is_none() {
                return Err(Problem::new(400, "Flutter was not found at that path."));
            }
        }
        files::write_json(&self.config(), &settings)?;
        Ok(settings)
    }

    pub fn selected(&self) -> Result<Option<Sdk>> {
        let settings = self.settings()?;
        let explicit = env::var_os("FREEFLOW_FLUTTER")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| settings.flutter_path.into());
        if !explicit.is_empty() {
            return Ok(locate(Path::new(&explicit)).map(|file| Sdk {
                file,
                version: None,
                mode: "external",
            }));
        }
        let sdk_home = self.home.join("sdk");
        let active: Value = files::read_json(&sdk_home.join("active.json"))?.unwrap_or_default();
        if let Some(version) = active["version"]
            .as_str()
            .filter(|version| VERSION.is_match(version))
        {
            if let Some(file) = locate(&sdk_home.join(version).join("flutter")) {
                return Ok(Some(Sdk {
                    file,
                    version: Some(version.into()),
                    mode: "managed",
                }));
            }
        }
        Ok(processes::executable(LAUNCHER).map(|file| Sdk {
            file,
            version: None,
            mode: "external",
        }))
    }

    pub fn require(&self) -> Result<Sdk> {
        self.selected()?.ok_or_else(|| {
            Problem::new(
                409,
                "Install the stable Flutter SDK in Settings or add Flutter to PATH.",
            )
        })
    }

    pub fn dart_analyzer(&self) -> Result<ProcessCommand> {
        let flutter = self.require()?;
        let name = if cfg!(windows) { "dart.exe" } else { "dart" };
        let path = dunce::canonicalize(&flutter.file)
            .ok()
            .and_then(|file| {
                file.parent()
                    .map(|bin| bin.join("cache/dart-sdk/bin").join(name))
            })
            .filter(|file| file.is_file())
            .or_else(|| processes::executable(name))
            .ok_or_else(|| {
                Problem::new(
                    422,
                    "The Dart SDK is missing. Select an installed Flutter SDK in Settings.",
                )
            })?;
        let mut command = ProcessCommand::new(path, ["language-server", "--protocol=lsp"]);
        command
            .env
            .push(("DART_SUPPRESS_ANALYTICS".into(), "true".into()));
        Ok(command)
    }

    pub fn status(&self) -> Result<Value> {
        let settings = self.settings()?;
        let selected = self.selected()?;
        let mut version = selected.as_ref().and_then(|sdk| sdk.version.clone());
        let mut channel = version.as_ref().map(|_| "stable".to_owned());
        if let Some(sdk) = &selected {
            if version.is_none() && standard(&sdk.file) {
                let path = sdk
                    .file
                    .parent()
                    .unwrap()
                    .join("cache/flutter.version.json");
                if let Ok(Some(info)) = files::read_json::<Value>(&path) {
                    version = info["frameworkVersion"]
                        .as_str()
                        .or(info["flutterVersion"].as_str())
                        .map(str::to_owned);
                    channel = info["channel"].as_str().map(str::to_owned);
                }
            }
        }
        let latest = self
            .manifest
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, release)| release.version.clone());
        let available = latest
            .as_ref()
            .map(|latest| version.as_ref() != Some(latest));
        Ok(
            json!({"found":selected.is_some(), "version":version, "channel":channel,
            "mode":selected.map(|sdk| sdk.mode), "latest":latest, "available":available,
            "platform":PLATFORM, "settings":settings}),
        )
    }

    pub fn command(
        &self,
        sdk: &Sdk,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Result<ProcessCommand> {
        let tmp = self.home.join("tmp/flutter");
        fs::create_dir_all(&tmp)?;
        let mut command =
            ProcessCommand::new(&sdk.file, ["--no-version-check", "--suppress-analytics"]);
        command.args.extend(args.into_iter().map(Into::into));
        command.env = [
            "FLUTTER_SUPPRESS_ANALYTICS",
            "DART_SUPPRESS_ANALYTICS",
            "CI",
        ]
        .into_iter()
        .map(|name| (name.into(), "true".into()))
        .collect();
        for name in if cfg!(windows) {
            &["TEMP", "TMP"][..]
        } else {
            &["TMPDIR"][..]
        } {
            command
                .env
                .push(((*name).into(), tmp.clone().into_os_string()));
        }
        if env::var_os("FREEFLOW_FLUTTER_DIRECT").is_some_and(|value| value == "1")
            && standard(&sdk.file)
        {
            let bin = sdk.file.parent().unwrap();
            let snapshot = bin.join("cache/flutter_tools.snapshot");
            let dart = bin.join("cache/dart-sdk/bin").join(if cfg!(windows) {
                "dart.exe"
            } else {
                "dart"
            });
            if snapshot.is_file() && dart.is_file() {
                command.file = dart;
                command.args.insert(0, snapshot.into_os_string());
                command.env.push((
                    "FLUTTER_ROOT".into(),
                    bin.parent().unwrap().as_os_str().into(),
                ));
            }
        }
        Ok(command)
    }

    pub async fn latest(&self, force: bool) -> Result<Release> {
        let _guard = self.refresh.lock().await;
        if !force {
            if let Some((time, release)) = &*self.manifest.lock().unwrap() {
                if time.elapsed() < Duration::from_secs(6 * 3600) {
                    return Ok(release.clone());
                }
            }
        }
        let platform = match PLATFORM {
            "win32" => "windows",
            "darwin" => "macos",
            other => other,
        };
        let arch = match env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => {
                return Err(Problem::new(
                    422,
                    "Managed Flutter is unavailable for this platform.",
                ));
            }
        };
        let unavailable = || {
            Problem::new(
                503,
                "The official Flutter release server is unreachable. Installed SDKs can still be used.",
            )
        };
        let manifest: Value = self
            .client
            .get(format!("{MIRROR}releases/releases_{platform}.json"))
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|_| unavailable())?
            .error_for_status()
            .map_err(|_| unavailable())?
            .json()
            .await
            .map_err(|_| unavailable())?;
        let hash = manifest["current_release"]["stable"]
            .as_str()
            .filter(|hash| !hash.is_empty());
        let missing = || {
            Problem::new(
                422,
                "No verified stable Flutter archive was found for this architecture.",
            )
        };
        let candidates: Vec<_> = manifest["releases"]
            .as_array()
            .ok_or_else(missing)?
            .iter()
            .filter(|item| {
                hash.is_some() && item["hash"].as_str() == hash && item["channel"] == "stable"
            })
            .collect();
        let release = candidates
            .iter()
            .copied()
            .find(|item| item["dart_sdk_arch"] == arch)
            .or_else(|| {
                (arch == "x64")
                    .then(|| {
                        candidates
                            .iter()
                            .copied()
                            .find(|item| item["dart_sdk_arch"].is_null())
                    })
                    .flatten()
            })
            .ok_or_else(missing)?;
        let version = release["version"]
            .as_str()
            .filter(|v| VERSION.is_match(v))
            .ok_or_else(missing)?;
        let sha256 = release["sha256"]
            .as_str()
            .filter(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(missing)?;
        let archive = release["archive"].as_str().ok_or_else(missing)?;
        let base = manifest["base_url"]
            .as_str()
            .ok_or_else(missing)?
            .trim_end_matches('/');
        let unexpected = || Problem::new(422, "Flutter returned an unexpected archive location.");
        let url = reqwest::Url::parse(&format!("{base}/"))
            .and_then(|base| base.join(archive))
            .map_err(|_| unexpected())?;
        if !url
            .as_str()
            .starts_with(&format!("{MIRROR}stable/{platform}/"))
            || !(url.path().ends_with(".zip") || url.path().ends_with(".tar.xz"))
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(unexpected());
        }
        let release = Release {
            version: version.into(),
            sha256: sha256.to_ascii_lowercase(),
            url,
        };
        *self.manifest.lock().unwrap() = Some((Instant::now(), release.clone()));
        Ok(release)
    }

    pub async fn install(
        &self,
        release: &Release,
        cancel: &CancellationToken,
        log: impl Fn(String),
    ) -> Result<Value> {
        let sdk_home = self.home.join("sdk");
        let destination = sdk_home.join(&release.version);
        if locate(&destination.join("flutter")).is_none() {
            let tar = processes::executable(if cfg!(windows) { "tar.exe" } else { "tar" })
                .ok_or_else(|| {
                    Problem::new(
                        409,
                        "Install tar so Freeflow can extract the official Flutter archive.",
                    )
                })?;
            fs::create_dir_all(&sdk_home)?;
            let stage = tempfile::Builder::new()
                .prefix("download-")
                .tempdir_in(&sdk_home)?;
            let archive = stage.path().join(if release.url.path().ends_with(".zip") {
                "flutter.zip"
            } else {
                "flutter.tar.xz"
            });
            let unpacked = stage.path().join("unpacked");
            fs::create_dir(&unpacked)?;
            log(format!("Downloading Flutter {} stable.", release.version));
            let download = async {
                let mut response = self
                    .client
                    .get(release.url.clone())
                    .timeout(Duration::from_secs(35 * 60))
                    .send()
                    .await
                    .map_err(Problem::internal)?
                    .error_for_status()
                    .map_err(Problem::internal)?;
                let length = response.content_length().filter(|length| *length > 0);
                let mut file = tokio::fs::File::create(&archive).await?;
                let mut hash = Sha256::new();
                let (mut bytes, mut progress) = (0_u64, 0_u64);
                while let Some(chunk) = response.chunk().await.map_err(Problem::internal)? {
                    bytes += chunk.len() as u64;
                    if let Some(length) = length {
                        let percent = bytes.saturating_mul(100) / length;
                        if percent >= progress + 10 {
                            progress = percent;
                            log(format!("Download {percent}%"));
                        }
                    }
                    hash.update(&chunk);
                    file.write_all(&chunk).await?;
                }
                file.sync_all().await?;
                if hex::encode(hash.finalize()) != release.sha256 {
                    return Err(Problem::new(
                        422,
                        "Flutter archive failed SHA-256 verification.",
                    ));
                }
                Ok(())
            };
            tokio::select! {
                result = download => result?,
                _ = cancel.cancelled() => return Err(Problem::new(409, "Command stopped.")),
            }
            log("Extracting the verified Flutter archive.".into());
            let command = ProcessCommand::new(
                tar,
                [
                    OsString::from("-xf"),
                    archive.into_os_string(),
                    "-C".into(),
                    unpacked.as_os_str().into(),
                ],
            );
            let status = processes::run(&command, None, cancel, &log).await?;
            if !status.success() || locate(&unpacked.join("flutter")).is_none() {
                return Err(Problem::new(
                    422,
                    "The verified archive could not be extracted as a Flutter SDK.",
                ));
            }
            fs::rename(unpacked, destination)?;
        }
        if cancel.is_cancelled() {
            return Err(Problem::new(409, "Command stopped."));
        }
        files::write_json(
            &sdk_home.join("active.json"),
            &json!({"version": release.version}),
        )?;
        log(format!("Flutter {} stable is ready.", release.version));
        Ok(json!({"version":release.version}))
    }

    pub async fn diagnostics(&self) -> Result<Value> {
        let command = self.command(&self.require()?, ["doctor", "-v"])?;
        let cancel = CancellationToken::new();
        let timer = cancel.clone();
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(45)).await;
            timer.cancel();
        });
        let mut lines = Vec::new();
        let result = processes::run(&command, None, &cancel, |line| lines.push(line)).await;
        watchdog.abort();
        let code = match result {
            Ok(status) => status.code(),
            Err(_) if cancel.is_cancelled() => None,
            Err(error) => return Err(error),
        };
        Ok(json!({"code":code,"signal":null,"output":lines.join("\n")}))
    }
}

fn standard(file: &Path) -> bool {
    file.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(LAUNCHER))
        && file
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
}
