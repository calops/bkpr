//! Bkpr's embedded decorative artwork. Generic preparation and scene resources
//! belong to the terminal UI framework; this wrapper owns only marker policy.
use bkpr_protocol::{ColorSource, Frame, Size};
use bkpr_tui_framework::{
    ColorProbe,
    graphics::{SvgAsset, SvgGraphics},
};
use std::{io, os::fd::RawFd};

#[used]
static NOTICES: &str = concat!(
    include_str!("../assets/NOTICE"),
    include_str!("../assets/MaterialDesign-LICENSE"),
    include_str!("../assets/Apache-2.0.txt"),
);
static ASSETS: &[SvgAsset] = &[
    SvgAsset {
        id: 1,
        svg: include_str!("../assets/bee.svg"),
    },
    SvgAsset {
        id: 2,
        svg: include_str!("../assets/hive.svg"),
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Artwork {
    Bee,
    Hive,
}

pub(crate) struct ChromeGraphics(SvgGraphics);
impl ChromeGraphics {
    pub(crate) fn new() -> io::Result<Self> {
        std::hint::black_box(NOTICES);
        SvgGraphics::new(ASSETS, 2, 1).map(Self)
    }
    pub(crate) fn wake_fd(&self) -> RawFd {
        self.0.wake_fd()
    }
    pub(crate) fn drain(&mut self) -> io::Result<bool> {
        self.0.drain()
    }
    pub(crate) fn prepare(&mut self, host: &ColorProbe, size: Size, sources: &[ColorSource]) {
        self.0.prepare(host, size, sources);
    }

    /// Call after fitting an existing two-cell decorative marker, never a label
    /// or semantic status glyph. Unready/unsupported restores its text fallback.
    pub(crate) fn decorate(
        &self,
        frame: &mut Frame,
        artwork: Artwork,
        col: u16,
        row: u16,
        source: Option<ColorSource>,
        placement_id: u32,
    ) -> bool {
        if col.checked_add(2).is_none_or(|end| end > frame.size.cols)
            || row >= frame.size.rows
            || (artwork == Artwork::Hive && col == 0)
        {
            return false;
        }
        let range = usize::from(col)..usize::from(col) + 2;
        let Some(cells) = frame
            .rows
            .get(usize::from(row))
            .and_then(|r| r.cells.get(range.clone()))
        else {
            return false;
        };
        if cells.iter().any(|cell| cell.width != 1) {
            return false;
        }
        let marker = match artwork {
            Artwork::Bee => ["<", ">"],
            Artwork::Hive => ["[", "]"],
        };
        if !cells
            .iter()
            .zip(marker)
            .all(|(cell, marker)| cell.text == marker)
            && !cells.iter().all(|cell| cell.text == " ")
        {
            return false;
        }
        // Retained UI cells may still contain blanks from the previous icon.
        // Restore the product's marker before a resize/color/capability fallback.
        for (cell, marker) in frame.rows[usize::from(row)].cells[range.clone()]
            .iter_mut()
            .zip(marker)
        {
            cell.text.clear();
            cell.text.push_str(marker);
        }
        let Some(source) = source else { return false };
        let (id, x, offset_x) = match artwork {
            Artwork::Bee => (1, i32::from(col), 0),
            // MAIN positions the hive half a cell left into its tree gutter.
            Artwork::Hive => (2, i32::from(col) - 1, frame.size.cell_width / 2),
        };
        if !self.0.place(
            frame,
            id,
            [x, i32::from(row)],
            [offset_x, 0],
            source,
            placement_id,
        ) {
            return false;
        }
        for cell in &mut frame.rows[usize::from(row)].cells[range] {
            cell.text.clear();
            cell.text.push(' ');
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unready_artwork_preserves_marker_label_and_cursor() {
        let chrome = ChromeGraphics::new().unwrap();
        let mut frame = bkpr_tui_framework::blank(Size {
            cols: 8,
            rows: 2,
            cell_width: 10,
            cell_height: 20,
        });
        bkpr_tui_framework::text(&mut frame, 1, 0, "● label", false);
        let original = frame.clone();
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Bee,
            1,
            0,
            Some(ColorSource::Default),
            1
        ));
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Hive,
            1,
            0,
            Some(ColorSource::Default),
            2
        ));
        assert_eq!(frame, original);
    }
    #[test]
    fn hierarchy_gutters_preserve_text_markers_until_artwork_is_ready() {
        let chrome = ChromeGraphics::new().unwrap();
        let mut frame = bkpr_tui_framework::blank(Size {
            cols: 28,
            rows: 3,
            cell_width: 10,
            cell_height: 20,
        });
        bkpr_tui_framework::text(&mut frame, 0, 0, "▾ [] Workspace", false);
        bkpr_tui_framework::text(&mut frame, 0, 1, "  ▾ ▪ Window", false);
        bkpr_tui_framework::text(&mut frame, 0, 2, "    ● <> Terminal", false);
        let original = frame.clone();
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Hive,
            2,
            0,
            Some(ColorSource::Default),
            1
        ));
        assert!(!chrome.decorate(
            &mut frame,
            Artwork::Bee,
            6,
            2,
            Some(ColorSource::Default),
            2
        ));
        assert_eq!(frame, original);
    }
}
