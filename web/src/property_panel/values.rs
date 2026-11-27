use super::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

struct Frame {
    label: String,
    kind: String,
    model: Value,
    parent: Option<(usize, &'static str)>,
}

#[derive(Default)]
struct Edit {
    context: Option<Context>,
    name: String,
    parameters: Value,
    removable: bool,
    frames: Vec<Frame>,
    render_version: u64,
}

thread_local! { static EDIT: RefCell<Edit> = RefCell::default(); }

fn button(action: &str, label: &str) -> String {
    format!(
        r#"<button type="button" class="button outline small" data-dialog-action="{action}">{label}</button>"#
    )
}

pub(crate) async fn open(name: String) -> Result {
    let context = Context::capture().await?;
    EDIT.with_borrow_mut(|edit| {
        *edit = Edit {
            context: Some(context.clone()),
            name: name.clone(),
            ..Edit::default()
        }
    });
    let title = if name.is_empty() {
        "Add property"
    } else {
        "Edit property value"
    };
    dialogs::open(View::PropertyValue { context: context.clone() }, title, "",
        r#"<div id="value-editor"><p id="value-status" class="dialog-help" role="status">Reading Dart parameters…</p></div>"#.into());
    let version = dialogs::version();
    let parameters = context.request("parameters", "", Value::Null).await?;
    if !dialogs::current_view(version) || !context.current() {
        return Ok(());
    }
    EDIT.with_borrow_mut(|edit| edit.parameters = parameters["parameters"].clone());
    if name.is_empty() {
        let options: String = items(&parameters["parameters"])
            .iter()
            .filter(|item| item["named"] == true)
            .map(|item| {
                let name = string(&item["name"]);
                format!(
                    r#"<option value="{}">{} · {}</option>"#,
                    h(name),
                    h(name),
                    h(string(&item["kind"]))
                )
            })
            .collect();
        let picker = if options.is_empty() {
            r#"<p class="dialog-help">Enter the property name in the Dart editor.</p>"#.into()
        } else {
            format!(
                r#"<label class="form-field"><span>Property</span><select id="value-property">{options}</select></label>{}"#,
                button("value-choose", "Edit property")
            )
        };
        dom::html(
            "value-editor",
            format!(
                r#"<div class="form">{picker}<div class="form-footer">{}</div><div id="value-status" class="property-check" role="status"></div></div>"#,
                button("value-code", "Open Dart editor")
            ),
        );
        Ok(())
    } else {
        choose(&context, version, name).await
    }
}

async fn choose(context: &Context, version: u64, name: String) -> Result {
    let kind = EDIT.with_borrow(|edit| {
        items(&edit.parameters)
            .iter()
            .find(|item| item["name"] == name && item["named"] == true)
            .map_or("", |item| string(&item["kind"]))
            .to_owned()
    });
    dom::text("value-status", "Reading property value…");
    let model = context
        .request("value-model", &name, json!({"dartType":kind}))
        .await?;
    if dialogs::current_view(version)
        && context.current()
        && dom::find("#value-property").is_none_or(|field| dom::value(&field) == name)
    {
        EDIT.with_borrow_mut(|edit| {
            edit.name = name.clone();
            edit.removable = model["removable"] == true;
            edit.frames = vec![Frame {
                label: name,
                kind,
                model: model["model"].clone(),
                parent: None,
            }];
        });
        render(true);
    }
    Ok(())
}

fn scalar(frame: &Frame) -> String {
    let expression = string(&frame.model["expression"]);
    let value = dom::display(&frame.model["value"]);
    let label = h(&frame.label);
    let field = match string(&frame.model["mode"]) {
        "text" => format!(
            r#"<textarea id="value-scalar" rows="2" maxlength="65536">{}</textarea>"#,
            h(&value)
        ),
        "number" => format!(
            r#"<input id="value-scalar" type="number" step="any" value="{}">"#,
            h(&value)
        ),
        "boolean" => format!(
            r#"<select id="value-scalar"><option value="false"{}>False</option><option value="true"{}>True</option></select>"#,
            if value == "true" { "" } else { " selected" },
            if value == "true" { " selected" } else { "" }
        ),
        "color" => {
            let rgb = if value.len() == 9 {
                format!("#{}", &value[3..])
            } else {
                value.clone()
            };
            return format!(
                r##"<label class="form-field"><span>{label}</span><span class="color-input"><input type="color" id="value-color-picker" aria-label="Choose color" value="{}"><input type="text" id="value-scalar" aria-label="Hex color" value="{}" maxlength="9" placeholder="#RRGGBB or #AARRGGBB"></span></label>"##,
                h(&rgb),
                h(&value)
            );
        }
        "enum" => {
            let options: String = items(&frame.model["choices"])
                .iter()
                .map(|choice| {
                    let code = string(choice);
                    format!(
                        r#"<option value="{}"{}>{}</option>"#,
                        h(code),
                        if code == expression { " selected" } else { "" },
                        h(code.rsplit('.').next().unwrap_or(code))
                    )
                })
                .collect();
            format!(
                r#"<select id="value-scalar"><option value="">Choose a value</option>{options}</select>"#
            )
        }
        "icon" => {
            let options = icon_options(&frame.model["choices"]);
            format!(
                r#"<input id="value-scalar" type="search" list="value-icon-options" value="{}" placeholder="Search icons" maxlength="128"><datalist id="value-icon-options">{options}</datalist>"#,
                h(&value)
            )
        }
        _ => format!(
            r#"<textarea id="value-scalar" rows="6" maxlength="65536">{}</textarea>"#,
            h(expression)
        ),
    };
    let label = if frame.model["mode"] == "dart" {
        "Dart expression".into()
    } else {
        label
    };
    format!(r#"<label class="form-field"><span>{label}</span>{field}</label>"#)
}

fn icon_options(choices: &Value) -> String {
    items(choices)
        .iter()
        .map(|choice| format!(r#"<option value="{}"></option>"#, h(string(choice))))
        .collect()
}

fn structured(frame: &Frame) -> String {
    let model = &frame.model;
    let mode = string(&model["mode"]);
    let rows = items(&model["rows"]);
    let body: String = rows.iter().enumerate().map(|(index,row)| {
        let n = index + 1;
        let row_label = h(string(&row["label"]));
        let label = if mode == "call" {
            format!(r#"<label class="form-field"><span>Parameter name</span><input type="text" maxlength="180" id="value-row-label-{index}" value="{row_label}"></label>"#)
        } else if row["label"].is_string() {
            format!(r#"<label class="form-field"><span>Key expression</span><textarea rows="1" maxlength="8192" id="value-row-label-{index}">{row_label}</textarea></label>{}"#,
                button(&format!("value-key-{index}"),"Edit key"))
        } else { String::new() };
        let moves: String = [("up", "Move up", index > 0), ("down", "Move down", n < rows.len()), ("remove", "Remove", true)].iter().map(|(action, label, enabled)| {
            format!(r#"<button type="button" class="button soft small" data-dialog-action="value-{action}-{index}" aria-label="{label} item {n}"{}>{label}</button>"#,
                if *enabled { "" } else { " disabled" })
        }).collect();
        format!(r#"<fieldset class="value-row"><legend>Item {n}</legend>{label}<label class="form-field"><span>Value</span><textarea id="value-row-code-{index}" rows="2" maxlength="65536">{}</textarea></label><div class="value-row-tools">{}{moves}</div></fieldset>"#,
            h(string(&row["value"])),button(&format!("value-nested-{index}"),"Edit value"))
    }).collect();
    let add = match mode {
        "call" => "Add argument",
        "map" => "Add entry",
        _ => "Add item",
    };
    format!(
        r#"<div class="value-rows"><code class="value-shape-head">{}</code>{body}{}<code>{}</code></div>"#,
        h(string(&model["head"])),
        button("value-add", add),
        h(string(&model["tail"]).trim())
    )
}

fn render(root: bool) {
    let content = EDIT.with_borrow_mut(|edit| {
        edit.render_version += 1;
        let frame = edit.frames.last().unwrap();
        let nested = frame.parent.is_some();
        let fields = if frame.model["rows"].is_array() { structured(frame) } else { scalar(frame) };
        let mode = if frame.model["mode"] == "dart" {
            button("value-visualize","Use visual editor")
        } else { button("value-dart","Dart expression") };
        let controls = if nested { button("value-back","Back") }
            else {
                format!("{}{}",button("value-check","Check Dart"),
                    if edit.removable { button("value-reset","Reset property") } else { String::new() })
            };
        let save = if nested { "Apply item" } else { "Save property" };
        let path = edit.frames.iter().map(|frame| h(&frame.label)).collect::<Vec<_>>().join(" / ");
        let kind = h(&frame.kind);
        format!(r#"<p class="value-caption"><strong>{path}</strong><code>{kind}</code></p>{fields}<div id="value-status" class="property-check" role="status"></div><div class="form-error" role="alert"></div><div class="form-footer">{mode}{controls}<button type="submit" class="button primary">{save}</button></div>"#)
    });
    if root {
        dom::html(
            "value-editor",
            format!(r#"<form id="value-form" class="form">{content}</form>"#),
        );
    } else {
        dom::html("value-form", content);
    }
}

fn field(name: &str) -> String {
    dom::find(&format!("#{name}"))
        .map(|field| dom::value(&field))
        .unwrap_or_default()
}

fn update_text(slot: &mut Value, value: String) {
    if string(slot).replace("\r\n", "\n") != value {
        *slot = json!(value);
    }
}

fn read_rows(model: &mut Value) {
    if let Some(rows) = model["rows"].as_array_mut() {
        for (index, row) in rows.iter_mut().enumerate() {
            update_text(&mut row["value"], field(&format!("value-row-code-{index}")));
            if let Some(label) = dom::find(&format!("#value-row-label-{index}")) {
                update_text(&mut row["label"], dom::value(&label));
            }
        }
    }
}

fn expression(model: &mut Value) -> Result<String> {
    read_rows(model);
    if let Some(rows) = model["rows"].as_array() {
        let mut code = string(&model["head"]).to_owned();
        for (index, row) in rows.iter().enumerate() {
            let value = string(&row["value"]);
            let label = string(&row["label"]);
            if value.trim().is_empty()
                || model["mode"] == "map" && row["label"].is_string() && label.trim().is_empty()
            {
                return Err(Error::new("Fill in each item or remove the empty row."));
            }
            if index > 0 {
                code.push(',');
            }
            code.push_str(string(&row["before"]));
            if !label.trim().is_empty() {
                code.push_str(label);
                let join = string(&row["join"]);
                code.push_str(if join.is_empty() { ": " } else { join });
            }
            code.push_str(value);
            code.push_str(string(&row["after"]));
        }
        if !rows.is_empty() && model["trailingComma"] == true {
            code.push(',');
        }
        code.push_str(string(&model["tail"]));
        return Ok(code);
    }
    let value = field("value-scalar");
    let original = string(&model["expression"]);
    let mode = string(&model["mode"]);
    if !original.is_empty()
        && (mode == "dart" && value == original.replace("\r\n", "\n")
            || matches!(mode, "text" | "number" | "boolean" | "color" | "icon")
                && value == dom::display(&model["value"]).replace("\r\n", "\n"))
    {
        return Ok(original.to_owned());
    }
    match mode {
        "text" => Ok(serde_json::to_string(&value)
            .unwrap_or_default()
            .replace('$', "\\$")),
        "color" => {
            let color = value.trim().strip_prefix('#').unwrap_or(&value);
            if !matches!(color.len(), 6 | 8) || !color.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(Error::new("Enter a six- or eight-digit hex color."));
            }
            let prefix = if color.len() == 6 { "ff" } else { "" };
            Ok(format!(
                "const {}(0x{prefix}{color})",
                string(&model["colorConstructor"])
            ))
        }
        "icon"
            if !value.trim().is_empty()
                && value.trim().len() <= 128
                && value
                    .trim()
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_') =>
        {
            Ok(format!("{}.{}", string(&model["reference"]), value.trim()))
        }
        "icon" => Err(Error::new(
            "Choose an icon from the Flutter SDK suggestions.",
        )),
        _ => Ok(value),
    }
}

fn mutate(action: &str) -> Result {
    EDIT.with_borrow_mut(|edit| -> Result {
        let frame = edit.frames.last_mut().unwrap();
        read_rows(&mut frame.model);
        let mode = string(&frame.model["mode"]);
        if action == "value-add" {
            let before = frame.model["beforeNew"].clone();
            let label = if matches!(mode, "call" | "map") {
                json!("")
            } else {
                Value::Null
            };
            let rows = frame.model["rows"].as_array_mut().unwrap();
            if rows.len() >= 120 {
                return Err(Error::new("Open larger collections in the Dart editor."));
            }
            rows.push(json!({"before":before,"label":label,"value":""}));
        } else {
            let Some((operation, index)) =
                action.rsplit_once('-').and_then(|(operation, index)| {
                    index.parse::<usize>().ok().map(|index| (operation, index))
                })
            else {
                return Ok(());
            };
            let rows = frame.model["rows"].as_array_mut().unwrap();
            if index >= rows.len() {
                return Ok(());
            }
            match operation {
                "value-remove" => {
                    rows.remove(index);
                }
                "value-up" if index > 0 => rows.swap(index, index - 1),
                "value-down" if index + 1 < rows.len() => rows.swap(index, index + 1),
                _ => {}
            }
        }
        Ok(())
    })?;
    render(false);
    Ok(())
}

fn draft() -> Result<(String, String)> {
    EDIT.with_borrow_mut(|edit| {
        let frame = edit
            .frames
            .last_mut()
            .ok_or_else(|| Error::new("Choose a property first."))?;
        Ok((edit.name.clone(), expression(&mut frame.model)?))
    })
}

pub(crate) async fn act(context: &Context, version: u64, action: &str) -> Result {
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    match action {
        "value-choose" => choose(context, version, field("value-property")).await?,
        "value-code" => super::open(EDIT.with_borrow(|edit| edit.name.clone())).await?,
        "value-dart" => {
            let (_, code) = draft()?;
            EDIT.with_borrow_mut(|edit| {
                edit.frames.last_mut().unwrap().model = json!({"mode":"dart","expression":code})
            });
            render(false);
        }
        "value-visualize" => {
            let (name, code) = draft()?;
            let kind = EDIT.with_borrow(|edit| edit.frames.last().unwrap().kind.clone());
            let next = context
                .request("value-model", &name, json!({"dartType":kind,"draft":code}))
                .await?;
            if dialogs::current_view(version) && draft().is_ok_and(|(_, current)| current == code) {
                if next["model"]["mode"] == "dart" && !code.trim().is_empty() {
                    return Err(Error::new("This value uses a Dart expression."));
                }
                EDIT.with_borrow_mut(|edit| {
                    edit.frames.last_mut().unwrap().model = next["model"].clone()
                });
                render(false);
            }
        }
        "value-back" => {
            EDIT.with_borrow_mut(|edit| {
                edit.frames.pop();
            });
            render(false);
        }
        "value-check" => {
            let (name, code) = draft()?;
            dom::text("value-status", "Checking Dart…");
            let result = context
                .request("value-check", &name, json!({"expression":code}))
                .await;
            if dialogs::current_view(version) && draft().is_ok_and(|(_, current)| current == code) {
                let message = match result {
                    Ok(result) => {
                        let diagnostics = items(&result["diagnostics"]);
                        if diagnostics.is_empty() {
                            "Dart found no problems in this file.".into()
                        } else {
                            diagnostics
                                .iter()
                                .take(5)
                                .map(|item| string(&item["message"]))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    Err(error) => error.message,
                };
                dom::text("value-status", &message);
            }
        }
        "value-reset" => {
            let name = EDIT.with_borrow(|edit| edit.name.clone());
            save(context, version, &name, json!({"remove":true})).await?;
        }
        _ if action.starts_with("value-nested-") || action.starts_with("value-key-") => {
            nested(context, version, action).await?
        }
        _ if action == "value-add"
            || action.starts_with("value-up-")
            || action.starts_with("value-down-")
            || action.starts_with("value-remove-") =>
        {
            mutate(action)?
        }
        _ => {}
    }
    Ok(())
}

async fn nested(context: &Context, version: u64, action: &str) -> Result {
    let (part, kind_field, caption) = if action.starts_with("value-key-") {
        ("label", "labelType", "Key")
    } else {
        ("value", "itemType", "Item")
    };
    let Some(index) = action
        .rsplit_once('-')
        .and_then(|(_, text)| text.parse::<usize>().ok())
    else {
        return Ok(());
    };
    let Some((name, kind, code, render_version)) = EDIT.with_borrow_mut(|edit| {
        let frame = edit.frames.last_mut()?;
        read_rows(&mut frame.model);
        let row = items(&frame.model["rows"]).get(index)?;
        Some((
            edit.name.clone(),
            string(&frame.model[kind_field]).to_owned(),
            string(&row[part]).to_owned(),
            edit.render_version,
        ))
    }) else {
        return Ok(());
    };
    let next = context
        .request("value-model", &name, json!({"dartType":kind,"draft":code}))
        .await?;
    if dialogs::current_view(version) && context.current() {
        let mut added = false;
        EDIT.with_borrow_mut(|edit| {
            if edit.render_version != render_version {
                return;
            }
            let frame = edit.frames.last_mut().unwrap();
            read_rows(&mut frame.model);
            if items(&frame.model["rows"])
                .get(index)
                .is_some_and(|row| string(&row[part]) == code)
            {
                edit.frames.push(Frame {
                    label: format!("{caption} {}", index + 1),
                    kind,
                    model: next["model"].clone(),
                    parent: Some((index, part)),
                });
                added = true;
            }
        });
        if added {
            render(false);
        }
    }
    Ok(())
}

async fn save(context: &Context, version: u64, name: &str, mut input: Value) -> Result {
    if !context.current() {
        return Err(Error::new("Select the widget again."));
    }
    crate::visual_context()?;
    let request = input.as_object_mut().unwrap();
    request.extend(
        json!({"path":context.path,"id":context.widget,"revision":context.revision,"name":name})
            .as_object()
            .unwrap()
            .clone(),
    );
    let mut result = dom::project_post(&context.project, "widget-property-save", input).await?;
    if context.current() {
        crate::refresh_visual_edit(
            &context.project,
            &context.path,
            &context.widget,
            result["source"].take(),
        )
        .await?;
    }
    dialogs::close_current(version);
    dom::toast("Property saved", false);
    Ok(())
}

pub(crate) async fn submit(context: &Context, version: u64) -> Result {
    let (name, code) = draft()?;
    if code.trim().is_empty() {
        return Err(Error::new("Enter a value."));
    }
    let applied = EDIT.with_borrow_mut(|edit| {
        let Some((index, part)) = edit.frames.last().and_then(|frame| frame.parent) else {
            return false;
        };
        edit.frames.pop();
        edit.frames.last_mut().unwrap().model["rows"][index][part] = json!(code);
        true
    });
    if applied {
        render(false);
        Ok(())
    } else {
        save(context, version, &name, json!({"expression":code})).await
    }
}

pub(super) fn initialize() {
    dom::listen(&id("modal-body"), "input", |event| {
        if dom::find("#value-form").is_none() {
            return;
        }
        dom::text("value-status", "");
        if dom::target(&event).is_some_and(|field| field.id() == "value-scalar")
            && dom::find("#value-icon-options").is_some()
        {
            dom::spawn(search_icons(field("value-scalar")));
        }
        if let Some(picker) = dom::target(&event).filter(|field| field.id() == "value-color-picker")
        {
            let input = id("value-scalar").unchecked_into::<HtmlInputElement>();
            let before = input.value();
            let after = dom::value(&picker);
            if before.len() == 9 && before.is_ascii() {
                input.set_value(&format!(
                    "{}{}",
                    &before[..3],
                    after.trim_start_matches('#')
                ));
            } else {
                input.set_value(&after);
            }
        }
    });
}

async fn search_icons(query: String) -> Result {
    if !query
        .bytes()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
    {
        return Ok(());
    }
    let Some((context, name, kind, rendered)) = EDIT.with_borrow(|edit| {
        let frame = edit.frames.last()?;
        Some((
            edit.context.clone()?,
            edit.name.clone(),
            frame.kind.clone(),
            edit.render_version,
        ))
    }) else {
        return Ok(());
    };
    let version = dialogs::version();
    let current = || {
        dialogs::current_view(version)
            && EDIT.with_borrow(|edit| edit.render_version == rendered)
            && field("value-scalar") == query
            && context.current()
    };
    gloo_timers::future::TimeoutFuture::new(180).await;
    if current() {
        let result = context
            .request(
                "value-model",
                &name,
                json!({"dartType":kind,"iconQuery":query}),
            )
            .await;
        if let Ok(result) = result
            && current()
        {
            dom::html(
                "value-icon-options",
                icon_options(&result["model"]["choices"]),
            );
        }
    }
    Ok(())
}
