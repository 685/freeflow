use crate::{
    files::{Problem, Result, Source},
    jobs::Jobs,
    mcp::{required, revision},
    projects::Project,
    visual,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn unchanged(jobs: &Jobs, project: &Project, file: &Source) -> Result<()> {
    if jobs.projects.file(project, &file.path)?.revision != file.revision {
        return Err(Problem::new(
            409,
            "The source changed. Refresh the component and try again.",
        ));
    }
    Ok(())
}

fn enrich(
    projects: &crate::projects::Projects,
    project: &Project,
    file: &Source,
    mut definition: Value,
) -> Result<Value> {
    let scope = visual::Scope::for_source(projects, project, &file.path, &file.content);
    let outline = visual::outline_in(&file.content, &scope)?;
    definition["widget"] = outline["components"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|owner| owner["name"] == definition["name"] && owner["widget"].is_string())
        .map_or(Value::Null, |owner| owner["widget"].clone());
    definition["path"] = json!(file.path);
    definition["revision"] = json!(file.revision);
    Ok(definition)
}

pub async fn navigate(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    if !path.ends_with(".dart") {
        return Err(Problem::new(400, "Select a Dart component."));
    }
    let source = jobs.projects.file(project, path)?;
    if source.revision.as_deref() != revision(input)? {
        return Err(Problem::new(
            409,
            "The source changed. Refresh the widget before navigating its component.",
        ));
    }
    let action = required(input, "action")?;
    if !matches!(action, "definition" | "references") {
        return Err(Problem::new(
            400,
            "Choose component definition or references.",
        ));
    }
    let scope = visual::Scope::for_source(&jobs.projects, project, path, &source.content);
    let outline = visual::outline_in(&source.content, &scope)?;
    let id = required(input, "id")?;
    let node = outline["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .ok_or_else(|| Problem::new(409, "Select the widget again after the source refreshes."))?;
    let owner = input["target"] == "owner" || node["component"] != true;
    let (name, symbol) = if owner {
        let owner = &node["ownerComponent"];
        (owner["name"].as_str(), owner["symbol"].as_u64())
    } else {
        (
            node["type"]
                .as_str()
                .and_then(|name| name.split('.').next()),
            node["symbol"].as_u64(),
        )
    };
    let (name, symbol) = name
        .zip(symbol)
        .ok_or_else(|| Problem::new(422, "This widget has no project component to navigate."))?;
    let result = jobs
        .analyzers
        .editor(
            jobs,
            project,
            &json!({"action":"definition","path":path,"text":source.content,"offset":symbol}),
        )
        .await?;
    let mut definition = None;
    for location in result["locations"].as_array().into_iter().flatten() {
        let file = jobs.projects.file(project, required(location, "path")?)?;
        if let Some(component) = visual::component_at(
            &file.content,
            location["offset"].as_u64().unwrap_or_default(),
            name,
        )? {
            definition = Some((enrich(&jobs.projects, project, &file, component)?, file));
            break;
        }
    }
    unchanged(jobs, project, &source)?;
    let Some((definition, file)) = definition else {
        return Ok(json!({"name":name,"definition":null,"locations":[],"truncated":false}));
    };
    if action == "definition" {
        unchanged(jobs, project, &file)?;
        return Ok(json!({"name":name,"definition":definition,"locations":[],"truncated":false}));
    }
    let references = jobs
        .analyzers
        .editor(
            jobs,
            project,
            &json!({"action":"references","path":file.path,
        "text":file.content,"offset":definition["symbol"]}),
        )
        .await?;
    let mut files: HashMap<String, (Source, Value)> = HashMap::new();
    let mut seen = HashSet::new();
    let mut omitted_files = HashSet::new();
    let mut source_bytes = 0;
    let mut locations = vec![];
    let mut skipped = 0;
    for location in references["locations"].as_array().into_iter().flatten() {
        let path = required(location, "path")?;
        let offset = location["offset"].as_u64().unwrap_or_default();
        if !seen.insert((path.to_owned(), offset)) {
            continue;
        }
        if omitted_files.contains(path) {
            skipped += 1;
            continue;
        }
        if !files.contains_key(path) {
            let file = jobs.projects.file(project, path)?;
            if source_bytes + file.content.len() > 16 * 1024 * 1024 {
                omitted_files.insert(path.to_owned());
                skipped += 1;
                continue;
            }
            source_bytes += file.content.len();
            let scope = visual::Scope::for_source(&jobs.projects, project, path, &file.content);
            let outline = visual::outline_in(&file.content, &scope)?;
            files.insert(path.into(), (file, outline));
        }
        let (referenced, outline) = &files[path];
        let line = location["line"].as_u64().unwrap_or(1);
        let snippet = referenced
            .content
            .lines()
            .nth(line.saturating_sub(1) as usize)
            .unwrap_or_default();
        let byte = byte_offset(&referenced.content, offset);
        if !byte.is_some_and(|byte| {
            referenced.content[byte..]
                .strip_prefix(name)
                .is_some_and(|rest| {
                    !rest.starts_with(|ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
                })
        }) {
            skipped += 1;
            continue;
        }
        let mut entry = location.clone();
        entry["revision"] = json!(referenced.revision);
        entry["snippet"] = json!(snippet.trim().chars().take(220).collect::<String>());
        entry["widget"] = outline["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|node| node["symbol"] == offset)
            .map_or(Value::Null, |node| node["id"].clone());
        locations.push(entry);
    }
    locations.sort_by(|left, right| {
        left["path"]
            .as_str()
            .cmp(&right["path"].as_str())
            .then_with(|| left["offset"].as_u64().cmp(&right["offset"].as_u64()))
    });
    unchanged(jobs, project, &source)?;
    unchanged(jobs, project, &file)?;
    for (file, _) in files.values() {
        unchanged(jobs, project, file)?;
    }
    Ok(
        json!({"name":name,"definition":definition,"locations":locations,"truncated":references["truncated"] == true || skipped > 0}),
    )
}

fn byte_offset(source: &str, position: u64) -> Option<usize> {
    let mut current = 0;
    for (byte, ch) in source.char_indices() {
        if current == position {
            return Some(byte);
        }
        current += ch.len_utf16() as u64;
        if current > position {
            return None;
        }
    }
    (current == position).then_some(source.len())
}
