use crate::{
    files::{self, Problem, Result, Session},
    jobs::Jobs,
    server::{read_widgets, widget_property, write_widgets},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static TOOLS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("mcp-tools.json")).expect("valid MCP tool definitions")
});

fn known(name: &str) -> bool {
    TOOLS
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == name)
}

pub fn definitions() -> &'static [Value] {
    TOOLS.as_array().expect("tool definitions")
}

pub fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str> {
    input[field]
        .as_str()
        .ok_or_else(|| Problem::new(400, format!("Enter a value for {field}.")))
}

pub fn revision(input: &Value) -> Result<Option<&str>> {
    match input.get("revision") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(Problem::new(
            400,
            "Pass the file revision, or null for a new file.",
        )),
    }
}

pub async fn call(
    jobs: &Arc<Jobs>,
    name: &str,
    args: &Value,
    scope: Option<&str>,
) -> Result<Value> {
    if !known(name) {
        return Err(Problem::new(400, "Unknown Freeflow tool."));
    }
    if !args.is_object() {
        return Err(Problem::new(400, "Tool arguments must be an object."));
    }
    let requested = args["projectId"].as_str().filter(|id| !id.is_empty());
    if scope.is_some() && requested.is_some() && requested != scope {
        return Err(Problem::new(
            403,
            "This MCP connection is scoped to a different project.",
        ));
    }
    let projects = &jobs.projects;
    if name == "list_projects" {
        let list = match scope {
            Some(scope) => vec![projects.get(scope)?],
            None => projects.list()?,
        };
        return Ok(json!({"projects":list.into_iter().map(|project| {
            json!({"id":project.id,"name":project.name,"root":project.root,"template":project.template})
        }).collect::<Vec<_>>()}));
    }
    let id = scope.or(requested).ok_or_else(|| {
        Problem::new(
            400,
            "Choose a project from list_projects and pass its projectId.",
        )
    })?;
    let project = projects.get(id)?;
    match name {
        "component_definition" | "component_references" => {
            let mut input = args.clone();
            input["action"] = json!(if name == "component_definition" {
                "definition"
            } else {
                "references"
            });
            crate::components::navigate(jobs, &project, &input).await
        }
        "list_files" => Ok(json!(projects.tree(&project)?)),
        "list_assets" => projects.asset_context(&project, args),
        "read_project_rules" => crate::project_rules::load(&project, args["path"].as_str()),
        "read_app_logs" => jobs.app_logs(&project, args),
        "edit_history" => Ok(projects.edit_history(&project)),
        "replay_edit" => projects.replay_edit(
            &project,
            required(args, "action")?,
            required(args, "revision")?,
        ),
        "read_file" => Ok(json!(projects.file(&project, required(args, "path")?)?)),
        "write_file" => Ok(json!(projects.save(
            &project,
            required(args, "path")?,
            required(args, "content")?.into(),
            revision(args)?
        )?)),
        "inspect_widgets" => read_widgets(projects, &project, required(args, "path")?),
        "read_widget_property" => widget_property(projects, &project, args),
        "edit_widget" => write_widgets(projects, &project, args),
        "start_preview" => jobs.start_preview(project),
        "preview_status" => Ok(json!({"preview":jobs.preview(&project.id)})),
        "reload_preview" => jobs.reload_preview(project),
        "analyze" | "test" => jobs.analyze(project, name),
        "build" => jobs.build(project, required(args, "target")?, args),
        "list_exports" => Ok(json!({"exports":jobs.artifacts(&project)?})),
        "job_status" => jobs.scoped(
            required(args, "jobId")?,
            &project,
            args["after"].as_u64().unwrap_or(0),
        ),
        _ => Err(Problem::new(400, "Unknown Freeflow tool.")),
    }
}

pub fn valid_session(session: &Session) -> bool {
    let port = session
        .origin
        .strip_prefix("http://127.0.0.1:")
        .unwrap_or_default();
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|value| value > 0)
        && session.token.len() == 64
        && session
            .token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("loopback HTTP client")
}

async fn tool_request(
    home: &Path,
    scope: Option<&str>,
    client: &reqwest::Client,
    params: &Value,
) -> anyhow::Result<Value> {
    let session: Session = files::read_json(&home.join("session.json"))?
        .filter(valid_session)
        .ok_or_else(|| anyhow::anyhow!("Start Freeflow before calling MCP tools."))?;
    let args = params
        .get("arguments")
        .filter(|args| !args.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let response = client
        .post(format!("{}/api/mcp", session.origin))
        .header("X-Freeflow-Token", session.token)
        .json(&json!({"name":params["name"],"arguments":args,"scope":scope}))
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "The Freeflow workbench is not reachable. Start it before calling MCP tools."
            )
        })?;
    let status = response.status();
    let data: Value = response.json().await?;
    anyhow::ensure!(
        status.is_success(),
        "{}",
        data["error"]
            .as_str()
            .unwrap_or("Freeflow returned an HTTP error.")
    );
    Ok(data)
}

fn response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

async fn message(
    home: &Path,
    scope: Option<&str>,
    client: &reqwest::Client,
    value: Value,
) -> Option<Value> {
    let id = &value["id"];
    if value["jsonrpc"] != "2.0" || !value["method"].is_string() {
        return Some(error(id, -32600, "Invalid JSON-RPC request."));
    }
    value.get("id")?;
    let params = &value["params"];
    Some(match value["method"].as_str().unwrap() {
        "initialize" => {
            let requested = params["protocolVersion"].as_str().unwrap_or_default();
            let protocol = match requested {
                "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25" => requested,
                _ => "2025-06-18",
            };
            response(
                id,
                json!({"protocolVersion":protocol, "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"freeflow","version":env!("CARGO_PKG_VERSION")},
                "instructions":"Use list_projects, then read files with their revisions before making edits. The local Freeflow workbench runs Flutter and shows the preview."}),
            )
        }
        "ping" => response(id, json!({})),
        "tools/list" => response(id, json!({"tools":&*TOOLS})),
        "tools/call" => {
            if !params["name"].as_str().is_some_and(known) {
                error(id, -32602, "Unknown Freeflow tool.")
            } else {
                match tool_request(home, scope, client, params).await {
                    Ok(result) => response(
                        id,
                        json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&result).unwrap()}],
                        "structuredContent":result}),
                    ),
                    Err(problem) => response(
                        id,
                        json!({"isError":true,"content":[{"type":"text","text":problem.to_string()}]}),
                    ),
                }
            }
        }
        _ => error(id, -32601, "Method not found."),
    })
}

pub async fn serve(home: &Path, scope: Option<&str>) -> anyhow::Result<()> {
    let client = client(Duration::from_secs(30));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let result = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let mut results = Vec::new();
                for entry in batch {
                    if let Some(result) = message(home, scope, &client, entry).await {
                        results.push(result);
                    }
                }
                (!results.is_empty()).then_some(json!(results))
            }
            Ok(value) => message(home, scope, &client, value).await,
            Err(_) => Some(error(&Value::Null, -32700, "Malformed JSON.")),
        };
        if let Some(result) = result {
            output.write_all(&serde_json::to_vec(&result)?).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }
    Ok(())
}
