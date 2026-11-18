use crate::edits::{ChangedFile, History};
use crate::files::{self, Problem, Result, Source, Tree};
use base64::Engine as _;
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
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
    writes: Mutex<History>,
}

pub struct FileEdit {
    pub path: String,
    pub revision: Option<String>,
    pub content: Option<String>,
}

struct PreparedEdit<'a> {
    path: &'a str,
    target: PathBuf,
    before: Option<Source>,
    after: Option<&'a str>,
    mode: u32,
}

fn file_mode(target: &Path, exists: bool) -> Result<u32> {
    #[cfg(unix)]
    if exists {
        use std::os::unix::fs::PermissionsExt;
        return Ok(fs::metadata(target)?.permissions().mode() & 0o777);
    }
    let _ = (target, exists);
    Ok(0o644)
}

fn validate_text(relative: &str, content: &str) -> Result<()> {
    if content.len() > files::MAX_TEXT || content.contains('\0') {
        return Err(Problem::new(413, "Enter text up to 2 MiB."));
    }
    if !files::is_text(relative.rsplit('/').next().unwrap_or(relative)) {
        return Err(Problem::new(415, "Choose a supported text file type."));
    }
    Ok(())
}

fn replace(target: &Path, content: Option<&str>, mode: u32) -> Result<()> {
    if let Some(content) = content {
        files::atomic_write(target, content, mode)
    } else {
        Ok(fs::remove_file(target)?)
    }
}

static SDK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*sdk:\s*flutter\s*(?:#.*)?$|flutter:\s*\{\s*sdk:\s*flutter\s*\}").unwrap()
});
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^name:[ \t]*['"]?([a-zA-Z_][a-zA-Z0-9_]*)['"]?[ \t]*(?:#.*)?\r?$"#).unwrap()
});
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

pub fn package_name(project: &Project) -> Option<String> {
    let manifest = files::project_file(&project.root, "pubspec.yaml", false).ok()?;
    let manifest = fs::read_to_string(manifest).ok()?;
    Some(NAME.captures(manifest.trim_start_matches('\u{feff}'))?[1].to_owned())
}

impl Projects {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            registry: Mutex::new(()),
            writes: Mutex::new(History::default()),
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
        files::write_json(&self.database(), &records)?;
        self.writes.lock().unwrap().forget(id);
        Ok(())
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
        let mut journal = self.writes.lock().unwrap();
        let target = files::project_file(&project.root, relative, true)?;
        let previous = self.agent_source(project, relative)?;
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
        validate_text(relative, &content)?;
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
        let mode = file_mode(&target, previous.is_some())?;
        if let Err(error) = files::atomic_write(&target, &content, mode) {
            if let Some(path) = history {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        history.into_iter().for_each(prune_history);
        let saved = Source {
            path: relative.into(),
            revision: Some(files::revision(&content)),
            content,
        };
        journal.record(
            &project.id,
            vec![ChangedFile {
                path: relative.into(),
                before: previous.map(|source| source.content),
                after: Some(saved.content.clone()),
                mode,
            }],
        );
        Ok(saved)
    }

    pub fn agent_source(&self, project: &Project, relative: &str) -> Result<Option<Source>> {
        let target = files::project_file(&project.root, relative, true)?;
        validate_text(relative, "")?;
        if !files::exists(&target)? {
            return Ok(None);
        }
        let source = files::read_source(&target, relative)?;
        if fs::read(&target)? != source.content.as_bytes() {
            return Err(Problem::new(422, "Choose a UTF-8 source file."));
        }
        Ok(Some(source))
    }

    pub fn apply_edits(&self, project: &Project, edits: &[FileEdit]) -> Result<()> {
        let mut journal = self.writes.lock().unwrap();
        let changes = self.apply_edits_inner(project, edits, &[])?;
        journal.record(&project.id, changes);
        Ok(())
    }

    fn apply_edits_inner(
        &self,
        project: &Project,
        edits: &[FileEdit],
        restore: &[ChangedFile],
    ) -> Result<Vec<ChangedFile>> {
        let mut seen = HashSet::new();
        let mut pending = Vec::with_capacity(edits.len());
        for edit in edits {
            let key = if cfg!(windows) {
                edit.path.to_lowercase()
            } else {
                edit.path.clone()
            };
            if !seen.insert(key) {
                return Err(Problem::new(400, "A file appears twice in this change."));
            }
            let target = files::project_file(&project.root, &edit.path, true)?;
            let before = self.agent_source(project, &edit.path)?;
            if before
                .as_ref()
                .and_then(|source| source.revision.as_deref())
                != edit.revision.as_deref()
            {
                return Err(Problem::new(
                    409,
                    format!("{} changed on disk. Reload and retry.", edit.path),
                ));
            }
            let after = edit.content.as_deref();
            if let Some(text) = after {
                validate_text(&edit.path, text)?;
            }
            if before.as_ref().map(|source| source.content.as_str()) != after {
                let mode = if before.is_none() {
                    restore
                        .iter()
                        .find(|file| file.path == edit.path)
                        .map_or(0o644, |file| file.mode)
                } else {
                    file_mode(&target, true)?
                };
                pending.push(PreparedEdit {
                    path: &edit.path,
                    target,
                    before,
                    after,
                    mode,
                });
            }
        }
        let history = pending
            .iter()
            .filter_map(|edit| edit.before.as_ref())
            .map(|source| self.remember(project, &source.path, source))
            .collect::<Result<Vec<_>>>()?;
        for (index, edit) in pending.iter().enumerate() {
            if let Err(error) = replace(&edit.target, edit.after, edit.mode) {
                let mut restored = true;
                for edit in pending[..index].iter().rev() {
                    let original = edit.before.as_ref().map(|source| source.content.as_str());
                    restored &= replace(&edit.target, original, edit.mode).is_ok();
                }
                if !restored {
                    return Err(Problem::new(
                        500,
                        "Some files could not be restored after a write failure. Check File History.",
                    ));
                }
                return Err(error);
            }
        }
        history.into_iter().for_each(prune_history);
        Ok(pending
            .into_iter()
            .map(|edit| ChangedFile {
                path: edit.path.into(),
                before: edit.before.map(|source| source.content),
                after: edit.after.map(str::to_owned),
                mode: edit.mode,
            })
            .collect())
    }

    pub fn edit_history(&self, project: &Project) -> Value {
        self.writes.lock().unwrap().status(&project.id)
    }

    pub fn replay_edit(&self, project: &Project, action: &str, revision: &str) -> Result<Value> {
        let mut journal = self.writes.lock().unwrap();
        let change = journal.pending(&project.id, action, revision)?;
        let edits: Vec<_> = change
            .files
            .iter()
            .map(|file| {
                let (expected, content) = if action == "undo" {
                    (&file.after, &file.before)
                } else {
                    (&file.before, &file.after)
                };
                FileEdit {
                    path: file.path.clone(),
                    revision: expected.as_ref().map(files::revision),
                    content: content.clone(),
                }
            })
            .collect();
        self.apply_edits_inner(project, &edits, &change.files)?;
        journal.complete(&project.id, action);
        Ok(
            json!({"history":journal.status(&project.id), "files":edits.iter().map(|edit| json!({"path":edit.path,"exists":edit.content.is_some()})).collect::<Vec<_>>()}),
        )
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

fn prune_history(path: PathBuf) {
    if let Ok(entries) = history_entries(path.parent().expect("history folder")) {
        for old in entries.into_iter().skip(30) {
            let _ = fs::remove_file(old);
        }
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
