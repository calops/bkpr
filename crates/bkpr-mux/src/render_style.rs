use super::*;

pub(super) fn transition(
    out: &mut impl Write,
    previous: Option<&Style>,
    style: &Style,
    frame: &Frame,
    host: &ColorProbe,
) -> Result<()> {
    if previous.is_none() {
        write!(out, "\x1b[0m")?;
    }
    if previous.is_none_or(|old| {
        old.foreground != style.foreground || old.foreground_source != style.foreground_source
    }) {
        color(
            out,
            style.foreground,
            style.foreground_source,
            false,
            frame,
            host,
        )?;
    }
    if previous.is_none_or(|old| {
        old.background != style.background || old.background_source != style.background_source
    }) {
        color(
            out,
            style.background,
            style.background_source,
            true,
            frame,
            host,
        )?;
    }
    // SGR 22 resets both intensities. Reapply the complete target pair when
    // disabling either, including bold-to-faint and faint-to-bold transitions.
    let reset_intensity =
        previous.is_some_and(|old| (old.bold && !style.bold) || (old.faint && !style.faint));
    if reset_intensity {
        write!(out, "\x1b[22m")?;
    }
    if style.bold && (reset_intensity || previous.is_none_or(|old| !old.bold)) {
        write!(out, "\x1b[1m")?;
    }
    if style.faint && (reset_intensity || previous.is_none_or(|old| !old.faint)) {
        write!(out, "\x1b[2m")?;
    }
    for (set, old, on, off) in [
        (style.italic, previous.is_some_and(|old| old.italic), 3, 23),
        (style.blink, previous.is_some_and(|old| old.blink), 5, 25),
        (
            style.inverse,
            previous.is_some_and(|old| old.inverse),
            7,
            27,
        ),
        (
            style.invisible,
            previous.is_some_and(|old| old.invisible),
            8,
            28,
        ),
        (
            style.strikethrough,
            previous.is_some_and(|old| old.strikethrough),
            9,
            29,
        ),
        (
            style.overline,
            previous.is_some_and(|old| old.overline),
            53,
            55,
        ),
    ] {
        if set != old {
            write!(out, "\x1b[{}m", if set { on } else { off })?;
        }
    }
    if previous.is_none_or(|old| old.underline != style.underline) {
        if style.underline == 0 {
            write!(out, "\x1b[24m")?;
        } else {
            write!(out, "\x1b[4:{}m", style.underline.min(5))?;
        }
    }
    if previous.is_none_or(|old| {
        old.underline_source != style.underline_source
            || (style.underline_source != ColorSource::Default
                && old.underline_color != style.underline_color)
    }) {
        match style.underline_source {
            ColorSource::Default => write!(out, "\x1b[59m")?,
            _ if host.truecolor => write!(
                out,
                "\x1b[58;2;{};{};{}m",
                style.underline_color[0], style.underline_color[1], style.underline_color[2]
            )?,
            _ => {}
        }
    }
    Ok(())
}
