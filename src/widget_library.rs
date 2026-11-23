use crate::{
    files::{self, Problem, Result},
    flutter::Flutter,
    jobs::Jobs,
    mcp::{required, revision},
    projects::{self, Project, Projects},
    visual::{
        self,
        library::{Declaration, Parameter, Unit},
    },
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Package {
    root: PathBuf,
    name: String,
    category: &'static str,
    roots: Vec<String>,
}
#[derive(Clone)]
struct LibraryFile {
    path: String,
    file: PathBuf,
    revision: String,
    unit: Unit,
}
#[derive(Clone)]
struct PackageFiles {
    package: Package,
    files: HashMap<String, LibraryFile>,
    truncated: bool,
    public: HashMap<String, Vec<(String, String)>>,
}
struct Cached {
    time: Instant,
    stamp: String,
    package: Arc<PackageFiles>,
}
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Mutex::default);
#[derive(Clone)]
struct Choice {
    value: Value,
    uris: Vec<String>,
    file: PathBuf,
    revision: String,
    local: Option<String>,
}
struct Catalog {
    choices: Vec<Choice>,
    names: HashSet<String>,
    ancestors: HashMap<String, HashSet<String>>,
    truncated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Insert,
    Wrap,
}

impl Operation {
    pub fn from_input(input: &Value) -> Result<Self> {
        match input["operation"].as_str() {
            None if input["operation"].is_null() => Ok(Self::Insert),
            Some("insert") => Ok(Self::Insert),
            Some("wrap") => Ok(Self::Wrap),
            _ => Err(Problem::new(400, "Choose insert or wrap.")),
        }
    }
}

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

fn normalized(root: &Path, relative: &str) -> Option<PathBuf> {
    if files::relative_parts(relative).is_err() {
        return None;
    }
    let root = dunce::canonicalize(root).ok()?;
    let path = dunce::canonicalize(root.join(relative)).ok()?;
    (path.starts_with(&root)
        && path.is_file()
        && path
            .extension()
            .is_some_and(|extension| extension == "dart"))
    .then_some(path)
}

fn export_path(package: &Package, from: &str, uri: &str) -> Option<String> {
    let base = reqwest::Url::from_directory_path(&package.root).ok()?;
    let url = if let Some(relative) = uri.strip_prefix(&format!("package:{}/", package.name)) {
        base.join(relative).ok()?
    } else if uri.contains(':') {
        return None;
    } else {
        reqwest::Url::from_file_path(package.root.join(from))
            .ok()?
            .join(uri)
            .ok()?
    };
    let file = url.to_file_path().ok()?;
    Some(
        file.strip_prefix(&package.root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

fn read_package(package: Package, refresh: bool) -> Arc<PackageFiles> {
    let mut cache = CACHE.lock().unwrap();
    let stamp = package
        .roots
        .iter()
        .map(|path| {
            format!(
                "{path}:{:?}",
                fs::metadata(package.root.join(path))
                    .and_then(|metadata| metadata.modified())
                    .ok()
            )
        })
        .collect::<String>();
    if !refresh {
        if let Some(cached) = cache.get(&package.root) {
            if cached.stamp == stamp && cached.time.elapsed() < Duration::from_secs(5) {
                return cached.package.clone();
            }
        }
    }
    let mut result = PackageFiles {
        package: package.clone(),
        files: HashMap::new(),
        public: HashMap::new(),
        truncated: package.roots.len() >= 128,
    };
    let mut queue = package.roots.clone();
    let mut bytes = 0;
    let (max_files, max_bytes) = match package.category {
        "Framework" => (1200, 32 * 1024 * 1024),
        "Project" => (128, 8 * 1024 * 1024),
        _ => (192, 4 * 1024 * 1024),
    };
    while let Some(path) = queue.pop() {
        if result.files.contains_key(&path) {
            continue;
        }
        if result.files.len() >= max_files || bytes >= max_bytes {
            result.truncated = true;
            break;
        }
        let Some(file) = normalized(&package.root, &path) else {
            continue;
        };
        if fs::metadata(&file).map_or(true, |metadata| {
            metadata.len() > files::MAX_TEXT as u64 || metadata.len() > (max_bytes - bytes) as u64
        }) {
            result.truncated = true;
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            result.truncated = true;
            continue;
        };
        if text.len() > files::MAX_TEXT {
            result.truncated = true;
            continue;
        }
        bytes += text.len();
        let Ok(unit) = visual::library::unit(&text) else {
            result.truncated = true;
            continue;
        };
        for export in &unit.exports {
            let Some(uri) = export_path(&package, &path, &export.uri) else {
                continue;
            };
            if normalized(&package.root, &uri).is_some() {
                queue.push(uri);
            }
        }
        result.files.insert(
            path.clone(),
            LibraryFile {
                path,
                file,
                revision: files::revision(&text),
                unit,
            },
        );
    }
    let (public, truncated) = public_symbols(&result);
    result.public = public;
    result.truncated |= truncated;
    let result = Arc::new(result);
    if cache.len() >= 32 && !cache.contains_key(&package.root) {
        if let Some(key) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.time)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&key);
        }
    }
    cache.insert(
        package.root,
        Cached {
            time: Instant::now(),
            stamp,
            package: result.clone(),
        },
    );
    result
}

fn roots(root: &Path, recursive: bool) -> Vec<String> {
    let mut result: Vec<String> = if recursive {
        files::walk(root, files::visible_name, true, 4500)
            .ok()
            .map(|tree| {
                tree.entries
                    .into_iter()
                    .filter(|entry| entry.kind == "file" && entry.path.ends_with(".dart"))
                    .map(|entry| entry.path)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        fs::read_dir(root)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .filter(|name| name.ends_with(".dart"))
                    .map(str::to_owned)
            })
            .collect()
    };
    result.sort();
    result.truncate(128);
    result
}

fn packages(
    projects: &Projects,
    project: Option<&Project>,
    refresh: bool,
) -> (Vec<Arc<PackageFiles>>, bool) {
    let mut packages = vec![];
    if let Ok(Some(sdk)) = Flutter::selected_at(&projects.home) {
        if let Some(root) = dunce::canonicalize(&sdk.file)
            .ok()
            .and_then(|file| {
                file.parent()?
                    .parent()
                    .map(|root| root.join("packages/flutter/lib"))
            })
            .filter(|root| root.join("src/widgets/framework.dart").is_file())
        {
            packages.push(read_package(
                Package {
                    root,
                    name: "flutter".into(),
                    category: "Framework",
                    roots: vec![
                        "widgets.dart".into(),
                        "material.dart".into(),
                        "cupertino.dart".into(),
                    ],
                },
                refresh,
            ));
        }
    }
    let Some(project) = project else {
        return (packages, false);
    };
    let cache = std::env::var_os("PUB_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(|root| PathBuf::from(root).join("Pub/Cache"))
            } else {
                dirs::home_dir().map(|root| root.join(".pub-cache"))
            }
        })
        .and_then(|path| dunce::canonicalize(path).ok());
    let config_dir = project.root.join(".dart_tool");
    let config_file = config_dir.join("package_config.json");
    let config = fs::symlink_metadata(&config_dir)
        .ok()
        .filter(|metadata| metadata.is_dir() && !files::is_link(metadata))
        .and_then(|_| fs::symlink_metadata(&config_file).ok())
        .filter(|metadata| {
            metadata.is_file()
                && !files::is_link(metadata)
                && metadata.len() <= files::MAX_TEXT as u64
        })
        .and_then(|_| files::read_json::<Value>(&config_file).ok().flatten())
        .unwrap_or_default();
    let base = reqwest::Url::from_directory_path(project.root.join(".dart_tool")).ok();
    let local = dunce::canonicalize(&project.root).ok();
    let entries: Vec<_> = config["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["name"] != "flutter"
                && entry["name"] != projects::package_name(project).unwrap_or_default()
        })
        .collect();
    let truncated = entries.len() > 30;
    for entry in entries.into_iter().take(30) {
        let Some(name) = entry["name"].as_str().filter(|name| {
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) else {
            continue;
        };
        let Some(root) = base
            .as_ref()
            .and_then(|base| base.join(entry["rootUri"].as_str()?).ok())
            .and_then(|mut uri| {
                // package_config rootUri denotes a directory even without `/`.
                let directory = format!("{}/", uri.path().trim_end_matches('/'));
                uri.set_path(&directory);
                uri.join(entry["packageUri"].as_str().unwrap_or("lib/"))
                    .ok()
            })
            .and_then(|uri| uri.to_file_path().ok())
            .and_then(|path| dunce::canonicalize(path).ok())
        else {
            continue;
        };
        if !local.as_ref().is_some_and(|local| root.starts_with(local))
            && !cache.as_ref().is_some_and(|cache| root.starts_with(cache))
        {
            continue;
        }
        let public = roots(&root, false);
        if public.is_empty() {
            continue;
        }
        packages.push(read_package(
            Package {
                root,
                name: name.into(),
                category: "Dependencies",
                roots: public,
            },
            refresh,
        ));
    }
    (packages, truncated)
}

fn ancestry(packages: &[Arc<PackageFiles>]) -> (HashSet<String>, HashMap<String, HashSet<String>>) {
    let declarations: Vec<&Declaration> = packages
        .iter()
        .flat_map(|package| package.files.values())
        .flat_map(|file| file.unit.classes.iter())
        .collect();
    let mut ancestors: HashMap<String, HashSet<String>> = HashMap::new();
    for base in [
        "StatelessWidget",
        "StatefulWidget",
        "ConsumerWidget",
        "ConsumerStatefulWidget",
        "HookWidget",
        "HookConsumerWidget",
        "InheritedWidget",
        "ProxyWidget",
        "RenderObjectWidget",
        "SingleChildRenderObjectWidget",
        "MultiChildRenderObjectWidget",
    ] {
        ancestors
            .entry(base.into())
            .or_default()
            .insert("Widget".into());
    }
    for class in &declarations {
        ancestors
            .entry(class.name.clone())
            .or_default()
            .extend(class.bases.clone());
    }
    for _ in 0..40 {
        let mut changed = false;
        for class in &declarations {
            let inherited: Vec<_> = ancestors
                .get(&class.name)
                .into_iter()
                .flatten()
                .flat_map(|base| ancestors.get(base).into_iter().flatten().cloned())
                .collect();
            let entry = ancestors.entry(class.name.clone()).or_default();
            for base in inherited {
                changed |= entry.insert(base);
            }
        }
        if !changed {
            break;
        }
    }
    let names = declarations
        .iter()
        .filter(|class| {
            class.name == "Widget"
                || ancestors
                    .get(&class.name)
                    .is_some_and(|bases| bases.contains("Widget"))
        })
        .map(|class| class.name.clone())
        .collect();
    (names, ancestors)
}

pub fn external_names(projects: &Projects, project: &Project) -> HashSet<String> {
    ancestry(&packages(projects, Some(project), false).0).0
}

fn public_symbols(package: &PackageFiles) -> (HashMap<String, Vec<(String, String)>>, bool) {
    let mut symbols: HashMap<String, HashSet<(String, String)>> = package
        .files
        .iter()
        .map(|(path, file)| {
            (
                path.clone(),
                file.unit
                    .classes
                    .iter()
                    .filter(|class| !class.name.starts_with('_'))
                    .map(|class| (path.clone(), class.name.clone()))
                    .collect(),
            )
        })
        .collect();
    let mut truncated = false;
    for pass in 0..64 {
        let mut changed = false;
        for (path, file) in &package.files {
            let mut additions = Vec::new();
            for export in &file.unit.exports {
                let relative = export_path(&package.package, path, &export.uri);
                if let Some(values) = relative.as_ref().and_then(|path| symbols.get(path)) {
                    additions.extend(
                        values
                            .iter()
                            .filter(|(_, name)| {
                                (export.show.is_empty() || export.show.contains(name))
                                    && !export.hide.contains(name)
                            })
                            .take(4000)
                            .cloned(),
                    );
                }
            }
            let target = symbols.entry(path.clone()).or_default();
            for value in additions {
                if target.len() >= 4000 {
                    truncated = true;
                    break;
                }
                changed |= target.insert(value);
            }
        }
        if !changed {
            break;
        }
        if pass == 63 {
            truncated = true;
        }
    }
    (
        symbols
            .into_iter()
            .map(|(path, values)| {
                let mut values: Vec<_> = values.into_iter().collect();
                values.sort();
                (path, values)
            })
            .collect(),
        truncated,
    )
}

fn catalog(projects: &Projects, project: &Project, path: &str, refresh: bool) -> Result<Catalog> {
    let (mut packages, dependencies_truncated) = packages(projects, Some(project), refresh);
    let root = project.root.join("lib");
    // Project sources are read afresh so a saved component is immediately visible.
    let own = Package {
        root: root.clone(),
        name: projects::package_name(project).unwrap_or_default(),
        category: "Project",
        roots: roots(&root, true),
    };
    packages.push(read_package(own, true));
    let (names, ancestors) = ancestry(&packages);
    let mut choices: HashMap<String, Choice> = HashMap::new();
    let mut truncated = dependencies_truncated || packages.iter().any(|package| package.truncated);
    for package in packages {
        let mut public: HashMap<(String, String), Vec<String>> = HashMap::new();
        for root in &package.package.roots {
            if package.files.get(root).is_none_or(|file| file.unit.part) {
                continue;
            }
            for (file, name) in package.public.get(root).into_iter().flatten().cloned() {
                public.entry((file, name)).or_default().push(format!(
                    "package:{}/{}",
                    package.package.name,
                    encoded(root)
                ));
            }
        }
        for file in package.files.values() {
            for class in &file.unit.classes {
                if !names.contains(&class.name) {
                    continue;
                }
                let local =
                    (package.package.category == "Project").then(|| format!("lib/{}", file.path));
                let same = local.as_deref() == Some(path);
                let mut uris = public
                    .get(&(file.path.clone(), class.name.clone()))
                    .cloned()
                    .unwrap_or_default();
                uris.sort_by_key(|uri| {
                    (
                        usize::from(!uri.ends_with(&format!("/{}", file.path))),
                        uri.len(),
                    )
                });
                uris.dedup();
                if uris.is_empty() && !same {
                    continue;
                }
                for constructor in &class.constructors {
                    if class.abstract_class && !constructor.factory
                        || !same
                            && (class.name.starts_with('_') || constructor.name.starts_with('_'))
                    {
                        continue;
                    }
                    let name = format!(
                        "{}{}",
                        class.name,
                        if constructor.name.is_empty() {
                            String::new()
                        } else {
                            format!(".{}", constructor.name)
                        }
                    );
                    let key = format!(
                        "{}:{}:{name}:{}",
                        package.package.category, package.package.name, file.path
                    );
                    let id = files::revision(&key);
                    let wrapper_hint =
                        constructor
                            .signature
                            .split_once('(')
                            .is_some_and(|(_, parameters)| {
                                parameters
                                    .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                                    .any(|token| {
                                        matches!(token, "child" | "children" | "Widget")
                                            || names.contains(token)
                                    })
                            });
                    let category = package.package.category;
                    let path = local.clone().unwrap_or_else(|| {
                        format!("package:{}/{}", package.package.name, file.path)
                    });
                    let project_path = file
                        .file
                        .strip_prefix(&project.root)
                        .ok()
                        .map(|path| path.to_string_lossy().replace('\\', "/"));
                    let signature: String = constructor.signature.chars().take(360).collect();
                    choices.insert(
                        key,
                        Choice {
                            value: json!({
                                "id": id, "name": name, "className": class.name,
                                "constructor": constructor.name, "category": category,
                                "package": package.package.name, "path": path,
                                "projectPath": project_path, "line": class.line,
                                "revision": file.revision, "signature": signature,
                                "constant": constructor.constant, "wrapperHint": wrapper_hint
                            }),
                            uris: uris.clone(),
                            file: file.file.clone(),
                            revision: file.revision.clone(),
                            local: local.clone(),
                        },
                    );
                }
            }
        }
    }
    let mut choices: Vec<_> = choices.into_values().collect();
    choices.sort_by_key(|choice| {
        (
            choice.value["category"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            choice.value["name"].as_str().unwrap_or_default().to_owned(),
            choice.value["package"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    });
    if choices.len() > 2000 {
        choices.truncate(2000);
        truncated = true;
    }
    Ok(Catalog {
        choices,
        names,
        ancestors,
        truncated,
    })
}

pub fn list(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let path = required(input, "path")?;
    if !path.ends_with(".dart") {
        return Err(Problem::new(400, "Select a Dart widget file."));
    }
    projects.file(project, path)?;
    let catalog = catalog(projects, project, path, input["refresh"] == true)?;
    let query = input["query"].as_str().unwrap_or_default().to_lowercase();
    let category = input["category"].as_str().unwrap_or_default();
    let mut entries: Vec<_> = catalog
        .choices
        .iter()
        .filter(|choice| {
            (category.is_empty() || choice.value["category"] == category)
                && (input["suggestedWrappers"] != true || choice.value["wrapperHint"] == true)
                && format!(
                    "{} {} {}",
                    choice.value["name"].as_str().unwrap_or_default(),
                    choice.value["package"].as_str().unwrap_or_default(),
                    choice.value["path"].as_str().unwrap_or_default()
                )
                .to_lowercase()
                .contains(&query)
        })
        .map(|choice| choice.value.clone())
        .collect();
    let limit = input["limit"].as_u64().unwrap_or(2000).min(2000) as usize;
    let truncated = catalog.truncated || entries.len() > limit;
    entries.truncate(limit);
    Ok(json!({"entries":entries,"truncated":truncated}))
}

fn source(projects: &Projects, project: &Project, input: &Value) -> Result<files::Source> {
    let path = required(input, "path")?;
    if !path.ends_with(".dart") {
        return Err(Problem::new(400, "Select a Dart widget file."));
    }
    let file = projects.file(project, path)?;
    if file.revision.as_deref() != revision(input)? {
        return Err(Problem::new(
            409,
            "This source changed. Select the widget again.",
        ));
    }
    Ok(file)
}

async fn signature(
    jobs: &Jobs,
    project: &Project,
    path: &str,
    text: &str,
    offset: usize,
) -> Result<Value> {
    let result = jobs
        .analyzers
        .saved_editor(
            jobs,
            project,
            &json!({"action":"signature","path":path,"text":text,"offset":offset}),
        )
        .await?;
    result["signatures"].as_array().and_then(|signatures|signatures.first()).filter(|signature|signature["label"].is_string()).cloned().ok_or_else(||Problem::new(422,"Dart could not resolve this constructor. Refresh Flutter dependencies and try again."))
}

fn target_type(parameter: &Parameter, catalog: &Catalog) -> Option<(String, bool)> {
    let kind = parameter.kind.trim().trim_end_matches('?');
    if kind.contains("Function") {
        return None;
    }
    let (kind, multiple) = if let Some(kind) = kind
        .strip_prefix("List<")
        .or_else(|| kind.strip_prefix("Iterable<"))
    {
        (kind.strip_suffix('>')?, true)
    } else {
        (kind, false)
    };
    let kind = kind
        .trim()
        .trim_end_matches('?')
        .split('<')
        .next()?
        .rsplit('.')
        .next()?;
    (kind == "Widget" || catalog.names.contains(kind)).then(|| (kind.into(), multiple))
}

fn accepts(catalog: &Catalog, class: &str, expected: &str) -> bool {
    expected == "Widget"
        || class == expected
        || catalog
            .ancestors
            .get(class)
            .is_some_and(|types| types.contains(expected))
}

fn wrap_target(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    file: &files::Source,
) -> Result<visual::library::WrapTarget> {
    let scope = visual::Scope::for_source(&jobs.projects, project, &file.path, &file.content);
    visual::library::wrap_target(&file.content, required(input, "id")?, &scope)
}

async fn wrapper_type(
    jobs: &Jobs,
    project: &Project,
    file: &files::Source,
    catalog: &Catalog,
    target: &visual::library::WrapTarget,
) -> Option<String> {
    let parent = target.parent.as_ref()?;
    let scope = visual::Scope::for_source(&jobs.projects, project, &file.path, &file.content);
    let (text, offset) =
        visual::property_completion_in(&file.content, &parent.widget, &scope).ok()?;
    let resolved = signature(jobs, project, &file.path, &text, offset)
        .await
        .ok()?;
    let parameters = visual::library::parameters(&resolved).ok()?;
    let param = if let Some(name) = &parent.name {
        parameters.iter().find(|p| p.named && &p.name == name)
    } else {
        parameters.iter().filter(|p| !p.named).nth(parent.position)
    }?;
    let (kind, multiple) = target_type(param, catalog)?;
    (multiple == parent.multiple).then_some(kind)
}

fn wrapper_slots(parameters: &[Parameter], catalog: &Catalog, child: &str) -> Vec<Value> {
    let mut slots: Vec<_> = parameters
        .iter()
        .filter_map(|parameter| {
            let (expected, multiple) = target_type(parameter, catalog)?;
            accepts(catalog, child, &expected)
                .then(|| json!({"name":parameter.name,"expected":expected,"multiple":multiple}))
        })
        .collect();
    slots.sort_by_key(|slot| match slot["name"].as_str() {
        Some("child") => 0,
        Some("children") => 1,
        _ => 2,
    });
    slots
}

async fn slots(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    file: &files::Source,
    catalog: &Catalog,
) -> Result<Vec<Value>> {
    let scope = visual::Scope::for_source(&jobs.projects, project, &file.path, &file.content);
    let (text, offset) =
        visual::property_completion_in(&file.content, required(input, "id")?, &scope)?;
    let signature = signature(jobs, project, &file.path, &text, offset).await?;
    let mut values = vec![];
    let mut position = 0;
    for parameter in visual::library::parameters(&signature)? {
        let index = (!parameter.named).then_some(position);
        if !parameter.named {
            position += 1;
        }
        let Some((expected, multiple)) = target_type(&parameter, catalog) else {
            continue;
        };
        if visual::library::slot_available(
            &file.content,
            required(input, "id")?,
            &parameter.name,
            multiple,
            index,
            &scope,
        )? {
            values.push(json!({"name":parameter.name,"expected":expected,"multiple":multiple,"position":index}));
        }
    }
    Ok(values)
}

pub async fn targets(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let operation = Operation::from_input(input)?;
    let file = source(&jobs.projects, project, input)?;
    let catalog = catalog(
        &jobs.projects,
        project,
        &file.path,
        input["refresh"] == true,
    )?;
    let (slots, child, expected) = match operation {
        Operation::Insert => (
            slots(jobs, project, input, &file, &catalog).await?,
            None,
            None,
        ),
        Operation::Wrap => {
            let target = wrap_target(jobs, project, input, &file)?;
            let expected = wrapper_type(jobs, project, &file, &catalog, &target).await;
            (vec![], Some(target.kind), expected)
        }
    };
    let entries: Vec<_> = catalog
        .choices
        .iter()
        .filter(|choice| {
            expected.as_ref().is_none_or(|expected| {
                accepts(
                    &catalog,
                    choice.value["className"].as_str().unwrap_or_default(),
                    expected,
                )
            })
        })
        .map(|choice| {
            let mut entry = choice.value.clone();
            let name = entry["className"].as_str().unwrap_or_default();
            let mut types: Vec<_> = catalog
                .ancestors
                .get(name)
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            types.push(name.into());
            types.push("Widget".into());
            types.sort();
            types.dedup();
            entry["types"] = json!(types);
            entry
        })
        .collect();
    source(&jobs.projects, project, input)?;
    Ok(json!({
        "slots":slots, "entries":entries, "truncated":catalog.truncated,
        "childType": child, "wrapperType": expected
    }))
}

fn choice(catalog: &Catalog, input: &Value) -> Result<Choice> {
    let entry = catalog
        .choices
        .iter()
        .find(|choice| input["entryId"] == choice.value["id"])
        .ok_or_else(|| {
            Problem::new(
                409,
                "This widget is no longer in the library. Refresh the widget library.",
            )
        })?;
    if input["entryRevision"] != entry.revision {
        return Err(Problem::new(
            409,
            "This widget definition changed. Refresh the widget library.",
        ));
    }
    current_choice(entry)?;
    Ok(entry.clone())
}

fn current_choice(choice: &Choice) -> Result<()> {
    let text = fs::read_to_string(&choice.file).map_err(|_| {
        Problem::new(
            409,
            "This widget definition was removed. Refresh the widget library.",
        )
    })?;
    if files::revision(text) != choice.revision {
        return Err(Problem::new(
            409,
            "This widget definition changed. Refresh the widget library.",
        ));
    }
    Ok(())
}

fn import_choice(file: &files::Source, choice: &Choice) -> Result<(String, String, usize)> {
    let name = required(&choice.value, "name")?;
    if choice.local.as_deref() == Some(&file.path) {
        return Ok((file.content.clone(), name.into(), 0));
    }
    visual::import_reference(&file.content, &choice.uris, name, "freeflowWidgets")
}

fn default_argument(parameter: &Parameter) -> Value {
    if !parameter.required {
        return json!({"mode":"omit","value":""});
    }
    let kind = parameter.kind.trim().trim_end_matches('?');
    match kind {
        "String" => {
            json!({"mode":"string","value":if parameter.name=="data"||parameter.name=="text"{"New text"}else{"Label"}})
        }
        "bool" => json!({"mode":"bool","value":false}),
        "int" | "double" | "num" => json!({"mode":"number","value":"1"}),
        "Widget"
        | "PreferredSizeWidget"
        | "EdgeInsets"
        | "EdgeInsetsGeometry"
        | "Alignment"
        | "AlignmentGeometry"
        | "BoxConstraints"
        | "Size"
        | "Offset"
        | "Matrix4"
        | "TextStyle"
        | "BorderRadius"
        | "BorderRadiusGeometry"
        | "Color"
        | "Axis"
        | "TextDirection"
        | "WidgetBuilder"
        | "IndexedWidgetBuilder"
        | "NullableIndexedWidgetBuilder"
        | "TransitionBuilder"
        | "VoidCallback" => json!({"mode":"default","value":""}),
        _ if parameter.kind.ends_with('?') => json!({"mode":"dart","value":"null"}),
        _ if kind.starts_with("List<") || kind.starts_with("Iterable<") => {
            json!({"mode":"dart","value":"const []"})
        }
        _ if kind.starts_with("Map<") => json!({"mode":"dart","value":"const {}"}),
        _ => json!({"mode":"dart","value":""}),
    }
}

struct Options {
    file: files::Source,
    choice: Choice,
    parameters: Vec<Parameter>,
    signature: Value,
    catalog: Catalog,
}
async fn options(jobs: &Jobs, project: &Project, input: &Value) -> Result<Options> {
    let file = source(&jobs.projects, project, input)?;
    let catalog = catalog(&jobs.projects, project, &file.path, false)?;
    let choice = choice(&catalog, input)?;
    let (text, callee, _) = import_choice(&file, &choice)?;
    let prefix = format!(
        "{text}\ndynamic _freeflowWidgetProbe{}() => {callee}(",
        uuid::Uuid::new_v4().simple()
    );
    let offset = prefix.encode_utf16().count();
    let text = format!("{prefix});\n");
    let signature = signature(jobs, project, &file.path, &text, offset).await?;
    let parameters = visual::library::parameters(&signature)?;
    source(&jobs.projects, project, input)?;
    Ok(Options {
        file,
        choice,
        parameters,
        signature,
        catalog,
    })
}

pub async fn configure(jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
    let operation = Operation::from_input(input)?;
    let options = options(jobs, project, input).await?;
    let slots = available_slots(jobs, project, input, &options, operation).await?;
    source(&jobs.projects, project, input)?;
    current_choice(&options.choice)?;
    let parameters: Vec<_> = options
        .parameters
        .iter()
        .map(|parameter| {
            let mut value = json!(parameter);
            value["argument"] = default_argument(parameter);
            value["suggestion"] = default_argument(&Parameter {
                required: true,
                ..parameter.clone()
            });
            value
        })
        .collect();
    Ok(json!({
        "entry":options.choice.value, "signature":options.signature["label"],
        "signatureRevision":signature_revision(&options.parameters)?,
        "parameters":parameters, "slots":slots
    }))
}

fn signature_revision(parameters: &[Parameter]) -> Result<String> {
    Ok(files::revision(serde_json::to_vec(parameters)?))
}

async fn available_slots(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    options: &Options,
    operation: Operation,
) -> Result<Vec<Value>> {
    match operation {
        Operation::Insert => slots(jobs, project, input, &options.file, &options.catalog).await,
        Operation::Wrap => {
            let target = wrap_target(jobs, project, input, &options.file)?;
            let slots = wrapper_slots(&options.parameters, &options.catalog, &target.kind);
            if slots.is_empty() {
                return Err(Problem::new(
                    422,
                    format!(
                        "This constructor has no parameter that accepts {}. Choose another wrapper.",
                        target.kind
                    ),
                ));
            }
            Ok(slots)
        }
    }
}

fn default_code(parameter: &Parameter, namespace: &str) -> Option<String> {
    let kind = parameter.kind.trim().trim_end_matches('?');
    let value = match kind {
        "Widget" => {
            if matches!(parameter.name.as_str(), "label" | "title") {
                format!("{namespace}Text('Label')")
            } else {
                format!("{namespace}SizedBox.shrink()")
            }
        }
        "PreferredSizeWidget" => format!(
            "{namespace}PreferredSize(preferredSize: {namespace}Size.zero, child: {namespace}SizedBox.shrink())"
        ),
        "EdgeInsets" | "EdgeInsetsGeometry" => format!("{namespace}EdgeInsets.all(16)"),
        "Alignment" | "AlignmentGeometry" => format!("{namespace}Alignment.center"),
        "BoxConstraints" => format!("{namespace}BoxConstraints()"),
        "Size" => format!("{namespace}Size.fromHeight(56)"),
        "Offset" => format!("{namespace}Offset.zero"),
        "Matrix4" => format!("{namespace}Matrix4.identity()"),
        "TextStyle" => format!("{namespace}TextStyle()"),
        "BorderRadius" | "BorderRadiusGeometry" => format!("{namespace}BorderRadius.zero"),
        "Color" => format!("{namespace}Color(0xff6750a4)"),
        "Axis" => format!("{namespace}Axis.vertical"),
        "TextDirection" => format!("{namespace}TextDirection.ltr"),
        "WidgetBuilder" => format!("(context) => {namespace}SizedBox.shrink()"),
        "IndexedWidgetBuilder" | "NullableIndexedWidgetBuilder" => {
            format!("(context, index) => {namespace}SizedBox.shrink()")
        }
        "TransitionBuilder" => format!("(context, child) => child ?? {namespace}SizedBox.shrink()"),
        "VoidCallback" => "() {}".into(),
        _ => return None,
    };
    Some(value)
}

fn argument(parameter: &Parameter, value: &Value, namespace: &str) -> Result<Option<String>> {
    let mode = value["mode"].as_str().unwrap_or("omit");
    let text = value["value"].as_str().unwrap_or_default().trim();
    if mode == "omit" {
        if parameter.required {
            return Err(Problem::new(422, format!("Enter {}.", parameter.name)));
        }
        return Ok(None);
    }
    let code = match mode {
        "string" => serde_json::to_string(
            value["value"]
                .as_str()
                .ok_or_else(|| Problem::new(400, "Enter a text value."))?,
        )?
        .replace('$', "\\$"),
        "bool" => match value["value"].as_bool().or_else(|| text.parse().ok()) {
            Some(true) => "true".into(),
            Some(false) => "false".into(),
            None => return Err(Problem::new(422, "Choose true or false.")),
        },
        "number" => {
            let number = text
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    Problem::new(422, format!("Enter a number for {}.", parameter.name))
                })?;
            if parameter.kind.trim_end_matches('?') == "int" && number.fract() != 0.0 {
                return Err(Problem::new(422, "Enter a whole number."));
            }
            text.into()
        }
        "default" => default_code(parameter, namespace).ok_or_else(|| {
            Problem::new(
                422,
                format!("Enter a Dart expression for {}.", parameter.name),
            )
        })?,
        "empty" => format!("{namespace}SizedBox.shrink()"),
        "dart" => {
            if text.is_empty() {
                if parameter.required {
                    return Err(Problem::new(
                        422,
                        format!("Enter a Dart expression for {}.", parameter.name),
                    ));
                }
                return Ok(None);
            }
            text.into()
        }
        _ => return Err(Problem::new(400, "Choose an argument value type.")),
    };
    Ok(Some(crate::visual::library::checked_expression(&code)?))
}

struct Prepared {
    file: files::Source,
    choice: Choice,
    content: String,
    selected: String,
}
async fn prepare(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    operation: Operation,
) -> Result<Prepared> {
    let options = options(jobs, project, input).await?;
    if input["signatureRevision"] != signature_revision(&options.parameters)? {
        return Err(Problem::new(
            409,
            "The constructor parameters changed. Choose the widget again.",
        ));
    }
    let available = available_slots(jobs, project, input, &options, operation).await?;
    let slot = required(input, "slot")?;
    let target = available
        .iter()
        .find(|target| target["name"] == slot)
        .ok_or_else(|| {
            Problem::new(
                409,
                "This child parameter is no longer available. Configure the widget again.",
            )
        })?;
    let multiple = target["multiple"] == true;
    if operation == Operation::Insert {
        let class = required(&options.choice.value, "className")?;
        let expected = required(target, "expected")?;
        if !accepts(&options.catalog, class, expected) {
            return Err(Problem::new(
                422,
                format!("This slot requires a {expected}. Choose a compatible widget."),
            ));
        }
    }
    let child = (operation == Operation::Wrap)
        .then(|| format!("_freeflowChild{}", uuid::Uuid::new_v4().simple()));
    let (mut text, callee, mut shift) = import_choice(&options.file, &options.choice)?;
    let arguments = input["arguments"].as_object();
    if child.is_some() && arguments.is_some_and(|arguments| arguments.contains_key(slot)) {
        return Err(Problem::new(
            400,
            "The wrapper's selected child parameter is filled by the original widget.",
        ));
    }
    if arguments.is_some_and(|arguments| {
        arguments.keys().any(|name| {
            !options
                .parameters
                .iter()
                .any(|parameter| parameter.name == *name)
        })
    }) {
        return Err(Problem::new(
            400,
            "Choose argument names from the constructor parameters.",
        ));
    }
    let values: Vec<_> = options
        .parameters
        .iter()
        .map(|parameter| {
            if let Some(child) = child.as_ref().filter(|_| parameter.name == slot) {
                let value = if multiple {
                    format!("[{child}]")
                } else {
                    child.clone()
                };
                json!({"mode":"dart","value":value})
            } else {
                arguments
                    .and_then(|arguments| arguments.get(&parameter.name))
                    .cloned()
                    .unwrap_or_else(|| default_argument(parameter))
            }
        })
        .collect();
    let needs_framework = values
        .iter()
        .zip(&options.parameters)
        .any(|(value, parameter)| {
            value["mode"] == "empty"
                || value["mode"] == "default"
                    && parameter.kind.trim_end_matches('?') != "VoidCallback"
        });
    let namespace = if needs_framework {
        let (next, name, added) = visual::import_reference(
            &text,
            &[
                "package:flutter/widgets.dart".into(),
                "package:flutter/material.dart".into(),
                "package:flutter/cupertino.dart".into(),
            ],
            "SizedBox",
            "freeflowFramework",
        )?;
        text = next;
        shift += added;
        name.strip_suffix("SizedBox").unwrap_or_default().to_owned()
    } else {
        String::new()
    };
    let mut args = vec![];
    let mut positional_gap = false;
    for (parameter, value) in options.parameters.iter().zip(&values) {
        match argument(parameter, value, &namespace)? {
            Some(value) => {
                if !parameter.named && positional_gap {
                    return Err(Problem::new(
                        422,
                        "Supply the earlier positional arguments first.",
                    ));
                }
                args.push(if parameter.named {
                    format!("{}: {value}", parameter.name)
                } else {
                    value
                });
            }
            None if !parameter.named => positional_gap = true,
            _ => {}
        }
    }
    let expression = format!("{callee}({})", args.join(", "));
    let id = required(input, "id")?
        .parse::<u64>()
        .map_err(|_| Problem::new(400, "Select a source widget."))?
        .checked_add(shift as u64)
        .ok_or_else(|| Problem::new(400, "Select a source widget."))?
        .to_string();
    let scope = visual::Scope::for_source(&jobs.projects, project, &options.file.path, &text);
    let (content, selected) = if let Some(child) = child {
        visual::library::wrap(&text, &id, &expression, &child, &scope)?
    } else {
        visual::library::insert(
            &text,
            &id,
            slot,
            multiple,
            target["position"].as_u64().map(|value| value as usize),
            &expression,
            &scope,
        )?
    };
    Ok(Prepared {
        file: options.file,
        choice: options.choice,
        content,
        selected,
    })
}

pub async fn apply(
    jobs: &Jobs,
    project: &Project,
    input: &Value,
    operation: Operation,
    save: bool,
) -> Result<Value> {
    if !input["operation"].is_null() && Operation::from_input(input)? != operation {
        return Err(Problem::new(
            400,
            "The selected operation does not match this action.",
        ));
    }
    let prepared = prepare(jobs, project, input, operation).await?;
    let diagnostics = jobs.analyzers.saved_editor(
        jobs,
        project,
        &json!({"action":"diagnose","path":prepared.file.path,"text":prepared.content,"offset":0}),
    ).await?;
    if !save {
        source(&jobs.projects, project, input)?;
        current_choice(&prepared.choice)?;
        return Ok(diagnostics);
    }
    let errors: Vec<_> = diagnostics["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic["severity"] == 1)
        .take(3)
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .collect();
    if !errors.is_empty() {
        return Err(Problem::new(
            422,
            format!("Dart found a problem. {}", errors.join(" ")),
        ));
    }
    current_choice(&prepared.choice)?;
    let file = jobs.projects.update(
        project,
        &prepared.file.path,
        prepared.file.revision.as_deref(),
        |_| {
            current_choice(&prepared.choice)?;
            Ok(prepared.content.clone())
        },
    )?;
    Ok(json!({"source":file,"widget":prepared.selected}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_resolve_cycles_filters_parts_and_encoded_paths_without_escape() {
        let home = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(home.path()).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(
            root.join("widgets.dart"),
            "export 'cycle.dart'; export 'src/tile%20%23.dart' show Tile; part 'src/piece.dart';",
        )
        .unwrap();
        fs::write(
            root.join("cycle.dart"),
            "export 'widgets.dart' hide Tile; class Visible extends Widget {} ",
        )
        .unwrap();
        fs::write(root.join("src/tile #.dart"), "class Tile extends StatelessWidget { Tile.named(); } class Hidden extends Widget {} abstract class State<T extends StatefulWidget> {} class PageState extends State<Tile> {}").unwrap();
        fs::write(
            root.join("src/piece.dart"),
            "part of '../widgets.dart'; class Piece extends Widget {}",
        )
        .unwrap();
        let package = Package {
            root: root.clone(),
            name: "sample".into(),
            category: "Dependencies",
            roots: vec!["widgets.dart".into()],
        };
        let files = read_package(package.clone(), true);
        assert!(!files.truncated);
        let symbols: HashSet<_> = files.public["widgets.dart"]
            .iter()
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(symbols, HashSet::from(["Tile", "Visible", "Piece"]));
        assert!(
            !files.public["cycle.dart"]
                .iter()
                .any(|(_, name)| name == "Tile")
        );
        let (names, _) = ancestry(&[files]);
        assert!(names.contains("Tile"));
        assert!(!names.contains("State") && !names.contains("PageState"));
        assert_eq!(
            export_path(
                &package,
                "widgets.dart",
                "package:sample/src/tile%20%23.dart"
            ),
            Some("src/tile #.dart".into())
        );
        assert!(export_path(&package, "widgets.dart", "../outside.dart").is_none());
        assert!(normalized(&root, "../outside.dart").is_none());
    }

    #[test]
    fn arguments_escape_interpolation_and_reject_invalid_values() {
        let param = Parameter {
            name: "label".into(),
            kind: "String".into(),
            named: true,
            required: true,
        };
        assert_eq!(
            argument(
                &param,
                &json!({"mode":"string","value":"$name\n\"quoted\""}),
                ""
            )
            .unwrap()
            .unwrap(),
            "\"\\$name\\n\\\"quoted\\\"\""
        );
        assert!(argument(&param, &json!({"mode":"omit"}), "").is_err());
        assert!(
            argument(
                &param,
                &json!({"mode":"dart","value":"Text('bad'); print('no')"}),
                ""
            )
            .is_err()
        );
        let param = Parameter {
            kind: "int".into(),
            ..param
        };
        assert!(argument(&param, &json!({"mode":"number","value":"2.5"}), "").is_err());
        assert!(argument(&param, &json!({"mode":"number","value":"NaN"}), "").is_err());
    }
}
