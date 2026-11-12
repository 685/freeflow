use gloo_timers::callback::Timeout;
use serde_json::{Value, json};
use std::{future::Future, sync::LazyLock};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Document, Element, Event, EventTarget, Headers, HtmlElement, HtmlInputElement,
    HtmlSelectElement, HtmlTextAreaElement, Request, RequestInit, Response, Window,
};

pub type Result<T = ()> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            message: message.into(),
        }
    }
}

impl From<JsValue> for Error {
    fn from(value: JsValue) -> Self {
        Self::new(
            value
                .as_string()
                .or_else(|| {
                    js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                        .ok()?
                        .as_string()
                })
                .unwrap_or_else(|| "The browser could not complete this action.".into()),
        )
    }
}

pub fn win() -> Window {
    web_sys::window().expect("browser window")
}
pub fn doc() -> Document {
    win().document().expect("browser document")
}
pub fn id(name: &str) -> HtmlElement {
    doc()
        .get_element_by_id(name)
        .expect("workbench element")
        .unchecked_into()
}
pub fn find(selector: &str) -> Option<Element> {
    doc().query_selector(selector).ok().flatten()
}

pub fn all(selector: &str) -> Vec<Element> {
    let Ok(nodes) = doc().query_selector_all(selector) else {
        return vec![];
    };
    (0..nodes.length())
        .filter_map(|i| nodes.item(i)?.dyn_into().ok())
        .collect()
}

pub fn attr(el: &Element, name: &str, value: &str) {
    let _ = el.set_attribute(name, value);
}
pub fn html(name: &str, content: impl AsRef<str>) {
    id(name).set_inner_html(content.as_ref());
}
pub fn text(name: &str, content: &str) {
    id(name).set_text_content(Some(content));
}
pub fn hidden(name: &str, hidden: bool) {
    id(name).set_hidden(hidden);
}
pub fn disabled(name: &str, disabled: bool) {
    let element = id(name);
    if disabled {
        attr(&element, "disabled", "");
    } else {
        let _ = element.remove_attribute("disabled");
    }
}
pub fn toggle(el: &Element, class: &str, active: bool) {
    let _ = el.class_list().toggle_with_force(class, active);
}

pub fn value(el: &Element) -> String {
    if let Some(input) = el.dyn_ref::<HtmlInputElement>() {
        input.value()
    } else if let Some(input) = el.dyn_ref::<HtmlSelectElement>() {
        input.value()
    } else if let Some(input) = el.dyn_ref::<HtmlTextAreaElement>() {
        input.value()
    } else {
        String::new()
    }
}
pub fn set_value(el: &Element, value: &str) {
    if let Some(input) = el.dyn_ref::<HtmlInputElement>() {
        input.set_value(value);
    } else if let Some(input) = el.dyn_ref::<HtmlSelectElement>() {
        input.set_value(value);
    } else if let Some(input) = el.dyn_ref::<HtmlTextAreaElement>() {
        input.set_value(value);
    }
}
pub fn target(event: &Event) -> Option<Element> {
    event.target()?.dyn_into().ok()
}
pub fn closest(event: &Event, selector: &str) -> Option<Element> {
    target(event)?.closest(selector).ok().flatten()
}

pub fn listen(target: &EventTarget, event: &str, callback: impl FnMut(Event) + 'static) {
    let handler = Closure::<dyn FnMut(Event)>::new(callback);
    target
        .add_event_listener_with_callback(event, handler.as_ref().unchecked_ref())
        .expect("browser listener");
    handler.forget();
}

pub fn click(name: &str, mut callback: impl FnMut() + 'static) {
    listen(&id(name), "click", move |_| callback());
}

pub fn spawn(future: impl Future<Output = Result> + 'static) {
    spawn_local(async move {
        if let Err(error) = future.await {
            toast(&error.message, true);
        }
    });
}

pub fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

pub fn string(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
pub fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        value => value.to_string(),
    }
}
pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
pub fn image(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "avif" | "gif" | "ico" | "jpg" | "jpeg" | "png" | "webp"
        )
    })
}
pub fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}
pub fn path_query(path: &str) -> String {
    format!("?path={}", encoded(path))
}
pub fn option(value: &str, selected: &str) -> String {
    format!(
        r#"<option value="{}"{}>{}</option>"#,
        escape(value),
        if selected == value { " selected" } else { "" },
        escape(value)
    )
}

static ICONS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("icons.json")).expect("valid icons"));

pub fn icon(name: &str, size: u32) -> String {
    let path = ICONS[name].as_str().unwrap_or_default();
    format!(
        r#"<svg width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{path}</svg>"#
    )
}

pub fn icons() {
    for element in all("[data-icon]") {
        let name = element.get_attribute("data-icon").unwrap_or_default();
        element.set_inner_html(&icon(&name, if name == "brand" { 24 } else { 18 }));
    }
}

pub fn toast(message: &str, error: bool) {
    let element = doc().create_element("div").expect("toast");
    element.set_class_name(if error { "toast error" } else { "toast" });
    element.set_inner_html(&format!(
        "{}<span>{}</span>",
        icon(if error { "warning" } else { "check-circle" }, 16),
        escape(message)
    ));
    let _ = id("toasts").append_child(&element);
    Timeout::new(if error { 6500 } else { 3500 }, move || element.remove()).forget();
}

pub async fn request(route: &str, method: &str, body: Option<&Value>) -> Result<Response> {
    let token = find("meta[name=freeflow-token]")
        .and_then(|meta| meta.get_attribute("content"))
        .ok_or_else(|| Error::new("Reload Freeflow to reconnect."))?;
    let headers = Headers::new()?;
    headers.set("X-Freeflow-Token", &token)?;
    let init = RequestInit::new();
    init.set_method(method);
    if let Some(value) = body {
        headers.set("Content-Type", "application/json")?;
        init.set_body(&JsValue::from_str(&value.to_string()));
    }
    init.set_headers(&headers);
    let request = Request::new_with_str_and_init(&format!("/api{route}"), &init)?;
    let response: Response = JsFuture::from(win().fetch_with_request(&request))
        .await?
        .dyn_into()?;
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    let error = json_response(&response).await.unwrap_or_else(|_| json!({}));
    let message = error["error"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Freeflow returned HTTP {status}."));
    Err(Error { status, message })
}

async fn json_response(response: &Response) -> Result<Value> {
    let text = JsFuture::from(response.text()?)
        .await?
        .as_string()
        .unwrap_or_default();
    serde_json::from_str(&text).map_err(|_| Error::new("Freeflow returned an invalid response."))
}
pub async fn api(route: &str) -> Result<Value> {
    json_response(&request(route, "GET", None).await?).await
}
pub async fn send(route: &str, method: &str, data: Value) -> Result<Value> {
    json_response(&request(route, method, Some(&data)).await?).await
}
pub async fn post(route: &str, data: Value) -> Result<Value> {
    send(route, "POST", data).await
}
pub fn project_route(project: &str, part: &str) -> String {
    format!("/projects/{project}/{part}")
}
pub async fn project_get(project: &str, part: &str) -> Result<Value> {
    api(&project_route(project, part)).await
}
pub async fn project_post(project: &str, part: &str, data: Value) -> Result<Value> {
    post(&project_route(project, part), data).await
}
