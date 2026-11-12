use crate::{
    activate, current,
    dom::{
        self, Error, Result, api, attr, basename, closest, display, escape as h, hidden, html,
        icon, id, items, option, path_query, post, project_get, project_post, project_route, send,
        string, toast,
    },
    import_project, load_file, new_project, project_id, refresh_state, refresh_tree, select,
    state::{self, Mode},
    track, visual_edit, wait_job,
};
use serde_json::{Map, Value, json};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Element, HtmlAnchorElement, HtmlDialogElement, HtmlElement, HtmlInputElement, KeyboardEvent,
    MouseEvent,
};

const MSG: &str = r#"<div class="form-error" role="alert"></div>"#;
const TARGETS: &[(&str, &str, &str, &str)] = &[
    ("web", "globe", "Web", "Static hosting ZIP"),
    ("android", "android", "Google Play", "Release app bundle"),
    ("ios", "apple", "App Store", "Signed IPA"),
    ("source", "zip", "Flutter source", "Portable project ZIP"),
    ("apk", "phone", "Android device", "Debug APK"),
];

#[derive(Clone, Default)]
pub(crate) enum View {
    #[default]
    Closed,
    Projects,
    Create,
    Import,
    Git {
        project: String,
    },
    GitClone,
    AgentSettings {
        project: String,
    },
    Theme {
        project: String,
        config: Value,
    },
    Ai {
        config: Value,
        client: String,
    },
    Backend {
        project: String,
        provider: String,
        config: Value,
    },
    Export {
        project: String,
        target: String,
        records: Vec<Value>,
        signing: Value,
        blocked: bool,
    },
    Signing {
        project: String,
    },
    Settings {
        config: Value,
    },
    History {
        project: String,
        file: String,
    },
    Version {
        project: String,
        file: String,
        old: Value,
    },
    Wrap {
        project: String,
        widget: Value,
    },
    NewFile {
        project: String,
    },
    Quick,
}

thread_local! { static MODAL: RefCell<(u64, View)> = const { RefCell::new((0, View::Closed)) }; }

fn dialog() -> HtmlDialogElement {
    id("modal").unchecked_into()
}
fn view() -> (u64, View) {
    MODAL.with_borrow(Clone::clone)
}
pub(crate) fn current_view(version: u64) -> bool {
    dialog().open() && MODAL.with_borrow(|view| view.0 == version)
}
fn close() {
    dialog().close();
}
pub(crate) fn close_current(version: u64) {
    if current_view(version) {
        close();
    }
}

pub(crate) fn open(view: View, title: &str, description: &str, body: String) {
    MODAL.with_borrow_mut(|state| {
        state.0 += 1;
        state.1 = view;
    });
    dom::text("modal-title", title);
    dom::text("modal-subtitle", description);
    hidden("modal-subtitle", description.is_empty());
    html("modal-body", body);
    id("modal-body").set_scroll_top(0);
    let dialog = dialog();
    if !dialog.open() {
        let _ = dialog.show_modal();
    }
    if let Some(focus) = dialog
        .query_selector("[autofocus]")
        .ok()
        .flatten()
        .and_then(|node| node.dyn_into::<HtmlElement>().ok())
    {
        let _ = focus.focus();
    }
}

fn button(id: &str, label: &str, symbol: &str, style: &str) -> String {
    let icon = if symbol.is_empty() {
        String::new()
    } else {
        icon(symbol, 16)
    };
    format!(
        r#"<button type="button" class="button {}" id="{}" data-dialog-action="{}">{icon}{}</button>"#,
        h(style),
        h(id),
        h(id),
        h(label)
    )
}

fn submit(label: &str) -> String {
    format!(
        r#"<button type="submit" class="button primary">{}</button>"#,
        h(label)
    )
}

fn field(name: &str, label: &str, value: &str, attrs: &str, help: &str) -> String {
    let help = if help.is_empty() {
        String::new()
    } else {
        format!("<small>{}</small>", h(help))
    };
    format!(
        r#"<label class="form-field"><span>{}</span><input name="{}" value="{}" {attrs}>{help}</label>"#,
        h(label),
        h(name),
        h(value)
    )
}

fn form(content: &str, actions: &str) -> String {
    format!(r#"<form class="form">{content}{MSG}<div class="form-footer">{actions}</div></form>"#)
}

fn callout(content: &str, warning: bool) -> String {
    format!(
        r#"<div class="callout{}">{}<div>{content}</div></div>"#,
        if warning { " warning" } else { "" },
        icon(if warning { "warning" } else { "info" }, 16)
    )
}

fn code(value: &str) -> String {
    format!(r#"<pre class="code-card" tabindex="0">{}</pre>"#, h(value))
}

fn external(url: &str, label: &str) -> String {
    format!(
        r#"<a href="{}" target="_blank" rel="noopener noreferrer">{}</a>"#,
        h(url),
        h(label)
    )
}

fn when(value: &Value) -> String {
    let date = js_sys::Date::new(&JsValue::from_str(string(value)));
    if date.get_time().is_finite() {
        date.to_locale_string("en", &JsValue::UNDEFINED).into()
    } else {
        display(value)
    }
}

fn size(value: &Value) -> String {
    let bytes = value.as_f64().unwrap_or(0.0);
    if bytes >= 1_048_576.0 {
        format!("{:.1} MiB", bytes / 1_048_576.0)
    } else {
        format!("{} KiB", (bytes / 1024.0).round().max(1.0))
    }
}

pub fn projects() {
    let list: String = state::read(|state| {
        state.projects.iter().map(|item| {
        let key = string(&item["id"]);
        let root = h(string(&item["root"]));
        let active = key == state.project_id();
        format!(r#"<div class="dialog-item"><div class="dialog-item-text"><strong>{}</strong><small class="ellipsis mono" title="{root}">{root}</small></div><button type="button" class="icon-button" data-dialog-action="folder-{key}" aria-label="Copy project folder" title="Copy folder">{}</button>{}</div>"#,
            h(string(&item["name"])), icon("copy", 16), button(&format!("project-{key}"), if active { "Active" } else { "Open" }, "",
                if active { "soft small" } else { "outline small" }))
    }).collect()
    });
    let content = if list.is_empty() {
        r#"<p class="dialog-help">Your Flutter projects will appear here.</p>"#
    } else {
        &list
    };
    open(
        View::Projects,
        "Projects",
        "",
        format!(
            r#"<div class="dialog-list">{content}</div><div class="form-footer">{}{}{}</div>"#,
            button("clone-project", "Clone repository", "branch", "outline"),
            button("import-folder", "Import folder", "folder", "outline"),
            button("create-project", "New project", "plus", "primary")
        ),
    );
}

pub fn create() {
    let options: String = state::read(|state| {
        state.templates.iter().enumerate().map(|(index, template)| {
        format!(r#"<label class="choice-card"><input type="radio" name="template" value="{}"{}><span><strong>{}</strong><small>{}</small></span></label>"#,
            h(string(&template["id"])), if index == 0 { " checked" } else { "" },
            h(string(&template["name"])), h(string(&template["description"])))
    }).collect()
    });
    let content = format!(
        r#"{}<div class="form-field"><span>Template</span><div class="choice-grid">{options}</div></div>{}"#,
        field(
            "name",
            "App name",
            "",
            r#"required maxlength="70" placeholder="My app" autofocus"#,
            ""
        ),
        field(
            "organization",
            "Organization ID",
            "com.example",
            "",
            "Use your reverse-DNS domain for new Android and iOS app identifiers."
        )
    );
    open(
        View::Create,
        "New Flutter project",
        "Create a Flutter project on this computer.",
        form(&content, &submit("Create project")),
    );
}

pub fn import() {
    let windows = state::read(|state| state.flutter["platform"] == "win32");
    let example = if windows {
        r"C:\Projects\my_app"
    } else {
        "/home/me/projects/my_app"
    };
    let input = field(
        "folder",
        "Project folder",
        "",
        &format!(r#"required autofocus placeholder="{}""#, h(example)),
        "",
    );
    open(
        View::Import,
        "Import Flutter project",
        "Choose an existing local project with a pubspec.yaml file.",
        form(&input, &submit("Import project")),
    );
}

pub async fn ai() -> Result {
    let query = state::read(|state| {
        if state.project_id().is_empty() {
            String::new()
        } else {
            format!("?project={}", state.project_id())
        }
    });
    render_ai(api(&format!("/ai{query}")).await?, "codex");
    Ok(())
}

fn render_ai(config: Value, client: &str) {
    let clients = [
        ("codex", "Codex"),
        ("claude", "Claude Code"),
        ("desktop", "Claude Desktop"),
    ];
    let tabs: String = clients
        .into_iter()
        .map(|(id, label)| {
            button(
                &format!("client-{id}"),
                label,
                "",
                if id == client { "active" } else { "soft" },
            )
        })
        .collect();
    let note = if client == "desktop" {
        "Merge this entry into your Claude Desktop MCP configuration, then restart Claude Desktop."
    } else if client == "claude" {
        "Run the command in your terminal, then start a new Claude Code session."
    } else {
        "Run the command in your terminal, then start a new Codex session."
    };
    let desc = state::read(|state| {
        state.project["name"].as_str().map_or_else(
            || "MCP access to your Freeflow projects.".into(),
            |name| format!("MCP access to {name}"),
        )
    });
    let snippet = code(string(&config[client]));
    let content = format!(
        r#"<div class="dialog-tabs">{tabs}</div><div class="form"><p class="dialog-help">{note} Keep Freeflow running while your AI client uses it.</p>{snippet}<div class="form-row">{}{}</div><p class="dialog-help">For ChatGPT, use Codex with this local MCP server. {}.</p></div>"#,
        button(
            "copy-config",
            if client == "desktop" {
                "Copy JSON"
            } else {
                "Copy command"
            },
            "copy",
            "outline"
        ),
        button("copy-prompt", "Copy starter prompt", "copy", "soft"),
        external(
            "https://help.openai.com/en/articles/12584461",
            "ChatGPT’s web app uses remote MCP servers"
        )
    );
    open(
        View::Ai {
            config,
            client: client.into(),
        },
        "AI Connect",
        &desc,
        content,
    );
}

async fn copy(value: &str, description: &str) -> Result {
    JsFuture::from(dom::win().navigator().clipboard().write_text(value))
        .await
        .map_err(|_| Error::new("Select and copy the text manually."))?;
    toast(&format!("{description} copied"), false);
    Ok(())
}

pub async fn backends(selected: &str) -> Result {
    let project = project_id()?;
    let config = project_get(&project, "backends").await?;
    if !current(&project) {
        return Ok(());
    }
    let provider = &config[selected];
    let firebase = selected == "firebase";
    let name = if firebase { "Firebase" } else { "Supabase" };
    let enabled = provider["enabled"] == true;
    let tabs = format!(
        r#"<div class="dialog-tabs">{}{}</div>"#,
        button(
            "firebase",
            "Firebase",
            "database",
            if firebase { "active" } else { "soft" }
        ),
        button(
            "supabase",
            "Supabase",
            "database",
            if firebase { "soft" } else { "active" }
        )
    );
    let mut content = if enabled {
        callout(&format!("{name} is configured for this project."), false)
    } else {
        String::new()
    };
    if firebase {
        let core = [
            ("projectId", "Project ID", true),
            ("appId", "Web app ID", true),
            ("apiKey", "Web API key", true),
            ("messagingSenderId", "Sender ID", true),
            ("authDomain", "Auth domain", false),
            ("storageBucket", "Storage bucket", false),
        ];
        let extra = [
            ("androidAppId", "Android app ID", false),
            ("androidApiKey", "Android API key", false),
            ("iosAppId", "iOS app ID", false),
            ("iosApiKey", "iOS API key", false),
            ("iosBundleId", "iOS bundle ID", false),
            ("iosClientId", "iOS OAuth client ID", false),
            ("databaseURL", "Database URL", false),
            ("measurementId", "Measurement ID", false),
        ];
        let fields = |list: &[(&str, &str, bool)]| {
            list.iter()
                .map(|(id, label, required)| {
                    field(
                        id,
                        label,
                        string(&provider[*id]),
                        if *required { "required" } else { "" },
                        "",
                    )
                })
                .collect::<String>()
        };
        content.push_str(&format!(r#"<p class="dialog-help">Register a web app in {} and paste its app settings.</p><div class="form-grid">{}</div><details><summary>Android, iOS and optional fields</summary><div class="form-grid">{}</div></details>"#,
            external("https://console.firebase.google.com/", "Firebase Project settings"), fields(&core), fields(&extra)));
    } else {
        content.push_str(&format!(
            r#"<p class="dialog-help">Find your project URL and publishable key in {}.</p>{}{}"#,
            external("https://supabase.com/dashboard", "Supabase Connect"),
            field(
                "url",
                "Project URL",
                string(&provider["url"]),
                r#"required type="url" placeholder="https://your-project.supabase.co""#,
                ""
            ),
            field(
                "key",
                "Publishable key",
                string(&provider["key"]),
                r#"required type="password" autocomplete="off" placeholder="sb_publishable_…""#,
                ""
            )
        ));
    }
    if config["wired"] != true {
        content.push_str(&format!(r#"<details><summary>Initialize from main.dart</summary><p class="dialog-help">Add the import and initialization to your existing main function before runApp.</p>{}</details>"#, code(string(&config["startupCode"]))));
    }
    if let Some(path) = config["definesFile"].as_str() {
        content.push_str(&format!(r#"<details><summary>Build outside Freeflow</summary><p class="dialog-help">Pass this Dart defines file with --dart-define-from-file when running or building from a terminal.</p>{}<div class="form-row">{}</div></details>"#,
            code(path), button("copy-defines", "Copy path", "copy", "soft")));
    }
    let actions = if enabled {
        button("disconnect", "Disconnect", "x", "danger") + &submit("Save configuration")
    } else {
        submit(&format!("Connect {name}"))
    };
    open(
        View::Backend {
            project,
            provider: selected.into(),
            config,
        },
        "Backends",
        "Connect your Flutter app to a project you own.",
        tabs + &form(&content, &actions),
    );
    Ok(())
}

async fn configure_backend(
    version: u64,
    project: String,
    provider: String,
    config: Value,
) -> Result {
    if !current(&project) {
        return Err(Error::new("Open the project again."));
    }
    let name = if provider == "firebase" {
        "Firebase"
    } else {
        "Supabase"
    };
    let enabled = config["enabled"] != false;
    let job = project_post(
        &project,
        "backends",
        json!({"provider":provider,"config":config}),
    )
    .await?;
    track(&job, true);
    wait_job(job).await?;
    toast(
        &format!(
            "{name} {}",
            if enabled {
                "configured"
            } else {
                "disconnected"
            }
        ),
        false,
    );
    if current_view(version) && current(&project) {
        backends(&provider).await?;
    }
    Ok(())
}

pub async fn exports(selected: &str) -> Result {
    let project = project_id()?;
    let records = project_get(&project, "artifacts").await?;
    let signing = project_get(&project, "signing").await?;
    if !current(&project) {
        return Ok(());
    }
    let (blocked, name) = state::read(|state| {
        (
            selected == "ios" && state.flutter["platform"] != "darwin",
            string(&state.project["name"]).to_owned(),
        )
    });
    let options: String = TARGETS.iter().map(|(target, symbol, label, description)| {
        let active = *target == selected;
        format!(r#"<button class="choice-card{}" type="button" id="target-{target}" data-dialog-action="target-{target}" aria-pressed="{active}">{}<span><strong>{}</strong><small>{}</small></span></button>"#,
            if active { " selected" } else { "" }, icon(symbol, 16), h(label), h(description))
    }).collect();
    let mut content = format!(r#"<div class="choice-grid">{options}</div>"#);
    if selected == "web" {
        content.push_str(&field(
            "baseHref",
            "Web base path",
            "/",
            "",
            "Use / for a domain or /my-app/ for a subfolder.",
        ));
    }
    if matches!(selected, "android" | "apk" | "ios") && !blocked {
        content.push_str(&format!(
            "<details><summary>Build options</summary>{}</details>",
            field(
                "flavor",
                "Flutter flavor",
                "",
                r#"placeholder="Optional""#,
                ""
            )
        ));
    }
    if selected == "android" {
        if signing["debugFallback"] == true {
            content.push_str(&callout(
                "This project uses a debug key. Configure an upload key for Play Store submission.",
                true,
            ));
        } else if signing["configured"] == true {
            let label = signing["appId"]
                .as_str()
                .map_or("Upload signing is configured.".into(), |id| {
                    format!("Upload signing is configured for {}.", h(id))
                });
            content.push_str(&callout(&label, false));
        }
        let configure = if signing["compatible"] == true {
            button("android-signing", "Configure signing", "key", "soft")
        } else {
            String::new()
        };
        content.push_str(&format!(
            r#"<div class="form-row">{configure}{}</div>"#,
            external("https://play.google.com/console/", "Google Play Console")
        ));
    }
    if selected == "ios" {
        let note = if blocked {
            "Build an App Store IPA on macOS with Xcode. Export Flutter source here and open the project on your Mac."
        } else {
            "Select your team and provisioning profile in Xcode before building."
        };
        content.push_str(&callout(note, blocked));
        content.push_str(&format!(
            r#"<p class="dialog-help">{}</p>"#,
            external("https://appstoreconnect.apple.com/", "App Store Connect")
        ));
    }
    if selected == "source" {
        content.push_str(&callout("Includes your Flutter source, assets and platform projects. Backend defines and signing keys stay in their local folders.", false));
    }
    let actions = if blocked {
        button("ios-source", "Switch to source export", "zip", "outline")
    } else {
        submit("Build export")
    };
    let records = items(&records).iter().take(8).cloned().collect::<Vec<_>>();
    let previous: String = records.iter().map(|entry| {
        let name = h(string(&entry["name"]));
        let key = h(string(&entry["id"]));
        let details = h(&format!("{} · {} · {}{}", string(&entry["label"]), size(&entry["size"]), when(&entry["date"]),
            if entry["debugSigned"] == true { " · Debug key" } else { "" }));
        format!(r#"<div class="dialog-item"><div class="dialog-item-text"><strong class="ellipsis">{name}</strong><small>{details}</small></div><button type="button" class="icon-button" id="download-{key}" data-dialog-action="download-{key}" aria-label="Download {name}" title="Download">{} </button></div>"#, icon("download",16))
    }).collect();
    let mut body = form(&content, &actions);
    if !previous.is_empty() {
        body.push_str(&format!(r#"<hr class="dialog-separator"><div class="modal-heading">Recent exports</div><div class="dialog-list">{previous}</div>"#));
    }
    open(
        View::Export {
            project,
            target: selected.into(),
            records,
            signing,
            blocked,
        },
        "Export app",
        &name,
        body,
    );
    Ok(())
}

fn signing(project: String, config: Value) {
    let content = format!(
        r#"{}{}<div class="form-grid">{}{}</div>{}<p class="dialog-help">{}</p>"#,
        field(
            "storeFile",
            "Keystore path",
            "",
            "required autofocus",
            "Absolute path to a .jks, .keystore or .p12 file."
        ),
        field(
            "keyAlias",
            "Key alias",
            "",
            r#"required placeholder="upload""#,
            ""
        ),
        field(
            "storePassword",
            "Keystore password",
            "",
            r#"required type="password" autocomplete="new-password""#,
            ""
        ),
        field(
            "keyPassword",
            "Key password",
            "",
            r#"required type="password" autocomplete="new-password""#,
            ""
        ),
        callout(
            "Freeflow writes the passwords to the local android/key.properties file used by Gradle and adds it to .gitignore.",
            false
        ),
        external(
            "https://docs.flutter.dev/deployment/android#sign-the-app",
            "Create an upload keystore"
        )
    );
    let actions = button("back-exports", "Back", "", "soft") + &submit("Save signing");
    open(
        View::Signing { project },
        "Android upload signing",
        config["appId"]
            .as_str()
            .unwrap_or("Use the upload key for this Google Play app."),
        form(&content, &actions),
    );
}

pub async fn settings() -> Result {
    refresh_state().await?;
    let sdk = state::read(|state| state.flutter.clone());
    let config = sdk["settings"].clone();
    let managed = sdk["mode"] == "managed";
    let found = sdk["found"] == true;
    let platform = if sdk["platform"] == "win32" {
        r"C:\src\flutter"
    } else {
        "/opt/flutter"
    };
    let mut status = if found {
        format!(
            "Flutter {} · {}",
            sdk["version"].as_str().unwrap_or("SDK detected"),
            if managed {
                "Freeflow-managed"
            } else {
                "Existing installation"
            }
        )
    } else {
        "Flutter SDK unavailable".into()
    };
    if let Some(latest) = sdk["latest"].as_str() {
        status.push_str(&format!(" · Latest stable {latest}"));
    }
    let content = format!(
        r#"{}<div class="form-row">{}{}{}</div><p class="dialog-help" id="settings-progress" hidden></p><pre class="code-card" id="doctor-output" hidden></pre>{}
<label class="check-row"><input type="checkbox" name="managedAutoUpdate"{}><span>Automatically update the Freeflow-managed SDK to the latest stable release</span></label>
<label class="check-row"><input type="checkbox" name="offline"{}><span>Use cached Dart packages offline</span></label>"#,
        callout(&h(&status), false),
        button("sdk-check", "Check latest stable", "reload", "outline"),
        button(
            "sdk-install",
            if managed {
                "Update managed SDK"
            } else {
                "Install managed SDK"
            },
            "download",
            "outline"
        ),
        button("sdk-doctor", "Flutter doctor", "terminal", "soft"),
        field(
            "flutterPath",
            "Flutter SDK path override",
            string(&config["flutterPath"]),
            &format!(r#"placeholder="{}""#, h(platform)),
            "Leave blank to use the Freeflow-managed SDK or Flutter on PATH."
        ),
        if config["managedAutoUpdate"] != false {
            " checked"
        } else {
            ""
        },
        if config["offline"] == true {
            " checked"
        } else {
            ""
        }
    );
    open(
        View::Settings { config },
        "Settings",
        "Flutter SDK and local build settings.",
        form(&content, &submit("Save settings")),
    );
    dom::disabled("sdk-doctor", !found);
    Ok(())
}

async fn sdk_action(action: &str, config: Value, version: u64) -> Result {
    match action {
        "sdk-check" => {
            let sdk = post("/sdk/check", json!({})).await?;
            toast(
                &format!("Latest stable Flutter {}", string(&sdk["latest"])),
                false,
            );
            if current_view(version) {
                settings().await?;
            }
        }
        "sdk-install" => {
            let job = post("/sdk/install", json!({})).await?;
            track(&job, true);
            if current_view(version) {
                hidden("settings-progress", false);
                dom::text(
                    "settings-progress",
                    "Downloading and verifying the Flutter SDK. Progress appears in the console.",
                );
            }
            let result = wait_job(job).await?;
            if !string(&config["flutterPath"]).is_empty() {
                let mut current = api("/state").await?["flutter"]["settings"].clone();
                if current["flutterPath"] == config["flutterPath"] {
                    current["flutterPath"] = json!("");
                    post("/settings", current).await?;
                }
            }
            refresh_state().await?;
            toast(
                &format!("Flutter {} stable installed", string(&result["version"])),
                false,
            );
            if current_view(version) {
                settings().await?;
            }
        }
        "sdk-doctor" => {
            let result = post("/sdk/doctor", json!({})).await?;
            if current_view(version) {
                hidden("doctor-output", false);
                dom::text(
                    "doctor-output",
                    result["output"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("Flutter doctor returned no output."),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

pub async fn history() -> Result {
    let project = project_id()?;
    let (file, drafted) = state::read(|state| {
        (
            state.current_file().to_owned(),
            state
                .drafts
                .contains_key(&state.draft_key(state.current_file())),
        )
    });
    if drafted {
        return Err(Error::new("Save the draft before restoring file history."));
    }
    let entries = project_get(&project, &format!("history{}", path_query(&file))).await?;
    if !current(&project) {
        return Ok(());
    }
    let mut list = String::new();
    for entry in items(&entries) {
        let label = when(&entry["date"]);
        let revision = string(&entry["revision"])
            .chars()
            .take(10)
            .collect::<String>();
        list.push_str(&format!(r#"<div class="dialog-item"><div class="dialog-item-text"><strong>{}</strong><small class="mono">{}</small></div>{}</div>"#,
            h(&label), h(&revision), button(&format!("version-{}", string(&entry["id"])), "View", "history", "soft")));
    }
    if list.is_empty() {
        list = r#"<p class="dialog-help">Freeflow keeps a copy of the previous version when you save an edit.</p>"#.into();
    }
    open(
        View::History {
            project,
            file: file.clone(),
        },
        "File history",
        &file,
        format!(r#"<div class="dialog-list">{list}</div>"#),
    );
    Ok(())
}

pub fn wrap(node: Value) {
    let Ok(project) = project_id() else {
        return;
    };
    let wrappers: String = state::read(|state| {
        items(&state.visual["wrappers"])
            .iter()
            .map(|item| option(string(item), ""))
            .collect()
    });
    let content = format!(
        r#"<label class="form-field">Wrapper<select name="widget" autofocus>{wrappers}</select></label>"#
    );
    open(
        View::Wrap {
            project,
            widget: node["id"].clone(),
        },
        "Wrap widget",
        string(&node["type"]),
        form(&content, &submit("Wrap widget")),
    );
}

pub fn new_file() {
    let Ok(project) = project_id() else {
        return;
    };
    let content = field(
        "path",
        "Project-relative path",
        "",
        r#"required autofocus placeholder="lib/screens/home.dart""#,
        "",
    );
    open(
        View::NewFile { project },
        "New project file",
        "",
        form(&content, &submit("Create file")),
    );
}

pub fn quick_open() {
    let content = format!(
        r#"<div class="form">{}<div class="dialog-list" id="quick-results"></div></div>"#,
        field(
            "search",
            "Search project files",
            "",
            r#"autofocus placeholder="lib/main.dart""#,
            ""
        )
    );
    open(View::Quick, "Open file", "", content);
    search("");
}

fn search(term: &str) {
    let term = term.trim().to_lowercase();
    let entries: String = state::read(|state| {
        state.tree.iter().filter(|entry| entry["type"] == "file")
        .map(|entry| string(&entry["path"])).filter(|path| path.to_lowercase().contains(&term)).take(16)
        .map(|path| format!(r#"<button type="button" class="button soft" data-dialog-action="quick" data-path="{}">{}{}</button>"#,
            h(path), icon("file", 16), h(path))).collect()
    });
    html(
        "quick-results",
        if entries.is_empty() {
            r#"<p class="dialog-help">No matching files.</p>"#
        } else {
            &entries
        },
    );
}

fn form_data(form: &Element) -> Value {
    let mut data = Map::new();
    if let Ok(fields) = form.query_selector_all("[name]") {
        for index in 0..fields.length() {
            let Some(field) = fields
                .item(index)
                .and_then(|node| node.dyn_into::<Element>().ok())
            else {
                continue;
            };
            let Some(name) = field.get_attribute("name") else {
                continue;
            };
            if let Some(input) = field.dyn_ref::<HtmlInputElement>() {
                match input.type_().as_str() {
                    "checkbox" => {
                        data.insert(name, json!(input.checked()));
                        continue;
                    }
                    "radio" if !input.checked() => continue,
                    _ => {}
                }
            }
            data.insert(name, json!(dom::value(&field)));
        }
    }
    Value::Object(data)
}

async fn submit_view(view: View, version: u64, mut input: Value) -> Result {
    match view {
        View::Create => {
            new_project(input, false).await?;
            close_current(version);
        }
        View::Import => {
            import_project(string(&input["folder"]).trim()).await?;
            close_current(version);
        }
        View::GitClone => crate::source_control::clone_project(version, input).await?,
        View::AgentSettings { project } if current(&project) => {
            crate::agent_panel::save_settings(&project, version, input).await?;
        }
        View::Theme { project, config } if current(&project) => {
            crate::theme::save(&project, version, &config, input).await?;
        }
        View::Git { project } if current(&project) => {
            crate::source_control::submit(&project, version, input).await?;
        }
        View::Backend {
            project, provider, ..
        } => configure_backend(version, project, provider, input).await?,
        View::Export {
            project,
            target,
            blocked,
            ..
        } if !blocked && current(&project) => {
            input["target"] = json!(target);
            let job = project_post(&project, "build", input).await?;
            track(&job, true);
            let result = wait_job(job).await?;
            toast(
                &format!("Exported {}", string(&result["artifact"]["name"])),
                false,
            );
            if current_view(version) && current(&project) {
                exports(&target).await?;
            }
        }
        View::Signing { project } if current(&project) => {
            let job = project_post(&project, "signing", input).await?;
            track(&job, true);
            wait_job(job).await?;
            toast("Android upload signing configured", false);
            if current_view(version) && current(&project) {
                exports("android").await?;
            }
        }
        View::Settings { .. } => {
            input["flutterPath"] = json!(string(&input["flutterPath"]).trim());
            post("/settings", input).await?;
            refresh_state().await?;
            toast("Settings saved", false);
            close_current(version);
        }
        View::Wrap { project, widget } if current(&project) => {
            visual_edit(json!({"id":widget,"operation":"wrap","widget":input["widget"]})).await?;
            close_current(version);
        }
        View::NewFile { project } if current(&project) => {
            let file = string(&input["path"]).trim();
            send(
                &project_route(&project, "file"),
                "PUT",
                json!({"path":file,"content":"","revision":null}),
            )
            .await?;
            refresh_tree(&project).await?;
            close_current(version);
            if current(&project) {
                select(file.into(), Mode::Code).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn act(view: View, version: u64, action: &str, element: &Element) -> Result {
    match view {
        View::Projects if action == "import-folder" => import(),
        View::Projects if action == "clone-project" => crate::source_control::clone_dialog(),
        View::Git { project } if current(&project) => {
            crate::source_control::act(&project, version, action, element).await?;
        }
        View::Theme { project, config } if current(&project) => {
            crate::theme::act(version, &config, action).await?;
        }
        View::Projects if action == "create-project" => create(),
        View::Projects => {
            if let Some((kind, key)) = action.split_once('-') {
                let project = state::read(|state| {
                    state
                        .projects
                        .iter()
                        .find(|project| project["id"] == key)
                        .cloned()
                });
                if let Some(project) = project {
                    if kind == "project" {
                        close_current(version);
                        activate(project).await?;
                    } else if kind == "folder" {
                        copy(string(&project["root"]), "Project folder").await?;
                    }
                }
            }
        }
        View::Ai { config, client } => {
            if let Some(next) = action.strip_prefix("client-") {
                render_ai(config, next);
            } else if action == "copy-config" {
                copy(string(&config[client]), "MCP setup").await?;
            } else if action == "copy-prompt" {
                copy(string(&config["prompt"]), "Starter prompt").await?;
            }
        }
        View::Backend {
            project,
            provider,
            config,
        } => match action {
            "firebase" | "supabase" => backends(action).await?,
            "copy-defines" => copy(string(&config["definesFile"]), "Dart defines path").await?,
            "disconnect" => {
                configure_backend(version, project, provider, json!({"enabled":false})).await?
            }
            _ => {}
        },
        View::Export {
            project,
            records,
            signing: config,
            ..
        } if current(&project) => {
            if let Some(target) = action.strip_prefix("target-") {
                exports(target).await?;
            } else if action == "ios-source" {
                exports("source").await?;
            } else if action == "android-signing" {
                signing(project, config);
            } else if let Some(key) = action.strip_prefix("download-") {
                if let Some(record) = records.iter().find(|record| record["id"] == key) {
                    let next = project_post(&project, "download", json!({"id":key})).await?;
                    let link: HtmlAnchorElement = dom::doc().create_element("a")?.unchecked_into();
                    link.set_href(string(&next["url"]));
                    link.set_download(string(&record["name"]));
                    dom::doc()
                        .body()
                        .expect("document body")
                        .append_child(&link)?;
                    link.click();
                    link.remove();
                }
            }
        }
        View::Signing { .. } if action == "back-exports" => exports("android").await?,
        View::Settings { config } => {
            attr(element, "disabled", "");
            let result = sdk_action(action, config, version).await;
            let _ = element.remove_attribute("disabled");
            result?;
        }
        View::History { project, file } if current(&project) => {
            if let Some(key) = action.strip_prefix("version-") {
                let old = project_get(
                    &project,
                    &format!(
                        "history-version{}&id={}",
                        path_query(&file),
                        dom::encoded(key)
                    ),
                )
                .await?;
                if current_view(version) {
                    let description = format!("{file} · {}", when(&old["date"]));
                    let body = format!(
                        r#"{}<div class="form-footer">{}{}</div>"#,
                        code(string(&old["content"])),
                        button("version-back", "Back", "", "soft"),
                        button("version-restore", "Restore version", "history", "primary")
                    );
                    open(
                        View::Version { project, file, old },
                        "File version",
                        &description,
                        body,
                    );
                }
            }
        }
        View::Version { project, file, old } => {
            if action == "version-back" {
                history().await?;
            }
            if action == "version-restore" {
                let revision = state::read(|state| {
                    if state.project_id() != project || state.current_file() != file {
                        return Err(Error::new("Open the file again before restoring."));
                    }
                    if state.drafts.contains_key(&state.draft_key(&file)) {
                        return Err(Error::new("Save the draft before restoring file history."));
                    }
                    Ok(state.source["revision"].clone())
                })?;
                send(
                    &project_route(&project, "file"),
                    "PUT",
                    json!({"path":file,"content":old["content"],"revision":revision}),
                )
                .await?;
                if current(&project) && state::read(|state| state.current_file() == file) {
                    load_file(&file, true).await?;
                }
                close_current(version);
                toast(&format!("Restored {}", basename(&file)), false);
            }
        }
        View::Quick if action == "quick" => {
            if let Some(file) = element.get_attribute("data-path") {
                close_current(version);
                select(file, Mode::Code).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn initialize() {
    dom::click("modal-close", close);
    dom::listen(&id("modal"), "click", |event| {
        if let Some(target) = dom::target(&event).filter(|target| target.id() == "modal") {
            if let Ok(mouse) = event.dyn_into::<MouseEvent>() {
                let box_ = target.get_bounding_client_rect();
                let (x, y) = (f64::from(mouse.client_x()), f64::from(mouse.client_y()));
                if x < box_.left() || x > box_.right() || y < box_.top() || y > box_.bottom() {
                    close();
                }
            }
        }
    });
    let body = id("modal-body");
    dom::listen(&body, "click", |event| {
        if let Some(element) = closest(&event, "[data-dialog-action]") {
            let action = element
                .get_attribute("data-dialog-action")
                .unwrap_or_default();
            let (version, view) = view();
            dom::spawn(async move { act(view, version, &action, &element).await });
        }
    });
    dom::listen(&body, "submit", |event| {
        event.prevent_default();
        let Some(form) = dom::target(&event) else {
            return;
        };
        let Some(button) = form.query_selector(r#"[type="submit"]"#).ok().flatten() else {
            return;
        };
        if button.has_attribute("disabled") {
            return;
        }
        let field = form.query_selector(".form-error").ok().flatten();
        let data = form_data(&form);
        let (version, view) = view();
        let caption = button.text_content();
        attr(&button, "disabled", "");
        button.set_text_content(Some("Working…"));
        if let Some(field) = &field {
            field.set_text_content(None);
        }
        spawn_local(async move {
            if let Err(error) = submit_view(view, version, data).await {
                if let Some(field) = field.filter(|_| current_view(version)) {
                    field.set_text_content(Some(&error.message));
                } else {
                    toast(&error.message, true);
                }
            }
            let _ = button.remove_attribute("disabled");
            button.set_text_content(caption.as_deref());
        });
    });
    dom::listen(&body, "input", |event| {
        if let Some(field) = dom::target(&event)
            .filter(|field| field.get_attribute("name").as_deref() == Some("search"))
        {
            search(&dom::value(&field));
        }
    });
    dom::listen(&body, "keydown", |event| {
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        if key.key() == "Enter" && matches!(view().1, View::Quick) {
            key.prevent_default();
            if let Some(button) = dom::find("#quick-results button")
                .and_then(|node| node.dyn_into::<HtmlElement>().ok())
            {
                button.click();
            }
        }
    });
}
