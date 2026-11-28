use crate::{
    files::{self, Problem, Result, Source},
    flutter::Sdk,
    jobs::Job,
    mcp::revision,
    processes::ProcessCommand,
    projects::{FileEdit, Project, Projects},
};
use serde_json::{Value, json};
use std::{ffi::OsString, fs};
use yaml_edit::{Mapping, YamlFile};

const CONFIG: &str = "flutter_launcher_icons.yaml";
const PACKAGE: &str = "flutter_launcher_icons";

fn optional(projects: &Projects, project: &Project, path: &str) -> Result<Option<Source>> {
    match projects.file(project, path) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.status.as_u16() == 404 => Ok(None),
        Err(error) => Err(error),
    }
}

fn yaml(text: &str, name: &str) -> Result<(YamlFile, Mapping)> {
    let yaml: YamlFile = text
        .parse()
        .map_err(|_| Problem::new(422, format!("Fix the YAML in {name}.")))?;
    let documents: Vec<_> = yaml.documents().collect();
    let root = if documents.len() == 1 {
        documents[0].as_mapping()
    } else {
        None
    }
    .ok_or_else(|| Problem::new(422, format!("{name} must contain one YAML mapping.")))?;
    Ok((yaml, root))
}

fn scalar(config: &Mapping, key: &str) -> Option<String> {
    config.get(key)?.as_scalar().map(|value| value.as_string())
}

fn enabled(config: &Mapping, platform: &str) -> bool {
    scalar(config, platform).is_some_and(|value| value != "false" && !value.is_empty())
}

fn configured(projects: &Projects, project: &Project) -> Result<Option<(String, Mapping)>> {
    if let Some(source) = optional(projects, project, CONFIG)? {
        let (_, root) = yaml(&source.content, CONFIG)?;
        return Ok(root.get_mapping(PACKAGE).map(|map| (CONFIG.into(), map)));
    }
    let Some(manifest) = optional(projects, project, "pubspec.yaml")? else {
        return Ok(None);
    };
    // Existing projects without launcher-icons configuration keep their
    // native build behavior even when they use unrelated YAML features.
    if !manifest.content.contains(PACKAGE) {
        return Ok(None);
    }
    let (_, root) = yaml(&manifest.content, "pubspec.yaml")?;
    Ok(root
        .get_mapping(PACKAGE)
        .map(|map| ("pubspec.yaml".into(), map)))
}

pub fn state(projects: &Projects, project: &Project) -> Result<Value> {
    let file = optional(projects, project, CONFIG)?;
    let manifest = projects.file(project, "pubspec.yaml")?;
    let assets = projects.asset_context(project, &json!({"category":"images","limit":200}))?;
    let images: Vec<_> = assets["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["path"].as_str())
        .filter(|path| path.to_ascii_lowercase().ends_with(".png"))
        .collect();
    let mut values = json!({});
    if let Some((_, config)) = configured(projects, project)? {
        for (platform, key) in [("android", "image_path_android"), ("ios", "image_path_ios")] {
            if enabled(&config, platform) {
                values[platform] =
                    json!(scalar(&config, key).or_else(|| scalar(&config, "image_path")));
            }
        }
        values["foreground"] = json!(scalar(&config, "adaptive_icon_foreground"));
        values["background"] = json!(scalar(&config, "adaptive_icon_background"));
    }
    Ok(
        json!({"values":values,"images":images,"manifestRevision":manifest.revision,
        "revision":file.and_then(|source|source.revision),
        "android":project.root.join("android/app/src/main").is_dir(),
        "ios":project.root.join("ios/Runner").is_dir()}),
    )
}

fn png(project: &Project, path: &str) -> Result<()> {
    let invalid = || {
        Problem::new(
            422,
            "Choose an uploaded square PNG image between 48 and 4096 pixels.",
        )
    };
    if !path.starts_with("assets/") || !path.to_ascii_lowercase().ends_with(".png") {
        return Err(invalid());
    }
    let target = files::project_file(&project.root, path, false)?;
    if fs::metadata(&target)?.len() > crate::projects::assets::MAX_ASSET as u64 {
        return Err(invalid());
    }
    let mut header = [0; 24];
    use std::io::Read;
    fs::File::open(target)?
        .read_exact(&mut header)
        .map_err(|_| invalid())?;
    let size = u32::from_be_bytes(header[16..20].try_into().unwrap());
    if !header.starts_with(b"\x89PNG\r\n\x1a\n")
        || &header[12..16] != b"IHDR"
        || header[16..20] != header[20..24]
        || !(48..=4096).contains(&size)
    {
        return Err(invalid());
    }
    Ok(())
}

fn formatted(yaml: YamlFile, old: &str) -> String {
    let text = yaml.to_string();
    if old.contains("\r\n") {
        text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        text
    }
}

pub fn save(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let manifest = projects.file(project, "pubspec.yaml")?;
    let previous = optional(projects, project, CONFIG)?;
    if manifest.revision.as_deref() != input["manifestRevision"].as_str()
        || previous
            .as_ref()
            .and_then(|source| source.revision.as_deref())
            != revision(input)?
    {
        return Err(Problem::new(
            409,
            "Launcher icon files changed. Open App icons again.",
        ));
    }
    let android = input["android"].as_str().unwrap_or_default();
    let ios = input["ios"].as_str().unwrap_or_default();
    let foreground = input["foreground"].as_str().unwrap_or_default();
    let background = input["background"].as_str().unwrap_or_default();
    if android.is_empty() && ios.is_empty() {
        return Err(Problem::new(422, "Choose an Android or iOS app icon."));
    }
    if (!android.is_empty() && !project.root.join("android/app/src/main").is_dir())
        || (!ios.is_empty() && !project.root.join("ios/Runner").is_dir())
    {
        return Err(Problem::new(
            422,
            "Add the selected native target to this Flutter project first.",
        ));
    }
    for path in [android, ios, foreground]
        .into_iter()
        .filter(|path| !path.is_empty())
    {
        png(project, path)?;
    }
    if !foreground.is_empty() {
        if android.is_empty() || background.is_empty() {
            return Err(Problem::new(
                422,
                "Choose an Android icon and an adaptive background.",
            ));
        }
        if let Some(hex) = background.strip_prefix('#') {
            if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(Problem::new(
                    422,
                    "Enter a background such as #FFFFFF or choose a PNG.",
                ));
            }
        } else {
            png(project, background)?;
        }
    }

    let (manifest_yaml, root) = yaml(&manifest.content, "pubspec.yaml")?;
    let inherited = if previous.is_none() {
        root.get_mapping(PACKAGE)
    } else {
        None
    };
    let initial = previous
        .as_ref()
        .map_or("flutter_launcher_icons: {}\n", |source| {
            source.content.as_str()
        });
    let (config_yaml, config_root) = yaml(initial, CONFIG)?;
    if let Some(inherited) = inherited {
        config_root.set(PACKAGE, inherited);
    } else if config_root.get(PACKAGE).is_none() {
        config_root.set(PACKAGE, Mapping::new());
    }
    let config = config_root
        .get_mapping(PACKAGE)
        .ok_or_else(|| Problem::new(422, "flutter_launcher_icons must be a YAML mapping."))?;
    for (platform, path, key) in [
        ("android", android, "image_path_android"),
        ("ios", ios, "image_path_ios"),
    ] {
        if path.is_empty() {
            config.set(platform, false);
            config.remove(key);
        } else {
            if !enabled(&config, platform) {
                config.set(platform, true);
            }
            config.set(key, path);
        }
    }
    config.set("remove_alpha_ios", true);
    if foreground.is_empty() {
        config.remove("adaptive_icon_foreground");
        config.remove("adaptive_icon_background");
    } else {
        config.set("adaptive_icon_foreground", foreground);
        config.set("adaptive_icon_background", background);
    }
    let config_text = formatted(config_yaml, initial);
    let mut edits = vec![FileEdit {
        path: CONFIG.into(),
        revision: previous.and_then(|source| source.revision),
        content: Some(config_text),
    }];
    if root
        .get_mapping("dependencies")
        .is_none_or(|mapping| mapping.get(PACKAGE).is_none())
    {
        if root.get("dev_dependencies").is_none() {
            root.set("dev_dependencies", Mapping::new());
        }
        let deps = root
            .get_mapping("dev_dependencies")
            .ok_or_else(|| Problem::new(422, "dev_dependencies must be a YAML mapping."))?;
        if deps.get(PACKAGE).is_none() {
            deps.set(PACKAGE, "^0.14.4");
            let text = formatted(manifest_yaml, &manifest.content);
            edits.push(FileEdit {
                path: manifest.path,
                revision: manifest.revision,
                content: Some(text),
            });
        }
    }
    projects.apply_edits(project, &edits)?;
    state(projects, project)
}

pub(crate) async fn generate(
    projects: &Projects,
    project: &Project,
    job: &Job,
    sdk: &Sdk,
    platform: &str,
) -> Result<()> {
    let Some(file) = configured(projects, project)?
        .and_then(|(file, config)| enabled(&config, platform).then_some(file))
    else {
        return Ok(());
    };
    let dart = sdk
        .file
        .parent()
        .ok_or_else(|| Problem::new(422, "Select an installed Flutter SDK."))?
        .join("cache/dart-sdk/bin")
        .join(if cfg!(windows) { "dart.exe" } else { "dart" });
    let args = ["run", PACKAGE, "-f", &file].map(OsString::from);
    job.log("Generating app launcher icons.");
    job.exec(&ProcessCommand::new(dart, args), &project.root)
        .await
}
