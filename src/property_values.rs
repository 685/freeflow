use crate::{
    files::{Problem, Result, Source},
    jobs::Jobs,
    mcp::required,
    projects::Project,
    visual::{
        self, Scope,
        library::{self, switching::Call},
    },
    widget_library::signature,
};
use serde_json::{Value, json};

pub(crate) async fn parameters(
    jobs: &Jobs,
    project: &Project,
    source: &Source,
    id: &str,
    scope: &Scope,
) -> Result<Value> {
    let call = Call::locate(&source.content, id, scope)?;
    let (text, offset) = call.signature_probe(None);
    let parameters = signature(jobs, project, &source.path, &text, offset)
        .await
        .and_then(|signature| library::parameters(&signature));
    let (parameters, available) = match parameters {
        Ok(parameters) => (parameters, true),
        Err(_) => (vec![], false),
    };
    Ok(json!({"parameters":parameters, "available":available}))
}

fn existing_framework_reference(source: &str, name: &str, material_only: bool) -> Option<String> {
    let mut uris = vec!["package:flutter/material.dart".into()];
    if !material_only {
        uris.extend([
            "package:flutter/widgets.dart".into(),
            "package:flutter/cupertino.dart".into(),
        ]);
    }
    let (_, name, shift) = visual::import_reference(source, &uris, name, "freeflowValues").ok()?;
    (shift == 0).then_some(name)
}

fn static_value(expression: &str) -> Option<(&str, &str)> {
    let (owner, name) = expression.rsplit_once('.')?;
    (!name.is_empty()
        && expression
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')))
    .then_some((owner, name))
}

fn enum_reference(kind: &str, expression: &str, namespace: &str) -> Option<String> {
    let type_name = kind.trim_end_matches('?');
    if !type_name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_uppercase)
        || !type_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || matches!(
            type_name,
            "Color" | "String" | "Widget" | "Object" | "IconData"
        )
    {
        return None;
    }
    if expression.is_empty() {
        return Some(format!("{namespace}{type_name}"));
    }
    let (owner, _) = static_value(expression)?;
    (owner.rsplit('.').next()? == type_name).then(|| owner.to_owned())
}

pub(crate) async fn model(
    jobs: &Jobs,
    project: &Project,
    source: &Source,
    input: &Value,
    scope: &Scope,
) -> Result<Value> {
    let id = required(input, "id")?;
    let name = required(input, "name")?;
    let kind = input["dartType"].as_str().unwrap_or_default();
    let mut data = visual::property_details_in(&source.content, id, name, scope)?;
    let override_text = input["draft"].as_str();
    let expression = override_text
        .or_else(|| data["expression"].as_str())
        .unwrap_or_default();
    let value_type = kind.trim_end_matches('?');
    let color = (value_type == "Color")
        .then(|| existing_framework_reference(&source.content, "Color", false))
        .flatten();
    let eol = if source.content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut model = visual::values::model(expression, kind, color.as_deref(), eol)?;
    if model["mode"] == "dart" && override_text.is_none() {
        let icons = value_type == "IconData";
        let reference = if icons {
            if expression.is_empty() {
                existing_framework_reference(&source.content, "Icons", true)
            } else {
                static_value(expression).map(|(owner, _)| owner.to_owned())
            }
        } else {
            let call = Call::locate(&source.content, id, scope)?;
            enum_reference(kind, expression, &call.namespace)
        };
        let action = if icons { "icon-values" } else { "enum-values" };
        if let Some(reference) = reference
            && let Ok(items) =
                members(jobs, project, source, input, scope, &reference, action).await
            && items.as_array().is_some_and(|items| !items.is_empty())
        {
            if icons {
                model["mode"] = json!("icon");
                model["choices"] = items;
                model["reference"] = json!(reference);
                model["value"] = json!(static_value(expression).map_or("", |(_, name)| name));
            } else {
                model["mode"] = json!("enum");
                model["choices"] = Value::Array(
                    items
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|item| Some(json!(format!("{reference}.{}", item.as_str()?))))
                        .collect(),
                );
            }
        }
    }
    data["model"] = model;
    Ok(data)
}

async fn members(
    jobs: &Jobs,
    project: &Project,
    source: &Source,
    input: &Value,
    scope: &Scope,
    reference: &str,
    action: &str,
) -> Result<Value> {
    let id = required(input, "id")?;
    let name = required(input, "name")?;
    let query = input["iconQuery"]
        .as_str()
        .filter(|query| {
            query.len() <= 128
                && query
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
        })
        .unwrap_or_default();
    let probe = if query.is_empty() {
        "_freeflowValue"
    } else {
        query
    };
    let text = visual::edit_in(
        &source.content,
        &json!({
            "id":id,"operation":"expression","name":name,"expression":format!("{reference}.{probe}")
        }),
        scope,
    )?;
    let detail = visual::property_details_in(&text, id, name, scope)?;
    let result = jobs.analyzers.saved_editor(jobs,project,&json!({
        "action":action,"path":source.path,"text":text,
        "offset":detail["offset"].as_u64().unwrap_or_default() + (reference.encode_utf16().count() + query.len() + 1) as u64
    })).await?;
    Ok(result["items"].clone())
}

pub(crate) async fn edit(
    jobs: &Jobs,
    project: &Project,
    source: &Source,
    input: &Value,
    scope: &Scope,
    save: bool,
) -> Result<Value> {
    let code = visual::edit_in(
        &source.content,
        &json!({
            "id":required(input,"id")?, "operation":"expression", "name":required(input,"name")?,
            "expression":input["expression"], "remove":input["remove"]
        }),
        scope,
    )?;
    let diagnostics = jobs
        .analyzers
        .saved_editor(
            jobs,
            project,
            &json!({
                "action":"diagnose","path":source.path,"text":code
            }),
        )
        .await?;
    if !save {
        return Ok(diagnostics);
    }
    let errors: Vec<_> = diagnostics["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|diagnostic| diagnostic["severity"] == 1)
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .take(3)
        .collect();
    if !errors.is_empty() {
        return Err(Problem::new(
            422,
            format!("Dart found a problem. {}", errors.join(" ")),
        ));
    }
    let saved = jobs
        .projects
        .save(project, &source.path, code, source.revision.as_deref())?;
    Ok(json!({"source":saved}))
}
