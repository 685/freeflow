use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, id, items, project_get, project_post, string},
    state,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

const KINDS: &[(&str, &str)] = &[
    ("color", "Colors"),
    ("typography", "Typography"),
    ("number", "Sizes & spacing"),
    ("insets", "Edge insets"),
    ("radius", "Border radius"),
    ("font", "Font families"),
];
thread_local! {static FILTER: RefCell<(String,String)> = RefCell::default();}

fn current(project: &str, generation: u64) -> bool {
    state::read(|state| state.project_id() == project && state.generation == generation)
}

pub fn controls(query: &str, kind: &str) -> String {
    let options: String = KINDS
        .iter()
        .map(|(value, label)| {
            format!(
                r#"<option value="{value}"{}>{label}</option>"#,
                if *value == kind { " selected" } else { "" }
            )
        })
        .collect();
    format!(
        r#"<div class="design-filters"><input id="design-search" type="search" aria-label="Search design values" placeholder="Search design values…" value="{}"><select id="design-kind" aria-label="Design value type"><option value="">All types</option>{options}</select></div>"#,
        h(query)
    )
}

pub fn rows(catalog: &Value, picker: bool) -> String {
    let mut output = String::new();
    for (index, entry) in items(&catalog["entries"]).iter().enumerate() {
        let name = string(&entry["name"]);
        let path = string(&entry["path"]);
        let color = entry["value"]
            .as_str()
            .and_then(|color| color.strip_prefix('#'))
            .filter(|hex| {
                matches!(hex.len(), 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
        let swatch = color.map_or_else(
            || {
                dom::icon(
                    if entry["kind"] == "typography" {
                        "type"
                    } else {
                        "palette"
                    },
                    17,
                )
            },
            |hex| {
                let css = if hex.len() == 8 {
                    format!("{}{}", &hex[2..], &hex[..2])
                } else {
                    hex.to_owned()
                };
                format!(r#"<span class="design-swatch" style="background:#{css}"></span>"#)
            },
        );
        output.push_str(&format!(r#"<button type="button" class="design-value-row" data-design-search="{}" data-design-kind="{}" data-dialog-action="design-{}-{index}">{swatch}<span><strong>{}</strong><small>{} · line {}</small><code>{}</code></span><span class="design-value-action">{}</span></button>"#,
            h(&format!("{name} {path}")),h(string(&entry["kind"])),if picker {"use"} else {"edit"},h(name),h(path),entry["line"],h(string(&entry["expression"])),if picker {"Use"} else {"Edit"}));
    }
    output
}

pub fn filter() {
    let query = dom::find("#design-search")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    let kind = dom::find("#design-kind")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    FILTER.with_borrow_mut(|filter| *filter = (query.clone(), kind.clone()));
    let query = query.to_lowercase();
    let mut shown = 0;
    for row in dom::all("#modal [data-design-search]") {
        let show = row
            .get_attribute("data-design-search")
            .unwrap_or_default()
            .to_lowercase()
            .contains(&query)
            && (kind.is_empty()
                || row.get_attribute("data-design-kind").as_deref() == Some(kind.as_str()));
        if show {
            let _ = row.remove_attribute("hidden");
            shown += 1;
        } else {
            dom::attr(&row, "hidden", "");
        }
    }
    if dom::find("#design-empty").is_some() {
        dom::hidden("design-empty", shown > 0);
    }
}

pub async fn open() -> Result {
    let project = crate::project_id()?;
    let generation = state::read(|state| state.generation);
    let version = dialogs::version();
    let was_open = id("modal")
        .unchecked_into::<web_sys::HtmlDialogElement>()
        .open();
    let catalog = project_get(&project, "theme-values").await?;
    if !current(&project, generation)
        || dialogs::version() != version
        || was_open && !dialogs::current_view(version)
    {
        return Ok(());
    }
    let (query, kind) = FILTER.with_borrow(Clone::clone);
    let brightness = state::read(|state| state.canvas.brightness.clone());
    let brightness_options: String = ["system", "light", "dark"]
        .iter()
        .map(|value| {
            dom::option(
                value,
                if brightness.is_empty() {
                    "system"
                } else {
                    &brightness
                },
            )
        })
        .collect();
    let content = format!(
        r#"<div class="dialog-tabs"><button type="button" class="button soft small" data-dialog-action="design-settings">App settings</button><button type="button" class="button soft small active">Design values</button></div><label class="form-field"><span>App brightness</span><select id="design-brightness" aria-label="App brightness in Theme">{brightness_options}</select></label>
        <p class="dialog-help">Edit shared values in your Dart files. Widgets keep references to the same values.</p>{}<div id="design-values-list" class="design-values-list">{}</div><p id="design-empty" class="dialog-help" hidden>No matching design values. Use the Agent to add shared colors or styles to your app.</p><p class="dialog-help">{}Indexed {} Dart files.</p><div class="form-error" role="alert"></div><div class="form-footer"><button type="button" class="button outline" data-dialog-action="design-refresh">Refresh</button><button type="button" class="button soft" data-dialog-action="design-agent">Edit with Agent</button></div>"#,
        controls(&query, &kind),
        rows(&catalog, false),
        if catalog["truncated"] == true {
            "Showing part of this project. "
        } else {
            ""
        },
        catalog["scannedFiles"]
    );
    dialogs::open(
        View::DesignValues {
            project,
            generation,
            catalog,
        },
        "App theme",
        "Design values",
        content,
    );
    filter();
    Ok(())
}

pub async fn act(
    project: &str,
    generation: u64,
    catalog: &Value,
    version: u64,
    action: &str,
) -> Result {
    if !current(project, generation) {
        return Err(Error::new("Open this project's theme again."));
    }
    match action {
        "design-settings" => crate::theme::open(None).await?,
        "design-refresh" => open().await?,
        "design-agent" => {
            dialogs::close_current(version);
            crate::agent_panel::prepare("Update the app's shared colors, typography, spacing and corner radii. Read read_design_values and the theme source first. My changes are: ").await?;
        }
        _ => {
            if let Some(entry) = action
                .strip_prefix("design-edit-")
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| items(&catalog["entries"]).get(index))
            {
                let path = string(&entry["path"]);
                if state::read(|state| state.drafts.contains_key(&state.draft_key(path))) {
                    return Err(Error::new(
                        "Save or discard the draft in this theme file first.",
                    ));
                }
                let read = project_get(
                    project,
                    &format!(
                        "theme-value?path={}&id={}&revision={}",
                        dom::encoded(path),
                        dom::encoded(string(&entry["id"])),
                        dom::encoded(string(&entry["revision"]))
                    ),
                )
                .await?;
                if current(project, generation) && dialogs::current_view(version) {
                    edit(project, generation, read);
                }
            }
        }
    }
    Ok(())
}

fn field(entry: &Value) -> String {
    let name = string(&entry["name"]);
    let label = match name {
        "value" => "Value",
        "fontSize" => "Font size",
        "fontWeight" => "Font weight",
        "fontStyle" => "Font style",
        "fontFamily" => "Font family",
        "backgroundColor" => "Background color",
        "letterSpacing" => "Letter spacing",
        "wordSpacing" => "Word spacing",
        "height" => "Line height",
        "topLeft" => "Top left",
        "topRight" => "Top right",
        "bottomLeft" => "Bottom left",
        "bottomRight" => "Bottom right",
        "topStart" => "Top start",
        "topEnd" => "Top end",
        "bottomStart" => "Bottom start",
        "bottomEnd" => "Bottom end",
        _ => name,
    };
    let kind = string(&entry["type"]);
    let value = dom::display(&entry["value"]);
    let disabled = if entry["editable"] == true {
        ""
    } else {
        "disabled"
    };
    let attributes = format!(
        r#"name="value_{}" aria-label="{}" {disabled}"#,
        h(name),
        h(label)
    );
    let control = match kind {
        "weight" | "fontStyle" => {
            let options: &[&str] = if kind == "weight" {
                &[
                    "normal", "bold", "w100", "w200", "w300", "w400", "w500", "w600", "w700",
                    "w800", "w900",
                ]
            } else {
                &["normal", "italic"]
            };
            let options: String = options
                .iter()
                .map(|option| dom::option(option, &value))
                .collect();
            format!(
                r#"<select {attributes}><option value="">Keep current</option>{options}</select>"#
            )
        }
        "number" | "radius" => format!(
            r#"<input type="number" step="any" {attributes} value="{}" placeholder="Keep current">"#,
            h(&value)
        ),
        _ => format!(
            r#"<input type="text" {attributes} value="{}" placeholder="{}" {}>"#,
            h(&value),
            if kind == "color" {
                "#RRGGBB or #AARRGGBB"
            } else {
                "Keep current"
            },
            if kind == "font" {
                "list=\"design-fonts\""
            } else {
                ""
            }
        ),
    };
    let expression = if entry["present"] == true && entry["editable"] != true {
        format!(
            "<small>Current Dart value · {}</small>",
            h(string(&entry["expression"]))
        )
    } else {
        String::new()
    };
    format!(
        r#"<label class="form-field"><span>{}</span>{control}{expression}</label>"#,
        h(label)
    )
}

fn edit(project: &str, generation: u64, entry: Value) {
    let fields: String = items(&entry["fields"]).iter().map(field).collect();
    let editable = items(&entry["fields"])
        .iter()
        .any(|field| field["editable"] == true);
    let body = format!(
        r#"<form class="form design-value-editor"><p class="dialog-help">Changes apply everywhere this value is referenced. Blank fields keep their current Dart value.</p><label class="check-row"><input type="checkbox" name="useExpression" id="design-use-expression" {} {}>Use Dart expression instead</label><div id="design-visual-fields" class="form-grid" {}>{fields}</div><datalist id="design-fonts"></datalist><label id="design-expression-field" class="form-field" {}><span>Dart expression</span><textarea name="expression" rows="8" maxlength="65536">{}</textarea></label><div id="design-check" class="property-check" role="status"></div><div class="form-error" role="alert"></div><div class="form-footer"><button type="button" class="button outline" data-dialog-action="design-back">All values</button><button type="button" class="button outline" data-dialog-action="design-check">Check Dart</button><button type="submit" class="button primary">Save design value</button></div></form>"#,
        if editable { "" } else { "checked" },
        if editable { "" } else { "disabled" },
        if editable { "" } else { "hidden" },
        if editable { "hidden" } else { "" },
        h(string(&entry["expression"]))
    );
    dialogs::open(
        View::DesignValue {
            project: project.into(),
            generation,
            entry: entry.clone(),
        },
        string(&entry["name"]),
        &format!("{} · line {}", string(&entry["path"]), entry["line"]),
        body,
    );
    let project = project.to_owned();
    let version = dialogs::version();
    dom::spawn(async move {
        if let Ok(assets) = project_get(&project, "assets").await {
            if current(&project, generation) && dialogs::current_view(version) {
                let options: String = items(&assets["entries"])
                    .iter()
                    .filter_map(|entry| entry["family"].as_str())
                    .map(|family| format!("<option value=\"{}\">", h(family)))
                    .collect();
                dom::html("design-fonts", options);
            }
        }
        Ok(())
    });
}

fn request(entry: &Value, input: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for field in items(&entry["fields"]) {
        let name = string(&field["name"]);
        fields.insert(name.into(), input[format!("value_{name}")].clone());
    }
    json!({"path":entry["path"],"revision":entry["revision"],"id":entry["id"],"mode":if input["useExpression"]==true {"expression"} else {"visual"},"fields":fields,"expression":input["expression"]})
}

pub async fn save(
    project: &str,
    generation: u64,
    entry: &Value,
    version: u64,
    input: Value,
) -> Result {
    if !current(project, generation) {
        return Err(Error::new("Open this theme again before saving."));
    }
    let path = string(&entry["path"]);
    if state::read(|state| state.drafts.contains_key(&state.draft_key(path))) {
        return Err(Error::new(
            "Save or discard the draft in this theme file first.",
        ));
    }
    project_post(project, "theme-values", request(entry, &input)).await?;
    if current(project, generation) && state::read(|state| state.current_file() == path) {
        crate::load_file(path, true).await?;
    }
    if current(project, generation) && dialogs::current_view(version) {
        open().await?;
    }
    dom::toast("Shared design value saved", false);
    Ok(())
}

pub async fn value_action(
    project: &str,
    generation: u64,
    entry: &Value,
    version: u64,
    action: &str,
) -> Result {
    if !current(project, generation) {
        return Err(Error::new("Open this theme again."));
    }
    if action == "design-back" {
        return open().await;
    }
    if action == "design-check" {
        let Some(form) = dom::find("#modal .design-value-editor") else {
            return Ok(());
        };
        let input = dialogs::form_data(&form);
        dom::text("design-check", "Checking Dart…");
        let result = project_post(project, "theme-value-check", request(entry, &input)).await;
        if current(project, generation)
            && dialogs::current_view(version)
            && dom::find("#modal .design-value-editor")
                .is_some_and(|form| dialogs::form_data(&form) == input)
        {
            match result {
                Ok(result) => {
                    let rows: String = items(&result["diagnostics"])
                        .iter()
                        .map(|diagnostic| format!("<li>{}</li>", h(string(&diagnostic["message"]))))
                        .collect();
                    if rows.is_empty() {
                        dom::text("design-check", "Dart found no problems in this file.");
                    } else {
                        dom::html("design-check", format!("<ul>{rows}</ul>"));
                    }
                }
                Err(error) => dom::text("design-check", &error.message),
            }
        }
    }
    Ok(())
}

pub fn initialize() {
    dom::listen(&id("modal-body"), "input", |event| {
        if dom::target(&event).is_some_and(|field| field.id() == "design-search") {
            filter();
        }
        if dom::find("#design-check").is_some() {
            dom::text("design-check", "");
        }
    });
    dom::listen(&id("modal-body"), "change", |event| {
        if let Some(field) = dom::target(&event) {
            if field.id() == "design-kind" {
                filter();
            }
            if field.id() == "design-brightness" {
                crate::preview_canvas::brightness(&dom::value(&field));
            }
            if field.id() == "design-use-expression" {
                let dart = field.unchecked_into::<HtmlInputElement>().checked();
                dom::hidden("design-visual-fields", dart);
                dom::hidden("design-expression-field", !dart);
            }
        }
    });
    wasm_bindgen_futures::spawn_local(async {
        loop {
            gloo_timers::future::TimeoutFuture::new(2000).await;
            let Some((version, project, generation, since)) = dialogs::design_catalog_context()
            else {
                continue;
            };
            if let Ok(catalog) = project_get(
                &project,
                &format!("theme-values?since={}", dom::encoded(&since)),
            )
            .await
            {
                if current(&project, generation)
                    && dialogs::current_view(version)
                    && catalog["unchanged"] != true
                {
                    dom::html("design-values-list", rows(&catalog, false));
                    dialogs::update_design_catalog(version, catalog);
                    filter();
                }
            }
        }
    });
}
