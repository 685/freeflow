use crate::{
    files::{self, Problem, Result},
    jobs::Jobs,
    mcp::required,
    processes::ProcessCommand,
    projects::{Project, Projects},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex as AsyncMutex,
    time::{self, Instant},
};

const MAX_FRAME: usize = 12 * 1024 * 1024;
const MAX_OPEN_FILES: usize = 16;

type Session = Arc<AsyncMutex<Option<Analyzer>>>;

#[derive(Default)]
pub struct Analyzers {
    sessions: Mutex<HashMap<String, Session>>,
}

impl Analyzers {
    pub fn prune(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || slot.try_lock().map_or(true, |guard| {
                    guard
                        .as_ref()
                        .is_some_and(|analyzer| analyzer.last.elapsed() < Duration::from_secs(300))
                })
        });
    }

    fn session(&self, id: &str) -> Session {
        self.prune();
        self.sessions
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }

    pub async fn editor(&self, jobs: &Jobs, project: &Project, input: &Value) -> Result<Value> {
        self.editor_snapshot(jobs, project, input, false).await
    }

    pub async fn saved_editor(
        &self,
        jobs: &Jobs,
        project: &Project,
        input: &Value,
    ) -> Result<Value> {
        self.editor_snapshot(jobs, project, input, true).await
    }

    pub(crate) async fn saved_definitions(
        &self,
        jobs: &Jobs,
        project: &Project,
        path: &str,
        text: &str,
        offset: usize,
    ) -> Result<Vec<Definition>> {
        let result = self
            .saved_editor(
                jobs,
                project,
                &json!({"action":"definition-paths","path":path,"text":text,"offset":offset}),
            )
            .await?;
        Ok(definition_entries(&result)
            .iter()
            .take(5)
            .filter_map(|entry| {
                Some(Definition {
                    file: definition_file(entry)?,
                    position: definition_position(entry)?,
                })
            })
            .collect())
    }

    async fn editor_snapshot(
        &self,
        jobs: &Jobs,
        project: &Project,
        input: &Value,
        saved: bool,
    ) -> Result<Value> {
        let action = required(input, "action")?;
        if !(matches!(
            action,
            "complete"
                | "properties"
                | "signature"
                | "diagnose"
                | "definition"
                | "references"
                | "hover"
        ) || saved && action == "definition-paths")
        {
            return Err(Problem::new(400, "Choose a Dart editor action."));
        }
        let path = required(input, "path")?;
        if !path.ends_with(".dart") {
            return Err(Problem::new(400, "Select a Dart source file."));
        }
        let target = files::project_file(&project.root, path, false)?;
        let source;
        let text = if let Some(draft) = input["text"].as_str() {
            draft
        } else {
            source = jobs.projects.file(project, path)?;
            &source.content
        };
        if text.len() > files::MAX_TEXT || text.contains('\0') {
            return Err(Problem::new(
                413,
                "Dart editor requests are limited to 2 MiB.",
            ));
        }
        let cursor = input["offset"].as_u64().unwrap_or(0);
        let position = position(text, cursor)?;
        let uri = file_uri(&target)?;
        let command = jobs.flutter.dart_analyzer()?;
        let session = self.session(&project.id);
        let mut session = time::timeout(Duration::from_secs(3), session.lock())
            .await
            .map_err(|_| Problem::new(503, "Dart analysis is busy. Try again shortly."))?;
        let outcome = time::timeout(Duration::from_secs(14), async {
            if session
                .as_ref()
                .is_none_or(|analyzer| analyzer.binary != command.file)
            {
                *session = Some(Analyzer::start(command, &project.root).await?);
            }
            let analyzer = session.as_mut().unwrap();
            analyzer.last = Instant::now();
            if saved {
                let others: Vec<_> = analyzer
                    .documents
                    .keys()
                    .filter(|open| open.as_str() != uri.as_str())
                    .cloned()
                    .collect();
                for other in others {
                    analyzer.documents.remove(&other);
                    analyzer
                        .notify(
                            "textDocument/didClose",
                            json!({"textDocument":{"uri":other}}),
                        )
                        .await?;
                }
            }
            analyzer
                .query(
                    action,
                    uri.as_str(),
                    text,
                    position,
                    &jobs.projects,
                    project,
                )
                .await
        })
        .await;
        match outcome {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                *session = None;
                Err(error)
            }
            Err(_) => {
                *session = None;
                Err(Problem::new(
                    504,
                    "Dart analysis is still starting. Try again shortly.",
                ))
            }
        }
    }

    pub fn forget(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }
}

fn file_uri(path: &Path) -> Result<Url> {
    Url::from_file_path(path)
        .map_err(|_| Problem::new(422, "Dart could not open this project path."))
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
struct Range {
    start: Position,
    end: Position,
}

pub(crate) struct Definition {
    pub file: PathBuf,
    position: Position,
}

impl Definition {
    pub fn points_to(&self, source: &str, byte: usize) -> bool {
        offset(source, self.position)
            .is_some_and(|offset| source[..byte].encode_utf16().count() == offset as usize)
    }
}

fn position(source: &str, offset: u64) -> Result<Position> {
    let mut left = usize::try_from(offset).unwrap_or(usize::MAX);
    for (index, line) in source.split('\n').enumerate() {
        let length = line.encode_utf16().count();
        if left <= length {
            let width = line.trim_end_matches('\r').encode_utf16().count();
            return Ok(Position {
                line: index as u32,
                character: left.min(width) as u32,
            });
        }
        left -= length + 1;
    }
    Err(Problem::new(
        400,
        "The editor position is outside the current file.",
    ))
}

fn offset(source: &str, position: Position) -> Option<u32> {
    let mut lines = source.split('\n');
    let prefix: usize = lines
        .by_ref()
        .take(position.line as usize)
        .map(|line| line.encode_utf16().count() + 1)
        .sum();
    let width = lines.next()?.trim_end_matches('\r').encode_utf16().count();
    Some((prefix + (position.character as usize).min(width)) as u32)
}

struct Document {
    revision: String,
    version: u64,
    used: Instant,
    diagnostics: Option<Value>,
}

struct Analyzer {
    _child: Child,
    binary: PathBuf,
    output: BufReader<ChildStdout>,
    input: ChildStdin,
    next_id: u64,
    last: Instant,
    documents: HashMap<String, Document>,
    workspace_analysis_complete: bool,
    analysis_idle: bool,
}

impl Analyzer {
    async fn start(command: ProcessCommand, root: &Path) -> Result<Self> {
        let root_uri = file_uri(root)?;
        let mut child = command.stdio(root)?;
        let mut server = Self {
            binary: command.file,
            output: BufReader::new(child.stdout.take().unwrap()),
            input: child.stdin.take().unwrap(),
            _child: child,
            last: Instant::now(),
            next_id: 0,
            documents: HashMap::new(),
            workspace_analysis_complete: false,
            analysis_idle: false,
        };
        let initialized = server
            .request(
                "initialize",
                json!({
                    "processId":std::process::id(),"rootUri":root_uri.as_str(),
                    "workspaceFolders":[{"uri":root_uri.as_str(),"name":"Flutter project"}],
                    "capabilities":{
                        "general":{"positionEncodings":["utf-16"]},
                        "textDocument":{
                            "publishDiagnostics":{"versionSupport":true},
                            "completion":{"completionItem":{"snippetSupport":false}},
                            "hover":{"contentFormat":["plaintext"]},
                            "definition":{"linkSupport":true}
                        }
                    }
                }),
            )
            .await?;
        server.workspace_analysis_complete =
            initialized["capabilities"]["experimental"]["workspaceAnalysisComplete"] == true;
        server.notify("initialized", json!({})).await?;
        Ok(server)
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let content = serde_json::to_vec(&value)?;
        self.input
            .write_all(format!("Content-Length: {}\r\n\r\n", content.len()).as_bytes())
            .await?;
        self.input.write_all(&content).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    async fn receive(&mut self) -> Result<Value> {
        let mut size = None;
        let mut line = String::new();
        for _ in 0..16 {
            line.clear();
            if self.output.read_line(&mut line).await? == 0 {
                return Err(Problem::new(502, "The Dart analyzer stopped."));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("Content-Length")
            {
                size = value.trim().parse::<usize>().ok();
            }
        }
        let size = size
            .filter(|size| (1..=MAX_FRAME).contains(size))
            .ok_or_else(|| Problem::new(502, "The Dart analyzer returned an invalid message."))?;
        let mut data = vec![0; size];
        self.output.read_exact(&mut data).await?;
        serde_json::from_slice(&data)
            .map_err(|_| Problem::new(502, "The Dart analyzer returned an invalid response."))
    }

    async fn event(&mut self, message: &Value) -> Result<()> {
        if message["method"] == "$/analyzerStatus" {
            self.analysis_idle = message["params"]["isAnalyzing"] == false;
        } else if message["method"] == "textDocument/publishDiagnostics" {
            let params = &message["params"];
            if let Some(uri) = params["uri"].as_str()
                && let Some(document) = self.documents.get_mut(uri)
                && params["version"]
                    .as_u64()
                    .is_none_or(|version| version == document.version)
            {
                document.diagnostics = Some(params["diagnostics"].clone());
            }
        } else if message["method"].is_string() && !message["id"].is_null() {
            let id = &message["id"];
            let result = match message["method"].as_str() {
                Some("workspace/configuration") => {
                    let count = message["params"]["items"].as_array().map_or(0, Vec::len);
                    json!({"result":vec![Value::Null;count]})
                }
                Some(
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create",
                ) => json!({"result":null}),
                _ => json!({"error":{"code":-32601,"message":"Unsupported client request"}}),
            };
            let mut response = result;
            response["jsonrpc"] = json!("2.0");
            response["id"] = id.clone();
            self.send(response).await?;
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        loop {
            self.next_id += 1;
            let id = self.next_id;
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            loop {
                let message = self.receive().await?;
                if message["id"] == id && message.get("method").is_none() {
                    if message["error"].is_null() {
                        return Ok(message["result"].clone());
                    }
                    // A new workspace may not have registered the opened file yet.
                    // The enclosing editor timeout bounds this startup retry.
                    if message["error"]["code"] == -32007 {
                        time::sleep(Duration::from_millis(100)).await;
                        break;
                    }
                    let detail = message["error"]["message"]
                        .as_str()
                        .map(|text| text.chars().take(600).collect::<String>())
                        .unwrap_or_else(|| "The request could not be completed.".into());
                    return Err(Problem::new(422, format!("Dart analysis: {detail}")));
                }
                self.event(&message).await?;
            }
        }
    }

    async fn sync(&mut self, uri: &str, text: &str) -> Result<()> {
        let revision = files::revision(text);
        if let Some(document) = self.documents.get_mut(uri) {
            document.used = Instant::now();
            if document.revision == revision {
                return Ok(());
            }
            document.version += 1;
            document.revision = revision;
            document.diagnostics = None;
            let version = document.version;
            return self.notify("textDocument/didChange", json!({
                "textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]
            })).await;
        }
        if self.documents.len() >= MAX_OPEN_FILES
            && let Some(old) = self
                .documents
                .iter()
                .min_by_key(|(_, document)| document.used)
                .map(|(uri, _)| uri.clone())
        {
            self.documents.remove(&old);
            self.notify("textDocument/didClose", json!({"textDocument":{"uri":old}}))
                .await?;
        }
        self.documents.insert(
            uri.to_owned(),
            Document {
                version: 1,
                revision,
                used: Instant::now(),
                diagnostics: None,
            },
        );
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument":{"uri":uri,"languageId":"dart","version":1,"text":text}
            }),
        )
        .await
    }

    async fn query(
        &mut self,
        action: &str,
        uri: &str,
        text: &str,
        position: Position,
        projects: &Projects,
        project: &Project,
    ) -> Result<Value> {
        self.sync(uri, text).await?;
        let params = json!({"textDocument":{"uri":uri},"position":position});
        match action {
            "diagnose" => {
                self.wait_for_diagnostics(uri).await?;
                let values = self.documents[uri].diagnostics.as_ref().unwrap();
                Ok(json!({"diagnostics":diagnostics(values)}))
            }
            "complete" | "properties" => {
                let mut result = self
                    .request("textDocument/completion", params.clone())
                    .await?;
                let prefix = input_prefix(text, position).unwrap_or_default();
                let mut items = completions(&result, prefix, action == "properties");
                if items.is_empty() && self.documents[uri].diagnostics.is_none() {
                    self.wait_for_diagnostics(uri).await?;
                    result = self.request("textDocument/completion", params).await?;
                    items = completions(&result, prefix, action == "properties");
                }
                Ok(json!({"items":items}))
            }
            "signature" => {
                let mut result = self
                    .request("textDocument/signatureHelp", params.clone())
                    .await?;
                if result["signatures"].as_array().is_none_or(Vec::is_empty) {
                    self.wait_for_diagnostics(uri).await?;
                    result = self.request("textDocument/signatureHelp", params).await?;
                }
                let signatures: Vec<_> = result["signatures"].as_array().into_iter().flatten().take(4).map(|signature| {
                    json!({"label":clip(&signature["label"],16000),"parameters":signature["parameters"].as_array().map(|parameters|parameters.iter().take(101).map(|parameter|json!({"label":parameter["label"]})).collect::<Vec<_>>())})
                }).collect();
                Ok(json!({"signatures":signatures}))
            }
            "definition" | "definition-paths" => {
                self.wait_for_diagnostics(uri).await?;
                let result = self.request("textDocument/definition", params).await?;
                if action == "definition-paths" {
                    return Ok(result);
                }
                Ok(json!({"locations":definitions(&result, projects, project, uri, text, 5)}))
            }
            "references" => {
                self.wait_for_diagnostics(uri).await?;
                let mut params = params;
                params["context"] = json!({"includeDeclaration":false});
                let result = self.request("textDocument/references", params).await?;
                let locations = definitions(&result, projects, project, uri, text, 200);
                Ok(
                    json!({"locations":locations,"truncated":result.as_array().is_some_and(|items| items.len() > 200)}),
                )
            }
            _ => {
                let result = self.request("textDocument/hover", params).await?;
                Ok(json!({"text":hover(&result["contents"])}))
            }
        }
    }

    async fn wait_for_diagnostics(&mut self, uri: &str) -> Result<()> {
        if self.documents[uri].diagnostics.is_some() {
            return Ok(());
        }
        if self.workspace_analysis_complete {
            self.request("dart/workspace/analysis/complete", Value::Null)
                .await?;
        } else {
            // Older Dart SDKs omit empty diagnostics. Wait for analysis, resolve
            // the latest overlay and consume its analysis-complete notification.
            self.request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":{"line":0,"character":0}}),
            )
            .await?;
            while !self.analysis_idle {
                let event = self.receive().await?;
                self.event(&event).await?;
            }
        }
        self.documents
            .get_mut(uri)
            .unwrap()
            .diagnostics
            .get_or_insert_with(|| json!([]));
        Ok(())
    }
}

fn clip(value: &Value, limit: usize) -> Option<String> {
    Some(value.as_str()?.chars().take(limit).collect())
}

fn input_prefix(source: &str, position: Position) -> Option<&str> {
    let line = source.split('\n').nth(position.line as usize)?;
    let mut width = 0;
    let mut end = 0;
    for (byte, ch) in line.char_indices() {
        width += ch.len_utf16();
        if width > position.character as usize {
            break;
        }
        end = byte + ch.len_utf8();
    }
    Some(
        line[..end]
            .rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
            .next()
            .unwrap_or_default(),
    )
}

fn completions(result: &Value, prefix: &str, properties: bool) -> Vec<Value> {
    let list = result.as_array().or_else(|| result["items"].as_array());
    let defaults = &result["itemDefaults"];
    let prefix = prefix.to_lowercase();
    let mut items = list
        .into_iter()
        .flatten()
        .filter(|item| {
            item["insertTextFormat"]
                .as_u64()
                .or(defaults["insertTextFormat"].as_u64())
                != Some(2)
        })
        .filter_map(|item| {
            let label = clip(&item["label"], 200)?;
            if properties {
                let name = label.trim_end().strip_suffix(':')?.trim();
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
                {
                    return None;
                }
            }
            let edit = &item["textEdit"];
            let text = clip(&edit["newText"], 4000)
                .or_else(|| clip(&item["textEditText"], 4000))
                .or_else(|| clip(&item["insertText"], 4000))
                .unwrap_or_else(|| label.clone());
            let range = edit
                .get("range")
                .or(edit.get("replace"))
                .or_else(|| {
                    let range = &defaults["editRange"];
                    range.get("replace").or(Some(range))
                })
                .and_then(|range| serde_json::from_value::<Range>(range.clone()).ok());
            let additional: Vec<_> = item["additionalTextEdits"]
                .as_array()
                .into_iter()
                .flatten()
                .take(8)
                .filter_map(|edit| {
                    let range: Range = serde_json::from_value(edit["range"].clone()).ok()?;
                    Some(json!({"range":range,"text":clip(&edit["newText"],4000)?}))
                })
                .collect();
            let filter = clip(&item["filterText"], 200).unwrap_or_else(|| label.clone());
            let direct = filter.to_lowercase().starts_with(&prefix);
            Some((
                !direct,
                clip(&item["sortText"], 200).unwrap_or_else(|| label.clone()),
                json!({"label":label,"filter":filter,"text":text,"range":range,
                    "detail":clip(&item["detail"],150),"additional":additional}),
            ))
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    items
        .into_iter()
        .take(if properties { 200 } else { 40 })
        .map(|(_, _, item)| item)
        .collect()
}

fn diagnostics(values: &Value) -> Vec<Value> {
    let mut values: Vec<_> = values.as_array().into_iter().flatten().collect();
    // Keep compile errors visible to callers that validate an edit, even when
    // the file contains more warnings than the response limit.
    values.sort_by_key(|item| item["severity"].as_u64().unwrap_or(3));
    values
        .into_iter()
        .take(120)
        .filter_map(|item| {
            let range: Range = serde_json::from_value(item["range"].clone()).ok()?;
            Some(json!({"range":range,"message":clip(&item["message"],1200)?,
            "severity":item["severity"].as_u64().unwrap_or(3)}))
        })
        .collect()
}

fn definition_entries(result: &Value) -> &[Value] {
    result
        .as_array()
        .map_or_else(|| std::slice::from_ref(result), Vec::as_slice)
}

fn definition_file(entry: &Value) -> Option<PathBuf> {
    let uri = entry["targetUri"].as_str().or(entry["uri"].as_str())?;
    dunce::canonicalize(Url::parse(uri).ok()?.to_file_path().ok()?).ok()
}

fn definition_position(entry: &Value) -> Option<Position> {
    let range = entry.get("targetSelectionRange").unwrap_or(&entry["range"]);
    serde_json::from_value(range["start"].clone()).ok()
}

fn definitions(
    result: &Value,
    projects: &Projects,
    project: &Project,
    current_uri: &str,
    current_text: &str,
    limit: usize,
) -> Vec<Value> {
    definition_entries(result)
        .iter()
        .filter_map(|entry| {
            let uri = entry["targetUri"].as_str().or(entry["uri"].as_str())?;
            let absolute = definition_file(entry)?;
            let path = absolute
                .strip_prefix(&project.root)
                .ok()?
                .to_str()?
                .replace('\\', "/");
            let position = definition_position(entry)?;
            let file;
            let text = if current_uri == uri {
                current_text
            } else {
                file = projects.file(project, &path).ok()?;
                &file.content
            };
            Some(json!({"path":path,"line":position.line+1,"offset":offset(text,position)?}))
        })
        .take(limit)
        .collect()
}

fn hover(contents: &Value) -> Option<String> {
    let text = clip(contents, 3000)
        .or_else(|| clip(&contents["value"], 3000))
        .or_else(|| {
            let lines: Vec<_> = contents
                .as_array()?
                .iter()
                .filter_map(|part| clip(part, 1000).or_else(|| clip(&part["value"], 1000)))
                .take(5)
                .collect();
            Some(lines.join("\n\n"))
        })?;
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_limits_cannot_hide_an_error_after_many_warnings() {
        let range = json!({"start":{"line":0,"character":0},"end":{"line":0,"character":1}});
        let mut values = vec![json!({"range":range,"message":"warning","severity":2}); 140];
        values.push(json!({"range":range,"message":"invalid constructor argument","severity":1}));
        let result = diagnostics(&json!(values));
        assert_eq!(result.len(), 120);
        assert_eq!(result[0]["message"], "invalid constructor argument");
    }

    #[test]
    fn property_suggestions_keep_named_parameters_before_applying_the_result_limit() {
        let mut input: Vec<_> = (0..80)
            .map(|index| json!({"label":format!("class{index}"),"insertText":"anything"}))
            .collect();
        input
            .push(json!({"label":"maxLines: ","insertText":"maxLines: ","detail":"int? maxLines"}));
        let result = completions(&json!({"items":input}), "", true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["label"], "maxLines: ");
        assert_eq!(completions(&json!({"items":input}), "", false).len(), 40);
    }

    #[test]
    fn dart_positions_follow_utf16_with_crlf_and_emoji() {
        let source = "😀!\r\nvalue";
        let second_line = position(source, 5).unwrap();
        assert_eq!((second_line.line, second_line.character), (1, 0));
        let before_exclamation = position(source, 2).unwrap();
        assert_eq!(
            (before_exclamation.line, before_exclamation.character),
            (0, 2)
        );
        assert_eq!(offset(source, second_line), Some(5));
        assert_eq!(offset(source, before_exclamation), Some(2));
        assert!(position(source, 11).is_err());
    }
}
