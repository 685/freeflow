use crate::files::{self, Problem, Result, Source, Tree};
use base64::Engine as _;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

pub const PLAN: &str = "FREEFLOW_PLAN.md";
const PLAN_BODY: &str = "\n\n## Goal\n\n## Screens\n\n- [ ] Home\n\n## Data and user flows\n";
const DAYMARK: &str = include_str!("../templates/daymark.dart");
const BLANK: &str = include_str!("../templates/blank.dart");
const BACKEND_STUB: &str = include_str!("../templates/freeflow_backends.dart");

pub fn templates() -> Value {
    serde_json::from_str(include_str!("../templates/catalog.json")).expect("valid template catalog")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

pub struct Projects {
    pub home: PathBuf,
    registry: Mutex<()>,
    writes: Mutex<()>,
}

static SDK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*sdk:\s*flutter\s*(?:#.*)?$|flutter:\s*\{\s*sdk:\s*flutter\s*\}").unwrap()
});
static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^name:\s*([a-zA-Z0-9_]+)").unwrap());
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9]+").unwrap());
static ORG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]*(?:\.[a-z][a-z0-9]*)+$").unwrap());
static HISTORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{13}-[0-9a-f]{6}$").unwrap());
static ASSET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+$").unwrap());

#[derive(Debug, Deserialize, Serialize)]
struct HistoryVersion {
    id: String,
    date: String,
    content: String,
    revision: String,
}

impl Projects {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            registry: Mutex::new(()),
            writes: Mutex::new(()),
        }
    }

    fn database(&self) -> PathBuf {
        self.home.join("projects.json")
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        Ok(files::read_json(&self.database())?.unwrap_or_default())
    }

    pub fn get(&self, id: &str) -> Result<Project> {
        let project = self
            .list()?
            .into_iter()
            .find(|project| project.id == id)
            .ok_or_else(|| Problem::new(404, "Project not found."))?;
        if !project.root.join("pubspec.yaml").is_file() {
            return Err(Problem::new(
                410,
                "The Flutter project folder has moved or is unavailable.",
            ));
        }
        Ok(project)
    }

    fn register(&self, mut project: Project) -> Result<Project> {
        let _guard = self.registry.lock().unwrap();
        let mut records = self.list()?;
        if let Some(existing) = records.iter().find(|record| record.root == project.root) {
            return Ok(existing.clone());
        }
        project.created_at = files::now();
        records.insert(0, project.clone());
        files::write_json(&self.database(), &records)?;
        Ok(project)
    }

    pub fn forget(&self, id: &str) -> Result<()> {
        let _guard = self.registry.lock().unwrap();
        let mut records = self.list()?;
        let count = records.len();
        records.retain(|project| project.id != id);
        if count == records.len() {
            return Err(Problem::new(404, "Project not found."));
        }
        files::write_json(&self.database(), &records)
    }

    pub fn import_folder(&self, input: &str) -> Result<Project> {
        let input = Path::new(input.trim());
        if !input.is_absolute() {
            return Err(Problem::new(
                400,
                "Enter the absolute path to a Flutter project.",
            ));
        }
        let root = dunce::canonicalize(input)
            .map_err(|_| Problem::new(404, "That project folder could not be opened."))?;
        let manifest = files::project_file(&root, "pubspec.yaml", false)
            .and_then(|file| fs::read_to_string(file).map_err(Problem::from))
            .map_err(|_| {
                Problem::new(
                    400,
                    "Choose a Flutter project folder containing pubspec.yaml.",
                )
            })?;
        if !SDK.is_match(&manifest) {
            return Err(Problem::new(
                400,
                "pubspec.yaml must declare a Flutter SDK dependency.",
            ));
        }
        let name = NAME
            .captures(&manifest)
            .map(|cap| cap[1].replace('_', " "))
            .unwrap_or_else(|| {
                root.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        self.register(Project {
            id: files::short_id(8),
            name,
            root,
            template: "imported".into(),
            slug: None,
            organization: None,
            created_at: String::new(),
        })
    }

    pub fn reserve(
        &self,
        name: &str,
        template: &str,
        organization: Option<&str>,
    ) -> Result<Project> {
        let name = name.trim();
        if name.is_empty()
            || name.encode_utf16().count() > 70
            || name.chars().any(|c| c <= '\u{1f}')
        {
            return Err(Problem::new(
                400,
                "Enter an app name of up to 70 characters.",
            ));
        }
        if !matches!(template, "blank" | "daymark") {
            return Err(Problem::new(400, "Select a starter template."));
        }
        let organization = organization
            .filter(|value| !value.is_empty())
            .unwrap_or("com.example");
        if organization.len() > 80 || !ORG.is_match(organization) {
            return Err(Problem::new(
                400,
                "Use a reverse-DNS organization such as com.example.",
            ));
        }
        let lower = name.to_lowercase();
        let normalized = SLUG.replace_all(&lower, "_");
        let stem = normalized
            .trim_matches('_')
            .chars()
            .take(44)
            .collect::<String>();
        let stem = stem.trim_end_matches('_');
        let slug = format!("app_{}", if stem.is_empty() { "starter" } else { stem });
        let id = files::short_id(8);
        let parent = self.home.join("projects");
        let root = parent.join(format!("{slug}-{id}"));
        fs::create_dir_all(&parent)?;
        fs::create_dir(&root)?;
        Ok(Project {
            id,
            name: name.into(),
            root: dunce::canonicalize(root)?,
            template: template.into(),
            slug: Some(slug),
            organization: Some(organization.into()),
            created_at: String::new(),
        })
    }

    pub fn finish_scaffold(&self, project: Project) -> Result<Project> {
        let content = if project.template == "daymark" {
            DAYMARK
        } else {
            BLANK
        };
        files::atomic_write(&project.root.join("lib/main.dart"), content, 0o644)?;
        files::atomic_write(
            &project.root.join("lib/freeflow_backends.dart"),
            BACKEND_STUB,
            0o644,
        )?;
        files::atomic_write(
            &project.root.join(PLAN),
            format!("# {}{PLAN_BODY}", project.name),
            0o644,
        )?;
        let test = project.root.join("test/widget_test.dart");
        if test.is_file() {
            let slug = project.slug.as_deref().expect("scaffolded project slug");
            files::atomic_write(
                &test,
                format!(
                    "import 'package:flutter_test/flutter_test.dart';\nimport 'package:{slug}/main.dart';\n\nvoid main() {{\n  testWidgets('starts the app', (tester) async {{\n    await tester.pumpWidget(const FreeflowApp());\n    expect(find.byType(FreeflowApp), findsOneWidget);\n  }});\n}}\n"
                ),
                0o644,
            )?;
        }
        self.register(project)
    }

    pub fn tree(&self, project: &Project) -> Result<Tree> {
        files::walk(&project.root, files::visible_name, true, 5000)
    }

    pub fn file(&self, project: &Project, relative: &str) -> Result<Source> {
        files::read_source(
            &files::project_file(&project.root, relative, false)?,
            relative,
        )
    }

    pub fn plan(&self, project: &Project) -> Result<Source> {
        match self.file(project, PLAN) {
            Err(error) if error.status.as_u16() == 404 => Ok(Source {
                path: PLAN.into(),
                content: format!("# App plan{PLAN_BODY}"),
                revision: None,
            }),
            result => result,
        }
    }

    pub fn save(
        &self,
        project: &Project,
        relative: &str,
        content: String,
        expected: Option<&str>,
    ) -> Result<Source> {
        self.update(project, relative, expected, |_| Ok(content))
    }

    pub fn update(
        &self,
        project: &Project,
        relative: &str,
        expected: Option<&str>,
        transform: impl FnOnce(Option<&Source>) -> Result<String>,
    ) -> Result<Source> {
        let _guard = self.writes.lock().unwrap();
        let target = files::project_file(&project.root, relative, true)?;
        let previous = files::exists(&target)?
            .then(|| files::read_source(&target, relative))
            .transpose()?;
        if expected
            != previous
                .as_ref()
                .and_then(|source| source.revision.as_deref())
        {
            return Err(Problem::new(
                409,
                "This file changed on disk. Reload it and retry your edit.",
            ));
        }
        let content = transform(previous.as_ref())?;
        if content.len() > files::MAX_TEXT || content.contains('\0') {
            return Err(Problem::new(413, "Enter text up to 2 MiB."));
        }
        if !files::is_text(relative.rsplit('/').next().unwrap_or(relative)) {
            return Err(Problem::new(415, "Choose a supported text file type."));
        }
        if previous
            .as_ref()
            .is_some_and(|source| source.content == content)
        {
            return Ok(previous.unwrap());
        }
        let history = previous
            .as_ref()
            .map(|source| self.remember(project, relative, source))
            .transpose()?;
        #[cfg(unix)]
        let mode = if previous.is_some() {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&target)?.permissions().mode() & 0o777
        } else {
            0o644
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        if let Err(error) = files::atomic_write(&target, &content, mode) {
            if let Some(path) = history {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        if let Some(path) = history {
            if let Ok(entries) = history_entries(path.parent().expect("history folder")) {
                for old in entries.into_iter().skip(30) {
                    let _ = fs::remove_file(old);
                }
            }
        }
        Ok(Source {
            path: relative.into(),
            revision: Some(files::revision(&content)),
            content,
        })
    }

    fn history_dir(&self, project: &Project, relative: &str) -> PathBuf {
        self.home
            .join("history")
            .join(&project.id)
            .join(files::revision(relative.to_lowercase()))
    }

    fn remember(&self, project: &Project, relative: &str, previous: &Source) -> Result<PathBuf> {
        let id = format!("{}-{}", Utc::now().timestamp_millis(), files::short_id(6));
        let file = self
            .history_dir(project, relative)
            .join(format!("{id}.json"));
        files::write_json(
            &file,
            &HistoryVersion {
                id,
                date: files::now(),
                content: previous.content.clone(),
                revision: previous.revision.clone().expect("saved revision"),
            },
        )?;
        Ok(file)
    }

    pub fn history(&self, project: &Project, relative: &str) -> Result<Vec<Value>> {
        files::project_file(&project.root, relative, false)?;
        history_entries(&self.history_dir(project, relative))?
            .into_iter()
            .map(|file| {
                let record: HistoryVersion = files::read_json(&file)?
                    .ok_or_else(|| Problem::new(404, "History entry not found."))?;
                Ok(json!({"id":record.id,"date":record.date,"revision":record.revision}))
            })
            .collect()
    }

    pub fn history_version(&self, project: &Project, relative: &str, id: &str) -> Result<Value> {
        if !HISTORY.is_match(id) {
            return Err(Problem::new(400, "Invalid history entry."));
        }
        files::project_file(&project.root, relative, false)?;
        files::read_json(
            &self
                .history_dir(project, relative)
                .join(format!("{id}.json")),
        )?
        .ok_or_else(|| Problem::new(404, "History entry not found."))
    }

    pub fn asset(&self, project: &Project, name: &str, encoded: &str) -> Result<Value> {
        if !files::is_image(name) || !ASSET.is_match(name) || name.starts_with('.') {
            return Err(Problem::new(
                400,
                "Use an image filename containing letters, numbers, dashes or underscores.",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(encoded))
            .map_err(|_| Problem::new(400, "Invalid image."))?;
        if bytes.is_empty() || bytes.len() > 10 * 1024 * 1024 {
            return Err(Problem::new(
                413,
                "Images must be between 1 byte and 10 MiB.",
            ));
        }
        let relative = format!("assets/{name}");
        let _guard = self.writes.lock().unwrap();
        let path = files::project_file(&project.root, &relative, true)?;
        files::atomic_create(&path, &bytes)?;
        Ok(json!({"path": relative}))
    }
}

fn history_entries(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if files::missing(&error) => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let filename = entry.file_name();
        if filename
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .is_some_and(|id| HISTORY.is_match(id))
            && entry.file_type()?.is_file()
        {
            paths.push(entry.path());
        }
    }
    paths.sort_unstable_by(|a, b| b.cmp(a));
    Ok(paths)
}
