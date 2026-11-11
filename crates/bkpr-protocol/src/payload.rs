//! Reply payloads with separate binary scene storage and diagnostic-only JSON conversion.
use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Reply metadata or pane data with an optional binary scene.
/// Serde emits diagnostic JSON; deserialization always produces [`Self::Control`].
#[derive(Clone, Debug)]
pub enum Payload {
    /// Extensible control-reply metadata.
    Control(
        /// Reply value preserved without scene interpretation.
        Value,
    ),
    /// Pane metadata and its separately encoded scene.
    Pane(
        /// Pane reply with optional binary scene bytes.
        PanePayload,
    ),
}
/// Pane reply whose binary scene is kept outside its metadata object.
#[derive(Clone, Debug)]
pub struct PanePayload {
    /// Pane metadata; diagnostic conversion requires an object without a `scene` key.
    pub metadata: Value,
    /// Optional binary scene; owned native decoding shares its response allocation.
    /// Borrowed native decoding copies these bytes, and scene decoding validates their contents.
    pub scene: Option<bytes::Bytes>,
}
impl From<Value> for Payload {
    fn from(value: Value) -> Self {
        Self::Control(value)
    }
}
impl From<PanePayload> for Payload {
    fn from(value: PanePayload) -> Self {
        Self::Pane(value)
    }
}
impl Payload {
    /// Convert to diagnostic JSON, expanding any binary scene into `frame` or delta `scene`.
    /// Reject invalid pane metadata or malformed scene bytes; control values pass through.
    pub fn into_json(self) -> Result<Value> {
        match self {
            Self::Control(value) => Ok(value),
            Self::Pane(mut pane) => {
                ensure!(pane.metadata.is_object(), "pane metadata must be an object");
                ensure!(
                    pane.metadata.get("scene").is_none(),
                    "scene must remain outside metadata"
                );
                if let Some(bytes) = pane.scene {
                    diagnostic_scene(&mut pane.metadata, &bytes)?;
                }
                Ok(pane.metadata)
            }
        }
    }
}
impl Serialize for Payload {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Control(value) => value.serialize(serializer),
            Self::Pane(pane) => {
                let mut value = pane.metadata.clone();
                if !value.is_object() || value.get("scene").is_some() {
                    return Err(serde::ser::Error::custom("invalid pane metadata"));
                }
                if let Some(bytes) = &pane.scene {
                    diagnostic_scene(&mut value, bytes).map_err(serde::ser::Error::custom)?;
                }
                value.serialize(serializer)
            }
        }
    }
}
impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::Control)
    }
}

fn diagnostic_scene(metadata: &mut Value, bytes: &[u8]) -> Result<()> {
    let value = crate::scene_wire::to_json(bytes)?;
    if value.get("base_revision").is_none() {
        metadata["frame"] = value;
    } else {
        metadata["scene"] = value;
    }
    Ok(())
}
