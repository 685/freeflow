use crate::{
    dialogs::{self, View},
    dom::{self, Error, Result, escape as h, id, items, string},
    property_panel::Context,
    state,
};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlInputElement};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Insert,
    Wrap,
}

#[derive(Clone, Default)]
struct Filter {
    query: String,
    category: String,
    all: bool,
}

thread_local! {
    static FILTERS: RefCell<[Filter; 2]> = RefCell::default();
    static CATALOG: RefCell<Option<(Operation, Rc<Value>)>> = const { RefCell::new(None) };
    static PARAMETERS: RefCell<Value> = const {RefCell::new(Value::Null)};
}

fn input(context: &Context, operation: Operation) -> Value {
    json!({
        "path": context.path, "revision": context.revision, "id": context.widget,
        "operation": if operation == Operation::Wrap { "wrap" } else { "insert" }
    })
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

fn show(context: Context, operation: Operation, catalog: Rc<Value>) {
    let wrapping = operation == Operation::Wrap;
    CATALOG.with_borrow_mut(|value| *value = Some((operation, catalog.clone())));
    let filter_state = FILTERS.with_borrow(|filters| filters[usize::from(wrapping)].clone());
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
            if *value == filter_state.category {
                " selected"
            } else {
                ""
            }
        )
    })
    .collect();
    let loading = catalog["loading"] == true;
    let error = string(&catalog["error"]);
    let controls = if wrapping {
        format!(
            r#"<label class="check-row"><input type="checkbox" id="widget-wrapper-all"{}>Include other constructors</label>"#,
            if filter_state.all { " checked" } else { "" }
        )
    } else {
        slot_select(
            items(&catalog["slots"]),
            "widget-library-slot",
            "Add to",
            "Child slot in widget library",
        )
    };
    let fallback = if wrapping
        && !loading
        && catalog["basicWraps"] == true
        && !items(&catalog["entries"])
            .iter()
            .any(|entry| entry["category"] == "Framework")
    {
        action("library-basic", "Basic wrappers", "soft")
    } else {
        String::new()
    };
    let warning = if catalog["truncated"] == true {
        r#"<p class="dialog-help">This library is bounded. Additional widgets can be added in the code editor.</p>"#
    } else {
        ""
    };
    let body = format!(
        r#"<div class="widget-library-controls">{controls}<div class="design-filters"><input id="widget-library-search" type="search" aria-label="Search widget library" placeholder="Search widgets or packages…" value="{}" autofocus><select id="widget-library-category" aria-label="Widget source">{categories}</select></div></div><div id="widget-library-problem" class="form-error" role="alert">{}</div><div id="widget-library-results" class="widget-library-results">{}</div><p id="widget-library-count" class="dialog-help"></p>{warning}<div class="form-footer">{fallback}<button type="button" class="button outline" data-dialog-action="library-refresh"{}>Refresh library</button></div>"#,
        h(&filter_state.query),
        h(error),
        if loading {
            "<p class=\"dialog-help\" role=\"status\">Reading widgets and checking child slots…</p>"
        } else {
            ""
        },
        if loading { " disabled" } else { "" }
    );
    let child = string(&catalog["childType"]);
    let (title, description) = if wrapping {
        let title = if child.is_empty() {
            "Wrap widget".to_owned()
        } else {
            format!("Wrap {child}")
        };
        let description = if let Some(kind) = catalog["wrapperType"]
            .as_str()
            .filter(|kind| *kind != "Widget")
        {
            format!("Choose a wrapper that produces a {kind}.")
        } else {
            "Choose a wrapper for the selected widget.".to_owned()
        };
        (title, description)
    } else {
        (
            "Add widget".into(),
            "Choose a widget, then configure its constructor.".into(),
        )
    };
    let ready = !loading && error.is_empty();
    dialogs::open(
        View::WidgetLibrary {
            context,
            operation,
            catalog,
        },
        &title,
        &description,
        body,
    );
    if ready {
        filter();
    }
}

fn slot_select(slots: &[Value], id: &str, label: &str, accessible_name: &str) -> String {
    let options: String = slots
        .iter()
        .map(|slot| {
            let name = string(&slot["name"]);
            let kind = string(&slot["expected"]);
            let display = if slot["multiple"] == true {
                format!("List of {kind}")
            } else {
                kind.to_owned()
            };
            format!(
                r#"<option value="{}">{} · {}</option>"#,
                h(name),
                h(&caption(name)),
                h(&display)
            )
        })
        .collect();
    format!(
        r#"<label class="form-field"><span>{label}</span><select id="{id}" aria-label="{accessible_name}">{options}</select></label>"#
    )
}

async fn load(context: Context, operation: Operation, refresh: bool) -> Result {
    show(context.clone(), operation, Rc::new(json!({"loading":true})));
    let version = dialogs::version();
    let mut request = input(&context, operation);
    request["refresh"] = json!(refresh);
    let result = dom::project_post(&context.project, "widget-library-targets", request).await;
    if !dialogs::current_view(version) || !context.current() {
        return Ok(());
    }
    let mut catalog = match result {
        Ok(catalog) if operation == Operation::Insert && items(&catalog["slots"]).is_empty() => {
            json!({"error":"This widget has no empty child slot. Select its parent or wrap the widget."})
        }
        Ok(catalog) => catalog,
        Err(error) => json!({"error":error.message}),
    };
    catalog["basicWraps"] = state::read(|state| {
        state
            .widget()
            .is_some_and(|node| node["wrappable"] != false)
    })
    .into();
    show(context, operation, Rc::new(catalog));
    Ok(())
}

pub async fn open(operation: Operation) -> Result {
    load(Context::capture().await?, operation, false).await
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
    let all = dom::find("#widget-wrapper-all")
        .and_then(|field| field.dyn_into::<HtmlInputElement>().ok())
        .is_some_and(|field| field.checked());
    let Some((operation, catalog)) = CATALOG.with_borrow(Clone::clone) else {
        return;
    };
    let wrapping = operation == Operation::Wrap;
    FILTERS.with_borrow_mut(|filters| {
        filters[usize::from(wrapping)] = Filter {
            query: query.clone(),
            category: category.clone(),
            all,
        };
    });
    if catalog["loading"] == true || catalog["error"].is_string() {
        return;
    }
    let query = query.trim().to_lowercase();
    let expected = items(&catalog["slots"])
        .iter()
        .find(|value| value["name"] == slot)
        .map(|value| string(&value["expected"]))
        .unwrap_or("Widget");
    let mut entries: Vec<_> = items(&catalog["entries"])
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let category_matches = category.is_empty() || entry["category"] == category;
            let type_matches =
                expected == "Widget" || items(&entry["types"]).iter().any(|kind| kind == expected);
            let suggested = !wrapping || all || entry["wrapperHint"] == true;
            let search = format!(
                "{} {} {}",
                string(&entry["name"]),
                string(&entry["package"]),
                string(&entry["path"])
            );
            category_matches && type_matches && suggested && search.to_lowercase().contains(&query)
        })
        .collect();
    if wrapping {
        entries.sort_by_key(|(_, entry)| {
            let name = string(&entry["name"]);
            let kind = string(&entry["className"]);
            (
                !name.eq_ignore_ascii_case(&query),
                match name {
                    "Padding" => 0,
                    "Center" => 1,
                    "Container" => 2,
                    "Align" => 3,
                    "SizedBox" => 4,
                    "Opacity" => 5,
                    _ if crate::modifier_kind::is_modifier(kind) => 6,
                    _ if entry["wrapperHint"] == true => 7,
                    _ => 8,
                },
            )
        });
    }
    let count = entries.len();
    let rows: String = entries.into_iter().take(80).map(|(index, entry)| {
        let icon = if entry["category"] == "Project" { "component" } else { "flutter" };
        format!(
            r#"<button type="button" class="design-value-row widget-library-choice" data-dialog-action="library-choice-{index}">{}<span><strong>{}</strong><small>{} · {}</small><code>{}</code></span><span class="design-value-action">Configure</span></button>"#,
            dom::icon(icon, 17), h(string(&entry["name"])), h(string(&entry["category"])),
            h(string(&entry["package"])), h(string(&entry["signature"]))
        )
    }).collect();
    dom::html(
        "widget-library-results",
        if count == 0 {
            r#"<p class="dialog-help">No constructors match these filters.</p>"#.to_owned()
        } else {
            rows
        },
    );
    dom::text(
        "widget-library-count",
        &if count > 80 {
            format!("Showing 80 of {count} matches. Narrow the search to see more.")
        } else {
            format!("{count} matches")
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
    operation: Operation,
    catalog: Rc<Value>,
    version: u64,
    name: &str,
    element: &Element,
) -> Result {
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    if name == "library-refresh" {
        return load(context.clone(), operation, true).await;
    }
    if name == "library-basic" && catalog["basicWraps"] == true {
        if let Some(node) = state::read(|state| state.widget().cloned()) {
            dialogs::wrap(node);
        }
        return Ok(());
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
    let slot = dom::find("#widget-library-slot")
        .map(|field| dom::value(&field))
        .unwrap_or_default();
    let mut request = input(context, operation);
    request["entryId"] = entry["id"].clone();
    request["entryRevision"] = entry["revision"].clone();
    dom::text("widget-library-problem", "");
    let old = element.inner_html();
    dom::attr(element, "disabled", "");
    element.set_text_content(Some("Loading constructor…"));
    let result = dom::project_post(&context.project, "widget-library-options", request).await;
    let _ = element.remove_attribute("disabled");
    element.set_inner_html(&old);
    if dialogs::current_view(version) && context.current() {
        match result {
            Ok(config) => constructor(context.clone(), operation, catalog, config, slot),
            Err(error) => dom::text("widget-library-problem", &error.message),
        }
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

fn parameter_row(parameter: &Value, index: usize, child_slot: &str) -> String {
    let mode = string(&parameter["argument"]["mode"]);
    let value = dom::display(&parameter["argument"]["value"]);
    format!(
        r#"<div class="form-field library-parameter" data-library-parameter="{}"{}><span><strong>{}</strong>{}<small>{}</small></span><select id="widget-argument-mode-{index}" data-library-argument="{index}" aria-label="Value type for {}">{}</select><div id="widget-argument-input-{index}" class="form-field">{}</div></div>"#,
        h(string(&parameter["name"])),
        if parameter["name"] == child_slot {
            " hidden"
        } else {
            ""
        },
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

fn constructor(
    context: Context,
    operation: Operation,
    catalog: Rc<Value>,
    config: Value,
    slot: String,
) {
    PARAMETERS.with_borrow_mut(|value| *value = config["parameters"].clone());
    let wrapping = operation == Operation::Wrap;
    let child_slot = if wrapping {
        items(&config["slots"])
            .first()
            .map_or("", |slot| string(&slot["name"]))
    } else {
        ""
    };
    let child = if wrapping {
        slot_select(
            items(&config["slots"]),
            "widget-wrapper-child",
            "Put selected widget in",
            "Wrapper child parameter",
        )
    } else {
        String::new()
    };
    let parameters = items(&config["parameters"]);
    let required: String = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter["required"] == true)
        .map(|(index, parameter)| parameter_row(parameter, index, child_slot))
        .collect();
    let optional: String = parameters
        .iter()
        .enumerate()
        .filter(|(_, parameter)| parameter["required"] != true)
        .map(|(index, parameter)| parameter_row(parameter, index, child_slot))
        .collect();
    let optional = if optional.is_empty() {
        String::new()
    } else {
        format!(
            r#"<details class="library-optional"><summary>Optional parameters</summary>{optional}</details>"#
        )
    };
    let intent = if wrapping {
        format!(
            "Wrap {}. Dart checks the change before saving.",
            h(string(&catalog["childType"]))
        )
    } else {
        format!(
            "Add to {}. Dart checks the change before saving.",
            h(&caption(&slot))
        )
    };
    let submit = if wrapping {
        "Wrap widget"
    } else {
        "Insert widget"
    };
    let body = format!(
        r#"<form class="form widget-constructor-form"><p class="dialog-help">{intent}</p>{child}{required}{optional}<div id="widget-library-check" class="dialog-help" role="status"></div><div class="form-error" role="alert"></div><div class="form-footer">{}{}<button type="submit" class="button primary">{submit}</button></div></form>"#,
        action("library-back", "Back", "soft"),
        action("library-check", "Check Dart", "outline")
    );
    let name = string(&config["entry"]["name"]);
    let title = if wrapping {
        format!("Wrap with {name}")
    } else {
        format!("Add {name}")
    };
    dialogs::open(
        View::WidgetConstructor {
            context,
            operation,
            catalog,
            config,
            slot,
        },
        &title,
        "Configure the widget's constructor parameters.",
        body,
    );
}

fn arguments(config: &Value, child_slot: Option<&str>) -> Value {
    let mut arguments = serde_json::Map::new();
    for (index, parameter) in items(&config["parameters"]).iter().enumerate() {
        let name = string(&parameter["name"]);
        if Some(name) == child_slot {
            continue;
        }
        let mode = dom::value(&id(&format!("widget-argument-mode-{index}")));
        let value = dom::find(&format!("#widget-argument-value-{index}"))
            .map(|field| dom::value(&field))
            .unwrap_or_default();
        arguments.insert(name.into(), json!({"mode":mode,"value":value}));
    }
    Value::Object(arguments)
}

fn request(context: &Context, operation: Operation, config: &Value, slot: &str) -> Value {
    let child = (operation == Operation::Wrap).then(|| dom::value(&id("widget-wrapper-child")));
    let mut input = input(context, operation);
    input["entryId"] = config["entry"]["id"].clone();
    input["entryRevision"] = config["entry"]["revision"].clone();
    input["signatureRevision"] = config["signatureRevision"].clone();
    input["slot"] = json!(child.as_deref().unwrap_or(slot));
    input["arguments"] = arguments(config, child.as_deref());
    input
}

pub async fn value_action(
    context: &Context,
    operation: Operation,
    catalog: Rc<Value>,
    config: &Value,
    slot: &str,
    version: u64,
    name: &str,
) -> Result {
    if name == "library-back" {
        show(context.clone(), operation, catalog);
        return Ok(());
    }
    if name != "library-check" {
        return Ok(());
    }
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    dom::text("widget-library-check", "Checking Dart…");
    let proposed = request(context, operation, config, slot);
    let result =
        dom::project_post(&context.project, "widget-library-check", proposed.clone()).await;
    if dialogs::current_view(version)
        && context.current()
        && proposed == request(context, operation, config, slot)
    {
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

pub async fn save(
    context: &Context,
    operation: Operation,
    config: &Value,
    slot: &str,
    version: u64,
) -> Result {
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    crate::visual_context()?;
    current_definition(&config["entry"])?;
    let route = if operation == Operation::Wrap {
        "widget-library-wrap"
    } else {
        "widget-library-insert"
    };
    let result = dom::project_post(
        &context.project,
        route,
        request(context, operation, config, slot),
    )
    .await?;
    if context.current() {
        crate::preview_inspector::show_tree(false);
        crate::load_file(&context.path, true).await?;
        state::change(|state| {
            let selected = string(&result["widget"]);
            if state.project_id() == context.project
                && state.generation == context.generation
                && state.visual["path"] == context.path
                && state.visual["revision"] == result["source"]["revision"]
                && state.node(selected).is_some()
            {
                state.widget = selected.into();
                state.reveal_source_widget();
                crate::view::explorer(state);
                crate::view::inspector(state);
            }
        });
    }
    dialogs::close_current(version);
    dom::toast(
        if operation == Operation::Wrap {
            "Widget wrapped"
        } else {
            "Widget inserted"
        },
        false,
    );
    Ok(())
}

pub fn initialize() {
    dom::listen(&id("modal-body"), "input", |event| {
        if dom::target(&event).is_some_and(|target| target.id() == "widget-library-search") {
            filter();
        }
        if dom::find(".widget-constructor-form").is_some() {
            dom::text("widget-library-check", "");
        }
    });
    dom::listen(&id("modal-body"), "change", |event| {
        let Some(target) = dom::target(&event) else {
            return;
        };
        if matches!(
            target.id().as_str(),
            "widget-library-category" | "widget-library-slot" | "widget-wrapper-all"
        ) {
            filter();
            return;
        }
        if target.id() == "widget-wrapper-child" {
            let selected = dom::value(&target);
            for row in dom::all("[data-library-parameter]") {
                let hidden = row.get_attribute("data-library-parameter").as_deref()
                    == Some(selected.as_str());
                if hidden {
                    dom::attr(&row, "hidden", "");
                } else {
                    let _ = row.remove_attribute("hidden");
                }
            }
            dom::text("widget-library-check", "");
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
        dom::text("widget-library-check", "");
    });
}
