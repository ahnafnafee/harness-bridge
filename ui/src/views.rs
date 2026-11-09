use crate::{client, model::*, workbench::*};
use gpui_kit::component::{
    button::*,
    checkbox::Checkbox,
    input::{Input, InputState},
    select::Select,
    *,
};
use gpui_kit::{prelude::*, *};
use serde_json::Value;

const INK: u32 = 0x182034;
const MUTED: u32 = 0x637089;
const COBALT: u32 = 0x193bd1;
const LINE: u32 = 0xe3e8f0;
const WASH: u32 = 0xedf1ff;

fn note(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn caption(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(MUTED))
        .child(text.into())
}
fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(22.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}
fn field(label: &str, input: &Entity<InputState>, busy: bool) -> Div {
    v_flex()
        .gap_2()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_owned()),
        )
        .child(Input::new(input).large().disabled(busy).w_full())
}
fn detail(label: &str, text: impl Into<SharedString>) -> Div {
    v_flex()
        .gap_1()
        .child(caption(label.to_owned()))
        .child(div().text_size(px(13.)).child(text.into()))
}
fn date(ms: Option<i64>) -> String {
    ms.map(|ms| {
        js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64))
            .to_locale_date_string("default", &wasm_bindgen::JsValue::UNDEFINED)
            .as_string()
            .unwrap_or_default()
    })
    .unwrap_or_else(|| "—".into())
}
fn json_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

impl Workbench {
    fn header(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let origin = web_sys::window()
            .and_then(|w| w.location().origin().ok())
            .unwrap_or_default();
        h_flex()
            .h(px(68.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgb(LINE))
            .px_5()
            .gap_4()
            .when(compact, |row| row.px_3().gap_1())
            .child(
                img(format!("{origin}/icon.png"))
                    .size(px(38.))
                    .when(compact, |image| image.size(px(30.)))
                    .rounded(px(10.)),
            )
            .when(!compact, |row| {
                row.child(
                    div()
                        .w(px(150.))
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("harness-bridge"),
                )
            })
            .child(
                Button::new("nav-library")
                    .ghost()
                    .label("Library")
                    .when(!self.history_view, |button| button.primary())
                    .h(px(36.))
                    .when(compact, |button| button.small().h(px(44.)))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.history_view = false;
                        this.close(cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("nav-history")
                    .ghost()
                    .label("History")
                    .when(self.history_view, |button| button.primary())
                    .h(px(36.))
                    .when(compact, |button| button.small().h(px(44.)))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.history_view = true;
                        this.close(cx);
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("import")
                    .label(if compact { "Import" } else { "Import file" })
                    .h(px(36.))
                    .when(compact, |button| button.small().h(px(44.)))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.open_import(window, cx))),
            )
            .when(true, |row| {
                row.child(
                    Button::new("settings")
                        .ghost()
                        .label(if compact {
                            "Sources"
                        } else {
                            "Source locations"
                        })
                        .h(px(36.))
                        .when(compact, |button| button.small().h(px(44.)))
                        .disabled(self.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                        ),
                )
            })
    }
    fn sources(&self, cx: &mut Context<Self>) -> Div {
        let mut rail = v_flex()
            .w(px(214.))
            .flex_shrink_0()
            .h_full()
            .p_4()
            .gap_1()
            .bg(rgb(0xf3f5f9))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(caption("Sources").px_2().py_3())
            .child(
                Button::new("all-sources")
                    .ghost()
                    .w_full()
                    .h(px(39.))
                    .accessibility_label("All sessions")
                    .child(
                        h_flex()
                            .w_full()
                            .child(div().flex_1().child("All sessions"))
                            .child(
                                caption(self.data.catalog.sessions.len().to_string())
                                    .when(self.source.is_empty() && !self.history_view, |c| {
                                        c.text_color(rgb(0xffffff))
                                    }),
                            ),
                    )
                    .when(self.source.is_empty() && !self.history_view, |button| {
                        button.primary()
                    })
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(|this, _, w, cx| this.choose_source(String::new(), w, cx)),
                    ),
            );
        for (i, source) in self.data.catalog.providers.iter().enumerate() {
            if source.problem.is_some() && !self.missing {
                continue;
            }
            let name = source.info.name.clone();
            rail = rail.child(
                Button::new(("source", i))
                    .ghost()
                    .w_full()
                    .h(px(39.))
                    .accessibility_label(source.info.label.clone())
                    .child(
                        h_flex()
                            .w_full()
                            .child(div().flex_1().child(source.info.label.clone()))
                            .child(
                                caption(source.sessions.to_string())
                                    .when(self.source == name && !self.history_view, |c| {
                                        c.text_color(rgb(0xffffff))
                                    }),
                            ),
                    )
                    .when(self.source == name && !self.history_view, |button| {
                        button.primary()
                    })
                    .disabled(self.busy)
                    .on_click(
                        cx.listener(move |this, _, w, cx| this.choose_source(name.clone(), w, cx)),
                    ),
            );
        }
        let missing = self
            .data
            .catalog
            .providers
            .iter()
            .filter(|s| s.problem.is_some())
            .count();
        rail.when(missing > 0, |rail| {
            rail.child(
                Button::new("missing")
                    .ghost()
                    .small()
                    .mt_4()
                    .label(format!(
                        "{}Not found ({missing})",
                        if self.missing { "Hide · " } else { "" }
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.missing = !this.missing;
                        cx.notify();
                    })),
            )
        })
        .child(
            Button::new("web-export")
                .ghost()
                .small()
                .mt_5()
                .label("Open a web export")
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, w, cx| this.open_settings(w, cx))),
        )
        .child(div().flex_1())
        .child(
            Button::new("manage")
                .ghost()
                .small()
                .label("Manage locations")
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, w, cx| this.open_settings(w, cx))),
        )
        .child(caption(format!("v{}", self.data.version)).px_3().mt_3())
    }
    fn library(&mut self, compact: bool, wide: bool, cx: &mut Context<Self>) -> Div {
        let filters = if compact {
            v_flex()
                .gap_2()
                .child(
                    Select::new(&self.source_choice)
                        .large()
                        .accessibility_label("Source harness")
                        .w_full()
                        .disabled(self.busy),
                )
                .child(
                    Select::new(&self.project_choice)
                        .large()
                        .accessibility_label("Project folder")
                        .w_full(),
                )
                .child(
                    Select::new(&self.sort_choice)
                        .large()
                        .accessibility_label("Sort sessions")
                        .w_full(),
                )
                .into_any_element()
        } else {
            h_flex()
                .gap_2()
                .child(
                    div().w(px(220.)).flex_shrink_0().child(
                        Select::new(&self.project_choice)
                            .large()
                            .accessibility_label("Project folder")
                            .w_full(),
                    ),
                )
                .child(
                    div().w(px(155.)).flex_shrink_0().child(
                        Select::new(&self.sort_choice)
                            .large()
                            .accessibility_label("Sort sessions")
                            .w_full(),
                    ),
                )
                .into_any_element()
        };
        let rows = self.rows(cx);
        let pages = rows.len().max(1).div_ceil(40);
        self.page = self.page.min(pages - 1);
        let start = self.page * 40;
        let end = (start + 40).min(rows.len());
        let mut list = v_flex()
            .id("sessions-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        for (i, s) in rows[start..end].iter().enumerate() {
            let key = s.key();
            let session = s.clone();
            let active = self.selected.as_ref().is_some_and(|s| s.key() == key);
            let id = if s.id.len() > 22 {
                format!(
                    "{}…{}",
                    s.id.chars().take(8).collect::<String>(),
                    s.id.chars()
                        .rev()
                        .take(8)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                )
            } else {
                s.id.clone()
            };
            let row =
                h_flex()
                    .w_full()
                    .gap_5()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(s.title().to_owned()),
                            )
                            .child(caption(id).text_size(px(10.)).font_family("IBM Plex Mono")),
                    )
                    .when(wide, |row| {
                        row.child(
                            v_flex()
                                .w(px(190.))
                                .min_w_0()
                                .gap_1()
                                .child(div().text_size(px(12.)).truncate().child(
                                    project(s.cwd.as_deref().unwrap_or_default()).to_owned(),
                                ))
                                .child(
                                    caption(s.cwd.clone().unwrap_or_default())
                                        .truncate()
                                        .text_size(px(10.)),
                                ),
                        )
                    })
                    .when(!compact, |row| {
                        row.child(
                            div()
                                .w(px(108.))
                                .text_size(px(12.))
                                .child(self.label(&s.provider)),
                        )
                    })
                    .child(
                        caption(date(s.time()))
                            .w(px(if compact { 74. } else { 88. }))
                            .flex_shrink_0(),
                    );
            list = list.child(
                Button::new(("session", i))
                    .ghost()
                    .w_full()
                    .h(px(67.))
                    .px_5()
                    .rounded_none()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(row)
                    .disabled(self.busy)
                    .when(active, |row| {
                        row.bg(rgb(WASH)).border_l_2().border_color(rgb(COBALT))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(session.clone(), window, cx)
                    })),
            );
        }
        if rows.is_empty() {
            list = list.child(
                v_flex()
                    .p_8()
                    .gap_3()
                    .child(heading("No matching sessions"))
                    .child(note(
                        "Choose another source or clear the search and project filters.",
                    )),
            );
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .px_6()
                    .pt_6()
                    .pb_4()
                    .gap_3()
                    .child(
                        div().flex_1().child(heading("Session library")).child(
                            note(format!(
                                "{} sessions · {}",
                                rows.len(),
                                if self.source.is_empty() {
                                    "All sources".into()
                                } else {
                                    self.label(&self.source)
                                }
                            ))
                            .mt_2(),
                        ),
                    )
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .label("Refresh")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.request("/api/catalog", None, Job::Catalog, w, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .px_6()
                    .pb_4()
                    .gap_2()
                    .child(Input::new(&self.search).large().cleanable(true).w_full())
                    .child(filters),
            )
            .child(
                h_flex()
                    .h(px(38.))
                    .px_5()
                    .border_y_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(0xf9fafe))
                    .gap_5()
                    .child(caption("SESSION").flex_1())
                    .when(wide, |row| row.child(caption("PROJECT").w(px(190.))))
                    .when(!compact, |row| row.child(caption("HARNESS").w(px(108.))))
                    .child(caption("UPDATED").w(px(if compact { 74. } else { 88. }))),
            )
            .child(list)
            .child(
                h_flex()
                    .h(px(50.))
                    .px_5()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .gap_2()
                    .child(
                        caption(if rows.is_empty() {
                            "0 sessions".into()
                        } else {
                            format!("{}–{} of {} sessions", start + 1, end, rows.len())
                        })
                        .flex_1(),
                    )
                    .child(
                        Button::new("previous")
                            .ghost()
                            .small()
                            .label("‹")
                            .disabled(self.page == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = this.page.saturating_sub(1);
                                cx.notify();
                            })),
                    )
                    .child(caption(format!("{} / {pages}", self.page + 1)))
                    .child(
                        Button::new("next")
                            .ghost()
                            .small()
                            .label("›")
                            .disabled(self.page + 1 >= pages)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page += 1;
                                cx.notify();
                            })),
                    ),
            )
    }
    fn history(&self) -> Div {
        let mut list = v_flex().id("history-scroll").flex_1().overflow_y_scroll();
        if self.activities.is_empty() {
            list = list.child(
                v_flex()
                    .p_6()
                    .gap_2()
                    .child(heading("No transfers yet"))
                    .child(note(
                        "Completed transfers and exports appear here for this browser tab.",
                    )),
            );
        }
        for (i, item) in self.activities.iter().enumerate() {
            let location = item.location.clone();
            list = list.child(
                v_flex()
                    .p_6()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(item.title.clone()),
                    )
                    .child(note(format!(
                        "{} → {} · {}",
                        item.from,
                        item.to,
                        date(Some(item.date as i64))
                    )))
                    .child(caption(item.location.clone()))
                    .child(
                        Button::new(("history-copy", i))
                            .ghost()
                            .small()
                            .label("Copy saved location")
                            .on_click(move |_, _, _| client::copy(location.clone())),
                    ),
            );
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .p_6()
                    .gap_2()
                    .child(heading("Transfer history"))
                    .child(note("Last 50 completed transfers and exports in this tab.")),
            )
            .child(list)
    }
    fn transfer_fields(&self, cx: &mut Context<Self>) -> Div {
        let to = selected(&self.destination, cx);
        let provider = self.data.providers.iter().find(|p| p.name == to);
        let from = if self.importing {
            "Portable file".into()
        } else {
            self.selected
                .as_ref()
                .map(|s| self.label(&s.provider))
                .unwrap_or_default()
        };
        let mut fields = v_flex().gap_5().child(
            h_flex()
                .p_4()
                .rounded_md()
                .bg(rgb(WASH))
                .gap_4()
                .child(detail("From", from).flex_1())
                .child(div().text_color(rgb(COBALT)).child("→"))
                .child(detail("To", self.label(&to)).flex_1()),
        );
        if self.importing {
            fields = fields
                .child(field("Portable file", &self.file, self.busy))
                .child(note(
                    "Use the full path on the computer running harness-bridge.",
                ));
        }
        fields.child(v_flex().gap_2().child(caption("Destination harness")).child(Select::new(&self.destination).accessibility_label("Destination harness").w_full().disabled(self.busy)).child(note(provider.map(|p|p.note.clone()).unwrap_or_default())))
            .child(field("Project folder",&self.cwd,self.busy)).child(note("The new session opens here. Recorded paths in its history stay as they are."))
            .child(field("Session title",&self.name,self.busy))
            .when(!self.importing,|fields|fields.child(Checkbox::new("children").label("Include saved child sessions").checked(self.children).disabled(self.busy).on_click(cx.listener(|this,value,_,cx|{this.children=*value;cx.notify();}))))
            .child(Button::new("context-options").ghost().justify_start().label(if self.advanced{"Hide context options"}else{"Context options"}).on_click(cx.listener(|this,_,_,cx|{this.advanced=!this.advanced;cx.notify();})))
            .when(self.advanced,|fields|fields.child(v_flex().gap_4().child(field("Resume character budget",&self.budget,self.busy))
                .child(Checkbox::new("prune").label("Shorten reasoning and tool outputs if needed").checked(self.prune).disabled(self.busy).on_click(cx.listener(|this,v,_,cx|{this.prune=*v;cx.notify();})))
                .child(Checkbox::new("rebuild").label("Recover context from readable history").checked(self.rebuild).disabled(self.busy).on_click(cx.listener(|this,v,_,cx|{this.rebuild=*v;cx.notify();})))
                .child(note("Recovery cannot restore hidden state. Character budgets are not token limits."))))
    }
    fn conversation(&self, cx: &mut Context<Self>) -> Div {
        let mut content = v_flex().gap_5();
        if let Some(s) = &self.selected {
            let id = s.id.clone();
            content = content
                .child(detail("Session id", s.id.clone()))
                .child(
                    Button::new("copy-id")
                        .ghost()
                        .small()
                        .label("Copy session id")
                        .on_click(move |_, _, _| client::copy(id.clone())),
                )
                .child(detail(
                    "Project folder",
                    s.cwd.clone().unwrap_or_else(|| "No recorded folder".into()),
                ));
        }
        if let Some(details) = &self.details {
            content = content.child(note(format!(
                "{} messages · {} tool calls · {} context characters",
                details["messages"], details["tool_calls"], details["resume_chars"]
            )));
            if let Some(reason) = details["unavailable_context"].as_str() {
                content = content.child(note(reason.to_owned()).text_color(rgb(0xa03d2e)));
            }
            if let Some(messages) = details["excerpt_messages"].as_array() {
                for message in messages {
                    content =
                        content.child(
                            v_flex()
                                .gap_2()
                                .pt_4()
                                .border_t_1()
                                .border_color(rgb(LINE))
                                .child(caption(if message["role"] == "user" {
                                    "YOU"
                                } else {
                                    "ASSISTANT"
                                }))
                                .child(div().text_size(px(13.)).child(
                                    message["text"].as_str().unwrap_or_default().to_owned(),
                                )),
                        );
                }
            }
            content=content.child(note("Excerpts show up to six messages. Transfers preserve the normalized archive and retained context."));
        } else {
            content = content.child(note("Reading the conversation…"));
        }
        content.child(
            Button::new("export-portable")
                .label("Export portable file")
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, w, cx| this.open_export(w, cx))),
        )
    }
    fn review(&self) -> Div {
        let outcome = self.plan.as_ref().map(|p| &p["preview"]["outcome"]);
        let mut content=v_flex().gap_5().child(note("Review the destination and conversion notes. Nothing is written until you choose Transfer session."))
            .child(detail(if self.importing {"Portable file"}else{"Session"},self.plan_title.clone())).child(detail("Route",format!("{} → {}",self.plan_from,self.plan_to)));
        if let Some(o) = outcome {
            content = content
                .child(detail("Native id", json_text(&o["native_id"])))
                .child(detail("Saved location", json_text(&o["location"])));
            for (key, label) in [
                ("records", "Transcript records"),
                ("items", "Transcript items"),
                ("events", "Events"),
                ("steps", "Steps"),
                ("cwd", "Project folder"),
                ("resume_command", "Resume command"),
            ] {
                if let Some(value) = o["extra"].get(key) {
                    content = content.child(detail(label, json_text(value)));
                }
            }
        }
        if let Some(warnings) = self
            .plan
            .as_ref()
            .and_then(|p| p["preview"]["warnings"].as_array())
        {
            for warning in warnings {
                content = content.child(note(json_text(warning)));
            }
        }
        content
    }
    fn completion(&self) -> Div {
        let mut content = v_flex()
            .gap_5()
            .child(heading(if self.plan_to == "Portable file" {
                "Portable file saved"
            } else {
                "Session transferred"
            }))
            .child(detail(
                if self.importing {
                    "Portable file"
                } else {
                    "Session"
                },
                self.plan_title.clone(),
            ))
            .child(detail(
                "Route",
                format!("{} → {}", self.plan_from, self.plan_to),
            ));
        if let Some(outcome) = &self.outcome {
            let location = outcome["location"].as_str().unwrap_or_default().to_owned();
            let copied = location.clone();
            content = content.child(detail("Saved location", location)).child(
                Button::new("copy-location")
                    .label("Copy saved location")
                    .on_click(move |_, _, _| client::copy(copied.clone())),
            );
            if let Some(id) = outcome["native_id"].as_str() {
                content = content.child(detail("Native id", id.to_owned()));
            }
        }
        content
    }
    fn settings(&self) -> Div {
        let mut content=v_flex().gap_5().child(note("Blank fields use default locations. Changes apply while this process is running. A web export accepts a ZIP, folder or JSON file."));
        for (i, (_, label)) in SOURCE_FIELDS.iter().enumerate() {
            content = content.child(field(label, &self.setting_inputs[i], self.busy));
        }
        content = content.child(heading("Discovery details"));
        for source in &self.data.catalog.providers {
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(detail(
                        &source.info.label,
                        source
                            .problem
                            .clone()
                            .unwrap_or_else(|| format!("{} sessions found", source.sessions)),
                    ))
                    .child(note(source.info.note.clone())),
            );
        }
        content
    }
    fn pane(&self, compact: bool, cx: &mut Context<Self>) -> Div {
        let title = match self.pane {
            Pane::Import => "Import portable file".into(),
            Pane::Export => "Export portable file".into(),
            Pane::Settings => "Source locations".into(),
            Pane::Review => "Review transfer".into(),
            Pane::Complete => "Transfer result".into(),
            _ => self
                .selected
                .as_ref()
                .map(|s| s.title().to_owned())
                .unwrap_or_default(),
        };
        let mut pane = v_flex()
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_l_1()
            .border_color(rgb(LINE))
            .when(compact, |p| p.flex_1().min_w_0())
            .when(!compact, |p| p.w(px(400.)))
            .child(
                v_flex()
                    .px_6()
                    .pt_5()
                    .pb_4()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                caption(
                                    self.selected
                                        .as_ref()
                                        .map(|s| self.label(&s.provider))
                                        .unwrap_or_default(),
                                )
                                .flex_1(),
                            )
                            .child(
                                Button::new("close-pane")
                                    .ghost()
                                    .small()
                                    .label("Close")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    ),
            );
        if matches!(self.pane, Pane::Transfer | Pane::Conversation) {
            pane = pane.child(
                h_flex()
                    .px_5()
                    .gap_3()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        Button::new("tab-transfer")
                            .ghost()
                            .label("Transfer")
                            .when(self.pane == Pane::Transfer, |button| button.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pane = Pane::Transfer;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("tab-conversation")
                            .ghost()
                            .label("Conversation")
                            .when(self.pane == Pane::Conversation, |button| button.primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pane = Pane::Conversation;
                                cx.notify();
                            })),
                    ),
            );
        }
        let content=match self.pane{
            Pane::Transfer|Pane::Import=>self.transfer_fields(cx),Pane::Conversation=>self.conversation(cx),Pane::Review=>self.review(),Pane::Complete=>self.completion(),Pane::Settings=>self.settings(),
            Pane::Export=>v_flex().gap_5().child(note("Save a versioned portable file containing this session's archive and retained context. Existing files are never replaced."))
                .child(field("Output file path",&self.file,self.busy)).child(Checkbox::new("export-children").label("Include saved child sessions").checked(self.children).disabled(self.busy).on_click(cx.listener(|this,v,_,cx|{this.children=*v;cx.notify();}))),
            _=>v_flex(),
        };
        pane = pane.child(
            v_flex()
                .id("inspector-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_6()
                .gap_4()
                .when(!self.error.is_empty(), |p| {
                    p.child(
                        note(self.error.clone())
                            .text_color(rgb(0xa03d2e))
                            .p_3()
                            .bg(rgb(0xfff3f0)),
                    )
                })
                .child(content),
        );
        let mut footer = v_flex().p_5().border_t_1().border_color(rgb(LINE));
        footer = match self.pane {
            Pane::Transfer | Pane::Import => footer.child(
                Button::new("preview")
                    .primary()
                    .h(px(44.))
                    .w_full()
                    .label("Preview transfer")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, w, cx| this.preview(w, cx))),
            ),
            Pane::Review => footer
                .gap_2()
                .child(
                    Button::new("confirm-transfer")
                        .primary()
                        .h(px(44.))
                        .w_full()
                        .label("Transfer session")
                        .disabled(self.busy || self.plan.is_none())
                        .on_click(cx.listener(|this, _, w, cx| this.write(w, cx))),
                )
                .child(
                    Button::new("review-back")
                        .ghost()
                        .label("Back")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.plan = None;
                            this.pane = if this.importing {
                                Pane::Import
                            } else {
                                Pane::Transfer
                            };
                            cx.notify();
                        })),
                ),
            Pane::Export => footer.child(
                Button::new("save-export")
                    .primary()
                    .h(px(44.))
                    .w_full()
                    .label("Save portable file")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, w, cx| this.export(w, cx))),
            ),
            Pane::Settings => footer.child(
                Button::new("save-settings")
                    .primary()
                    .h(px(44.))
                    .w_full()
                    .label("Save source locations")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, w, cx| this.save_settings(w, cx))),
            ),
            Pane::Complete => footer.child(
                Button::new("done")
                    .primary()
                    .h(px(44.))
                    .w_full()
                    .label("Done")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.close(cx);
                        this.request("/api/catalog", None, Job::Catalog, w, cx);
                    })),
            ),
            _ => footer,
        };
        pane.child(footer)
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width;
        let compact = width < px(1050.);
        let pane_open = self.pane != Pane::None;
        let wide = width > px(if pane_open { 1650. } else { 1120. });
        let mut body = h_flex().flex_1().min_h_0().min_w_0().items_stretch();
        if !compact {
            body = body.child(self.sources(cx));
        }
        if !compact || !pane_open {
            body = body.child(if self.history_view {
                self.history()
            } else {
                self.library(compact, wide, cx)
            });
        }
        if pane_open {
            body = body.child(self.pane(compact, cx));
        }
        v_flex()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !this.busy {
                    this.close(cx);
                }
                if event.keystroke.key == "/"
                    && window.focused_input(cx).is_none()
                    && this.pane == Pane::None
                {
                    this.search.read(cx).focus_handle(cx).focus(window, cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .bg(rgb(0xffffff))
            .text_color(rgb(INK))
            .font_family("IBM Plex Sans")
            .text_size(px(14.))
            .child(self.header(compact, cx))
            .when(self.pane == Pane::None && !self.error.is_empty(), |root| {
                root.child(
                    note(self.error.clone())
                        .px_6()
                        .py_3()
                        .text_color(rgb(0xa03d2e))
                        .bg(rgb(0xfff3f0)),
                )
            })
            .child(body)
            .child(
                h_flex()
                    .h(px(27.))
                    .flex_shrink_0()
                    .px_5()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(caption(self.status.clone()).text_size(px(10.))),
            )
    }
}
