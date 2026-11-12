use crate::dom::{items, string};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

pub const MAIN: &str = "lib/main.dart";
pub const PLAN: &str = "FREEFLOW_PLAN.md";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Design,
    Code,
    Plan,
}
impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Code => "code",
            Self::Plan => "plan",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "code" => Self::Code,
            "plan" => Self::Plan,
            _ => Self::Design,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Draft {
    pub content: String,
    pub revision: Value,
}

pub struct State {
    pub projects: Vec<Value>,
    pub templates: Vec<Value>,
    pub project: Value,
    pub flutter: Value,
    pub mode: Mode,
    pub widgets: bool,
    pub file: String,
    pub last_dart: String,
    pub source: Value,
    pub file_error: String,
    pub tabs: Vec<String>,
    pub tree: Vec<Value>,
    pub visual: Value,
    pub widget: String,
    pub folders: HashSet<String>,
    pub collapsed: HashSet<String>,
    pub drafts: HashMap<(String, String), Draft>,
    pub preview: Value,
    pub preview_key: String,
    pub device: String,
    pub logs: Vec<Value>,
    pub console_job: String,
    pub console_open: bool,
    pub asset_url: Option<String>,
    pub generation: u64,
    pub file_load: u64,
    pub dragged: Option<Value>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            projects: vec![],
            templates: vec![],
            project: Value::Null,
            flutter: Value::Null,
            mode: Mode::Design,
            widgets: true,
            file: MAIN.into(),
            last_dart: MAIN.into(),
            source: Value::Null,
            file_error: String::new(),
            tabs: vec![],
            tree: vec![],
            visual: Value::Null,
            widget: String::new(),
            folders: ["lib".into(), "assets".into()].into(),
            collapsed: HashSet::new(),
            drafts: HashMap::new(),
            preview: Value::Null,
            preview_key: String::new(),
            device: "phone".into(),
            logs: vec![],
            console_job: String::new(),
            console_open: false,
            asset_url: None,
            generation: 0,
            file_load: 0,
            dragged: None,
        }
    }
}

impl State {
    pub fn project_id(&self) -> &str {
        string(&self.project["id"])
    }
    pub fn current_file(&self) -> &str {
        if self.mode == Mode::Plan {
            PLAN
        } else {
            &self.file
        }
    }
    pub fn draft_key(&self, path: &str) -> (String, String) {
        (self.project_id().to_owned(), path.to_owned())
    }
    pub fn node(&self, id: &str) -> Option<&Value> {
        items(&self.visual["nodes"])
            .iter()
            .find(|node| node["id"] == id)
    }
    pub fn widget(&self) -> Option<&Value> {
        self.node(&self.widget)
    }
    pub fn siblings(&self, node: &Value) -> Vec<&Value> {
        items(&self.visual["nodes"])
            .iter()
            .filter(|entry| {
                entry["movable"] == true
                    && entry["parent"] == node["parent"]
                    && entry["listSlot"] == node["listSlot"]
            })
            .collect()
    }
}

thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

pub fn read<R>(function: impl FnOnce(&State) -> R) -> R {
    STATE.with_borrow(function)
}
pub fn change<R>(function: impl FnOnce(&mut State) -> R) -> R {
    STATE.with_borrow_mut(function)
}
