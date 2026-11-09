//! Keyboard-first terminal workbench. Native stores are read in workers;
//! writes require an explicit action on a successfully prepared plan.

use crate::{
    app::{self, Catalog, SessionDetails, TransferPlan, TransferRequest},
    ir::{SessionRef, WriteOutcome},
    providers::{self, Homes},
};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::{
    io::{self, IsTerminal},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

const ACCENT: Color = Color::Cyan;

#[derive(Default, Clone)]
struct Input {
    value: String,
    cursor: usize,
}

impl Input {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self { value, cursor }
    }
    fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.value[..self.cursor].char_indices().last().unwrap().0;
                self.value.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.value.len() => {
                let end =
                    self.cursor + self.value[self.cursor..].chars().next().unwrap().len_utf8();
                self.value.drain(self.cursor..end);
            }
            KeyCode::Left if self.cursor > 0 => {
                self.cursor = self.value[..self.cursor].char_indices().last().unwrap().0
            }
            KeyCode::Right if self.cursor < self.value.len() => {
                self.cursor += self.value[self.cursor..].chars().next().unwrap().len_utf8()
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.len(),
            _ => {}
        }
    }
    fn display(&self, focused: bool) -> String {
        if focused {
            format!(
                "{}|{}",
                &self.value[..self.cursor],
                &self.value[self.cursor..]
            )
        } else {
            self.value.clone()
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Library,
    Transfer,
    Import,
    Export,
    Preview,
    Result,
    Details,
    Help,
}

enum Update {
    Catalog(Catalog),
    Details(Result<SessionDetails, String>),
    Plan(Result<Box<TransferPlan>, String>),
    Written(Result<WriteOutcome, String>),
    Exported(Result<serde_json::Value, String>),
}

struct Workbench {
    homes: Homes,
    catalog: Catalog,
    provider: Option<String>,
    query: Input,
    searching: bool,
    selected: usize,
    table: TableState,
    screen: Screen,
    source: Option<SessionRef>,
    destinations: Vec<&'static str>,
    destination: usize,
    fields: [Input; 3],
    file: Input,
    flags: [bool; 3],
    focus: usize,
    plan: Option<Box<TransferPlan>>,
    output: String,
    error: Option<String>,
    busy: Option<String>,
    write_in_progress: bool,
    scroll: u16,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
}

impl Workbench {
    fn new(homes: Homes, provider: Option<String>) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            homes,
            catalog: Catalog::default(),
            provider,
            query: Input::default(),
            searching: false,
            selected: 0,
            table: TableState::default(),
            screen: Screen::Library,
            source: None,
            destinations: providers::CATALOG
                .iter()
                .filter(|p| p.writable)
                .map(|p| p.name)
                .collect(),
            destination: 1,
            fields: [
                Input::default(),
                Input::default(),
                Input::new(crate::resume::DEFAULT_MAX_CHARS.to_string()),
            ],
            file: Input::default(),
            flags: [false; 3],
            focus: 0,
            plan: None,
            output: String::new(),
            error: None,
            busy: None,
            write_in_progress: false,
            scroll: 0,
            sender,
            receiver,
        }
    }
    fn refresh(&mut self) {
        self.busy = Some("Reading local session stores...".into());
        self.error = None;
        let homes = self.homes.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(Update::Catalog(app::discover(&homes)));
        });
    }
    fn visible(&self) -> Vec<&SessionRef> {
        self.catalog
            .sessions
            .iter()
            .filter(|r| {
                self.provider.as_ref().is_none_or(|p| p == &r.provider)
                    && app::matches(r, &self.query.value)
            })
            .collect()
    }
    fn current(&self) -> Option<SessionRef> {
        self.visible().get(self.selected).map(|r| (*r).clone())
    }
    fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.visible().len().saturating_sub(1));
    }
    fn cycle_provider(&mut self, delta: isize) {
        let names: Vec<_> = std::iter::once(None)
            .chain(
                self.catalog
                    .providers
                    .iter()
                    .map(|s| Some(s.info.name.to_string())),
            )
            .collect();
        let index = names.iter().position(|p| p == &self.provider).unwrap_or(0);
        self.provider =
            names[(index as isize + delta).rem_euclid(names.len() as isize) as usize].clone();
        self.selected = 0;
    }
    fn begin_transfer(&mut self, import: bool) {
        self.source = if import { None } else { self.current() };
        if !import && self.source.is_none() {
            return;
        }
        self.screen = if import {
            Screen::Import
        } else {
            Screen::Transfer
        };
        self.fields = [
            Input::default(),
            Input::default(),
            Input::new(crate::resume::DEFAULT_MAX_CHARS.to_string()),
        ];
        self.flags = [false; 3];
        self.focus = 0;
        self.error = None;
        self.plan = None;
        self.file = Input::default();
        if let Some(source) = &self.source {
            self.fields[0] = Input::new(source.cwd.clone().unwrap_or_default());
            self.fields[1] = Input::new(source.title.clone().unwrap_or_default());
            self.destination = self
                .destinations
                .iter()
                .position(|p| *p == "codex" && *p != source.provider)
                .unwrap_or_else(|| {
                    self.destinations
                        .iter()
                        .position(|p| *p != source.provider)
                        .unwrap_or(0)
                });
        }
    }
    fn prepare(&mut self) {
        let request = (|| -> anyhow::Result<TransferRequest> {
            let mut request = if let Some(source) = &self.source {
                TransferRequest::new(source, self.destinations[self.destination])
            } else {
                let empty = SessionRef {
                    provider: String::new(),
                    id: String::new(),
                    title: None,
                    cwd: None,
                    created_ms: None,
                    updated_ms: None,
                    locator: String::new(),
                    migrated: false,
                };
                let mut request = TransferRequest::new(&empty, self.destinations[self.destination]);
                anyhow::ensure!(
                    !self.file.value.trim().is_empty(),
                    "enter a portable session file"
                );
                request.file = Some(self.file.value.trim().into());
                request
            };
            request.cwd = Some(self.fields[0].value.clone());
            request.name = Some(self.fields[1].value.clone());
            request.resume_max_chars = self.fields[2].value.parse().map_err(|_| {
                anyhow::anyhow!("context budget must be a whole number of characters")
            })?;
            request.include_subagents = self.flags[0];
            request.prune_resume_context = self.flags[1];
            request.rebuild_resume_context = self.flags[2];
            Ok(request)
        })();
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        self.busy = Some("Preparing conversion and checking retained context...".into());
        self.error = None;
        let homes = self.homes.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send(Update::Plan(
                TransferPlan::prepare(request, &homes)
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}")),
            ));
        });
    }
    fn updates(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.receiver.try_recv() {
            changed = true;
            self.busy = None;
            self.write_in_progress = false;
            self.scroll = 0;
            match update {
                Update::Catalog(catalog) => {
                    self.catalog = catalog;
                    self.selected = self.selected.min(self.visible().len().saturating_sub(1));
                }
                Update::Details(Ok(details)) => {
                    self.output=format!("{}\n{} / {}\n\n{} events  ·  {} messages  ·  {} tool calls\nRetained context: {} characters\n{}\n\n{}",
                    details.reference.title.as_deref().unwrap_or("Untitled session"),details.reference.provider,details.reference.id,
                    details.events,details.messages,details.tool_calls,details.resume_chars,
                    details.unavailable_context.map(|s|format!("Recovery required: {s}")).unwrap_or_default(),details.excerpt);
                    self.screen = Screen::Details;
                }
                Update::Plan(Ok(plan)) => {
                    self.plan = Some(plan);
                    self.screen = Screen::Preview;
                }
                Update::Written(Ok(outcome)) => {
                    self.output = outcome_text(&outcome, false);
                    self.screen = Screen::Result;
                }
                Update::Exported(Ok(outcome)) => {
                    self.output=format!("Export saved\n\n{}\n\n{} sessions · {} events · {} bytes\n\nCopy this file to the receiving machine and choose Import a file.",outcome["location"].as_str().unwrap_or_default(),outcome["sessions"],outcome["events"],outcome["bytes"]);
                    self.screen = Screen::Result;
                }
                Update::Details(Err(error))
                | Update::Plan(Err(error))
                | Update::Written(Err(error))
                | Update::Exported(Err(error)) => self.error = Some(error),
            }
        }
        changed
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return !self.write_in_progress;
        }
        if self.busy.is_some() {
            return !self.write_in_progress
                && self.screen == Screen::Library
                && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc);
        }
        if self.screen == Screen::Library {
            if self.searching {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => self.searching = false,
                    _ => self.query.key(key),
                };
                self.selected = 0;
                return false;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::PageDown => self.move_selection(10),
                KeyCode::PageUp => self.move_selection(-10),
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = self.visible().len().saturating_sub(1),
                KeyCode::Tab | KeyCode::Right => self.cycle_provider(1),
                KeyCode::BackTab | KeyCode::Left => self.cycle_provider(-1),
                KeyCode::Char('/') => self.searching = true,
                KeyCode::Char('r') => self.refresh(),
                KeyCode::Char('m') | KeyCode::Enter => self.begin_transfer(false),
                KeyCode::Char('i') => self.begin_transfer(true),
                KeyCode::Char('e') => {
                    if let Some(source) = self.current() {
                        self.file = Input::new(format!("{}.hbridge.json", source.id));
                        self.source = Some(source);
                        self.flags[0] = false;
                        self.focus = 0;
                        self.error = None;
                        self.screen = Screen::Export;
                    }
                }
                KeyCode::Char('v') => {
                    if let Some(reference) = self.current() {
                        let homes = self.homes.clone();
                        let sender = self.sender.clone();
                        self.busy = Some("Reading the selected conversation...".into());
                        std::thread::spawn(move || {
                            let _ = sender.send(Update::Details(
                                app::inspect(&reference, &homes).map_err(|e| format!("{e:#}")),
                            ));
                        });
                    }
                }
                KeyCode::Char('?') => {
                    self.output = HELP.into();
                    self.screen = Screen::Help;
                    self.scroll = 0;
                }
                _ => {}
            }
            return false;
        }
        if key.code == KeyCode::Esc {
            if self.screen == Screen::Preview {
                self.screen = if self.source.is_some() {
                    Screen::Transfer
                } else {
                    Screen::Import
                };
                self.plan = None;
            } else {
                self.screen = Screen::Library;
                self.error = None;
                self.plan = None;
            }
            return false;
        }
        if matches!(self.screen, Screen::Transfer | Screen::Import) {
            let offset = usize::from(self.screen == Screen::Import);
            let count = 7;
            if key.code != KeyCode::Enter {
                self.error = None;
            }
            match key.code {
                KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % count,
                KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + count - 1) % count,
                KeyCode::Enter => self.prepare(),
                KeyCode::Left | KeyCode::Right if self.focus == offset => {
                    let delta = if key.code == KeyCode::Right { 1 } else { -1 };
                    for _ in 0..self.destinations.len() {
                        self.destination = (self.destination as isize + delta)
                            .rem_euclid(self.destinations.len() as isize)
                            as usize;
                        if self
                            .source
                            .as_ref()
                            .is_none_or(|s| s.provider != self.destinations[self.destination])
                        {
                            break;
                        }
                    }
                }
                KeyCode::Char(' ') if self.focus >= 4 + offset => {
                    self.flags[self.focus - 4] = !self.flags[self.focus - 4]
                }
                _ => {
                    if self.screen == Screen::Import && self.focus == 0 {
                        self.file.key(key);
                    } else if self.focus > offset && self.focus < 4 + offset {
                        self.fields[self.focus - 1 - offset].key(key);
                    }
                }
            }
        } else if self.screen == Screen::Export {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => self.focus = 1 - self.focus,
                KeyCode::Char(' ') if self.focus == 1 => self.flags[0] = !self.flags[0],
                KeyCode::Enter => {
                    if self.file.value.trim().is_empty() {
                        self.error = Some("choose a new export file path".into());
                        return false;
                    }
                    if let Some(reference) = self.source.clone() {
                        let homes = self.homes.clone();
                        let sender = self.sender.clone();
                        let file = self.file.value.trim().to_string();
                        let children = self.flags[0];
                        self.busy = Some("Saving portable export...".into());
                        self.write_in_progress = true;
                        self.error = None;
                        std::thread::spawn(move || {
                            let _ = sender.send(Update::Exported(
                                app::export(&reference, &homes, Path::new(&file), children, false)
                                    .map_err(|e| format!("{e:#}")),
                            ));
                        });
                    }
                }
                _ if self.focus == 0 => self.file.key(key),
                _ => {}
            }
        } else if self.screen == Screen::Preview && key.code == KeyCode::Char('w') {
            if let Some(plan) = self.plan.take() {
                let sender = self.sender.clone();
                self.busy = Some("Writing the destination session...".into());
                self.write_in_progress = true;
                self.error = None;
                std::thread::spawn(move || {
                    let _ = sender.send(Update::Written(
                        (*plan).execute().map_err(|e| format!("{e:#}")),
                    ));
                });
            }
        } else {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::Enter if self.screen == Screen::Result => {
                    self.screen = Screen::Library;
                    self.refresh();
                }
                _ => {}
            }
        }
        false
    }
    fn paste(&mut self, text: &str) {
        if self.busy.is_some() {
            return;
        }
        match self.screen {
            Screen::Library => {
                self.searching = true;
                self.query.insert(text);
                self.selected = 0;
            }
            Screen::Export if self.focus == 0 => self.file.insert(text),
            Screen::Import if self.focus == 0 => self.file.insert(text),
            Screen::Import | Screen::Transfer => {
                let offset = usize::from(self.screen == Screen::Import);
                if self.focus > offset && self.focus < offset + 4 {
                    self.fields[self.focus - 1 - offset].insert(text);
                }
            }
            _ => {}
        }
    }
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 50 || area.height < 15 {
            frame.render_widget(Paragraph::new("harness-bridge\n\nEnlarge the terminal to at least 50 × 15.\nPress q or Ctrl+C to exit.").wrap(Wrap{trim:false}),area);
            return;
        }
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);
        let header = Line::from(vec![
            Span::styled(
                " harness-bridge ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {}  ·  {} local sessions",
                env!("CARGO_PKG_VERSION"),
                self.catalog.sessions.len()
            )),
        ]);
        frame.render_widget(
            Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
            rows[0],
        );
        self.draw_library(frame, rows[1]);
        let hint = if let Some(busy) = &self.busy {
            busy.clone()
        } else if self.screen == Screen::Library {
            if self.searching {
                "Type to filter · Enter keep search · Esc return to list · Ctrl+U clear".into()
            } else {
                "↑↓ select  ·  Tab source  ·  / search  ·  Enter migrate  ·  v view  ·  e export  ·  i import  ·  r refresh  ·  ? help  ·  q quit".into()
            }
        } else if self.screen == Screen::Preview {
            "w write session  ·  Esc change options  ·  ↑↓ / PgUp PgDn scroll".into()
        } else if matches!(self.screen, Screen::Transfer | Screen::Import) {
            "Tab next  ·  ←→ destination / cursor  ·  Space toggle  ·  Enter preview  ·  Esc cancel"
                .into()
        } else if self.screen == Screen::Export {
            "Tab next  ·  Space toggle  ·  Enter save export  ·  Esc cancel".into()
        } else {
            "↑↓ / PgUp PgDn scroll  ·  Esc return  ·  Enter refresh after import".into()
        };
        frame.render_widget(
            Paragraph::new(hint)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::TOP)),
            rows[2],
        );
        if self.screen != Screen::Library {
            self.draw_overlay(frame, rows[1]);
        }
        if let Some(error) = &self.error {
            let h = (rows[1].height / 2).clamp(5, 10);
            let rect = Rect {
                x: rows[1].x + 1,
                y: rows[1].bottom().saturating_sub(h),
                width: rows[1].width.saturating_sub(2),
                height: h,
            };
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(clean(error))
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::bordered()
                            .title(" Could not complete this step ")
                            .border_style(Style::default().fg(Color::Red)),
                    ),
                rect,
            );
        }
    }
    fn draw_library(&mut self, frame: &mut Frame, area: Rect) {
        let columns = Layout::horizontal([Constraint::Length(21), Constraint::Min(25)]).split(area);
        let mut sources = vec![
            Line::styled("Sources", Style::default().add_modifier(Modifier::BOLD)),
            Line::raw(""),
        ];
        sources.push(Line::styled(
            format!(
                "{} All sessions",
                if self.provider.is_none() { ">" } else { " " }
            ),
            Style::default().fg(if self.provider.is_none() {
                ACCENT
            } else {
                Color::Reset
            }),
        ));
        for status in &self.catalog.providers {
            sources.push(Line::styled(
                format!(
                    "{} {:<11} {}",
                    if self.provider.as_deref() == Some(status.info.name) {
                        ">"
                    } else {
                        " "
                    },
                    status.info.name,
                    if status.problem.is_some() {
                        "—".into()
                    } else {
                        status.sessions.to_string()
                    }
                ),
                Style::default().fg(if self.provider.as_deref() == Some(status.info.name) {
                    ACCENT
                } else {
                    Color::Reset
                }),
            ));
        }
        sources.push(Line::raw(""));
        sources.push(Line::raw("— unavailable"));
        frame.render_widget(
            Paragraph::new(sources).block(Block::default().borders(Borders::RIGHT)),
            columns[0],
        );
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(7),
        ])
        .split(columns[1]);
        frame.render_widget(
            Paragraph::new(format!(" {}", self.query.display(self.searching))).block(
                Block::bordered()
                    .title(" Search title, folder or id ")
                    .border_style(Style::default().fg(if self.searching {
                        ACCENT
                    } else {
                        Color::Reset
                    })),
            ),
            rows[0],
        );
        let references = self.visible();
        let count = references.len();
        let data: Vec<_> = references
            .iter()
            .map(|reference| {
                let date = reference
                    .updated_ms
                    .or(reference.created_ms)
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map_or("—".into(), |date| date.format("%b %d").to_string());
                Row::new(vec![
                    Cell::from(reference.provider.clone()),
                    Cell::from(clean(
                        reference.title.as_deref().unwrap_or("Untitled session"),
                    )),
                    Cell::from(date),
                ])
            })
            .collect();
        let table = Table::new(
            data,
            [
                Constraint::Length(11),
                Constraint::Min(10),
                Constraint::Length(7),
            ],
        )
        .header(
            Row::new(["Harness", "Conversation", "Updated"])
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        )
        .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
        .highlight_symbol("> ")
        .block(
            Block::default()
                .title(format!(" {count} sessions "))
                .borders(Borders::BOTTOM),
        );
        self.table.select((count > 0).then_some(self.selected));
        frame.render_stateful_widget(table, rows[1], &mut self.table);
        let detail = if let Some(reference) = self.current() {
            format!(
                "{}\n{}\n{}\n{}",
                clean(reference.title.as_deref().unwrap_or("Untitled session")),
                reference.id,
                clean(reference.cwd.as_deref().unwrap_or("No working directory")),
                if reference.migrated {
                    "Previously imported into Codex. Enter previews another transfer."
                } else {
                    "Enter previews a transfer. Press v to read a conversation excerpt."
                }
            )
        } else if let Some(status) = self
            .catalog
            .providers
            .iter()
            .find(|s| self.provider.as_deref() == Some(s.info.name))
        {
            format!(
                "{}\n\n{}\n{}",
                status.info.label,
                status
                    .problem
                    .as_deref()
                    .unwrap_or("No sessions in this source."),
                status.info.note
            )
        } else if self.busy.is_some() {
            "Reading local session stores...".into()
        } else {
            "No matching sessions. Press / to change your search, r to refresh, or i to import a portable file.\nUse --export <path> to browse web chat exports; --qoder-home and other home flags select custom stores.".into()
        };
        frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), rows[2]);
    }
    fn draw_overlay(&self, frame: &mut Frame, area: Rect) {
        let rect = center(area, 94, area.height);
        frame.render_widget(Clear, rect);
        let (title,text)=match self.screen {
            Screen::Transfer|Screen::Import=>(if self.screen==Screen::Import{" Import a portable file "}else{" Migrate a conversation "},self.form_text()),
            Screen::Export=>(" Save a portable export ",format!("{}\n\n{}File\n  {}\n\n{}[{}] Include saved child sessions\n\nChoose a new file path. Existing files are never overwritten.",
                clean(self.source.as_ref().and_then(|r|r.title.as_deref()).unwrap_or("Selected conversation")),if self.focus==0{"> "}else{"  "},self.file.display(self.focus==0),if self.focus==1{"> "}else{"  "},if self.flags[0]{"x"}else{" "})),
            Screen::Preview=>(" Review before writing ",self.plan.as_ref().map(|plan|format!("{}\n\n{}\n\nPress w to write this session.",outcome_text(&plan.preview,true),plan.warnings.iter().map(|s|format!("• {s}")).collect::<Vec<_>>().join("\n\n"))).unwrap_or_else(||"No plan prepared. Press Esc to return.".into())),
            Screen::Details=>(" Conversation excerpt ",self.output.clone()),Screen::Help=>(" Keyboard and workflow ",self.output.clone()),
            _=>(" Result ",self.output.clone()),
        };
        let paragraph = Paragraph::new(clean(&text)).wrap(Wrap { trim: false });
        let scroll = if matches!(
            self.screen,
            Screen::Transfer | Screen::Import | Screen::Export
        ) {
            // Follow the focused field even when labels or long paths wrap.
            let prefix = text
                .lines()
                .take_while(|line| !line.starts_with("> "))
                .collect::<Vec<_>>()
                .join("\n");
            let width = rect.width.saturating_sub(2).max(1) as usize;
            let field_line: usize = prefix
                .lines()
                .map(|line| Line::raw(line).width().max(1).div_ceil(width))
                .sum();
            field_line
                .saturating_sub(rect.height.saturating_sub(5) as usize)
                .min(u16::MAX as usize) as u16
        } else {
            self.scroll
        };
        frame.render_widget(
            paragraph
                .block(
                    Block::bordered()
                        .title(title)
                        .border_style(Style::default().fg(ACCENT)),
                )
                .scroll((scroll, 0)),
            rect,
        );
    }
    fn form_text(&self) -> String {
        let offset = usize::from(self.screen == Screen::Import);
        let mark = |i| if self.focus == i { "> " } else { "  " };
        let mut text = if offset == 1 {
            format!(
                "{}Portable file\n  {}\n\n",
                mark(0),
                self.file.display(self.focus == 0)
            )
        } else {
            let reference = self.source.as_ref().unwrap();
            format!(
                "{}\n{} / {}\n\n",
                clean(reference.title.as_deref().unwrap_or("Untitled session")),
                reference.provider,
                reference.id
            )
        };
        text.push_str(&format!(
            "{}Destination    < {} >\n\n",
            mark(offset),
            providers::info(self.destinations[self.destination])
                .map(|p| p.label)
                .unwrap_or("")
        ));
        for (index, label) in [
            "Destination folder (blank keeps the source folder)",
            "Display title (blank keeps the source title)",
            "Resume character budget",
        ]
        .iter()
        .enumerate()
        {
            text.push_str(&format!(
                "{}{}\n  {}\n\n",
                mark(index + 1 + offset),
                label,
                self.fields[index].display(self.focus == index + 1 + offset)
            ));
        }
        for (index, label) in [
            if offset == 1 {
                "All bundled children are imported automatically"
            } else {
                "Include saved child sessions"
            },
            "Shorten reasoning and tool outputs if over budget",
            "Rebuild unavailable context from readable history",
        ]
        .iter()
        .enumerate()
        {
            if offset == 1 && index == 0 {
                text.push_str("  All bundled child sessions are included\n");
                continue;
            }
            text.push_str(&format!(
                "{}[{}] {}\n",
                mark(index + 4),
                if self.flags[index] { "x" } else { " " },
                label
            ));
        }
        text.push_str("\nEnter previews the conversion. Writing is a separate step.");
        text
    }
}

use std::path::Path;

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn outcome_text(outcome: &WriteOutcome, preview: bool) -> String {
    let policy = &outcome.extra["resume_context_policy"];
    let mut text = format!(
        "{}\n\nDestination: {}\nSession id: {}\nLocation: {}",
        if preview {
            "Conversion ready; nothing written"
        } else {
            "Session written"
        },
        outcome.provider,
        outcome.native_id,
        outcome.location
    );
    if let Some(chars) = policy["retained_chars"]
        .as_u64()
        .or_else(|| policy["input_chars"].as_u64())
    {
        text.push_str(&format!("\nResume context: {chars} characters"));
    }
    if let Some(children) = outcome.extra["child_sessions"].as_array() {
        text.push_str(&format!("\nSaved child sessions: {}", children.len()));
    }
    if let Some(command) = outcome.extra["resume_command"].as_str() {
        text.push_str(&format!("\n\nContinue with: {command}"));
    }
    if outcome.provider == "codex" {
        text.push_str("\n\nRefresh the Codex window if the session is not visible.");
    }
    text
}

const HELP:&str="Browse\n\n↑/↓ or j/k select a conversation. Page Up/Down moves ten rows.\nTab or ←/→ changes the source. / searches titles, folders and ids.\nEnter opens migration options. v reads an excerpt.\ne saves a portable file. i imports one from another machine.\nr refreshes local discovery. q exits.\n\nTransfer\n\nTab moves between fields. ←/→ changes the destination.\nText fields support cursor movement, Home/End, Delete and paste.\nCtrl+U clears a text field. Space toggles an option.\nEnter prepares a preview without writing. Read the destination and\nconversion notes, then press w to write. Esc returns to the form.\n\nSources are never changed. Reimporting the same source updates the\nsame destination id and can replace turns added to that copy.\nContext recovery and shortening are explicit options.\n\nUse --export <path> when starting to browse a web chat data export.\nUse home override flags to inspect custom or isolated stores.";

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}

pub fn run(homes: Homes, provider: Option<String>) -> anyhow::Result<()> {
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "the terminal workbench needs an interactive terminal; use list --json for scripts"
    );
    let provider = provider
        .map(|name| providers::info(&name).map(|p| p.name.to_owned()))
        .transpose()?;
    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let mut app = Workbench::new(homes, provider);
    app.refresh();
    let mut dirty = true;
    loop {
        dirty |= app.updates();
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100))? {
            dirty = true;
            match event::read()? {
                Event::Key(key) if app.key(key) => break,
                Event::Paste(text) => app.paste(&text),
                Event::Mouse(mouse) if app.screen == Screen::Library && app.busy.is_none() => {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => app.move_selection(3),
                        MouseEventKind::ScrollUp => app.move_selection(-3),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_editing_keeps_cursor_boundaries() {
        let mut input = Input::new("a文🦀");
        input.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        input.insert("é\u{1b}\n");
        assert_eq!(input.value, "aé🦀");
        input.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.value, "aé");
    }
    #[test]
    fn renders_empty_and_small_terminal_without_panicking() {
        for (width, height) in [(110, 36), (50, 15), (20, 8)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut app = Workbench::new(Homes::default(), None);
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
    }
    #[test]
    fn write_key_cannot_write_without_a_prepared_plan() {
        let mut app = Workbench::new(Homes::default(), None);
        app.screen = Screen::Preview;
        assert!(!app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)));
        assert!(app.busy.is_none());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.plan.is_none());
    }
}
