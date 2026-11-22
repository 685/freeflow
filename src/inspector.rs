use crate::{
    files::{self, Problem, Result, Source},
    projects::{self, Project, Projects},
    visual,
};
use serde_json::{Value, json};
use std::{collections::HashMap, fs, path::PathBuf};

const ENTRY: &str = include_str!("../templates/preview.dart.in");

pub struct Entry {
    _folder: tempfile::TempDir,
    pub target: String,
}

pub fn entry(project: &Project) -> Option<Entry> {
    let package = projects::package_name(project)?;
    files::project_file(&project.root, "lib/main.dart", false).ok()?;
    let tool = project.root.join(".dart_tool");
    let metadata = fs::symlink_metadata(&tool).ok()?;
    if !metadata.is_dir() || files::is_link(&metadata) {
        return None;
    }
    let folder = tempfile::Builder::new()
        .prefix("freeflow-preview-")
        .tempdir_in(&tool)
        .ok()?;
    let library = reqwest::Url::from_directory_path(project.root.join("lib")).ok()?;
    fs::write(
        folder.path().join("main.dart"),
        ENTRY.replace("@PACKAGE@", &package).replace(
            "@LIBRARY_URI@",
            &json!(library.as_str()).to_string().replace('$', "\\$"),
        ),
    )
    .ok()?;
    let name = folder.path().file_name()?.to_str()?;
    let target = format!(".dart_tool/{name}/main.dart");
    Some(Entry {
        _folder: folder,
        target,
    })
}

fn local_path(project: &Project, package: &str, uri: &str) -> Option<String> {
    let relative = if let Some(path) = uri.strip_prefix("package:") {
        let (name, rest) = path.split_once('/')?;
        if name != package {
            return None;
        }
        format!("lib/{rest}")
    } else if let Some(rest) = uri.strip_prefix("org-dartlang-app:/lib/") {
        format!("lib/{rest}")
    } else {
        let absolute = if uri.starts_with("file:") {
            reqwest::Url::parse(uri).ok()?.to_file_path().ok()?
        } else {
            PathBuf::from(uri)
        };
        if !absolute.is_absolute() {
            return None;
        }
        let path = dunce::canonicalize(absolute).ok()?;
        path.strip_prefix(&project.root)
            .ok()?
            .to_str()?
            .replace('\\', "/")
    };
    (relative.starts_with("lib/") && relative.ends_with(".dart")).then_some(relative)
}

fn position(source: &str, line: u64, column: u64) -> Option<usize> {
    let mut lines = source.split_inclusive('\n');
    let mut offset = 0;
    for previous in lines
        .by_ref()
        .take(usize::try_from(line.checked_sub(1)?).ok()?)
    {
        offset += previous.encode_utf16().count();
    }
    let text = lines.next()?;
    let column = usize::try_from(column.saturating_sub(1)).ok()?;
    Some(offset + column.min(text.trim_end_matches(['\r', '\n']).encode_utf16().count()))
}

pub fn resolve(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let locations = input["locations"]
        .as_array()
        .filter(|locations| locations.len() <= 24)
        .ok_or_else(|| Problem::new(400, "Choose a widget in the preview."))?;
    let package = projects::package_name(project).unwrap_or_default();
    let mut sources: HashMap<String, Option<(Source, Value)>> = HashMap::new();
    let mut fallback = Value::Null;
    for location in locations {
        let Some(uri) = location["file"].as_str().filter(|uri| uri.len() <= 2048) else {
            continue;
        };
        let Some(path) = local_path(project, &package, uri) else {
            continue;
        };
        let Some(line) = location["line"]
            .as_u64()
            .filter(|line| (1..=250_000).contains(line))
        else {
            continue;
        };
        let column = location["column"].as_u64().unwrap_or(1);
        let kind = location["type"]
            .as_str()
            .and_then(|kind| kind.split('<').next())
            .unwrap_or_default();
        let Some((source, outline)) = sources.entry(path.clone()).or_insert_with(|| {
            let source = projects.file(project, &path).ok()?;
            let scope = visual::Scope::for_source(projects, project, &path, &source.content);
            let outline = visual::outline_in(&source.content, &scope).unwrap_or_default();
            Some((source, outline))
        }) else {
            continue;
        };
        let Some(offset) = position(&source.content, line, column) else {
            continue;
        };
        let node = outline["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| {
                node["line"] == line
                    && node["type"]
                        .as_str()
                        .is_some_and(|name| name.split('.').next() == Some(kind))
            })
            .min_by_key(|node| node["column"].as_u64().unwrap_or_default().abs_diff(column))
            .filter(|node| node["column"].as_u64().unwrap_or_default().abs_diff(column) <= 12);
        let hit = json!({"path":path,"line":line,"position":offset,"revision":source.revision,
            "widget":node.and_then(|node| node["id"].as_str())});
        if node.is_some() {
            return Ok(hit);
        }
        if fallback.is_null() {
            fallback = hit;
        }
    }
    Ok(fallback)
}
