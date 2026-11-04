//! Paths are native raw bytes, never lossy display text or JSON path adapters.
use anyhow::Result;
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

pub(super) fn path_bytes(path: &Path) -> Result<Cow<'_, [u8]>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Cow::Borrowed(path.as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        Ok(Cow::Borrowed(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("path is not UTF-8 on this platform"))?
                .as_bytes(),
        ))
    }
}
pub(super) fn read_path(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    {
        Ok(std::str::from_utf8(bytes)?.into())
    }
}
