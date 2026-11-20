use crate::{
    files::{Problem, Result},
    jobs::Jobs,
    mcp::{required, revision},
    projects::{self, Project, Projects},
    visual,
};
use serde_json::{Value, json};

pub fn catalog(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let tree = projects.tree(project)?;
    if let Some(path) = input["path"].as_str().filter(|path| !path.is_empty()) {
        crate::files::relative_parts(path)?;
        if !path.starts_with("lib/") || !path.ends_with(".dart") {
            return Err(Problem::new(400, "Choose a Dart file inside lib."));
        }
    }
    let mut stamp = String::new();
    for file in tree
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == "file" && entry.path.starts_with("lib/") && entry.path.ends_with(".dart")
        })
        .take(128)
    {
        if let Ok(metadata) = crate::files::project_file(&project.root, &file.path, false)
            .and_then(|path| std::fs::metadata(path).map_err(Into::into))
        {
            stamp.push_str(&format!(
                "{}:{}:{:?};",
                file.path,
                metadata.len(),
                metadata.modified().ok()
            ));
        }
    }
    let version = crate::files::revision(stamp);
    if input["since"] == version {
        return Ok(json!({"unchanged":true,"version":version}));
    }
    let mut entries = vec![];
    let mut bytes = 0;
    let mut files = 0;
    let mut truncated = tree.truncated;
    let path = input["path"].as_str().unwrap_or_default();
    let kind = input["kind"].as_str().unwrap_or_default();
    let query = input["query"].as_str().unwrap_or_default().to_lowercase();
    for file in tree.entries.iter().filter(|entry| {
        entry.kind == "file"
            && entry.path.starts_with("lib/")
            && entry.path.ends_with(".dart")
            && (path.is_empty() || entry.path == path)
    }) {
        if files >= 128 || bytes >= 8 * 1024 * 1024 {
            truncated = true;
            break;
        }
        let source = match projects.file(project, &file.path) {
            Ok(source) => source,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        bytes += source.content.len();
        files += 1;
        if bytes > 8 * 1024 * 1024 {
            truncated = true;
            break;
        }
        let values = match visual::design_values(&source.content) {
            Ok(values) => values,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        truncated |= values.as_array().unwrap().len() >= 500;
        for mut entry in values.as_array().unwrap().iter().cloned() {
            if !kind.is_empty() && entry["kind"] != kind {
                continue;
            }
            if !format!(
                "{} {}",
                entry["name"].as_str().unwrap_or_default(),
                source.path
            )
            .to_lowercase()
            .contains(&query)
            {
                continue;
            }
            if entries.len() >= 500 {
                truncated = true;
                break;
            }
            entry["path"] = json!(source.path);
            entry["revision"] = json!(source.revision);
            entry["visual"] = json!(
                entry["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field["editable"] == true)
            );
            let value = entry["expression"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(180)
                .collect::<String>();
            entry["expression"] = json!(value);
            entry.as_object_mut().unwrap().remove("fields");
            entries.push(entry);
        }
    }
    Ok(json!({"entries":entries,"truncated":truncated,"scannedFiles":files,"version":version}))
}

pub fn read(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    if !path.ends_with(".dart") {
        return Err(Problem::new(400, "Choose a Dart design value."));
    }
    let source = projects.file(project, path)?;
    if input["revision"].is_string() && source.revision.as_deref() != revision(input)? {
        return Err(Problem::new(
            409,
            "This theme file changed. Refresh the design values.",
        ));
    }
    let mut entry =
        visual::design_reference(&source.content, &json!({"valueId":required(input,"id")?}))?;
    entry["path"] = json!(source.path);
    entry["revision"] = json!(source.revision);
    Ok(entry)
}

pub fn write(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    if !path.ends_with(".dart") {
        return Err(Problem::new(400, "Choose a Dart design value."));
    }
    let source = projects.update(project, path, revision(input)?, |source| {
        visual::edit_design_value(
            &source
                .ok_or_else(|| Problem::new(404, "Theme file not found."))?
                .content,
            input,
        )
    })?;
    Ok(json!(source))
}

pub fn context(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let mut catalog = catalog(projects, project, input)?;
    let entries = catalog["entries"].as_array().unwrap();
    let mut selected = vec![];
    let mut bytes = 0;
    for entry in entries.iter().take(200) {
        bytes += serde_json::to_vec(entry)?.len();
        if bytes > 96 * 1024 {
            break;
        }
        selected.push(entry.clone());
    }
    catalog["truncated"] = json!(catalog["truncated"] == true || selected.len() < entries.len());
    catalog["entries"] = json!(selected);
    Ok(catalog)
}

pub async fn check(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    let source = jobs.projects.file(project, path)?;
    if source.revision.as_deref() != revision(input)? {
        return Err(Problem::new(
            409,
            "This theme file changed. Refresh the design values.",
        ));
    }
    let text = visual::edit_design_value(&source.content, input)?;
    jobs.analyzers
        .editor(
            jobs,
            project,
            &json!({"action":"diagnose","path":path,"text":text,"offset":0}),
        )
        .await
}

fn uris(project: &Project, target: &str, value: &str) -> Vec<String> {
    fn encoded(path: &str) -> String {
        let mut result = String::new();
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                result.push(byte as char);
            } else {
                result.push_str(&format!("%{byte:02X}"));
            }
        }
        result
    }
    let mut result = vec![];
    if let Some(package) = projects::package_name(project) {
        if let Some(path) = value.strip_prefix("lib/") {
            result.push(format!("package:{package}/{}", encoded(path)));
        }
    }
    let from: Vec<_> = target.split('/').collect();
    let to: Vec<_> = value.split('/').collect();
    let common = from[..from.len() - 1]
        .iter()
        .zip(&to)
        .take_while(|(a, b)| a == b)
        .count();
    result.push(format!(
        "{}{}",
        "../".repeat(from.len() - 1 - common),
        encoded(&to[common..].join("/"))
    ));
    result
}

pub fn bind(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    let value_path = required(input, "valuePath")?;
    if !path.ends_with(".dart") || !value_path.ends_with(".dart") {
        return Err(Problem::new(400, "Choose Dart project files."));
    }
    let mut selected = String::new();
    let source = projects.update(project,path,revision(input)?,|source| {
        let source = source.ok_or_else(|| Problem::new(404,"Widget file not found."))?;
        let value_source = projects.file(project,value_path)?;
        if value_source.revision.as_deref()!=input["valueRevision"].as_str() {return Err(Problem::new(409,"This design value changed. Refresh the values before using it."));}
        let value = visual::design_reference(&value_source.content,input)?;
        if value["constant"]!=true {return Err(Problem::new(422,"Choose a const design value for this widget."));}
        if path!=value_path && (value["public"]!=true || value["part"]==true) {return Err(Problem::new(422,"Choose a public value from a Dart library, or a value in this file."));}
        let (text,expression,shift) = if path==value_path {(source.content.clone(),required(&value,"name")?.into(),0)} else {visual::design_import(&source.content,&uris(project,path,value_path),required(&value,"name")?)?};
        let old = required(input,"id")?.parse::<u64>().map_err(|_| Problem::new(400,"Choose a source widget."))?;
        selected=old.checked_add(shift as u64).ok_or_else(|| Problem::new(400,"Choose a source widget."))?.to_string();
        let scope = visual::Scope::for_source(project,path,&text);
        visual::design_binding(&text,&json!({"id":selected,"operation":"expression","name":required(input,"name")?,"expression":expression}),&scope)
    })?;
    Ok(json!({"source":source,"widget":selected}))
}
