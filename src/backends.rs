use crate::{
    files::{self, Problem, Result, Source},
    projects::{Project, Projects},
};
use base64::Engine as _;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

const FILE: &str = "lib/freeflow_backends.dart";
const STUB: &str = include_str!("../templates/freeflow_backends.dart");
const FIREBASE_OPTIONS: &str = include_str!("../templates/firebase-options.dart");
pub const STARTUP: &str = "import 'freeflow_backends.dart';\n\nFuture<void> main() async {\n  WidgetsFlutterBinding.ensureInitialized();\n  await FreeflowBackends.initialize();\n  runApp(const MyApp());\n}";

const FIREBASE: &[(&str, &str)] = &[
    ("apiKey", "FIREBASE_API_KEY"),
    ("appId", "FIREBASE_APP_ID"),
    ("projectId", "FIREBASE_PROJECT_ID"),
    ("messagingSenderId", "FIREBASE_SENDER_ID"),
    ("authDomain", "FIREBASE_AUTH_DOMAIN"),
    ("storageBucket", "FIREBASE_STORAGE_BUCKET"),
    ("databaseURL", "FIREBASE_DATABASE_URL"),
    ("measurementId", "FIREBASE_MEASUREMENT_ID"),
    ("androidAppId", "FIREBASE_ANDROID_APP_ID"),
    ("androidApiKey", "FIREBASE_ANDROID_API_KEY"),
    ("iosAppId", "FIREBASE_IOS_APP_ID"),
    ("iosApiKey", "FIREBASE_IOS_API_KEY"),
    ("iosBundleId", "FIREBASE_IOS_BUNDLE_ID"),
    ("iosClientId", "FIREBASE_IOS_CLIENT_ID"),
];
const SUPABASE: &[(&str, &str)] = &[("url", "SUPABASE_URL"), ("key", "SUPABASE_PUBLISHABLE_KEY")];
static PROJECT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]{2,62}$").unwrap());
static API_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]{20,}$").unwrap());
static APP_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^1:[0-9]+:(web|android|ios):[0-9a-z]+$").unwrap());
static PUBLIC_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^sb_publishable_[a-zA-Z0-9_-]+$").unwrap());
static JWT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+$").unwrap());

type Defines = BTreeMap<String, String>;

fn fields(provider: &str) -> Result<&'static [(&'static str, &'static str)]> {
    match provider {
        "firebase" => Ok(FIREBASE),
        "supabase" => Ok(SUPABASE),
        _ => Err(Problem::new(400, "Choose a backend provider.")),
    }
}

fn clean(value: &Value, label: &str, required: bool) -> Result<String> {
    let value = value.as_str().unwrap_or_default().trim();
    if (value.is_empty() && required)
        || value.encode_utf16().count() > 3000
        || value.chars().any(|c| c <= '\u{1f}')
    {
        return Err(Problem::new(400, format!("Enter a valid {label}.")));
    }
    Ok(value.to_owned())
}

pub fn validate(provider: &str, input: &Value) -> Result<Option<Defines>> {
    let names = fields(provider)?;
    if !input.is_object() {
        return Err(Problem::new(400, "Choose a backend provider."));
    }
    if input["enabled"] == false {
        return Ok(None);
    }
    if provider == "supabase" {
        let value = clean(&input["url"], "Supabase project URL", true)?;
        let error = || {
            Problem::new(
                400,
                "Use the project URL from Supabase Connect, such as https://example.supabase.co.",
            )
        };
        let url = reqwest::Url::parse(&value).map_err(|_| error())?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (local && url.scheme() == "http"))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(error());
        }
        let key = clean(&input["key"], "Supabase publishable key", true)?;
        let legacy_anon = JWT.is_match(&key)
            && key
                .split('.')
                .nth(1)
                .and_then(|body| {
                    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(body)
                        .ok()?;
                    serde_json::from_slice::<Value>(&decoded).ok()
                })
                .is_some_and(|payload| payload["role"] == "anon");
        if !PUBLIC_KEY.is_match(&key) && !legacy_anon {
            return Err(Problem::new(
                400,
                "Use a Supabase publishable key or legacy anon key.",
            ));
        }
        return Ok(Some(BTreeMap::from([
            ("url".into(), url.origin().ascii_serialization()),
            ("key".into(), key),
        ])));
    }

    let mut output = Defines::new();
    for &(name, _) in names {
        output.insert(
            name.into(),
            clean(
                &input[name],
                &format!("Firebase {name}"),
                matches!(name, "apiKey" | "appId" | "projectId" | "messagingSenderId"),
            )?,
        );
    }
    let valid_id = |value: &str, platform: &str| {
        APP_ID.is_match(value)
            && value
                .split(':')
                .nth(2)
                .is_some_and(|part| part.eq_ignore_ascii_case(platform))
    };
    if !PROJECT_ID.is_match(&output["projectId"])
        || !output["messagingSenderId"]
            .bytes()
            .all(|b| b.is_ascii_digit())
        || !valid_id(&output["appId"], "web")
        || !API_KEY.is_match(&output["apiKey"])
    {
        return Err(Problem::new(
            400,
            "Copy the web app configuration from Firebase Project settings.",
        ));
    }
    for platform in ["android", "ios"] {
        let id = &output[&format!("{platform}AppId")];
        if !id.is_empty() && !valid_id(id, platform) {
            return Err(Problem::new(
                400,
                format!("Enter a Firebase {platform} app ID for the matching registered app."),
            ));
        }
        let key = &output[&format!("{platform}ApiKey")];
        if !key.is_empty() && !API_KEY.is_match(key) {
            return Err(Problem::new(
                400,
                format!("Enter a valid Firebase {platform} API key."),
            ));
        }
    }
    Ok(Some(output))
}

fn marker() -> &'static str {
    STUB.lines().next().unwrap()
}

pub fn generate(firebase: bool, supabase: bool) -> String {
    if !firebase && !supabase {
        return STUB.into();
    }
    let mut output = format!("{}\n", marker());
    if firebase {
        output.push_str("import 'package:firebase_core/firebase_core.dart';\nimport 'package:flutter/foundation.dart';\n");
    }
    if supabase {
        output.push_str("import 'package:supabase_flutter/supabase_flutter.dart';\n");
    }
    output.push_str("\nclass FreeflowBackends {\n  static Future<void> initialize() async {\n");
    if firebase {
        output.push_str("    await Firebase.initializeApp(options: _firebaseOptions());\n");
    }
    if supabase {
        output.push_str("    const supabaseUrl = String.fromEnvironment('SUPABASE_URL');\n    const supabaseKey = String.fromEnvironment('SUPABASE_PUBLISHABLE_KEY');\n    if (supabaseUrl.isEmpty || supabaseKey.isEmpty) throw StateError('Add Supabase settings in Freeflow.');\n    await Supabase.initialize(url: supabaseUrl, publishableKey: supabaseKey);\n");
    }
    output.push_str("  }\n");
    if firebase {
        output.push_str(FIREBASE_OPTIONS);
    }
    output.push_str("}\n");
    output
}

fn config_path(home: &Path, project: &Project) -> PathBuf {
    home.join("backends").join(format!("{}.json", project.id))
}

pub fn state(projects: &Projects, project: &Project) -> Result<Value> {
    let path = config_path(&projects.home, project);
    let defines: Defines = files::read_json(&path)?.unwrap_or_default();
    let provider = |fields: &[(&str, &str)]| {
        let mut result = Map::new();
        for &(name, key) in fields {
            result.insert(
                name.into(),
                json!(defines.get(key).map_or("", String::as_str)),
            );
        }
        let enabled = result
            .values()
            .any(|value| value.as_str().is_some_and(|value| !value.is_empty()));
        result.insert("enabled".into(), json!(enabled));
        result
    };
    let wired = projects
        .file(project, "lib/main.dart")
        .is_ok_and(|entry| entry.content.contains("FreeflowBackends.initialize("));
    Ok(
        json!({"firebase":provider(FIREBASE), "supabase":provider(SUPABASE), "wired":wired,
        "definesFile":(!defines.is_empty()).then_some(path), "startupCode":STARTUP}),
    )
}

pub fn options(home: &Path, project: &Project) -> Result<Vec<String>> {
    let file = config_path(home, project);
    let defines: Defines = files::read_json(&file)?.unwrap_or_default();
    Ok(if defines.is_empty() {
        vec![]
    } else {
        vec![format!("--dart-define-from-file={}", file.display())]
    })
}

pub struct Change {
    provider: String,
    previous: Option<Defines>,
    defines: Defines,
    before: Option<Source>,
    enabled: bool,
}

impl Change {
    pub fn prepare(
        projects: &Projects,
        project: &Project,
        provider: &str,
        input: &Value,
    ) -> Result<Self> {
        let config = validate(provider, input)?;
        let before = match projects.file(project, FILE) {
            Ok(before) if before.content.starts_with(marker()) => Some(before),
            Ok(_) => {
                return Err(Problem::new(
                    409,
                    "lib/freeflow_backends.dart is already in use. Rename it or connect the backend in your own code.",
                ));
            }
            Err(error) if error.status.as_u16() == 404 => None,
            Err(error) => return Err(error),
        };
        let previous: Option<Defines> = files::read_json(&config_path(&projects.home, project))?;
        let mut defines = previous.clone().unwrap_or_default();
        for &(name, key) in fields(provider)? {
            defines.remove(key);
            if let Some(value) = config
                .as_ref()
                .and_then(|config| config.get(name))
                .filter(|value| !value.is_empty())
            {
                defines.insert(key.to_owned(), value.clone());
            }
        }
        Ok(Self {
            provider: provider.into(),
            previous,
            defines,
            before,
            enabled: config.is_some(),
        })
    }

    pub fn dependencies(&self) -> &[&str] {
        match (self.enabled, self.provider.as_str()) {
            (true, "firebase") => &["firebase_core", "firebase_auth", "cloud_firestore"],
            (true, "supabase") => &["supabase_flutter"],
            _ => &[],
        }
    }

    pub fn apply(
        self,
        projects: &Projects,
        project: &Project,
        log: impl Fn(String),
    ) -> Result<Value> {
        let path = config_path(&projects.home, project);
        let firebase = self.defines.contains_key("FIREBASE_APP_ID");
        let supabase = self.defines.contains_key("SUPABASE_URL");
        files::write_json(&path, &self.defines)?;
        if let Err(error) = projects.save(
            project,
            FILE,
            generate(firebase, supabase),
            self.before
                .as_ref()
                .and_then(|source| source.revision.as_deref()),
        ) {
            match self.previous {
                Some(previous) => files::write_json(&path, &previous)?,
                None => fs::remove_file(&path)?,
            }
            return Err(error);
        }
        let state = state(projects, project)?;
        let action = if self.enabled {
            "configured"
        } else {
            "disconnected"
        };
        log(format!("{} is {action}.", self.provider));
        if state["wired"] == false && (firebase || supabase) {
            log("Call FreeflowBackends.initialize() before runApp in lib/main.dart.".into());
        }
        Ok(json!({"provider":self.provider, "enabled":self.enabled, "wired":state["wired"]}))
    }
}
