//! Formatting-preserving edits limited to installer-owned native settings.
use super::types::Format;
use anyhow::{Context, Result, bail, ensure};
use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstObject, CstRootNode},
};
use serde_json::Value;
use std::path::Path;

/// Parsed native document, retaining comments and unrelated formatting.
pub(super) enum Document {
    /// A native JSON or JSONC document.
    Json(CstRootNode),
    /// A native TOML document.
    Toml(Box<toml_edit::DocumentMut>),
}
impl Document {
    /// Parse existing native settings or initialize a missing document.
    pub fn parse(format: Format, bytes: Option<&[u8]>, path: &Path) -> Result<Self> {
        let text = match bytes {
            Some(value) => {
                std::str::from_utf8(value).context("native configuration is not UTF-8")?
            }
            None => match format {
                Format::Json => "{}\n",
                Format::Toml => "",
            },
        };
        match format {
            Format::Json => {
                let root = CstRootNode::parse(text, &ParseOptions::default()).map_err(|_| {
                    anyhow::anyhow!("invalid JSONC configuration in {}", path.display())
                })?;
                ensure!(
                    root.object_value().is_some(),
                    "native JSON configuration must be an object: {}",
                    path.display()
                );
                Ok(Self::Json(root))
            }
            Format::Toml => Ok(Self::Toml(Box::new(text.parse().map_err(|_| {
                anyhow::anyhow!("invalid TOML configuration in {}", path.display())
            })?))),
        }
    }

    /// Read one native slot without changing the document.
    pub fn get(&self, keys: &[String]) -> Result<Option<Value>> {
        ensure!(!keys.is_empty(), "empty native configuration key");
        match self {
            Self::Json(root) => {
                let mut object = root
                    .object_value()
                    .context("native JSON root is not an object")?;
                for (index, key) in keys.iter().enumerate() {
                    let property = unique_property(&object, key)?;
                    let Some(property) = property else {
                        return Ok(None);
                    };
                    if index + 1 == keys.len() {
                        return Ok(property.value().and_then(|value| value.to_serde_value()));
                    }
                    object = property
                        .object_value()
                        .context("native configuration parent is not an object")?;
                }
                unreachable!()
            }
            Self::Toml(document) => {
                let parsed: toml::Value = toml::from_str(&document.to_string())
                    .map_err(|_| anyhow::anyhow!("edited TOML is invalid"))?;
                let mut current = &parsed;
                for key in keys {
                    let table = current
                        .as_table()
                        .context("native configuration parent is not a table")?;
                    let Some(value) = table.get(key) else {
                        return Ok(None);
                    };
                    current = value;
                }
                Ok(Some(serde_json::to_value(current)?))
            }
        }
    }

    /// Replace or remove exactly one native slot.
    pub fn set(&mut self, keys: &[String], value: Option<&Value>) -> Result<()> {
        let (key, parents) = keys
            .split_last()
            .context("empty native configuration key")?;
        match self {
            Self::Json(root) => {
                let mut object = root
                    .object_value()
                    .context("native JSON root is not an object")?;
                for parent in parents {
                    object = match unique_property(&object, parent)? {
                        Some(property) => property
                            .object_value()
                            .context("native configuration parent is not an object")?,
                        None if value.is_none() => return Ok(()),
                        None => object
                            .append(parent, CstInputValue::Object(Vec::new()))
                            .object_value()
                            .context("create native configuration parent")?,
                    };
                }
                match (unique_property(&object, key)?, value) {
                    (Some(property), Some(value)) => {
                        property.set_value(input(value));
                    }
                    (None, Some(value)) => {
                        object.append(key, input(value));
                    }
                    (Some(property), None) => {
                        property.remove();
                    }
                    (None, None) => {}
                }
            }
            Self::Toml(document) => {
                let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
                for parent in parents {
                    if !table.contains_key(parent) {
                        if value.is_none() {
                            return Ok(());
                        }
                        table.insert(parent, toml_edit::Item::Table(toml_edit::Table::new()));
                    }
                    table = table
                        .get_mut(parent)
                        .and_then(toml_edit::Item::as_table_like_mut)
                        .context("native configuration parent is not a table")?;
                }
                if let Some(value) = value {
                    let generated = toml::to_string(&serde_json::json!({"owned":value}))
                        .context("encode native TOML slot")?;
                    let mut generated: toml_edit::DocumentMut = generated
                        .parse()
                        .context("parse generated native TOML slot")?;
                    table.insert(
                        key,
                        generated
                            .remove("owned")
                            .context("missing generated native TOML slot")?,
                    );
                } else {
                    table.remove(key);
                }
            }
        }
        Ok(())
    }

    /// Replace or remove one array member while preserving neighboring trivia.
    pub fn array_change(
        &mut self,
        keys: &[String],
        index: Option<usize>,
        value: Option<&Value>,
    ) -> Result<()> {
        match self {
            Self::Json(root) => {
                if self_get_json(root, keys)?.is_none() {
                    if value.is_none() {
                        return Ok(());
                    }
                    self.set(keys, Some(&Value::Array(Vec::new())))?;
                }
                let Self::Json(root) = self else {
                    unreachable!()
                };
                let array = self_get_json(root, keys)?
                    .and_then(|node| node.as_array())
                    .context("native integration list is not an array")?;
                if let Some(index) = index {
                    let old = array
                        .elements()
                        .get(index)
                        .cloned()
                        .context("native integration list changed")?;
                    if let Some(value) = value {
                        array.insert(index, input(value));
                    }
                    old.remove();
                } else if let Some(value) = value {
                    array.append(input(value));
                }
            }
            Self::Toml(_) => {
                let mut values = match self.get(keys)? {
                    None => Vec::new(),
                    Some(Value::Array(values)) => values,
                    Some(_) => bail!("native integration list is not an array"),
                };
                match (index, value) {
                    (Some(index), Some(value)) => {
                        *values
                            .get_mut(index)
                            .context("native integration list changed")? = value.clone()
                    }
                    (Some(index), None) => {
                        ensure!(index < values.len(), "native integration list changed");
                        values.remove(index);
                    }
                    (None, Some(value)) => values.push(value.clone()),
                    (None, None) => {}
                }
                self.set(keys, Some(&Value::Array(values)))?;
            }
        }
        Ok(())
    }

    /// Render edited native syntax, retaining untouched document formatting.
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Json(root) => root.to_string().into_bytes(),
            Self::Toml(document) => document.to_string().into_bytes(),
        }
    }
}

fn unique_property(
    object: &CstObject,
    key: &str,
) -> Result<Option<jsonc_parser::cst::CstObjectProp>> {
    let mut result = None;
    for property in object.properties() {
        if property.decoded_name().as_deref() == Some(key) {
            ensure!(result.is_none(), "duplicate native integration key: {key}");
            result = Some(property);
        }
    }
    Ok(result)
}
fn self_get_json(
    root: &CstRootNode,
    keys: &[String],
) -> Result<Option<jsonc_parser::cst::CstNode>> {
    let mut object = root
        .object_value()
        .context("native JSON root is not an object")?;
    for (index, key) in keys.iter().enumerate() {
        let Some(property) = unique_property(&object, key)? else {
            return Ok(None);
        };
        if index + 1 == keys.len() {
            return Ok(property.value());
        }
        object = property
            .object_value()
            .context("native configuration parent is not an object")?;
    }
    bail!("empty native configuration key")
}
fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(values.iter().map(input).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), input(value)))
                .collect(),
        ),
    }
}
