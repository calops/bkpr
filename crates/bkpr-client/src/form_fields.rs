//! Bkpr's focused name/directory fields, adapted from main's focused.rs and
//! ui.rs::Input (9ad93bd). Backend submission and popup lifetime stay with App.
use crate::keyguide::{self, Action, Context};
use bkpr_protocol::{Cursor, KeyEvent, Size};
use bkpr_tui_framework::{Rect, Role, RoleSpan, TextInput, Theme, UiSurface};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct FormFields {
    pub primary: TextInput,
    pub directory: TextInput,
    active_directory: bool,
    inherited_directory: PathBuf,
}
impl FormFields {
    pub fn new(value: String, inherited_directory: PathBuf) -> Self {
        Self {
            primary: TextInput::new(value, crate::TEXT_INPUT_LIMIT),
            directory: TextInput::new(String::new(), crate::TEXT_INPUT_LIMIT),
            active_directory: false,
            inherited_directory,
        }
    }
    pub fn inherit_directory(&mut self, directory: PathBuf) {
        self.inherited_directory = directory;
    }
    pub fn directory(&self) -> PathBuf {
        if self.directory.value.is_empty() {
            self.inherited_directory.clone()
        } else {
            PathBuf::from(&self.directory.value)
        }
    }
    pub fn edit(&mut self, event: &KeyEvent, creation: bool) {
        let context = if creation {
            Context::Create
        } else {
            Context::Edit
        };
        match keyguide::lookup(context, event) {
            Some(Action::CycleField) if creation => self.active_directory = !self.active_directory,
            Some(Action::Edit(action)) => {
                self.active_mut().edit(action);
            }
            None if event.modifiers & 14 == 0 => {
                self.active_mut().insert(&event.text);
            }
            _ => {}
        }
    }
    pub fn paste(&mut self, text: &str) {
        self.active_mut().insert(text);
    }
    fn active_mut(&mut self) -> &mut TextInput {
        if self.active_directory {
            &mut self.directory
        } else {
            &mut self.primary
        }
    }
    pub fn paint(
        &self,
        surface: &mut UiSurface,
        size: Size,
        title: &str,
        creation: bool,
        error: Option<&str>,
        pending: bool,
    ) {
        let width = size.cols;
        let mut lines = Vec::new();
        let mut row = |y: u16, text: String, selected: bool| {
            if y < size.rows {
                lines.push((
                    Rect {
                        x: 0,
                        y,
                        width,
                        height: 1,
                    },
                    text,
                    selected,
                ));
            }
        };
        let tiny = size.rows < 4;
        if !tiny {
            row(0, title.into(), false);
        }
        let compact = creation && size.rows < 7;
        let showing_directory = creation && compact && self.active_directory;
        let field_y = if tiny {
            0
        } else if creation && !compact && self.active_directory {
            4
        } else {
            2
        };
        let visible_width = width.saturating_sub(2) as usize;
        let (first, first_column) = if showing_directory {
            self.directory.visible(visible_width)
        } else {
            self.primary.visible(visible_width)
        };
        if !tiny {
            row(
                1,
                if showing_directory {
                    "Working directory".into()
                } else if creation {
                    "Name (blank chooses automatically)".into()
                } else {
                    "Name".into()
                },
                false,
            );
        }
        row(
            if tiny { 0 } else { 2 },
            format!("  {first}"),
            !pending && (!self.active_directory || showing_directory),
        );
        let mut column = first_column;
        if creation && !compact {
            let (directory, directory_column) = self.directory.visible(visible_width);
            row(3, "Working directory".into(), false);
            row(
                4,
                format!("  {directory}"),
                !pending && self.active_directory,
            );
            row(
                5,
                format!(
                    "Blank directory keeps: {}",
                    self.inherited_directory.display()
                ),
                false,
            );
            if self.active_directory {
                column = directory_column;
            }
        }
        let message = error.or(self.primary.error()).or(self.directory.error());
        if size.rows > 1 {
            row(
                size.rows - 1,
                if let Some(message) = message {
                    message.into()
                } else if pending {
                    "Submitting; waiting for backend receipt".into()
                } else if creation {
                    "Tab changes field · Enter creates · Esc cancels".into()
                } else {
                    "Enter saves · Esc cancels".into()
                },
                false,
            );
        }
        surface.paint(
            size,
            lines,
            (!pending && width > 0 && size.rows > 0).then_some(Cursor {
                x: (column as u16)
                    .saturating_add(2)
                    .min(width.saturating_sub(1)),
                y: field_y,
                visible: true,
                blinking: true,
                shape: bkpr_protocol::CursorShape::Bar,
                color: None,
            }),
        );
    }

    pub(crate) fn apply_theme(
        &self,
        surface: &mut UiSurface,
        theme: &Theme,
        creation: bool,
        error: Option<&str>,
        pending: bool,
    ) {
        let size = surface.frame.size;
        let tiny = size.rows < 4;
        let compact = creation && size.rows < 7;
        let mut spans = Vec::with_capacity(7);
        let mut row = |y: u16, role| {
            if y < size.rows {
                spans.push(RoleSpan {
                    rect: Rect {
                        x: 0,
                        y,
                        width: size.cols,
                        height: 1,
                    },
                    role,
                });
            }
        };
        if !tiny {
            row(0, Role::Title);
            row(1, Role::Muted);
        }
        row(
            if tiny { 0 } else { 2 },
            if pending { Role::Muted } else { Role::Panel },
        );
        if creation && !compact {
            row(3, Role::Muted);
            row(4, if pending { Role::Muted } else { Role::Panel });
            row(5, Role::Muted);
        }
        if size.rows > 1 {
            row(
                size.rows - 1,
                if error
                    .or(self.primary.error())
                    .or(self.directory.error())
                    .is_some()
                {
                    Role::Error
                } else {
                    Role::Muted
                },
            );
        }
        surface.apply_theme(theme, Role::Panel, &spans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::KeyAction;
    #[test]
    fn field_switching_keeps_values_and_paste_bound_to_the_active_field() {
        let mut fields = FormFields::new("workspace".into(), "/inherited".into());
        fields.edit(
            &KeyEvent {
                key: "Tab".into(),
                text: String::new(),
                modifiers: 0,
                action: KeyAction::Press,
                unshifted_codepoint: 9,
            },
            true,
        );
        fields.paste("/chosen/界");
        assert_eq!(fields.primary.value, "workspace");
        assert_eq!(fields.directory(), PathBuf::from("/chosen/界"));
        fields.edit(
            &KeyEvent {
                key: "KeyU".into(),
                text: "u".into(),
                modifiers: 2,
                action: KeyAction::Press,
                unshifted_codepoint: 117,
            },
            true,
        );
        assert_eq!(fields.directory(), PathBuf::from("/inherited"));
    }
    #[cfg(unix)]
    #[test]
    fn blank_directory_preserves_inherited_non_utf8_path_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let directory = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/dir-\xff".to_vec()));
        let fields = FormFields::new(String::new(), directory.clone());
        let size = Size {
            cols: 60,
            rows: 9,
            cell_width: 9,
            cell_height: 18,
        };
        let mut surface = UiSurface::new(size);
        fields.paint(
            &mut surface,
            size,
            "Create terminal window",
            true,
            None,
            false,
        );
        fields.apply_theme(&mut surface, &Theme::default(), true, None, false);
        assert!(surface.frame.cursor.visible);
        assert!(fields.directory.value.is_empty());
        assert_eq!(fields.directory(), directory);
    }
}
