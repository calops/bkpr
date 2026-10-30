use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// Copy resources while the native hold-begin callback still has the terminal
/// state that produced the render snapshot. Later VT can delete/change them.
pub(super) unsafe fn capture(terminal: GhosttyTerminal, state: &mut CallbackState) -> Result<()> {
    unsafe {
        check(ghostty_render_state_get(
            state.render,
            GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR,
            out(&mut state.images),
        ))?;
        let mut graphics = ptr::null_mut();
        check(ghostty_terminal_get(
            terminal,
            GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
            out(&mut graphics),
        ))?;
        let mut seen = HashSet::new();
        while ghostty_render_state_image_iterator_next(state.images) {
            let mut id = 0u32;
            let mut generation = 0u64;
            check(ghostty_render_state_image_get(
                state.images,
                GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
                out(&mut id),
            ))?;
            check(ghostty_render_state_image_get(
                state.images,
                GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION,
                out(&mut generation),
            ))?;
            let key = (id, generation);
            if seen.insert(key)
                && !state.image_cache.contains_key(&key)
                && let Some(image) = super::render::read_image(graphics, id, generation)?
            {
                state.image_cache.insert(key, Arc::new(image));
            }
        }
        state.image_cache.retain(|key, _| seen.contains(key));
        state.held_links.clear();
        check(ghostty_render_state_get(
            state.render,
            GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
            out(&mut state.rows),
        ))?;
        let mut y = 0;
        while ghostty_render_state_row_iterator_next(state.rows) {
            check(ghostty_render_state_row_get(
                state.rows,
                GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                out(&mut state.cells),
            ))?;
            let mut x = 0;
            while ghostty_render_state_row_cells_next(state.cells) {
                let mut raw = GhosttyCell::default();
                let mut linked = false;
                check(ghostty_render_state_row_cells_get(
                    state.cells,
                    GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                    out(&mut raw),
                ))?;
                check(ghostty_cell_get(
                    raw,
                    GHOSTTY_CELL_DATA_HAS_HYPERLINK,
                    out(&mut linked),
                ))?;
                if linked && let Some(link) = super::render::read_hyperlink(terminal, x, y)? {
                    state.held_links.insert((x, y), link);
                }
                x += 1;
            }
            y += 1;
        }
        Ok(())
    }
}

pub(super) type ImageCache = HashMap<(u32, u64), Arc<Image>>;

pub(super) fn release(state: &mut CallbackState) -> Result<()> {
    if state.rebuild_after_hold {
        let mut render = ptr::null_mut();
        unsafe {
            check(ghostty_render_state_new(ptr::null(), &mut render))?;
        }
        let previous = std::mem::replace(&mut state.render, render);
        unsafe {
            ghostty_render_state_free(previous);
        }
        state.rebuild_after_hold = false;
    }
    state.hold_since = None;
    state.held_links.clear();
    Ok(())
}
