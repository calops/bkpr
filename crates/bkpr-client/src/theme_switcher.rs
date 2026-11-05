//! Client-local palette preview using the framework's searchable list and overlay owner.
use super::*;
use crate::themes::{THEMES, ThemeId};
use bkpr_tui_framework::search_list::{SearchList, SearchListIntent};

pub(super) struct Picker {
    pub(super) list: SearchList<ThemeId>,
    original: ThemeId,
}

impl App<'_> {
    pub(super) fn picker_open(&self) -> bool {
        self.finder.is_some() || self.theme_picker.is_some()
    }

    pub(super) fn restore_theme_preview(&mut self) {
        if let Some(picker) = self.theme_picker.take() {
            self.theme = picker.original;
        }
    }

    pub(super) fn begin_theme_picker(&mut self) {
        self.begin_modal();
        self.finder = None;
        self.state.cancel();
        self.retire_resize("theme picker opened");
        self.selection_focus_lost();
        self.close_search();
        self.popover.hide();
        self.theme_picker = Some(Picker {
            list: SearchList::new(
                THEMES.iter().map(|(id, _)| (*id, id.name().to_owned())),
                &self.theme,
            ),
            original: self.theme,
        });
    }

    pub(super) fn preview_theme(&mut self) {
        if let Some(selected) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.list.selected())
        {
            self.theme = *selected;
        }
    }

    pub(super) fn theme_picker_intent(&mut self, intent: SearchListIntent) {
        self.preview_theme();
        match intent {
            SearchListIntent::None => {}
            SearchListIntent::Cancel => self.cancel_modal(),
            SearchListIntent::Accept => {
                // Removing the preview owner first keeps the choice when focus is restored.
                self.theme_picker = None;
                self.cancel_modal();
            }
        }
    }
}
