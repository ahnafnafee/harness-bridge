use crate::{client, model::*};
use gpui_kit::component::{
    input::{InputEvent, InputState},
    select::{SelectEvent, SelectItem, SelectState},
    *,
};
use gpui_kit::{prelude::*, *};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone)]
pub struct OptionItem {
    pub title: String,
    pub value: String,
}
impl OptionItem {
    fn new(value: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            title: title.into(),
        }
    }
}
impl SelectItem for OptionItem {
    type Value = String;
    fn title(&self) -> SharedString {
        self.title.clone().into()
    }
    fn value(&self) -> &String {
        &self.value
    }
}
type Choice = Entity<SelectState<Vec<OptionItem>>>;
pub const SOURCE_FIELDS: &[(&str, &str)] = &[
    ("dsh", "DeepSeek Harness"),
    ("codex", "Codex"),
    ("claude", "Claude Code"),
    ("zcode_db", "ZCode database"),
    ("agy", "Antigravity"),
    ("qoder", "Qoder home"),
    ("qoder_ide_db", "Qoder desktop database"),
    ("export", "Web chat data export"),
];

#[derive(Clone, Copy, PartialEq)]
pub enum Pane {
    None,
    Transfer,
    Conversation,
    Review,
    Complete,
    Import,
    Export,
    Settings,
}
pub enum Job {
    Catalog,
    Inspect(String),
    Preview,
    Write,
    Export,
    Settings,
}

pub struct Workbench {
    pub data: CatalogReply,
    pub key: String,
    pub error: String,
    pub status: String,
    pub busy: bool,
    pub history_view: bool,
    pub activities: Vec<Activity>,
    pub source: String,
    pub selected: Option<Session>,
    pub details: Option<Value>,
    pub pane: Pane,
    pub plan: Option<Value>,
    pub outcome: Option<Value>,
    pub plan_title: String,
    pub plan_from: String,
    pub plan_to: String,
    pub importing: bool,
    pub page: usize,
    pub search: Entity<InputState>,
    pub source_choice: Choice,
    pub project_choice: Choice,
    pub sort_choice: Choice,
    pub destination: Choice,
    pub cwd: Entity<InputState>,
    pub name: Entity<InputState>,
    pub budget: Entity<InputState>,
    pub file: Entity<InputState>,
    pub setting_inputs: Vec<Entity<InputState>>,
    pub children: bool,
    pub prune: bool,
    pub rebuild: bool,
    pub advanced: bool,
    pub missing: bool,
    pub subscriptions: Vec<Subscription>,
}

fn input(placeholder: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()))
}
fn choice(items: Vec<OptionItem>, window: &mut Window, cx: &mut App) -> Choice {
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(0)), window, cx))
}
pub fn value(field: &Entity<InputState>, cx: &App) -> String {
    field.read(cx).value().to_string()
}
fn text(field: &Entity<InputState>, cx: &App) -> Option<String> {
    let s = value(field, cx);
    (!s.trim().is_empty()).then(|| s.trim().to_owned())
}
pub fn selected(choice: &Choice, cx: &App) -> String {
    choice
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_default()
}
fn set(field: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
    field.update(cx, |state, cx| state.set_value(value, window, cx));
}

impl Workbench {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            data: CatalogReply::default(),
            key: client::access_key().unwrap_or_default(),
            error: String::new(),
            status: "Reading session stores…".into(),
            busy: false,
            history_view: false,
            activities: client::storage_get("bridge-gpui-history")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            source: String::new(),
            selected: None,
            details: None,
            pane: Pane::None,
            plan: None,
            outcome: None,
            plan_title: String::new(),
            plan_from: String::new(),
            plan_to: String::new(),
            importing: false,
            page: 0,
            search: input("Search sessions, folders or ids", window, cx),
            source_choice: choice(vec![OptionItem::new("", "All sessions")], window, cx),
            project_choice: choice(vec![OptionItem::new("", "All projects")], window, cx),
            sort_choice: choice(
                vec![
                    OptionItem::new("Newest first", "Newest first"),
                    OptionItem::new("Oldest first", "Oldest first"),
                    OptionItem::new("Title A–Z", "Title A–Z"),
                ],
                window,
                cx,
            ),
            destination: choice(Vec::new(), window, cx),
            cwd: input("Keep the source folder", window, cx),
            name: input("Keep the source title", window, cx),
            budget: input("750000", window, cx),
            file: input("Full path on this computer", window, cx),
            setting_inputs: SOURCE_FIELDS
                .iter()
                .map(|_| input("Default location", window, cx))
                .collect(),
            children: false,
            prune: false,
            rebuild: false,
            advanced: false,
            missing: false,
            subscriptions: Vec::new(),
        };
        set(&this.budget, "750000".into(), window, cx);
        this.subscriptions.push(
            cx.subscribe_in(&this.search, window, |this, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.page = 0;
                    cx.notify();
                }
            }),
        );
        this.subscriptions.push(cx.subscribe_in(
            &this.source_choice,
            window,
            |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    if this.busy {
                        this.sync_sources(window, cx);
                        return;
                    }
                    this.choose_source(name.clone(), window, cx);
                }
            },
        ));
        for control in [&this.project_choice, &this.sort_choice] {
            this.subscriptions.push(cx.subscribe_in(
                control,
                window,
                |this, _, _: &SelectEvent<Vec<OptionItem>>, _, cx| {
                    this.page = 0;
                    cx.notify();
                },
            ));
        }
        this.subscriptions.push(cx.subscribe_in(
            &this.destination,
            window,
            |_, _, _: &SelectEvent<Vec<OptionItem>>, _, cx| cx.notify(),
        ));
        this.request("/api/catalog", None, Job::Catalog, window, cx);
        this
    }

    pub fn label(&self, name: &str) -> String {
        self.data
            .providers
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| name.into())
    }
    pub fn rows(&self, cx: &App) -> Vec<Session> {
        let project = selected(&self.project_choice, cx);
        let folder = project.strip_prefix("folder:");
        filtered(
            &self.data.catalog.sessions,
            &self.source,
            folder,
            &value(&self.search, cx),
            &selected(&self.sort_choice, cx),
            &self.data.providers,
        )
    }
    pub fn choose_source(&mut self, source: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.source = source;
        self.page = 0;
        self.history_view = false;
        self.close(cx);
        self.sync_sources(window, cx);
        self.sync_projects(window, cx);
    }
    fn sync_sources(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut labels = vec![OptionItem::new("", "All sessions")];
        labels.extend(
            self.data
                .catalog
                .providers
                .iter()
                .map(|p| OptionItem::new(&p.info.name, &p.info.label)),
        );
        let label = self.source.clone();
        self.source_choice.update(cx, |s, cx| {
            s.set_items(labels, window, cx);
            s.set_selected_value(&label, window, cx);
        });
    }
    fn sync_projects(&self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = vec![OptionItem::new("", "All projects")];
        let folders: BTreeSet<_> = self
            .data
            .catalog
            .sessions
            .iter()
            .filter(|s| self.source.is_empty() || s.provider == self.source)
            .map(|s| s.cwd.clone().unwrap_or_default())
            .collect();
        items.extend(folders.iter().map(|s| {
            let basename = project(s);
            let title =
                if !s.is_empty() && folders.iter().filter(|p| project(p) == basename).count() > 1 {
                    format!("{basename} · {s}")
                } else {
                    basename.to_owned()
                };
            OptionItem::new(format!("folder:{s}"), title)
        }));
        self.project_choice.update(cx, |s, cx| {
            s.set_items(items, window, cx);
            s.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });
    }
    fn destinations(&self, from: &str, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<_> = self
            .data
            .providers
            .iter()
            .filter(|p| p.writable && p.name != from)
            .map(|p| OptionItem::new(&p.name, &p.label))
            .collect();
        let default = if from == "codex" { "claude" } else { "codex" }.to_owned();
        self.destination.update(cx, |s, cx| {
            s.set_items(items, window, cx);
            s.set_selected_value(&default, window, cx);
        });
    }
    pub fn select(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.plan = None;
        self.outcome = None;
        self.error.clear();
        self.details = None;
        self.importing = false;
        self.pane = Pane::Transfer;
        self.children = false;
        self.prune = false;
        self.rebuild = false;
        set(
            &self.cwd,
            session.cwd.clone().unwrap_or_default(),
            window,
            cx,
        );
        set(&self.name, session.title().into(), window, cx);
        self.destinations(&session.provider, window, cx);
        let body = json!({"provider":session.provider,"id":session.id});
        let key = session.key();
        self.selected = Some(session);
        self.request("/api/inspect", Some(body), Job::Inspect(key), window, cx);
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            self.pane = Pane::None;
            self.selected = None;
            self.plan = None;
            self.error.clear();
            cx.notify();
        }
    }
    pub fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.close(cx);
        self.pane = Pane::Import;
        self.importing = true;
        self.children = false;
        self.prune = false;
        self.rebuild = false;
        self.error.clear();
        set(&self.file, String::new(), window, cx);
        set(&self.cwd, String::new(), window, cx);
        set(&self.name, String::new(), window, cx);
        self.destinations("", window, cx);
        cx.notify();
    }
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.close(cx);
        self.pane = Pane::Settings;
        self.error.clear();
        for (i, (name, _)) in SOURCE_FIELDS.iter().enumerate() {
            set(
                &self.setting_inputs[i],
                self.data.settings[*name]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                window,
                cx,
            );
        }
        cx.notify();
    }
    pub fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut settings = json!({});
        for (i, (name, _)) in SOURCE_FIELDS.iter().enumerate() {
            settings[*name] = json!(text(&self.setting_inputs[i], cx));
        }
        self.plan = None;
        self.request("/api/settings", Some(settings), Job::Settings, window, cx);
    }
    pub fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let destination = selected(&self.destination, cx);
        let Some(to) = self
            .data
            .providers
            .iter()
            .find(|p| p.writable && p.name == destination)
            .map(|p| p.name.clone())
        else {
            self.error = "Choose a destination harness".into();
            cx.notify();
            return;
        };
        let Ok(budget) = value(&self.budget, cx).parse::<usize>() else {
            self.error = "Enter a valid resume character budget".into();
            cx.notify();
            return;
        };
        let (from, id, file) = if self.importing {
            (String::new(), String::new(), text(&self.file, cx))
        } else {
            let Some(s) = &self.selected else {
                return;
            };
            (s.provider.clone(), s.id.clone(), None)
        };
        if self.importing && file.is_none() {
            self.error = "Enter the portable file path".into();
            cx.notify();
            return;
        }
        self.plan_title = text(&self.name, cx)
            .or_else(|| file.clone())
            .or_else(|| self.selected.as_ref().map(|s| s.title().into()))
            .unwrap_or_default();
        self.plan_from = if self.importing {
            "Portable file".into()
        } else {
            self.label(&from)
        };
        self.plan_to = self.label(&to);
        let body = json!({"from":from,"id":id,"file":file,"to":to,"cwd":text(&self.cwd,cx),"name":text(&self.name,cx),"include_subagents":self.children,"resume_max_chars":budget,"prune_resume_context":self.prune,"rebuild_resume_context":self.rebuild});
        self.request("/api/preview", Some(body), Job::Preview, window, cx);
    }
    pub fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(plan) = self.plan.take() else {
            return;
        };
        self.request(
            "/api/write",
            Some(json!({"plan_id":plan["plan_id"]})),
            Job::Write,
            window,
            cx,
        );
    }
    pub fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.selected else {
            return;
        };
        let Some(path) = text(&self.file, cx) else {
            self.error = "Enter a portable file path".into();
            cx.notify();
            return;
        };
        self.plan_title = s.title().into();
        self.plan_from = self.label(&s.provider);
        self.plan_to = "Portable file".into();
        self.request("/api/export",Some(json!({"provider":s.provider,"id":s.id,"path":path,"include_subagents":self.children})),Job::Export,window,cx);
    }
    pub fn open_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(s) = &self.selected else {
            return;
        };
        set(&self.file, format!("{}.hbridge.json", s.id), window, cx);
        self.children = false;
        self.pane = Pane::Export;
        self.error.clear();
        cx.notify();
    }
    pub fn request(
        &mut self,
        path: &'static str,
        body: Option<Value>,
        job: Job,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error.clear();
        self.status = match &job {
            Job::Catalog => "Refreshing session stores…",
            Job::Inspect(_) => "Reading conversation…",
            Job::Preview => "Preparing preview. No session is being written…",
            Job::Write => "Transferring the reviewed session…",
            Job::Export => "Saving portable file…",
            Job::Settings => "Saving source locations…",
        }
        .into();
        let key = self.key.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client::request(path, body, &key).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Err(error) => {
                        this.error = error;
                        this.status = "Could not complete this step".into();
                    }
                    Ok(result) => this.receive(job, result, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn receive(&mut self, job: Job, result: Value, window: &mut Window, cx: &mut Context<Self>) {
        match job {
            Job::Catalog => match serde_json::from_value(result) {
                Ok(data) => {
                    self.data = data;
                    self.sync_sources(window, cx);
                    self.sync_projects(window, cx);
                    self.status = format!(
                        "{} sessions across {} sources",
                        self.data.catalog.sessions.len(),
                        self.data
                            .catalog
                            .providers
                            .iter()
                            .filter(|p| p.problem.is_none())
                            .count()
                    );
                }
                Err(_) => self.error = "The local service returned an invalid catalog".into(),
            },
            Job::Inspect(key) => {
                if self.selected.as_ref().is_some_and(|s| s.key() == key) {
                    self.details = Some(result);
                }
                self.status = "Conversation loaded".into();
            }
            Job::Preview => {
                self.plan = Some(result);
                self.pane = Pane::Review;
                self.status = "Preview ready. No session has been written".into();
            }
            Job::Write | Job::Export => {
                let location = result["location"]
                    .as_str()
                    .or_else(|| result["path"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                self.activities.insert(
                    0,
                    Activity {
                        title: self.plan_title.clone(),
                        from: self.plan_from.clone(),
                        to: self.plan_to.clone(),
                        location,
                        date: js_sys::Date::now(),
                    },
                );
                self.activities.truncate(50);
                if let Ok(history) = serde_json::to_string(&self.activities) {
                    client::storage_set("bridge-gpui-history", &history);
                }
                self.outcome = Some(result);
                self.pane = Pane::Complete;
                self.status = "Completed".into();
            }
            Job::Settings => {
                self.pane = Pane::None;
                self.request("/api/catalog", None, Job::Catalog, window, cx);
            }
        }
    }
}
