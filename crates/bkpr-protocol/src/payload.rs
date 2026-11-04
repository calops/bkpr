use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Domain replies distinguish high-volume pane scenes from extensible metadata.
#[derive(Clone, Debug)]
pub enum Payload {
    Control(Value),
    Pane(PanePayload),
}
#[derive(Clone, Debug)]
pub struct PanePayload {
    pub metadata: Value,
    /// A slice of the owning response buffer; native decoding doesn't copy scenes.
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
    /// Human-facing conversion only; native scene consumers keep binary storage.
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
