use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, Metadata},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tempfile::NamedTempFile;

pub type Result<T> = std::result::Result<T, Problem>;
pub const MAX_TEXT: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct Problem {
    pub status: StatusCode,
    pub message: String,
}

impl Problem {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("valid HTTP status"),
            message: message.into(),
        }
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        eprintln!("[Freeflow] {error}");
        Self::new(500, "Freeflow could not complete the request.")
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Problem {}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<io::Error> for Problem {
    fn from(error: io::Error) -> Self {
        Self::internal(error)
    }
}

impl From<serde_json::Error> for Problem {
    fn from(error: serde_json::Error) -> Self {
        Self::internal(error)
    }
}

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn revision(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

pub fn short_id(length: usize) -> String {
    uuid::Uuid::new_v4().simple().to_string()[..length].to_owned()
}

pub fn random_token(bytes: usize) -> Result<String> {
    let mut data = vec![0; bytes];
    getrandom::fill(&mut data).map_err(Problem::internal)?;
    Ok(hex::encode(data))
}

pub fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if missing(&error) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(serde_json::from_str(
            content.trim_start_matches('\u{feff}'),
        )?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_temp(path: &Path, content: &[u8], mode: u32) -> Result<NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| Problem::new(400, "Select a file path."))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".freeflow-tmp-")
        .tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

pub fn atomic_write(path: &Path, content: impl AsRef<[u8]>, mode: u32) -> Result<()> {
    write_temp(path, content.as_ref(), mode)?
        .persist(path)
        .map_err(|error| Problem::from(error.error))?;
    Ok(())
}

pub fn atomic_create(path: &Path, content: &[u8]) -> Result<()> {
    atomic_create_mode(path, content, 0o644)
}

pub fn atomic_create_mode(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    write_temp(path, content, mode)?
        .persist_noclobber(path)
        .map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                Problem::new(409, "An asset with that name already exists.")
            } else {
                error.error.into()
            }
        })?;
    Ok(())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_write(path, bytes, 0o600)
}

static PRIVATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)^(?:\.env(?:\.|$)|service[-_]?account|firebase-adminsdk|appstoreconnect|authkey_|credentials\.)|\.(?:p8|p12|jks|keystore|pem|mobileprovision|key|log|apk|aab|ipa)$"
).unwrap()
});
static TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)\.(?:dart|ya?ml|json|md|txt|xml|plist|html|css|scss|js|mjs|ts|swift|java|kt|kts|gradle|rb|sh|properties|xcconfig|pbxproj|entitlements|sql|svg|lock|toml|iml|arb|webmanifest|gitignore|gitattributes)$"
).unwrap()
});
static RESERVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)").unwrap());

pub fn is_text(name: &str) -> bool {
    TEXT.is_match(name)
        || matches!(
            name,
            "Podfile" | "Gemfile" | ".metadata" | ".cursorrules" | "LICENSE" | "README"
        )
}

pub fn visible_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        ".git"
        | ".dart_tool"
        | ".freeflow"
        | ".idea"
        | ".gradle"
        | ".symlinks"
        | ".cxx"
        | "build"
        | "node_modules"
        | "pods"
        | "key.properties"
        | "local.properties"
        | ".flutter-plugins"
        | ".flutter-plugins-dependencies"
        | ".netrc"
        | ".npmrc"
        | ".ssh"
        | ".firebase"
        | ".fvm"
        | "deriveddata"
        | "ephemeral"
        | ".settings"
        | ".vscode" => false,
        ".gitignore" | ".metadata" | ".gitattributes" | ".github" | ".env.example"
        | ".cursorrules" => true,
        _ => !name.starts_with('.') && !PRIVATE.is_match(name),
    }
}

pub fn relative_parts(value: &str) -> Result<Vec<&str>> {
    if value.is_empty() || value.encode_utf16().count() > 700 {
        return Err(Problem::new(400, "Enter a project-relative file path."));
    }
    let parts: Vec<_> = value.split('/').collect();
    if parts.iter().any(|part| {
        part.is_empty()
            || !visible_name(part)
            || part.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
            || part.chars().any(|c| c <= '\u{1f}')
            || part.ends_with(['.', ' '])
            || RESERVED.is_match(part)
    }) {
        return Err(Problem::new(400, "That project path is unavailable."));
    }
    Ok(parts)
}

pub fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

pub fn project_file(root: &Path, relative: &str, create: bool) -> Result<PathBuf> {
    let parts = relative_parts(relative)?;
    let mut target = root.to_owned();
    for (index, part) in parts.iter().enumerate() {
        target.push(part);
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                target.extend(parts[index + 1..].iter().copied());
                return Ok(target);
            }
            Err(error) if missing(&error) => {
                return Err(Problem::new(404, "Project file not found."));
            }
            Err(error) => return Err(error.into()),
        };
        if is_link(&metadata) {
            return Err(Problem::new(
                403,
                "Linked files cannot be edited through Freeflow.",
            ));
        }
        if index + 1 == parts.len() {
            if !metadata.is_file() {
                return Err(Problem::new(400, "Select a project file."));
            }
        } else if !metadata.is_dir() {
            return Err(Problem::new(400, "A path component is not a folder."));
        }
    }
    Ok(target)
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Tree {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub fn walk(
    root: &Path,
    include: fn(&str) -> bool,
    directories: bool,
    limit: usize,
) -> Result<Tree> {
    struct Walker {
        include: fn(&str) -> bool,
        directories: bool,
        limit: usize,
        tree: Tree,
    }
    impl Walker {
        fn visit(&mut self, parent: &Path, prefix: &str, depth: usize) -> Result<()> {
            if depth > 28 {
                self.tree.truncated = true;
                return Ok(());
            }
            let mut children = Vec::new();
            for entry in fs::read_dir(parent)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !(self.include)(&name) {
                    continue;
                }
                let meta = fs::symlink_metadata(entry.path())?;
                if !is_link(&meta) && (meta.is_dir() || meta.is_file()) {
                    children.push((!meta.is_dir(), name));
                }
            }
            children.sort_by_cached_key(|(file, name)| (*file, name.to_lowercase(), name.clone()));
            for (file, name) in children {
                if self.tree.entries.len() >= self.limit {
                    self.tree.truncated = true;
                    break;
                }
                let relative = format!("{prefix}{name}");
                if file || self.directories {
                    self.tree.entries.push(Entry {
                        path: relative.clone(),
                        kind: if file { "file" } else { "directory" },
                    });
                }
                if !file {
                    self.visit(&parent.join(name), &format!("{relative}/"), depth + 1)?;
                }
            }
            Ok(())
        }
    }
    let mut walker = Walker {
        include,
        directories,
        limit,
        tree: Tree {
            entries: vec![],
            truncated: false,
        },
    };
    walker.visit(root, "", 0)?;
    Ok(walker.tree)
}

#[derive(Clone, Debug, Serialize)]
pub struct Source {
    pub path: String,
    pub content: String,
    pub revision: Option<String>,
}

pub fn read_source(file: &Path, relative: &str) -> Result<Source> {
    if !is_text(relative.rsplit('/').next().unwrap_or(relative)) {
        return Err(Problem::new(415, "This file is not editable as text."));
    }
    if fs::metadata(file)?.len() > MAX_TEXT as u64 {
        return Err(Problem::new(
            413,
            "Text files are limited to 2 MiB in the editor.",
        ));
    }
    let bytes = fs::read(file)?;
    if bytes.contains(&0) {
        return Err(Problem::new(415, "This file contains binary data."));
    }
    let content = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Source {
        path: relative.into(),
        revision: Some(revision(&content)),
        content,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Session {
    pub origin: String,
    pub token: String,
    pub pid: u32,
}
