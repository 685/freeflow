use crate::{
    files::{self, Problem, Result},
    jobs::Job,
    mcp::required,
};
use reqwest::{Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, net::IpAddr, path::Path, time::Duration};

const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const LOCAL_BASE: &str = "http://127.0.0.1:11434/v1";

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Provider {
    base_url: String,
    model: String,
    api_key: String,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            base_url: LOCAL_BASE.into(),
            model: String::new(),
            api_key: String::new(),
        }
    }
}

impl Provider {
    pub fn load(home: &Path) -> Result<Self> {
        Ok(files::read_json(&home.join("agent-provider.json"))?.unwrap_or_default())
    }

    pub fn public(&self) -> Value {
        json!({"baseUrl":self.base_url,"model":self.model,"hasKey":!self.api_key.is_empty(),
            "configured":!self.model.is_empty()})
    }

    pub fn save(home: &Path, input: &Value) -> Result<Value> {
        let base_url = endpoint(required(input, "baseUrl")?)?;
        let model = required(input, "model")?.trim().to_owned();
        let key = input["apiKey"].as_str().unwrap_or_default().trim();
        if model.is_empty() || model.len() > 250 || model.chars().any(char::is_control) {
            return Err(Problem::new(400, "Enter a model name."));
        }
        if key.len() > 2048 || key.chars().any(char::is_control) {
            return Err(Problem::new(400, "Enter a valid API key."));
        }
        let old = Self::load(home)?;
        let api_key = if input["clearKey"] == true {
            String::new()
        } else if !key.is_empty() {
            key.to_owned()
        } else if old.base_url == base_url {
            old.api_key
        } else {
            String::new()
        };
        let provider = Self {
            base_url,
            model,
            api_key,
        };
        files::write_json(&home.join("agent-provider.json"), &provider)?;
        Ok(provider.public())
    }

    pub fn ready(&self) -> Result<()> {
        if self.model.is_empty() {
            return Err(Problem::new(
                422,
                "Set an AI provider and model in Agent settings.",
            ));
        }
        Ok(())
    }

    pub async fn complete(
        &self,
        job: &Job,
        client: &reqwest::Client,
        messages: &[Value],
        tools: &[Value],
        progress: &Value,
    ) -> Result<Completion> {
        let endpoint = endpoint(&self.base_url)?;
        let endpoint = if endpoint.ends_with("/chat/completions") {
            endpoint
        } else {
            format!("{endpoint}/chat/completions")
        };
        let mut request = client.post(endpoint).json(&json!({
            "model": self.model, "messages": messages, "tools": tools, "stream": true,
        }));
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let response = request.send().await.map_err(network)?;
        let status = response.status();
        if !status.is_success() {
            return Err(Problem::new(
                502,
                match status.as_u16() {
                    401 | 403 => "The AI provider rejected the API key.".into(),
                    404 => "The AI provider could not find the model or Chat Completions endpoint."
                        .into(),
                    429 => "The AI provider rate-limited the request. Try again later.".into(),
                    _ => format!(
                        "The AI provider returned HTTP {status}. Check the model and provider settings."
                    ),
                },
            ));
        }
        let streaming = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .is_some_and(|header| header.contains("text/event-stream"));
        if streaming {
            read_stream(response, job, progress).await
        } else {
            let data = read_json(response).await?;
            let mut result = Completion::default();
            result.append(&data["choices"][0]["message"])?;
            result.finish(&data["choices"][0]["finish_reason"])?;
            Ok(result)
        }
    }
}

fn endpoint(input: &str) -> Result<String> {
    let url = Url::parse(input.trim())
        .ok()
        .filter(|url| {
            let local = url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .trim_matches(['[', ']'])
                        .parse::<IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            (url.scheme() == "https" || (url.scheme() == "http" && local))
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && input.len() <= 1500
        })
        .ok_or_else(|| Problem::new(400, "Enter an HTTPS base URL or a local HTTP base URL."))?;
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn network(error: reqwest::Error) -> Problem {
    Problem::new(
        502,
        if error.is_timeout() {
            "The AI provider timed out."
        } else {
            "Freeflow could not reach the AI provider. Check that it is running and the URL is correct."
        },
    )
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(12))
        .timeout(Duration::from_secs(150))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Agent HTTP client")
}

#[derive(Default)]
pub struct Completion {
    pub text: String,
    pub calls: BTreeMap<u64, ToolCall>,
}

#[derive(Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCall {
    pub fn message(&self) -> Value {
        json!({"id":self.id,"type":"function","function":{"name":self.name,"arguments":self.arguments}})
    }
}

impl Completion {
    fn append(&mut self, delta: &Value) -> Result<()> {
        if let Some(text) = delta["content"].as_str() {
            self.text.push_str(text);
        }
        for (position, tool) in delta["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let index = tool["index"].as_u64().unwrap_or(position as u64);
            if index >= 12 {
                return Err(Problem::new(
                    502,
                    "The AI provider returned too many tool calls.",
                ));
            }
            let call = self.calls.entry(index).or_default();
            if let Some(id) = tool["id"].as_str().filter(|id| !id.is_empty()) {
                call.id = id.to_owned();
            }
            if let Some(name) = tool["function"]["name"]
                .as_str()
                .filter(|name| !name.is_empty())
            {
                call.name = name.to_owned();
            }
            if let Some(arguments) = tool["function"]["arguments"].as_str() {
                call.arguments.push_str(arguments);
            }
        }
        Ok(())
    }

    fn finish(&self, reason: &Value) -> Result<()> {
        match reason.as_str() {
            Some("length") => Err(Problem::new(
                502,
                "The model reached its output limit. Try a smaller change.",
            )),
            Some("content_filter") => Err(Problem::new(502, "The AI provider filtered its reply.")),
            _ => Ok(()),
        }
    }
}

fn too_large() -> Problem {
    Problem::new(502, "The AI provider returned more than 2 MiB.")
}

async fn read_json(mut response: Response) -> Result<Value> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if data.len() + chunk.len() > MAX_RESPONSE {
            return Err(too_large());
        }
        data.extend(&chunk);
    }
    serde_json::from_slice(&data)
        .map_err(|_| Problem::new(502, "The AI provider did not return a chat completion."))
}

async fn read_stream(mut response: Response, job: &Job, progress: &Value) -> Result<Completion> {
    let mut result = Completion::default();
    let mut pending = Vec::new();
    let mut received = 0;
    let mut shown = 0;
    let mut finished = false;
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        job.check()?;
        received += chunk.len();
        if received > MAX_RESPONSE {
            return Err(too_large());
        }
        pending.extend(&chunk);
        let mut start = 0;
        for end in pending
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        {
            let line = &pending[start..end];
            start = end + 1;
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let data = data.trim_ascii();
            if data == b"[DONE]" {
                return Ok(result);
            }
            let event: Value = serde_json::from_slice(data).map_err(|_| {
                Problem::new(502, "The AI provider returned an invalid streaming event.")
            })?;
            if !event["error"].is_null() {
                return Err(Problem::new(
                    502,
                    "The AI provider stopped the reply with an error.",
                ));
            }
            let choice = &event["choices"][0];
            result.append(&choice["delta"])?;
            result.finish(&choice["finish_reason"])?;
            finished |= choice["finish_reason"].is_string();
        }
        pending.drain(..start);
        if result.text.len() != shown {
            shown = result.text.len();
            let mut event = progress.clone();
            event["partial"] = json!(result.text);
            job.report(event);
        }
    }
    if finished {
        Ok(result)
    } else {
        Err(Problem::new(
            502,
            "The AI provider closed the reply before it finished.",
        ))
    }
}
