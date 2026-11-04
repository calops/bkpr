//! Extensible control metadata only. Terminal scenes never pass through this tree.
use super::{Budget, generated::native_wire as g};
use anyhow::{Result, ensure};
use planus::{Builder, Offset};
use serde_json::{Map, Number, Value};

pub(super) fn write_value(
    builder: &mut Builder,
    value: &Value,
    budget: &mut Budget,
    depth: usize,
) -> Result<Offset<g::Value>> {
    budget.node(depth)?;
    let body = match value {
        Value::Null => {
            let value = g::NullValue::create(builder);
            g::ValueBody::create_null_value(builder, value)
        }
        Value::Bool(value) => {
            let value = g::BoolValue::create(builder, *value);
            g::ValueBody::create_bool_value(builder, value)
        }
        Value::Number(value) => {
            if let Some(value) = value.as_u64() {
                let value = g::UnsignedValue::create(builder, value);
                g::ValueBody::create_unsigned_value(builder, value)
            } else if let Some(value) = value.as_i64() {
                let value = g::SignedValue::create(builder, value);
                g::ValueBody::create_signed_value(builder, value)
            } else {
                let value = value
                    .as_f64()
                    .ok_or_else(|| anyhow::anyhow!("unsupported metadata number"))?;
                ensure!(value.is_finite(), "non-finite metadata number");
                let value = g::FloatValue::create(builder, value);
                g::ValueBody::create_float_value(builder, value)
            }
        }
        Value::String(value) => {
            budget.bytes(value.len())?;
            let value = g::StringValue::create(builder, value.as_str());
            g::ValueBody::create_string_value(builder, value)
        }
        Value::Array(values) => {
            budget.vector(values.len(), std::mem::size_of::<Value>())?;
            let mut offsets = Vec::with_capacity(values.len());
            for value in values {
                offsets.push(write_value(builder, value, budget, depth + 1)?);
            }
            let values = builder.create_vector(&offsets);
            let value = g::ArrayValue::create(builder, values);
            g::ValueBody::create_array_value(builder, value)
        }
        Value::Object(values) => {
            budget.vector(values.len(), std::mem::size_of::<(String, Value)>())?;
            let mut offsets = Vec::with_capacity(values.len());
            for (key, value) in values {
                budget.bytes(key.len())?;
                let key = builder.create_string(key);
                let value = write_value(builder, value, budget, depth + 1)?;
                offsets.push(g::Entry::create(builder, key, value));
            }
            let entries = builder.create_vector(&offsets);
            let value = g::ObjectValue::create(builder, entries);
            g::ValueBody::create_object_value(builder, value)
        }
    };
    Ok(g::Value::create(builder, body))
}

pub(super) fn read_value(
    value: g::ValueRef<'_>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Value> {
    budget.node(depth)?;
    Ok(match value.body()? {
        g::ValueBodyRef::NullValue(_) => Value::Null,
        g::ValueBodyRef::BoolValue(value) => Value::Bool(value.value()?),
        g::ValueBodyRef::SignedValue(value) => Value::Number(value.value()?.into()),
        g::ValueBodyRef::UnsignedValue(value) => Value::Number(value.value()?.into()),
        g::ValueBodyRef::FloatValue(value) => Value::Number(
            Number::from_f64(value.value()?)
                .ok_or_else(|| anyhow::anyhow!("non-finite metadata number"))?,
        ),
        g::ValueBodyRef::StringValue(value) => {
            let text = value.value()?;
            budget.bytes(text.len())?;
            Value::String(text.to_owned())
        }
        g::ValueBodyRef::ArrayValue(value) => {
            let values = value.values()?;
            budget.vector(values.len(), std::mem::size_of::<Value>())?;
            let mut output = Vec::with_capacity(values.len());
            for value in values {
                output.push(read_value(value?, budget, depth + 1)?);
            }
            Value::Array(output)
        }
        g::ValueBodyRef::ObjectValue(value) => {
            let entries = value.entries()?;
            budget.vector(entries.len(), std::mem::size_of::<(String, Value)>())?;
            let mut output = Map::new();
            for entry in entries {
                let entry = entry?;
                let key = entry.key()?;
                budget.bytes(key.len())?;
                ensure!(
                    !output.contains_key(key),
                    "duplicate native metadata object key"
                );
                let value = read_value(entry.value()?, budget, depth + 1)?;
                output.insert(key.to_owned(), value);
            }
            Value::Object(output)
        }
    })
}
