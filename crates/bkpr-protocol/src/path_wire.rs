//! UTF-8 paths retain their string wire form. Other legal Unix paths retain exact bytes.
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use std::path::{Path, PathBuf};

pub struct WirePath<'a>(pub &'a Path);
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
pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    WirePath(path).serialize(serializer)
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Encoded {
        Text(String),
        Unix { unix_bytes: Vec<u8> },
    }
    match Encoded::deserialize(deserializer)? {
        Encoded::Text(text) => Ok(PathBuf::from(text)),
        Encoded::Unix { unix_bytes } => {
            if unix_bytes.len() > 65536 || unix_bytes.contains(&0) {
                return Err(serde::de::Error::custom("invalid Unix path bytes"));
            }
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
pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref().map(WirePath).serialize(serializer)
    }
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
}
