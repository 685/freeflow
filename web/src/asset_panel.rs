use crate::{
    dom::{
        self, Error, Result, basename, closest, escape as h, id, items, project_get, project_post,
        string,
    },
    state::{self, Mode, State},
};
use base64::Engine as _;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{DragEvent, File, FileList, HtmlInputElement};

const CATEGORIES: &[(&str, &str)] = &[
    ("images", "Images"),
    ("audio", "Audio"),
    ("videos", "Videos"),
    ("fonts", "Fonts"),
    ("documents", "Documents"),
];

pub fn category(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "ico" | "bmp" | "wbmp" | "svg" => {
            Some("images")
        }
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" | "opus" => Some("audio"),
        "mp4" | "m4v" | "webm" | "mov" | "ogv" => Some("videos"),
        "ttf" | "otf" => Some("fonts"),
        "pdf" | "txt" | "json" | "csv" | "xml" | "md" | "arb" | "zip" => Some("documents"),
        _ => None,
    }
}

pub fn previewable(path: &str) -> bool {
    category(path).is_some_and(|category| category != "documents")
        || path.rsplit('.').next().is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "pdf" | "zip")
        })
}

pub fn preview(path: &str, url: &str) -> String {
    let media = match category(path) {
        Some("images") => format!(r#"<img alt="{}" src="{}">"#, h(basename(path)), h(url)),
        Some("audio") => format!(
            r#"<audio controls preload="metadata" src="{}" aria-label="{}"></audio>"#,
            h(url),
            h(basename(path))
        ),
        Some("videos") => format!(
            r#"<video controls preload="metadata" src="{}" aria-label="{}"></video>"#,
            h(url),
            h(basename(path))
        ),
        _ => dom::icon(
            if category(path) == Some("fonts") {
                "type"
            } else {
                "file"
            },
            40,
        ),
    };
    let family = state::read(|state| {
        items(&state.asset_catalog["entries"])
            .iter()
            .find(|entry| entry["path"] == path)
            .and_then(|entry| entry["family"].as_str())
            .map(str::to_owned)
    });
    let family = family.map_or(String::new(), |family| {
        format!("<p>Font family · <code>{}</code></p>", h(&family))
    });
    format!(
        r#"{media}<code>{}</code>{family}<a class="button outline small" href="{}" download="{}">Download file</a>"#,
        h(path),
        h(url),
        h(basename(path))
    )
}

fn draft_free(project: &str, path: Option<&str>) -> Result {
    if state::read(|state| {
        state
            .drafts
            .contains_key(&(project.into(), "pubspec.yaml".into()))
            || path.is_some_and(|path| state.drafts.contains_key(&(project.into(), path.into())))
    }) {
        return Err(Error::new(
            "Save or discard unsaved edits in pubspec.yaml and this asset first.",
        ));
    }
    Ok(())
}

pub async fn refresh_current() -> Result {
    refresh(&crate::project_id()?).await
}

async fn open_app_icons() -> Result {
    let project = crate::project_id()?;
    let config = project_get(&project, "launcher-icons").await?;
    if !crate::current(&project) {
        return Ok(());
    }
    let mut controls = String::new();
    for (key, label, enabled) in [
        ("android", "Android", config["android"] == true),
        ("ios", "iOS", config["ios"] == true),
        (
            "foreground",
            "Android adaptive foreground",
            config["android"] == true,
        ),
    ] {
        if !enabled {
            continue;
        }
        let current = string(&config["values"][key]);
        let choices: String = items(&config["images"])
            .iter()
            .map(|entry| {
                let path = string(entry);
                format!(
                    r#"<option value="{}"{}>{}</option>"#,
                    h(path),
                    if path == current { " selected" } else { "" },
                    h(path)
                )
            })
            .collect();
        controls.push_str(&format!(r#"<label class="form-field"><span>{label}</span><select name="{key}" aria-label="{label}"><option value="">Choose PNG image</option>{choices}</select></label>"#));
    }
    if config["android"] == true {
        controls.push_str(&format!(r##"<label class="form-field"><span>Adaptive background</span><input name="background" value="{}" placeholder="#FFFFFF or assets/images/background.png" maxlength="700"></label>"##,
            h(string(&config["values"]["background"]))));
    }
    let ready = !controls.is_empty() && !items(&config["images"]).is_empty();
    let body = if ready {
        format!(
            r#"<form class="form">{controls}<p class="dialog-help">Adaptive icons use the foreground PNG and a solid color or a background PNG.</p><div class="form-error" role="alert"></div><div class="form-footer"><button class="button primary" type="submit">Save app icons</button></div></form>"#
        )
    } else if controls.is_empty() {
        r#"<p class="dialog-help">Add Android or iOS to this Flutter project to configure launcher icons.</p>"#.into()
    } else {
        r#"<p class="dialog-help">Upload a square PNG in Assets to use it as an app icon.</p>"#
            .into()
    };
    crate::dialogs::open(
        crate::dialogs::View::AppIcons { project, config },
        "App icons",
        "Freeflow generates the selected launcher icons before native builds.",
        body,
    );
    Ok(())
}

pub(crate) async fn save_app_icons(
    project: &str,
    version: u64,
    config: &Value,
    mut input: Value,
) -> Result {
    if !crate::current(project) {
        return Ok(());
    }
    draft_free(project, Some("flutter_launcher_icons.yaml"))?;
    input["revision"] = config["revision"].clone();
    input["manifestRevision"] = config["manifestRevision"].clone();
    project_post(project, "launcher-icons", input).await?;
    if crate::current(project) {
        crate::refresh_tree(project).await?;
        let file = state::read(|state| state.current_file().to_owned());
        if matches!(
            file.as_str(),
            "pubspec.yaml" | "flutter_launcher_icons.yaml"
        ) {
            crate::load_file(&file, true).await?;
        }
    }
    crate::dialogs::close_current(version);
    dom::toast("App icons saved", false);
    Ok(())
}

pub async fn refresh(project: &str) -> Result {
    let context = state::change(|state| {
        if state.project_id() != project {
            return None;
        }
        state.asset_request += 1;
        Some((state.generation, state.asset_request))
    });
    let Some((generation, request)) = context else {
        return Ok(());
    };
    let catalog = project_get(project, "assets").await?;
    state::change(|state| {
        if state.generation == generation && state.asset_request == request {
            state.asset_catalog = catalog;
            if state.assets_open && !state.widgets {
                render(state);
            }
        }
    });
    Ok(())
}

pub fn render(state: &State) {
    if dom::value(&id("asset-search")) != state.asset_query {
        id("asset-search")
            .unchecked_into::<HtmlInputElement>()
            .set_value(&state.asset_query);
    }
    dom::disabled("upload-assets", state.asset_uploading);
    dom::disabled("refresh-assets", state.asset_uploading);
    let term = state.asset_query.to_lowercase();
    let mut content = String::new();
    if let Some(error) = state.asset_catalog["manifestError"].as_str() {
        content.push_str(&format!(
            r#"<p class="tree-empty" role="status">{}</p>"#,
            h(error)
        ));
    }
    for (category, label) in CATEGORIES {
        let entries: Vec<_> = items(&state.asset_catalog["entries"])
            .iter()
            .filter(|entry| {
                entry["category"] == *category
                    && (format!("{} {}", string(&entry["path"]), string(&entry["family"]))
                        .to_lowercase()
                        .contains(&term))
            })
            .collect();
        if entries.is_empty() {
            continue;
        }
        content.push_str(&format!(
            r#"<h3 class="asset-category">{label} <span>{}</span></h3>"#,
            entries.len()
        ));
        for entry in entries {
            let path = string(&entry["path"]);
            let registered = entry["registered"] == true;
            let register = if !registered && entry["revision"].is_string() {
                format!(
                    r#"<button type="button" class="icon-button" data-asset-register="{}" aria-label="Register {} in Flutter" title="Register in Flutter" {}>{}</button>"#,
                    h(path),
                    h(basename(path)),
                    if state.asset_uploading {
                        "disabled"
                    } else {
                        ""
                    },
                    dom::icon("plus", 14)
                )
            } else {
                String::new()
            };
            content.push_str(&format!(r#"<div class="asset-row{}" data-asset-path="{}"><button type="button" class="asset-open" data-asset-open="{}" title="{}"><strong class="ellipsis">{}</strong><small>{}{} · {}</small></button>{register}<button type="button" class="icon-button" data-asset-delete="{}" aria-label="Delete {}" title="Delete asset. Undo restores the file and Flutter declaration." {}>{}</button></div>"#,
                if state.file == path { " selected" } else { "" }, h(path), h(path), h(path), h(basename(path)),
                if registered { "Flutter asset" } else { "Not registered" },
                entry["family"].as_str().map_or(String::new(), |family| format!(" · {}", h(family))),
                size(entry["size"].as_u64().unwrap_or(0)), h(path), h(basename(path)),
                if state.asset_uploading || !entry["revision"].is_string() { "disabled" } else { "" }, dom::icon("trash", 14)));
        }
    }
    if content.is_empty() {
        content.push_str(if state.asset_catalog.is_null() {
            r#"<p class="tree-empty">Loading assets…</p>"#
        } else if term.is_empty() {
            r#"<p class="tree-empty">Upload images, audio, videos, fonts or documents. Files are added to Flutter automatically.</p>"#
        } else { r#"<p class="tree-empty">No matching assets.</p>"# });
    }
    if state.asset_catalog["truncated"] == true {
        content.push_str(r#"<p class="tree-empty">Showing part of this project's files.</p>"#);
    }
    dom::html("explorer-tree", content);
}

fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn files(list: FileList) -> Vec<File> {
    (0..list.length())
        .filter_map(|index| list.item(index))
        .collect()
}

async fn upload(files: Vec<File>) -> Result {
    if files.is_empty() {
        return Ok(());
    }
    let project = crate::project_id()?;
    draft_free(&project, None)?;
    if files.len() > 100 {
        return Err(Error::new("Upload up to 100 files at a time."));
    }
    for file in &files {
        if file.size() < 1.0 || file.size() > (10 * 1024 * 1024) as f64 {
            return Err(Error::new(format!(
                "{} must be between 1 byte and 10 MiB.",
                file.name()
            )));
        }
        if category(&file.name()).is_none() {
            return Err(Error::new(format!(
                "{} is not a supported asset type.",
                file.name()
            )));
        }
    }
    let generation = state::change(|state| {
        if state.asset_uploading {
            return None;
        }
        state.asset_uploading = true;
        state.assets_open = true;
        state.widgets = false;
        crate::view::explorer(state);
        Some(state.generation)
    })
    .ok_or_else(|| Error::new("An asset upload is already running."))?;
    let result = async {
        let mut uploaded = 0;
        let mut failures = vec![];
        for file in files {
            if !state::read(|state| state.generation == generation) {
                break;
            }
            draft_free(&project, None)?;
            let name = file.name();
            let data = JsFuture::from(file.array_buffer()).await?;
            let bytes = js_sys::Uint8Array::new(&data).to_vec();
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            match project_post(&project, "asset", json!({"name":name,"data":data})).await {
                Ok(_) => uploaded += 1,
                Err(error) => failures.push(format!("{name}: {}", error.message)),
            }
        }
        crate::refresh_tree(&project).await?;
        refresh(&project).await?;
        if crate::current(&project) {
            if state::read(|state| state.current_file() == "pubspec.yaml") {
                crate::load_file("pubspec.yaml", true).await?;
            }
            if uploaded > 0 {
                dom::toast(
                    &format!("{uploaded} asset(s) uploaded and registered in Flutter"),
                    false,
                );
            }
            if !failures.is_empty() {
                return Err(Error::new(
                    failures.into_iter().take(3).collect::<Vec<_>>().join("\n"),
                ));
            }
        }
        Ok(())
    }
    .await;
    state::change(|state| {
        if state.generation == generation {
            state.asset_uploading = false;
            if state.assets_open && !state.widgets {
                render(state);
            }
        }
    });
    result
}

async fn change(path: String, remove: bool) -> Result {
    let project = crate::project_id()?;
    draft_free(&project, Some(&path))?;
    let (entry, manifest) = state::read(|state| {
        (
            items(&state.asset_catalog["entries"])
                .iter()
                .find(|entry| entry["path"] == path)
                .cloned(),
            state.asset_catalog["manifestRevision"].clone(),
        )
    });
    let entry = entry.ok_or_else(|| Error::new("Refresh Assets before deleting this file."))?;
    project_post(&project, "asset", json!({"action":if remove {"delete"} else {"register"},"path":path,"revision":entry["revision"],"manifestRevision":manifest})).await?;
    crate::refresh_tree(&project).await?;
    refresh(&project).await?;
    if crate::current(&project) {
        let active = state::change(|state| {
            if remove {
                state.tabs.retain(|tab| tab != &path);
            }
            (remove && state.current_file() == path).then(|| state.last_dart.clone())
        });
        if let Some(fallback) = active {
            crate::select(fallback, Mode::Code).await?;
        } else if state::read(|state| state.current_file() == "pubspec.yaml") {
            crate::load_file("pubspec.yaml", true).await?;
        }
        dom::toast(
            if remove {
                "Asset deleted. Use Undo saved edit to restore it."
            } else {
                "Asset registered in Flutter"
            },
            false,
        );
    }
    Ok(())
}

pub fn initialize() {
    for button in ["add-asset", "upload-assets"] {
        dom::click(button, || id("asset-input").click());
    }
    dom::click("refresh-assets", || dom::spawn(refresh_current()));
    dom::click("app-icons", || dom::spawn(open_app_icons()));
    dom::listen(&id("asset-input"), "change", |event| {
        let Some(input) =
            dom::target(&event).and_then(|element| element.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let selected = input.files().map(files).unwrap_or_default();
        input.set_value("");
        dom::spawn(upload(selected));
    });
    dom::listen(&id("asset-search"), "input", |event| {
        if let Some(input) = dom::target(&event) {
            state::change(|state| {
                state.asset_query = dom::value(&input);
                render(state);
            });
        }
    });
    for area in ["asset-drop", "explorer-tree"] {
        dom::listen(&id(area), "dragover", |event| {
            if state::read(|state| state.assets_open && !state.widgets) {
                event.prevent_default();
                dom::toggle(&id("asset-drop"), "drag-over", true);
            }
        });
        dom::listen(&id(area), "dragleave", |_| {
            dom::toggle(&id("asset-drop"), "drag-over", false)
        });
        dom::listen(&id(area), "drop", |event| {
            if !state::read(|state| state.assets_open && !state.widgets) {
                return;
            }
            event.prevent_default();
            dom::toggle(&id("asset-drop"), "drag-over", false);
            if let Ok(event) = event.dyn_into::<DragEvent>() {
                if let Some(list) = event.data_transfer().and_then(|transfer| transfer.files()) {
                    dom::spawn(upload(files(list)));
                }
            }
        });
    }
    dom::listen(&id("explorer-tree"), "click", |event| {
        if let Some(path) = closest(&event, "[data-asset-open]")
            .and_then(|element| element.get_attribute("data-asset-open"))
        {
            dom::spawn(crate::select(path, Mode::Code));
        } else if let Some(path) = closest(&event, "[data-asset-delete]")
            .and_then(|element| element.get_attribute("data-asset-delete"))
        {
            dom::spawn(change(path, true));
        } else if let Some(path) = closest(&event, "[data-asset-register]")
            .and_then(|element| element.get_attribute("data-asset-register"))
        {
            dom::spawn(change(path, false));
        }
    });
}
