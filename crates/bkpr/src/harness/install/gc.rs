//! Cleanup of verified unreferenced implementation bundles, never native user settings.
use super::*;

/// Collect obsolete bundles while the harness installation's exclusive lease is held.
pub(super) fn collect(paths: &Paths, kind: HarnessKind, uninstall: bool) -> Result<Vec<String>> {
    let root = paths.data.join(kind.as_str());
    let versions = root.join("versions");
    let mut referenced = BTreeSet::new();
    let profiles = root.join("profiles");
    if profiles.exists() {
        safe_path(&profiles)?;
        for profile in fs::read_dir(&profiles)? {
            let profile = profile?.path();
            safe_path(&profile)?;
            if !profile.is_dir() {
                continue;
            }
            if let Some(manifest) = record(&profile.join("manifest.json"), kind)? {
                referenced.insert(manifest.root);
            }
            if let Some(journal) = journal(&profile.join("pending.json"), kind)? {
                referenced.extend(journal.owners.into_iter().map(|owner| owner.root));
            }
            if fs::read_dir(&profile)?.next().is_none() {
                fs::remove_dir(&profile)?;
            }
        }
    }
    if !versions.exists() {
        return Ok(Vec::new());
    }
    safe_path(&versions)?;
    let mut candidates = Vec::new();
    for version in fs::read_dir(&versions)? {
        let version = version?;
        let path = version.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || referenced.contains(&path) {
            continue;
        }
        let name = version.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_hexdigit) || name.len() != 64 {
            continue;
        }
        candidates.push((metadata.modified()?, path));
    }
    candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let retain = if uninstall { 0 } else { 2 };
    let mut notes = Vec::new();
    for (_, path) in candidates.into_iter().skip(retain) {
        match remove_bundle(&path) {
            Ok(true) => {}
            Ok(false) => {}
            Err(_) => notes.push(format!(
                "Retained changed or unverifiable integration bundle: {}",
                path.display()
            )),
        }
    }
    sync_directory(&versions)?;
    Ok(notes)
}

fn remove_bundle(root: &Path) -> Result<bool> {
    safe_path(root)?;
    let Some(assets): Option<Vec<StoredFile>> = ownership(&root.join(".bundle.json"))? else {
        if fs::read_dir(root)?.next().is_none() {
            fs::remove_dir(root)?;
            return Ok(true);
        }
        bail!("unowned integration bundle");
    };
    let lease = private_file(&root.join(".lease"), true)?;
    match FileExt::try_lock_exclusive(&lease) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    let _lease = Lease(lease);
    let mut allowed = BTreeSet::from([root.join(".bundle.json"), root.join(".lease")]);
    for file in &assets {
        ensure!(
            file.path.starts_with(root) && file.path != root,
            "invalid bundle ownership path"
        );
        safe_path(&file.path)?;
        if let Some(bytes) = read(&file.path)? {
            ensure!(digest(&bytes) == file.digest, "modified bundle asset");
        }
        allowed.insert(file.path.clone());
        for parent in file
            .path
            .ancestors()
            .skip(1)
            .take_while(|path| *path != root)
        {
            allowed.insert(parent.to_owned());
        }
    }
    let mut directories = vec![root.to_owned()];
    let mut index = 0;
    while index < directories.len() {
        for entry in fs::read_dir(&directories[index])? {
            let path = entry?.path();
            ensure!(allowed.contains(&path), "foreign bundle contents");
            let metadata = fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "symlink bundle contents"
            );
            if metadata.is_dir() {
                directories.push(path);
            }
        }
        index += 1;
    }
    for path in allowed.iter().filter(|path| path.is_file()) {
        fs::remove_file(path)?;
    }
    directories.sort_unstable_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in directories {
        fs::remove_dir(path)?;
    }
    Ok(true)
}
