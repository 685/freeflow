use crate::{
    files::{self, Problem, Result},
    projects::{Project, Projects},
};
use regex::Regex;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::LazyLock};

const GRADLE: &str = "android/app/build.gradle.kts";
const DEBUG: &str = r#"signingConfig = signingConfigs.getByName("debug")"#;
const RELEASE: &str = r#"signingConfig = signingConfigs.getByName("release")"#;
const KEY_HEADER: &str = "# Freeflow Android release signing\n";
const HEADER: &str = "val freeflowKeystore = Properties().apply {\n    val propertiesFile = rootProject.file(\"key.properties\")\n    if (propertiesFile.isFile) propertiesFile.inputStream().use { load(it) }\n}\n\n";
const CONFIG: &str = r#"    signingConfigs {
        create("release") {
            storeFile = freeflowKeystore.getProperty("storeFile")?.let { file(it) }
            storePassword = freeflowKeystore.getProperty("storePassword")
            keyAlias = freeflowKeystore.getProperty("keyAlias")
            keyPassword = freeflowKeystore.getProperty("keyPassword")
        }
    }

"#;
static APP_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"applicationId\s*=\s*"([^"]+)""#).unwrap());
static ANDROID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^android \{\r?$").unwrap());
static BUILD_TYPES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^    buildTypes \{\r?$").unwrap());
static SIGNING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*signingConfigs\s*\{").unwrap());

pub fn status(projects: &Projects, project: &Project) -> Value {
    let gradle = projects
        .file(project, GRADLE)
        .map(|source| source.content)
        .unwrap_or_default();
    let managed = gradle.contains("val freeflowKeystore =");
    let configured = managed
        && gradle.contains(RELEASE)
        && fs::symlink_metadata(project.root.join("android/key.properties"))
            .is_ok_and(|metadata| metadata.is_file() && !files::is_link(&metadata));
    json!({"managed":managed, "configured":configured, "compatible":managed || gradle.contains(DEBUG),
        "appId":APP_ID.captures(&gradle).map(|cap| cap[1].to_owned()), "debugFallback":gradle.contains(DEBUG)})
}

pub fn configure(projects: &Projects, project: &Project, input: &Value) -> Result<Value> {
    let store_file = input["storeFile"].as_str().unwrap_or_default();
    let path = Path::new(store_file);
    if !path.is_absolute()
        || !path.extension().is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().to_str(),
                Some("jks" | "keystore" | "p12")
            )
        })
    {
        return Err(Problem::new(
            400,
            "Enter the absolute path to your upload keystore.",
        ));
    }
    if !path.is_file() {
        return Err(Problem::new(400, "The upload keystore was not found."));
    }
    let mut values = KEY_HEADER.to_owned();
    for key in ["storeFile", "keyAlias", "storePassword", "keyPassword"] {
        let value = input[key].as_str().unwrap_or_default();
        if value.is_empty()
            || value.encode_utf16().count() > 1000
            || value.chars().any(|c| c <= '\u{1f}')
        {
            return Err(Problem::new(
                400,
                "Enter the upload key alias and both passwords.",
            ));
        }
        values.push_str(key);
        values.push('=');
        for unit in value.encode_utf16() {
            if matches!(unit, 0..=32 | 92 | 127..=65535) {
                values.push_str(&format!("\\u{unit:04x}"));
            } else {
                values.push(char::from_u32(u32::from(unit)).unwrap());
            }
        }
        values.push('\n');
    }
    let gradle = projects.file(project, GRADLE).map_err(|_| {
        Problem::new(
            422,
            "Configure signing in this project’s Android Gradle file.",
        )
    })?;
    let key_file = project.root.join("android/key.properties");
    let previous = match fs::symlink_metadata(&key_file) {
        Ok(metadata) if metadata.is_file() && !files::is_link(&metadata) => {
            let content = fs::read_to_string(&key_file)?;
            if !content.starts_with(KEY_HEADER) {
                return Err(Problem::new(
                    409,
                    "This project already has its own Android signing settings.",
                ));
            }
            Some(content)
        }
        Ok(_) => {
            return Err(Problem::new(
                409,
                "This project already has its own Android signing settings.",
            ));
        }
        Err(error) if files::missing(&error) => None,
        Err(error) => return Err(error.into()),
    };
    let managed =
        gradle.content.contains("val freeflowKeystore =") && gradle.content.contains(RELEASE);
    if !managed
        && (!gradle.content.contains(DEBUG)
            || !ANDROID.is_match(&gradle.content)
            || !BUILD_TYPES.is_match(&gradle.content)
            || SIGNING.is_match(&gradle.content))
    {
        return Err(Problem::new(
            422,
            "This Gradle layout needs manual Android signing configuration.",
        ));
    }

    let ignore = match projects.file(project, ".gitignore") {
        Ok(ignore) => Some(ignore),
        Err(error) if error.status.as_u16() == 404 => None,
        Err(error) => return Err(error),
    };
    let original = ignore.as_ref().map_or("", |source| &source.content);
    if !original
        .lines()
        .any(|line| line == "/android/key.properties")
    {
        projects.save(
            project,
            ".gitignore",
            format!(
                "{}\n\n/android/key.properties\n*.jks\n*.keystore\n*.p12\n",
                original.trim_end()
            ),
            ignore
                .as_ref()
                .and_then(|source| source.revision.as_deref()),
        )?;
    }
    files::atomic_write(&key_file, values, 0o600)?;
    if !managed {
        let imports = if gradle
            .content
            .lines()
            .any(|line| line.trim() == "import java.util.Properties")
        {
            ""
        } else {
            "import java.util.Properties\n\n"
        };
        let patched = gradle
            .content
            .replacen("android {", &format!("{HEADER}android {{"), 1)
            .replacen("    buildTypes {", &format!("{CONFIG}    buildTypes {{"), 1)
            .replacen(DEBUG, RELEASE, 1);
        if let Err(error) = projects.save(
            project,
            GRADLE,
            format!("{imports}{patched}"),
            gradle.revision.as_deref(),
        ) {
            match previous {
                Some(previous) => files::atomic_write(&key_file, previous, 0o600)?,
                None => fs::remove_file(&key_file)?,
            }
            return Err(error);
        }
    }
    Ok(status(projects, project))
}
