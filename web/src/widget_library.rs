use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, id, items, string},
    property_panel::Context,
    state,
};
use serde_json::{Value, json};
use std::cell::RefCell;
use web_sys::Element;

thread_local! {
    static FILTER: RefCell<(String,String)> = RefCell::default();
    static CATALOG: RefCell<Value> = const {RefCell::new(Value::Null)};
    static PARAMETERS: RefCell<Value> = const {RefCell::new(Value::Null)};
}

fn input(context: &Context) -> Value {
    json!({"path":context.path,"revision":context.revision,"id":context.widget})
}
fn caption(name: &str) -> String {
    let mut value = String::new();
    for (index, ch) in name.chars().enumerate() {
        if index == 0 {
            value.extend(ch.to_uppercase());
        } else {
            if ch.is_uppercase() {
                value.push(' ');
            }
            value.push(ch.to_ascii_lowercase());
        }
    }
    value
}
fn action(name: &str, label: &str, style: &str) -> String {
    format!(
        r#"<button type="button" class="button {style}" data-dialog-action="{name}">{label}</button>"#
    )
}

fn show(context: Context, catalog: Value) {
    CATALOG.with_borrow_mut(|value| *value = catalog.clone());
    let (query, category) = FILTER.with_borrow(Clone::clone);
    let categories: String = [
        ("", "All widgets"),
        ("Framework", "Flutter framework"),
        ("Project", "Project"),
        ("Dependencies", "Dependencies"),
    ]
    .iter()
    .map(|(value, label)| {
        format!(
            r#"<option value="{value}"{}>{label}</option>"#,
            if *value == category { " selected" } else { "" }
        )
    })
    .collect();
    let slots: String = items(&catalog["slots"])
        .iter()
        .map(|slot| {
            format!(
                r#"<option value="{}">{} · {}</option>"#,
                h(string(&slot["name"])),
                h(&caption(string(&slot["name"]))),
                h(string(&slot["expected"]))
            )
        })
        .collect();
    let loading = catalog["loading"] == true;
    let error = catalog["error"]
        .as_str()
        .map(|error| format!(r#"<p class="form-error" role="alert">{}</p>"#, h(error)))
        .unwrap_or_default();
    let warning = if catalog["truncated"] == true {
        r#"<p class="dialog-help">This library is bounded. Additional widgets can be added in the code editor.</p>"#
    } else {
        ""
    };
    let body = format!(
        r#"<div class="widget-library-controls"><label class="form-field"><span>Add to</span><select id="widget-library-slot" aria-label="Child slot in widget library">{slots}</select></label><div class="design-filters"><input id="widget-library-search" type="search" aria-label="Search widget library" placeholder="Search widgets or packages…" value="{}" autofocus><select id="widget-library-category" aria-label="Widget source">{categories}</select></div></div>{error}<div id="widget-library-results" class="widget-library-results">{}</div><p id="widget-library-count" class="dialog-help"></p>{warning}<div class="form-footer"><button type="button" class="button outline" data-dialog-action="library-refresh"{}>Refresh library</button></div>"#,
        h(&query),
        if loading {
            "<p class=\"dialog-help\" role=\"status\">Reading widgets and checking child slots…</p>"
        } else {
            ""
        },
        if loading { " disabled" } else { "" }
    );
    dialogs::open(
        View::WidgetLibrary { context, catalog },
        "Add widget",
        "Choose a widget, then configure its constructor.",
        body,
    );
    if !loading {
        filter();
    }
}

async fn load(context: Context, refresh: bool) -> Result {
    show(context.clone(), json!({"loading":true}));
    let version = dialogs::version();
    let mut request = input(&context);
    request["refresh"] = json!(refresh);
    let result = dom::project_post(&context.project, "widget-library-targets", request).await;
    if !dialogs::current_view(version) || !context.current() {
        return Ok(());
    }
    match result {
        Ok(catalog) if items(&catalog["slots"]).is_empty() => show(
            context,
            json!({"error":"This widget has no empty child slot. Select its parent or wrap the widget."}),
        ),
        Ok(catalog) => show(context, catalog),
        Err(error) => show(context, json!({"error":error.message})),
    }
    Ok(())
}

pub async fn open() -> Result {
    load(Context::capture().await?, false).await
}

fn filter() {
    let query = dom::find("#widget-library-search")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    let category = dom::find("#widget-library-category")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    let slot = dom::find("#widget-library-slot")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    FILTER.with_borrow_mut(|value| *value = (query.clone(), category.clone()));
    let query = query.to_lowercase();
    let (rows,count)=CATALOG.with_borrow(|catalog| {
        let expected=items(&catalog["slots"]).iter().find(|value|value["name"]==slot).map(|value|string(&value["expected"])).unwrap_or("Widget");
        let entries:Vec<_>=items(&catalog["entries"]).iter().enumerate().filter(|(_,entry)|{
            (category.is_empty()||entry["category"]==category)&&(expected=="Widget"||items(&entry["types"]).iter().any(|kind|kind==expected))&&format!("{} {} {}",string(&entry["name"]),string(&entry["package"]),string(&entry["path"])).to_lowercase().contains(&query)
        }).collect();
        let rows:String=entries.iter().take(80).map(|(index,entry)|format!(r#"<button type="button" class="design-value-row widget-library-choice" data-dialog-action="library-choice-{index}">{}<span><strong>{}</strong><small>{} · {}</small><code>{}</code></span><span class="design-value-action">Configure</span></button>"#,dom::icon(if entry["category"]=="Project"{"component"}else{"flutter"},17),h(string(&entry["name"])),h(string(&entry["category"])),h(string(&entry["package"])),h(string(&entry["signature"])))).collect();
        (rows,entries.len())
    });
    dom::html(
        "widget-library-results",
        if count == 0 {
            r#"<p class="dialog-help">No compatible widgets match these filters.</p>"#.to_owned()
        } else {
            rows
        },
    );
    dom::text(
        "widget-library-count",
        &if count > 80 {
            format!("Showing 80 of {count} compatible widgets. Narrow the search to see more.")
        } else {
            format!("{count} compatible widgets")
        },
    );
}

fn current_definition(entry: &Value) -> Result {
    let path = string(&entry["projectPath"]);
    if !path.is_empty() && state::read(|state| state.drafts.contains_key(&state.draft_key(path))) {
        return Err(Error::new(
            "Save the component's code draft before using this widget.",
        ));
    }
    Ok(())
}

pub async fn act(
    context: &Context,
    catalog: &Value,
    version: u64,
    name: &str,
    element: &Element,
) -> Result {
    if !context.current() {
        return Err(Error::new("Select the parent widget again."));
    }
    if name == "library-refresh" {
        return load(context.clone(), true).await;
    }
    let Some(index) = name
        .strip_prefix("library-choice-")
        .and_then(|index| index.parse::<usize>().ok())
    else {
        return Ok(());
    };
    let Some(entry) = items(&catalog["entries"]).get(index) else {
        return Ok(());
    };
    current_definition(entry)?;
    let slot = dom::value(&id("widget-library-slot"));
    let mut request = input(context);
    request["entryId"] = entry["id"].clone();
    request["entryRevision"] = entry["revision"].clone();
    let old = element.inner_html();
    dom::attr(element, "disabled", "");
    element.set_text_content(Some("Loading constructor…"));
    let result = dom::project_post(&context.project, "widget-library-options", request).await;
    let _ = element.remove_attribute("disabled");
    element.set_inner_html(&old);
    let config = result?;
    if dialogs::current_view(version) && context.current() {
        constructor(context.clone(), catalog.clone(), config, slot);
    }
    Ok(())
}

fn modes(parameter: &Value, selected: &str) -> String {
    let mut modes = vec![];
    if parameter["required"] != true {
        modes.push(("omit", "Use constructor default"));
    }
    match string(&parameter["kind"]).trim_end_matches('?') {
        "String" => modes.push(("string", "Text")),
        "bool" => modes.push(("bool", "Boolean")),
        "int" | "num" | "double" => modes.push(("number", "Number")),
        "Widget" | "PreferredSizeWidget" => {
            modes.push(("default", "Suggested widget"));
            modes.push(("empty", "Empty widget"));
        }
        _ if parameter["suggestion"]["mode"] == "default" => {
            modes.push(("default", "Suggested value"))
        }
        _ => {}
    }
    modes.push(("dart", "Dart expression"));
    modes
        .iter()
        .map(|(value, label)| {
            format!(
                r#"<option value="{value}"{}>{label}</option>"#,
                if *value == selected { " selected" } else { "" }
            )
        })
        .collect()
}

fn argument_input(parameter: &Value, index: usize, mode: &str, value: &str) -> String {
    let label = h(&caption(string(&parameter["name"])));
    match mode {
        "omit" => r#"<small>The constructor's default value is kept.</small>"#.into(),
        "default" => r#"<small>A value suitable for this parameter is supplied.</small>"#.into(),
        "empty" => r#"<small>An empty placeholder widget is supplied.</small>"#.into(),
        "bool" => format!(
            r#"<select id="widget-argument-value-{index}" aria-label="{label}"><option value="false"{}>False</option><option value="true"{}>True</option></select>"#,
            if value != "true" { " selected" } else { "" },
            if value == "true" { " selected" } else { "" }
        ),
        "dart" => format!(
            r#"<textarea id="widget-argument-value-{index}" aria-label="{label}" rows="2" maxlength="8192" placeholder="Dart expression in the target file">{}</textarea>"#,
            h(value)
        ),
        _ => format!(
            r#"<input id="widget-argument-value-{index}" aria-label="{label}" type="{}" {} value="{}" maxlength="4096">"#,
            if mode == "number" { "number" } else { "text" },
            if mode == "number" {
                r#"step="any""#
            } else {
                ""
            },
            h(value)
        ),
    }
}

fn parameter_row(parameter: &Value, index: usize) -> String {
    let mode = string(&parameter["argument"]["mode"]);
    let value = dom::display(&parameter["argument"]["value"]);
    format!(
        r#"<div class="form-field library-parameter"><span><strong>{}</strong>{}<small>{}</small></span><select id="widget-argument-mode-{index}" data-library-argument="{index}" aria-label="Value type for {}">{}</select><div id="widget-argument-input-{index}" class="form-field">{}</div></div>"#,
        h(&caption(string(&parameter["name"]))),
        if parameter["required"] == true {
            r#"<span class="required-label">Required</span>"#
        } else {
            ""
        },
        h(string(&parameter["kind"])),
        h(&caption(string(&parameter["name"]))),
        modes(parameter, mode),
        argument_input(parameter, index, mode, &value)
    )
}

fn constructor(context: Context, catalog: Value, config: Value, slot: String) {
    PARAMETERS.with_borrow_mut(|value| *value = config["parameters"].clone());
    let parameters = items(&config["parameters"]);
    let required: String = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter["required"] == true)
        .map(|(index, parameter)| parameter_row(parameter, index))
        .collect();
    let optional: String = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter["required"] != true)
        .map(|(index, parameter)| parameter_row(parameter, index))
        .collect();
    let optional = if optional.is_empty() {
        String::new()
    } else {
        format!(
            r#"<details class="library-optional"><summary>Optional parameters</summary>{optional}</details>"#
        )
    };
    let body = format!(
        r#"<form class="form widget-constructor-form"><p class="dialog-help">Add to {}. Dart checks this change before saving. Imports and the new widget undo together.</p>{required}{optional}<div id="widget-library-check" class="dialog-help" role="status"></div><div class="form-error" role="alert"></div><div class="form-footer">{}{}<button type="submit" class="button primary">Insert widget</button></div></form>"#,
        h(&caption(&slot)),
        action("library-back", "Back", "soft"),
        action("library-check", "Check Dart", "outline")
    );
    let title = format!("Add {}", string(&config["entry"]["name"]));
    dialogs::open(
        View::WidgetConstructor {
            context,
            catalog,
            config,
            slot,
        },
        &title,
        "Configure the widget's constructor parameters.",
        body,
    );
}

fn arguments(config: &Value) -> Value {
    let mut arguments = serde_json::Map::new();
    for (index, parameter) in items(&config["parameters"]).iter().enumerate() {
        let mode = dom::value(&id(&format!("widget-argument-mode-{index}")));
        let value = dom::find(&format!("#widget-argument-value-{index}"))
            .map(|field| dom::value(&field))
            .unwrap_or_default();
        arguments.insert(
            string(&parameter["name"]).into(),
            json!({"mode":mode,"value":value}),
        );
    }
    Value::Object(arguments)
}

fn request(context: &Context, config: &Value, slot: &str) -> Value {
    let mut input = input(context);
    input["entryId"] = config["entry"]["id"].clone();
    input["entryRevision"] = config["entry"]["revision"].clone();
    input["signatureRevision"] = config["signatureRevision"].clone();
    input["slot"] = json!(slot);
    input["arguments"] = arguments(config);
    input
}

pub async fn value_action(
    context: &Context,
    catalog: &Value,
    config: &Value,
    slot: &str,
    version: u64,
    name: &str,
) -> Result {
    if name == "library-back" {
        show(context.clone(), catalog.clone());
        return Ok(());
    }
    if name != "library-check" {
        return Ok(());
    }
    if !context.current() {
        return Err(Error::new("Select the parent widget again."));
    }
    dom::text("widget-library-check", "Checking Dart…");
    let result = dom::project_post(
        &context.project,
        "widget-library-check",
        request(context, config, slot),
    )
    .await;
    if dialogs::current_view(version) {
        match result {
            Ok(result) => {
                let errors: Vec<_> = items(&result["diagnostics"])
                    .iter()
                    .filter(|diagnostic| diagnostic["severity"] == 1)
                    .take(5)
                    .map(|diagnostic| string(&diagnostic["message"]))
                    .collect();
                dom::text(
                    "widget-library-check",
                    &if errors.is_empty() {
                        "Dart found no errors in this file.".into()
                    } else {
                        errors.join("\n")
                    },
                );
            }
            Err(error) => dom::text("widget-library-check", &error.message),
        }
    }
    Ok(())
}

pub async fn save(context: &Context, config: &Value, slot: &str, version: u64) -> Result {
    if !context.current() {
        return Err(Error::new("Select the parent widget again."));
    }
    crate::visual_context()?;
    current_definition(&config["entry"])?;
    let result = dom::project_post(
        &context.project,
        "widget-library-insert",
        request(context, config, slot),
    )
    .await?;
    if context.current() {
        state::change(|state| {
            state.widget = string(&result["widget"]).into();
            state.rendered.show = false;
        });
        crate::load_file(&context.path, true).await?;
    }
    dialogs::close_current(version);
    dom::toast("Widget inserted", false);
    Ok(())
}

pub fn initialize() {
    dom::listen(&id("modal-body"), "input", |event| {
        if dom::target(&event).is_some_and(|target| target.id() == "widget-library-search") {
            filter();
        }
    });
    dom::listen(&id("modal-body"), "change", |event| {
        let Some(target) = dom::target(&event) else {
            return;
        };
        if matches!(
            target.id().as_str(),
            "widget-library-category" | "widget-library-slot"
        ) {
            filter();
            return;
        }
        let Some(index) = target
            .get_attribute("data-library-argument")
            .and_then(|index| index.parse::<usize>().ok())
        else {
            return;
        };
        let parameter = PARAMETERS.with_borrow(|parameters| items(parameters).get(index).cloned());
        let Some(parameter) = parameter else {
            return;
        };
        let mode = dom::value(&target);
        let value = dom::find(&format!("#widget-argument-value-{index}"))
            .map(|field| dom::value(&field))
            .unwrap_or_else(|| dom::display(&parameter["suggestion"]["value"]));
        dom::html(
            &format!("widget-argument-input-{index}"),
            argument_input(&parameter, index, &mode, &value),
        );
    });
}
