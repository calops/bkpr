use crate::ui;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zellij_tile::prelude::*;

pub const UPDATE: &str = "shephrd-tooltip";
pub const READY: &str = "shephrd-tooltip-ready";

#[derive(Clone, Serialize, Deserialize)]
pub struct Tooltip {
    pub instance: String,
    pub revision: u64,
    pub visible: bool,
    pub lines: Vec<String>,
    pub coordinates: FloatingPaneCoordinates,
}

pub struct Controller {
    pub id: Option<u32>,
    pub pending: Option<Tooltip>,
    revision: u64,
    launched: bool,
    instance: String,
}

impl Default for Controller {
    fn default() -> Self {
        let instance = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_nanos().to_string())
            .unwrap_or_else(|_| "clock-unavailable".to_owned());
        Self {
            id: None,
            pending: None,
            revision: 0,
            launched: false,
            instance,
        }
    }
}

impl Controller {
    pub fn update(
        &mut self,
        owner: u32,
        configuration: &BTreeMap<String, String>,
        tooltip: Option<(Vec<String>, FloatingPaneCoordinates)>,
    ) {
        let (visible, lines, coordinates) = match tooltip {
            Some((lines, coordinates)) => (true, lines, coordinates),
            None => (false, Vec::new(), FloatingPaneCoordinates::default()),
        };
        if self.pending.as_ref().is_some_and(|old| {
            old.visible == visible && old.lines == lines && old.coordinates == coordinates
        }) {
            return;
        }
        self.revision = self.revision.saturating_add(1);
        self.pending = Some(Tooltip {
            instance: self.instance.clone(),
            revision: self.revision,
            visible,
            lines,
            coordinates,
        });
        if self.id.is_some() || visible {
            self.send(owner, configuration);
        }
    }

    pub fn ready(&mut self, id: u32, owner: u32, configuration: &BTreeMap<String, String>) {
        self.id = Some(id);
        self.launched = true;
        self.send(owner, configuration);
    }

    fn send(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        let Some(tooltip) = &self.pending else { return };
        let Ok(payload) = serde_json::to_string(tooltip) else {
            return;
        };
        let mut message = MessageToPlugin::new(UPDATE).with_payload(payload);
        if let Some(id) = self.id {
            message.destination_plugin_id = Some(id);
        } else {
            let Some(url) = configuration.get("plugin_url") else {
                return;
            };
            let mut config = configuration.clone();
            config.insert("role".to_owned(), "overlay".to_owned());
            config.insert("owner".to_owned(), owner.to_string());
            message.plugin_url = Some(url.clone());
            message.plugin_config = config;
            message.new_plugin_args = Some(NewPluginArgs {
                should_float: Some(true),
                should_focus: Some(false),
                pane_title: Some("shephrd details".to_owned()),
                ..NewPluginArgs::default()
            });
            message.floating_pane_coordinates = Some(tooltip.coordinates.clone());
            // Exact URL/config identity makes repeated sends target the same plugin instance.
            self.launched = true;
        }
        pipe_message_to_plugin(message);
    }

    pub fn forget(&mut self, id: u32) {
        if self.id == Some(id) {
            self.id = None;
            self.launched = false;
            self.pending = None;
        }
    }

    pub fn close(&mut self, owner: u32, configuration: &BTreeMap<String, String>) {
        if let Some(id) = self.id.take() {
            close_plugin_pane(id);
        } else if self.launched {
            // An in-flight load is still sent the hidden state; it also watches owner lifetime.
            self.update(owner, configuration, None);
        }
        self.launched = false;
    }
}

pub fn geometry(
    sidebar: &PaneInfo,
    tab: &TabInfo,
    row: usize,
    lines: &[String],
) -> Option<FloatingPaneCoordinates> {
    let right = sidebar.pane_x.saturating_add(sidebar.pane_columns);
    let right_space = tab.display_area_columns.saturating_sub(right);
    let left_space = sidebar.pane_x;
    let (x, available) = if right_space >= 12 {
        (right, right_space)
    } else if left_space >= 12 {
        (0, left_space)
    } else {
        return None;
    };
    let width = lines
        .iter()
        .map(|line| ui::width(&ui::clean(line)))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .clamp(12, 64)
        .min(available);
    let height = lines
        .len()
        .saturating_add(2)
        .min(tab.display_area_rows)
        .max(1);
    let x = if right_space >= 12 {
        x
    } else {
        left_space.saturating_sub(width)
    };
    let y = sidebar
        .pane_content_y
        .saturating_add(row)
        .min(tab.display_area_rows.saturating_sub(height));
    Some(FloatingPaneCoordinates {
        pinned: Some(true),
        borderless: Some(true),
        ..FloatingPaneCoordinates::default()
            .with_x_fixed(x)
            .with_y_fixed(y)
            .with_width_fixed(width)
            .with_height_fixed(height)
    })
}

#[derive(Default)]
pub struct Overlay {
    pub owner: Option<u32>,
    pub id: u32,
    granted: bool,
    owner_seen: bool,
    state: Option<Tooltip>,
    error: Option<String>,
    theme: ui::Theme,
}

impl Overlay {
    pub fn load(&mut self, configuration: &BTreeMap<String, String>, id: u32) {
        self.id = id;
        self.owner = configuration
            .get("owner")
            .and_then(|owner| owner.parse().ok());
        set_selectable(false);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::BeforeClose,
            EventType::ModeUpdate,
        ]);
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);
    }

    pub fn event(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                self.granted = true;
                set_selectable(false);
                if let Some(owner) = self
                    .owner
                    .filter(|owner| get_pane_info(PaneId::Plugin(*owner)).is_some())
                {
                    self.owner_seen = true;
                    self.apply();
                    pipe_message_to_plugin(
                        MessageToPlugin::new(READY)
                            .with_destination_plugin_id(owner)
                            .with_payload(self.id.to_string()),
                    );
                } else {
                    close_self();
                }
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.error = Some("Tooltip permissions denied".to_owned());
            }
            Event::ModeUpdate(mode) => self.theme.update(mode.style),
            Event::PaneUpdate(manifest) => {
                let found = manifest
                    .panes
                    .values()
                    .flatten()
                    .any(|pane| pane.is_plugin && Some(pane.id) == self.owner);
                if found {
                    self.owner_seen = true;
                } else if self.owner_seen && self.granted {
                    close_self();
                }
            }
            Event::PaneClosed(PaneId::Plugin(id)) if Some(id) == self.owner && self.granted => {
                close_self()
            }
            _ => {}
        }
        true
    }

    pub fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name != UPDATE
            || !matches!(message.source, PipeSource::Plugin(id) if Some(id) == self.owner)
        {
            return false;
        }
        let Some(payload) = message.payload else {
            return false;
        };
        if payload.len() > 32 * 1024 {
            return false;
        }
        let Ok(tooltip) = serde_json::from_str::<Tooltip>(&payload) else {
            return false;
        };
        if self.state.as_ref().is_some_and(|current| {
            tooltip.instance == current.instance && tooltip.revision <= current.revision
        }) {
            return false;
        }
        self.state = Some(tooltip);
        self.apply();
        if self.granted
            && let Some(owner) = self.owner
        {
            pipe_message_to_plugin(
                MessageToPlugin::new(READY)
                    .with_destination_plugin_id(owner)
                    .with_payload(self.id.to_string()),
            );
        }
        true
    }

    fn apply(&self) {
        if !self.granted {
            return;
        }
        let pane = PaneId::Plugin(self.id);
        match &self.state {
            Some(state) if state.visible => {
                change_floating_panes_coordinates(vec![(pane, state.coordinates.clone())]);
                // Unlike show_self(), this does not transfer focus from the real terminal.
                show_pane_with_id(pane, true, false);
            }
            _ => hide_self(),
        }
    }

    pub fn render(&self, rows: usize, columns: usize) {
        for row in 0..rows {
            let text = if let Some(error) = &self.error {
                if row == 1 { error.as_str() } else { "" }
            } else {
                self.state
                    .as_ref()
                    .and_then(|state| row.checked_sub(1).and_then(|i| state.lines.get(i)))
                    .map(String::as_str)
                    .unwrap_or("")
            };
            let line = if columns >= 2 {
                format!(" {} ", ui::fit(text, columns - 2))
            } else {
                ui::fit(text, columns)
            };
            let style = if row == 1 { "\u{1b}[1;39m" } else { ui::MUTED };
            print!(
                "\u{1b}[{};1H{}{}{style}{line}{}",
                row + 1,
                ui::RESET,
                self.theme.hovered,
                ui::RESET
            );
        }
    }
}
