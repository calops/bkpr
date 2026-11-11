//! Serde paths as UTF-8 strings or exact non-UTF-8 Unix bytes, without canonicalization.
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeStruct,
};
use std::{
    fmt,
    path::{Path, PathBuf},
};

const MAX_UNIX_PATH_BYTES: usize = 65_536;

/// JSON shape shared by native path decoding and generated control schemas.
#[derive(schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum PathSchema {
    /// UTF-8 path string.
    Text(String),
    /// Unix path represented by its original bytes.
    Unix {
        /// Raw path bytes.
        unix_bytes: Vec<u8>,
    },
}

impl<'de> Deserialize<'de> for PathSchema {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EncodedPath;
        impl<'de> Visitor<'de> for EncodedPath {
            type Value = PathSchema;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a path string or Unix byte object")
            }
            fn visit_str<E: Error>(self, text: &str) -> Result<Self::Value, E> {
                Ok(PathSchema::Text(text.to_owned()))
            }
            fn visit_string<E: Error>(self, text: String) -> Result<Self::Value, E> {
                Ok(PathSchema::Text(text))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                struct UnixPath {
                    #[serde(deserialize_with = "deserialize_unix_bytes")]
                    unix_bytes: Vec<u8>,
                }
                let path = UnixPath::deserialize(MapAccessDeserializer::new(map))?;
                Ok(PathSchema::Unix {
                    unix_bytes: path.unix_bytes,
                })
            }
        }
        deserializer.deserialize_any(EncodedPath)
    }
}

fn deserialize_unix_bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct UnixBytes;
    impl<'de> Visitor<'de> for UnixBytes {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an array of non-NUL Unix path bytes")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let capacity = sequence.size_hint().unwrap_or(0);
            if capacity > MAX_UNIX_PATH_BYTES {
                return Err(A::Error::custom("invalid Unix path bytes"));
            }
            let mut bytes = Vec::with_capacity(capacity);
            while let Some(byte) = sequence.next_element::<u8>()? {
                if bytes.len() == MAX_UNIX_PATH_BYTES || byte == 0 {
                    return Err(A::Error::custom("invalid Unix path bytes"));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    deserializer.deserialize_seq(UnixBytes)
}

/// Borrowed path serialized as a UTF-8 string or, on Unix, a `unix_bytes` object.
pub struct WirePath<'a>(
    /// Path to serialize without normalization or lossy conversion.
    pub &'a Path,
);
impl Serialize for WirePath<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(path) = self.0.to_str() {
            return serializer.serialize_str(path);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let mut object = serializer.serialize_struct("UnixPath", 1)?;
            object.serialize_field("unix_bytes", self.0.as_os_str().as_bytes())?;
            object.end()
        }
        #[cfg(not(unix))]
        {
            Err(serde::ser::Error::custom(
                "path cannot be represented on this platform",
            ))
        }
    }
}
/// Serialize a path losslessly; reject non-UTF-8 paths on non-Unix platforms.
pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    WirePath(path).serialize(serializer)
}
/// Decode a path string or Unix byte object without filesystem validation.
/// Byte objects are Unix-only, NUL-free and limited to 65,536 bytes during decoding.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    use PathSchema as Encoded;
    match Encoded::deserialize(deserializer)? {
        Encoded::Text(text) => Ok(PathBuf::from(text)),
        Encoded::Unix { unix_bytes } => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                Ok(PathBuf::from(std::ffi::OsString::from_vec(unix_bytes)))
            }
            #[cfg(not(unix))]
            {
                let _ = unix_bytes;
                Err(serde::de::Error::custom(
                    "Unix path bytes unsupported on this platform",
                ))
            }
        }
    }
}
/// Serde adapters using the same path encoding with `null` for an absent path.
pub mod optional {
    use super::*;
    /// Serialize an optional path, preserving exact bytes where supported.
    pub fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref().map(WirePath).serialize(serializer)
    }
    /// Decode an optional path with the same platform and byte-object limits as [`super::deserialize`].
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        #[derive(Deserialize)]
        struct PathValue(#[serde(with = "super")] PathBuf);
        Ok(Option::<PathValue>::deserialize(deserializer)?.map(|v| v.0))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    #[derive(Serialize, Deserialize)]
    struct Paths {
        #[serde(with = "super")]
        cwd: PathBuf,
        #[serde(with = "super::optional")]
        foreground: Option<PathBuf>,
    }
    #[test]
    fn ordinary_strings_and_non_utf8_bytes_round_trip_losslessly() {
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/non-utf8-\xff".to_vec()));
        let value = serde_json::to_value(Paths {
            cwd: PathBuf::from("/ordinary"),
            foreground: Some(path.clone()),
        })
        .unwrap();
        assert_eq!(value["cwd"], "/ordinary");
        assert_eq!(
            value["foreground"]["unix_bytes"]
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            255
        );
        let decoded: Paths = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.foreground, Some(path));
    }

    #[test]
    fn byte_paths_preserve_the_boundary_and_reject_invalid_values() {
        use std::os::unix::ffi::OsStrExt;

        for bytes in [Vec::new(), vec![255; 65_536]] {
            let decoded: Paths = serde_json::from_value(serde_json::json!({
                "cwd": {"unix_bytes": &bytes},
                "foreground": null
            }))
            .unwrap();
            assert_eq!(decoded.cwd.as_os_str().as_bytes(), bytes);
            assert_eq!(decoded.foreground, None);
        }
        for bytes in [
            serde_json::json!([0]),
            serde_json::json!([1, 0]),
            serde_json::json!([-1]),
            serde_json::json!([256]),
            serde_json::json!(["1"]),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<Paths>(serde_json::json!({
                    "cwd": {"unix_bytes": bytes},
                    "foreground": null
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn optional_null_and_unknown_fields_preserve_path_decoding() {
        let decoded: Paths = serde_json::from_str(
            r#"{"cwd":{"ignored":[1,2],"unix_bytes":[47,255]},"foreground":null}"#,
        )
        .unwrap();
        assert_eq!(
            decoded.cwd,
            PathBuf::from(std::ffi::OsString::from_vec(vec![47, 255]))
        );
        assert_eq!(decoded.foreground, None);
        for input in [
            r#"{"cwd":null,"foreground":null}"#,
            r#"{"cwd":{},"foreground":null}"#,
            r#"{"cwd":"/ordinary","foreground":{"unix_bytes":null}}"#,
            r#"{"cwd":{"unix_bytes":[47],"unix_bytes":[48]},"foreground":null}"#,
        ] {
            assert!(serde_json::from_str::<Paths>(input).is_err());
        }
    }

    #[test]
    fn oversized_byte_path_is_rejected_before_parsing_invalid_tail() {
        let input = format!(
            "{{\"cwd\":{{\"unix_bytes\":[{}2,invalid_unread_tail",
            "1,".repeat(65_536)
        );
        let error = serde_json::from_str::<Paths>(&input).err().unwrap();
        assert!(error.is_data(), "{error}");
    }
}
