//! Client-local palette preview using the framework's searchable list and overlay owner.
use super::*;
use crate::themes::{THEMES, ThemeId};
use bkpr_tui_framework::search_list::{SearchList, SearchListIntent};

pub(super) struct Picker {
    pub(super) list: SearchList<ThemeId>,
    original: ThemeId,
}

impl Picker {
    /// Paint shared picker geometry with Bkpr-owned input tint and native-color fallback.
    pub(super) fn paint(
        &mut self,
        surface: &mut UiSurface,
        size: Size,
        theme: &Theme,
        host: &ColorProbe,
        override_supported: bool,
    ) {
        self.list.set_title(if override_supported {
            "Themes"
        } else {
            "Themes · RGB unavailable; using terminal colors"
        });
        self.list.paint(surface, size);
        let sections = bkpr_tui_framework::picker::PickerLayout::new(size, false);
        let backgrounds = sections
            .as_ref()
            .map(|layout| crate::appearance::picker_backgrounds(host, layout, false));
        let compact = [BackgroundSpan {
            rect: self.list.content_layout(size).input,
            background: crate::appearance::region_background(host, Some(6))
                .map_or(ColorSource::Default, ColorSource::Rgb),
        }];
        self.list.apply_theme(
            surface,
            theme,
            &[],
            backgrounds
                .as_ref()
                .map_or(&compact[..], |spans| &spans[..]),
        );
    }
}

impl App {
    pub(super) fn picker_open(&self) -> bool {
        self.finder.is_some() || self.theme_picker.is_some() || self.recovery.open()
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
        self.retire_resize();
        self.selection_focus_lost();
        self.close_search();
        self.popover.hide();
        let mut list = SearchList::new(
            THEMES.iter().map(|(id, _)| (*id, id.name().to_owned())),
            Some(&self.theme),
        );
        list.set_layout(bkpr_tui_framework::search_list::ListLayout::Cards { details: false });
        self.theme_picker = Some(Picker {
            list,
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
