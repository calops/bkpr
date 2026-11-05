//! TUI application and typed local/remote daemon transport. No backend
//! implementation dependency is linked into a frontend.
mod app;
mod appearance;
pub mod attachments;
mod chrome_graphics;
mod controller;
mod finder;
mod form_fields;
mod keyguide;
mod metadata;
mod popover;
mod project_colors;
const TEXT_INPUT_LIMIT: usize = 4096;
pub mod remote;
mod themes;
mod tiling;
pub mod transport;
mod tree;
mod updates;
use anyhow::{Result, bail};
pub use app::Options;
pub use attachments::{PrepareLocal, SessionTarget};
pub use bkpr_protocol::SpawnDefaults;
use bkpr_protocol::*;
use bkpr_tui_framework::{ClientEvent, ReplyDemux, decode_input, key_event, kitty_key};
use std::path::Path;
use termwiz::input::{InputEvent, InputParser};

/// Run one physical terminal with independently owned local and remote sessions.
pub fn run_sessions(targets: Vec<SessionTarget>, options: Options) -> Result<()> {
    app::run_sessions(targets, options)
}

/// Physical decoder probe: ordered acceptance, never child processing or replay.
pub fn keyboard(socket: &Path, pane: &str, view: &str, bytes: &[u8]) -> Result<()> {
    let input = updates::InputPump::new(&transport::Endpoint::admin(socket))?;
    let mut parser = InputParser::new();
    let mut demux = ReplyDemux::default();
    for event in decode_input(&mut parser, demux.feed(bytes), false) {
        let request = match event {
            ClientEvent::Legacy(InputEvent::Key(key)) => PaneRequest::Key {
                view_id: view.into(),
                event: key_event(key),
            },
            ClientEvent::Reply(reply) if reply.starts_with(b"key;") => PaneRequest::Key {
                view_id: view.into(),
                event: kitty_key(&reply[4..])?,
            },
            ClientEvent::Legacy(InputEvent::Paste(text)) => PaneRequest::Paste {
                view_id: view.into(),
                data: text.into_bytes(),
            },
            _ => bail!("keyboard probe accepts only keys and bracketed paste"),
        };
        if matches!(&request,PaneRequest::Key{event,..}if event.key=="BracketRight"&&event.modifiers&2!=0)
        {
            bail!("detach shortcut is not a keyboard injection");
        }
        input.send(Operation::Pane {
            pane_id: pane.into(),
            request,
        })?;
    }
    input.finish()
}
