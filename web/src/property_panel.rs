use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, id, items, project_post, string},
    state,
};
use serde_json::{Value, json};
use std::cell::RefCell;

pub(crate) mod values;

thread_local! { static SEARCH: RefCell<String> = RefCell::default(); }

#[derive(Clone)]
pub(crate) struct Context {
    pub(crate) project: String,
    pub(crate) path: String,
    pub(crate) revision: Value,
    pub(crate) widget: String,
    pub(crate) generation: u64,
}

impl Context {
    pub(crate) async fn capture() -> Result<Self> {
        let generation = state::read(|state| state.generation);
        for _ in 0..80 {
            let (current, pending) = state::read(|state| {
                (
                    state.generation == generation,
                    state.edit_history_busy || state.edit_history_writes > 0,
                )
            });
            if !current {
                return Err(Error::new("The project changed. Select its widget again."));
            }
            if !pending {
                break;
            }
            gloo_timers::future::TimeoutFuture::new(25).await;
        }
        if state::read(|state| state.edit_history_busy || state.edit_history_writes > 0) {
            return Err(Error::new(
                "The previous edit is still refreshing. Try again shortly.",
            ));
        }
        let (project, path, revision) = crate::visual_context()?;
        let (widget, generation) = state::read(|state| {
            (
                state.widget().map(|node| string(&node["id"]).to_owned()),
                state.generation,
            )
        });
        Ok(Self {
            project,
            path,
            revision,
            widget: widget.ok_or_else(|| Error::new("Select a widget first."))?,
            generation,
        })
    }

    pub(crate) fn current(&self) -> bool {
        state::read(|state| {
            state.project_id() == self.project
                && state.generation == self.generation
                && state.visual["path"] == self.path
                && state.widget().is_some_and(|node| node["id"] == self.widget)
        })
    }

    async fn request(&self, action: &str, name: &str, extra: Value) -> Result<Value> {
        let mut input = json!({"action":action,"path":self.path,"revision":self.revision,"id":self.widget,"name":name});
        if let Some(extra) = extra.as_object() {
            input.as_object_mut().unwrap().extend(extra.clone());
        }
        project_post(&self.project, "widget-property", input).await
    }

    async fn save(&self, name: &str, expression: Value, remove: bool) -> Result {
        if !self.current() {
            return Err(Error::new(
                "Select the widget again before saving this property.",
            ));
        }
        crate::visual_edit_at(self.project.clone(), self.path.clone(), self.revision.clone(),
            json!({"id":self.widget,"operation":"expression","name":name,"expression":expression,"remove":remove})).await
    }
}

pub fn search() -> String {
    SEARCH.with_borrow(Clone::clone)
}

pub fn filter() {
    let term = search().to_lowercase();
    for card in dom::all("#inspector [data-property-search]") {
        let text = card
            .get_attribute("data-property-search")
            .unwrap_or_default()
            .to_lowercase();
        dom::attr(&card, "hidden", "");
        if term.is_empty() || text.contains(&term) {
            let _ = card.remove_attribute("hidden");
        }
    }
}

pub async fn reset(name: String) -> Result {
    Context::capture()
        .await?
        .save(&name, Value::Null, true)
        .await
}

pub async fn open_design_values(name: String, kind: String) -> Result {
    let context = Context::capture().await?;
    let mut catalog = dom::project_get(&context.project, "theme-values").await?;
    if !context.current() {
        return Ok(());
    }
    let entries: Vec<_> = items(&catalog["entries"])
        .iter()
        .filter(|entry| {
            entry["constant"] == true
                && (entry["path"] == context.path
                    || entry["public"] == true && entry["part"] != true)
        })
        .cloned()
        .collect();
    catalog["entries"] = json!(entries);
    let target = if name.is_empty() {
        r#"<label class="form-field"><span>Property</span><input id="design-property" placeholder="style or decoration.borderRadius" maxlength="180" required></label>"#.to_owned()
    } else {
        String::new()
    };
    let body = format!(
        r#"<div class="form">{target}{}<div class="design-values-list">{}</div><p id="design-empty" class="dialog-help" hidden>No matching reusable constants. Open App theme to edit or add design values.</p><p class="dialog-help">This adds a reference to the shared value and imports its file when needed.</p><div class="form-error" role="alert"></div></div>"#,
        crate::design_values::controls("", &kind),
        crate::design_values::rows(&catalog, true)
    );
    dialogs::open(
        View::DesignPicker {
            context,
            name: name.clone(),
            catalog,
        },
        "Use design value",
        &format!(
            "Set {} from a shared constant",
            if name.is_empty() { "a property" } else { &name }
        ),
        body,
    );
    crate::design_values::filter();
    Ok(())
}

pub async fn pick_design_value(
    context: &Context,
    name: &str,
    catalog: &Value,
    version: u64,
    action: &str,
) -> Result {
    let Some(entry) = action
        .strip_prefix("design-use-")
        .and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| items(&catalog["entries"]).get(index))
    else {
        return Ok(());
    };
    let name = if name.is_empty() {
        dom::find("#design-property")
            .map(|field| dom::value(&field))
            .unwrap_or_default()
    } else {
        name.into()
    };
    if name.trim().is_empty() {
        return Err(Error::new("Enter the widget property to set."));
    }
    if !context.current() {
        return Err(Error::new(
            "Select the widget again before using this value.",
        ));
    }
    if state::read(|state| {
        state
            .drafts
            .contains_key(&state.draft_key(string(&entry["path"])))
    }) {
        return Err(Error::new(
            "Save the theme file draft before using its values.",
        ));
    }
    let result=dom::project_post(&context.project,"theme-binding",json!({"path":context.path,"revision":context.revision,"id":context.widget,"name":name.trim(),"valuePath":entry["path"],"valueRevision":entry["revision"],"valueId":entry["id"]})).await?;
    if context.current() {
        state::change(|state| state.widget = string(&result["widget"]).into());
        crate::load_file(&context.path, true).await?;
    }
    dialogs::close_current(version);
    dom::toast("Shared design value applied", false);
    Ok(())
}

pub async fn open_assets(name: String, category: String) -> Result {
    let context = Context::capture().await?;
    let catalog = dom::project_get(&context.project, "assets").await?;
    if !context.current() {
        return Ok(());
    }
    let entries: Vec<_> = items(&catalog["entries"])
        .iter()
        .filter(|entry| {
            entry["registered"] == true
                && entry["category"] == category
                && (category != "fonts" || entry["family"].is_string())
                && (category != "images"
                    || !string(&entry["path"])
                        .to_ascii_lowercase()
                        .ends_with(".svg"))
        })
        .cloned()
        .collect();
    let rows: String = entries.iter().enumerate().map(|(index, entry)| {
        let path = string(&entry["path"]);
        let label = entry["family"].as_str().unwrap_or(path);
        format!(r#"<button type="button" class="asset-choice" data-asset-search="{}" data-dialog-action="asset-use-{index}"><strong>{}</strong><small>{}</small></button>"#, h(&format!("{path} {label}")), h(label), h(path))
    }).collect();
    dialogs::open(
        View::AssetPicker {
            context,
            name: name.clone(),
            entries,
        },
        "Choose asset",
        &format!("Set {name} from this project's Flutter assets."),
        format!(
            r#"<div class="asset-picker"><input id="asset-picker-search" type="search" aria-label="Search available assets" placeholder="Search assets…" autofocus><div class="asset-choices">{}</div><p id="asset-picker-empty" class="dialog-help" {}>No matching assets. Upload {} in Assets first.</p><div class="form-error" role="alert"></div></div>"#,
            rows,
            if rows.is_empty() { "" } else { "hidden" },
            h(&category)
        ),
    );
    Ok(())
}

pub async fn pick_asset(
    context: &Context,
    name: &str,
    entries: &[Value],
    version: u64,
    action: &str,
) -> Result {
    let Some(entry) = action
        .strip_prefix("asset-use-")
        .and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| entries.get(index))
    else {
        return Ok(());
    };
    let value = entry["family"]
        .as_str()
        .unwrap_or_else(|| string(&entry["path"]));
    let expression = serde_json::to_string(value).unwrap().replace('$', "\\$");
    context.save(name, json!(expression), false).await?;
    dialogs::close_current(version);
    Ok(())
}

pub async fn open(name: String) -> Result {
    let context = Context::capture().await?;
    let data = if name.is_empty() {
        Value::Null
    } else {
        context.request("read", &name, Value::Null).await?
    };
    if !context.current() {
        return Ok(());
    }
    let adding = name.is_empty();
    dialogs::open(
        View::WidgetProperty {
            context: context.clone(),
        },
        if adding {
            "Add property"
        } else {
            "Edit property"
        },
        "Edit this widget’s Dart value, expression or callback.",
        format!(
            r#"<form class="form property-editor">
        <label class="form-field"><span>Property</span><input name="property" list="property-suggestions" value="{}" placeholder="maxLines or style.fontSize" required {}><datalist id="property-suggestions"></datalist></label>
        {}
        <label class="form-field"><span>Dart expression</span><textarea name="expression" rows="9" maxlength="65536" required {} placeholder="'Hello', 24, Colors.blue or () {{ … }}">{}</textarea></label>
        <p class="dialog-help">Use a dot to edit a value inside an existing constructor, such as style.letterSpacing. Changes join the project’s Undo history.</p>
        <div id="property-check" class="property-check" role="status"></div><div class="form-error" role="alert"></div>
        <div class="form-footer"><button type="button" class="button outline" data-dialog-action="property-theme">Use design value</button><button type="button" class="button outline" data-dialog-action="property-check">Check Dart</button><button type="button" class="button outline" data-dialog-action="property-remove" {}>Reset property</button><button type="submit" class="button primary">Save property</button></div>
        </form>"#,
            h(&name),
            if adding { "autofocus" } else { "readonly" },
            if adding {
                "<p id=\"property-suggestion-status\" class=\"dialog-help\">Loading properties from Dart…</p>"
            } else {
                ""
            },
            if adding { "" } else { "autofocus" },
            h(string(&data["expression"])),
            if data["removable"] == true {
                ""
            } else {
                "hidden"
            }
        ),
    );
    if adding {
        let version = dialogs::version();
        dom::spawn(async move {
            let result = context.request("suggest", "", Value::Null).await;
            if dialogs::current_view(version) {
                match result {
                    Ok(result) => {
                        let options: String = items(&result["items"])
                            .iter()
                            .filter_map(|item| {
                                let name =
                                    string(&item["label"]).trim_end().strip_suffix(':')?.trim();
                                Some(format!(
                                    "<option value=\"{}\">{}</option>",
                                    h(name),
                                    h(string(&item["detail"]))
                                ))
                            })
                            .collect();
                        let empty = options.is_empty();
                        dom::html("property-suggestions", options);
                        dom::text(
                            "property-suggestion-status",
                            if empty {
                                "No additional parameters found here. You can enter a property name."
                            } else {
                                "Suggestions come from this constructor’s Dart parameters. You can also enter a property name."
                            },
                        );
                    }
                    Err(_) => dom::text(
                        "property-suggestion-status",
                        "Dart suggestions are unavailable. Enter the property name to continue.",
                    ),
                }
            }
            Ok(())
        });
    }
    Ok(())
}

pub async fn submit(context: &Context, version: u64, input: Value) -> Result {
    context
        .save(
            string(&input["property"]).trim(),
            input["expression"].clone(),
            false,
        )
        .await?;
    dialogs::close_current(version);
    Ok(())
}

pub async fn act(context: &Context, version: u64, action: &str) -> Result {
    let Some(name) = dom::find("#modal [name=property]").map(|field| dom::value(&field)) else {
        return Ok(());
    };
    let expression = dom::find("#modal [name=expression]")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    match action {
        "property-theme" => {
            open_design_values(name.trim().into(), String::new()).await?;
        }
        "property-remove" => {
            context.save(&name, Value::Null, true).await?;
            dialogs::close_current(version);
        }
        "property-check" => {
            dom::text("property-check", "Checking Dart…");
            let result = context
                .request("check", &name, json!({"expression":expression}))
                .await;
            if dialogs::current_view(version)
                && dom::find("#modal [name=property]")
                    .is_some_and(|field| dom::value(&field) == name)
                && dom::find("#modal [name=expression]")
                    .is_some_and(|field| dom::value(&field) == expression)
            {
                match result {
                    Ok(result) => {
                        let diagnostics = items(&result["diagnostics"]);
                        if diagnostics.is_empty() {
                            dom::text("property-check", "Dart found no problems in this file.");
                        } else {
                            let rows: String = diagnostics
                                .iter()
                                .map(|item| format!("<li>{}</li>", h(string(&item["message"]))))
                                .collect();
                            dom::html("property-check", format!("<ul>{rows}</ul>"));
                        }
                    }
                    Err(error) => dom::text("property-check", &error.message),
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn initialize() {
    values::initialize();
    dom::listen(&id("modal-body"), "input", |event| {
        if let Some(field) = dom::target(&event).filter(|field| field.id() == "asset-picker-search")
        {
            let term = dom::value(&field).to_lowercase();
            let mut matches = 0;
            for entry in dom::all("#modal [data-asset-search]") {
                let shown = entry
                    .get_attribute("data-asset-search")
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&term);
                if shown {
                    matches += 1;
                    let _ = entry.remove_attribute("hidden");
                } else {
                    dom::attr(&entry, "hidden", "");
                }
            }
            dom::hidden("asset-picker-empty", matches > 0);
        }
        if dom::target(&event).is_some_and(|field| {
            matches!(
                field.get_attribute("name").as_deref(),
                Some("expression" | "property")
            )
        }) && dom::find("#property-check").is_some()
        {
            dom::text("property-check", "");
        }
    });
    dom::listen(&id("inspector"), "input", |event| {
        if let Some(field) = dom::target(&event).filter(|field| field.id() == "property-search") {
            SEARCH.with_borrow_mut(|term| *term = dom::value(&field));
            filter();
        }
    });
    dom::listen(&id("inspector"), "click", |event| {
        if let Some(element) = dom::closest(&event, "[data-property-theme]") {
            dom::spawn(open_design_values(
                element
                    .get_attribute("data-property-theme")
                    .unwrap_or_default(),
                element
                    .get_attribute("data-design-kind")
                    .unwrap_or_default(),
            ));
        } else if let Some(element) = dom::closest(&event, "[data-property-asset]") {
            dom::spawn(open_assets(
                element
                    .get_attribute("data-property-asset")
                    .unwrap_or_default(),
                element
                    .get_attribute("data-asset-category")
                    .unwrap_or_default(),
            ));
        } else if let Some(element) = dom::closest(&event, "[data-property-value]") {
            dom::spawn(values::open(
                element
                    .get_attribute("data-property-value")
                    .unwrap_or_default(),
            ));
        } else if let Some(element) = dom::closest(&event, "[data-property-code]") {
            dom::spawn(open(
                element
                    .get_attribute("data-property-code")
                    .unwrap_or_default(),
            ));
        } else if let Some(element) = dom::closest(&event, "[data-property-reset]") {
            dom::spawn(reset(
                element
                    .get_attribute("data-property-reset")
                    .unwrap_or_default(),
            ));
        }
    });
}
