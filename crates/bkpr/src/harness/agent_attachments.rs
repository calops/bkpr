use anyhow::{Context, Result, bail, ensure};
use bkpr_protocol::{
    AgentRequest, Payload,
    channels::{
        AttachmentId, AttachmentMediaType, AttachmentUpload, ChannelId, ChannelRequest,
        ChannelResponse, MAX_ATTACHMENT_BYTES,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub(super) const MAX_PATH_BYTES: usize = 4096;

/// Translate a launch-local path operation into a typed channel transfer.
#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::harness) enum AttachmentToolRequest {
    ChannelUpload {
        channel_id: ChannelId,
        path: PathBuf,
        alt_text: Option<String>,
    },
    ChannelFetch {
        channel_id: ChannelId,
        attachment_id: AttachmentId,
        path: PathBuf,
    },
}

impl AttachmentToolRequest {
    pub(super) fn dispatch(
        self,
        send: impl FnOnce(AgentRequest) -> Result<Payload>,
    ) -> Result<Value> {
        let (request, destination) = match self {
            Self::ChannelUpload {
                channel_id,
                path,
                alt_text,
            } => {
                validate_path(&path)?;
                let bytes = read_png(&path)?;
                (
                    ChannelRequest::UploadAttachment {
                        channel_id,
                        upload: AttachmentUpload {
                            media_type: AttachmentMediaType::Png,
                            alt_text,
                            bytes,
                        },
                    },
                    None,
                )
            }
            Self::ChannelFetch {
                channel_id,
                attachment_id,
                path,
            } => {
                validate_path(&path)?;
                (
                    ChannelRequest::GetAttachment {
                        channel_id,
                        attachment_id,
                    },
                    Some(path),
                )
            }
        };
        let payload = send(AgentRequest::Channels {
            request: Box::new(request),
        })?;
        match (destination, payload) {
            (None, Payload::Channels(ChannelResponse::AttachmentUploaded { attachment })) => {
                Ok(json!({"kind":"attachment_uploaded","attachment":attachment}))
            }
            (Some(path), Payload::Channels(ChannelResponse::Attachment { attachment, bytes })) => {
                write_png(&path, &bytes)?;
                Ok(json!({"kind":"attachment_fetched","attachment":attachment}))
            }
            _ => bail!("unexpected native attachment response"),
        }
    }
}

fn validate_path(path: &Path) -> Result<()> {
    let name = path.to_str().context("attachment path must be UTF-8")?;
    super::super::validate_text(name, MAX_PATH_BYTES, "attachment path")?;
    ensure!(
        path.is_absolute() && path.file_name().is_some(),
        "attachment path must name an absolute local file"
    );
    Ok(())
}

fn read_png(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .context("open local PNG source")?;
    let metadata = file.metadata().context("inspect local PNG source")?;
    ensure!(metadata.is_file(), "PNG source must be a regular file");
    let limit = MAX_ATTACHMENT_BYTES as u64;
    ensure!(
        (1..=limit).contains(&metadata.len()),
        "PNG source size outside bounds (1 byte to 4 MiB)"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .context("read local PNG source")?;
    ensure!(
        (1..=MAX_ATTACHMENT_BYTES).contains(&bytes.len()),
        "PNG source size outside bounds (1 byte to 4 MiB)"
    );
    Ok(bytes)
}

fn write_png(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("attachment destination has no parent directory")?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).context("open temporary local PNG destination")?;
    file.write_all(bytes)
        .context("write local PNG destination")?;
    file.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("publish local PNG destination without overwriting")?;
    Ok(())
}
