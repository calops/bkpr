use anyhow::Result;
use bkpr_protocol::{Operation, PaneContext};
use std::{io::Write, path::Path};

pub(crate) fn execute(socket: &Path, pane_id: Option<String>, json: bool) -> Result<()> {
    let context: PaneContext = serde_json::from_value(bkpr_client::transport::checked(
        socket,
        Operation::Context { pane_id },
    )?)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if json {
        serde_json::to_writer(&mut output, &context)?;
    } else {
        serde_json::to_writer_pretty(&mut output, &context)?;
    }
    writeln!(output)?;
    Ok(())
}
