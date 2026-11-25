use super::*;

fn fixture() -> Result<(tempfile::TempDir, Paths)> {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let paths = Paths {
        home: root.path().join("home"),
        data: root.path().join("data"),
        program: PathBuf::from("/fixture/bkpr-a"),
        cwd: root.path().to_owned(),
        environment: BTreeMap::new(),
    };
    directory(&paths.home, true)?;
    Ok((root, paths))
}
fn apply(
    paths: &Paths,
    definition: &Definition,
    owners: &[Manifest],
    mode: Mode,
) -> Result<Manifest> {
    let plan = plan(definition, paths, owners, mode)?;
    let path = location(paths, definition);
    directory(path.parent().unwrap(), true)?;
    if mode != Mode::Uninstall {
        publish_bundle(definition)?;
    }
    commit(&plan, &path, mode == Mode::Uninstall)?;
    Ok(plan.target)
}

#[test]
fn codex_installs_transport_only_and_preserves_user_policy() -> Result<()> {
    let (_root, mut paths) = fixture()?;
    let config = paths.home.join(".codex/config.toml");
    directory(config.parent().unwrap(), false)?;
    let original = "# native user settings\nmodel = 'fixture-model'\n[mcp_servers.other]\ncommand = 'other' # keep this comment\n";
    fs::write(&config, original)?;
    let first = codex::definition(&paths)?;
    let installed = apply(&paths, &first, &[], Mode::Install)?;
    let parse =
        |file: &Path| -> Result<toml::Value> { Ok(toml::from_str(&fs::read_to_string(file)?)?) };
    let value = parse(&config)?;
    assert!(value["mcp_servers"]["bkpr"].get("tools").is_none());
    let mut document = config::Document::parse(Format::Toml, Some(&fs::read(&config)?), &config)?;
    let tool_path = |tool: &str| ["mcp_servers", "bkpr", "tools", tool].map(String::from);
    let mut send_mode = tool_path("send_message").to_vec();
    send_mode.push("approval_mode".into());
    document.set(&send_mode, Some(&Value::String("prompt".into())))?;
    fs::write(&config, document.bytes())?;
    verify_record(&installed)?;
    paths.program = PathBuf::from("/fixture/bkpr-b");
    let updated = codex::definition(&paths)?;
    let installed = apply(&paths, &updated, &[installed], Mode::Update)?;
    let updated = codex::definition(&paths)?;
    let installed = apply(&paths, &updated, &[installed], Mode::Update)?;
    let value = parse(&config)?;
    let tools = &value["mcp_servers"]["bkpr"]["tools"];
    assert_eq!(
        tools["send_message"]["approval_mode"].as_str(),
        Some("prompt")
    );
    assert!(tools.get("ack_message").is_none());
    assert_eq!(
        value["mcp_servers"]["bkpr"]["command"].as_str(),
        Some("/fixture/bkpr-b")
    );
    let mut document = config::Document::parse(Format::Toml, Some(&fs::read(&config)?), &config)?;
    document.set(
        &["mcp_servers".into(), "bkpr".into(), "enabled".into()],
        Some(&Value::Bool(false)),
    )?;
    fs::write(&config, document.bytes())?;
    let disabled = codex::definition(&paths)?;
    assert!(!disabled.blockers.is_empty());
    let before = fs::read(&config)?;
    let error = plan(
        &disabled,
        &paths,
        std::slice::from_ref(&installed),
        Mode::Uninstall,
    )
    .err()
    .context("uninstall must retain the user's Codex preferences")?;
    assert!(
        error
            .to_string()
            .contains("mcp_servers.bkpr.tools.send_message.approval_mode")
    );
    assert!(error.to_string().contains("mcp_servers.bkpr.enabled"));
    assert_eq!(fs::read(&config)?, before);
    document.set(&tool_path("send_message"), None)?;
    document.set(
        &["mcp_servers".into(), "bkpr".into(), "enabled".into()],
        None,
    )?;
    fs::write(&config, document.bytes())?;
    apply(
        &paths,
        &codex::definition(&paths)?,
        &[installed],
        Mode::Uninstall,
    )?;
    let text = fs::read_to_string(&config)?;
    let value: toml::Value = toml::from_str(&text)?;
    assert!(value["mcp_servers"].get("bkpr").is_none());
    assert_eq!(
        value["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    assert!(text.contains("# keep this comment"));
    assert_eq!(value["model"].as_str(), Some("fixture-model"));
    Ok(())
}

#[test]
fn claude_exact_user_allow_remains_unowned_and_removed_default_stays_removed() -> Result<()> {
    let (_root, paths) = fixture()?;
    let first = claude::definition(&paths)?;
    let mut defaults = first.entries.iter().filter(|entry| entry.default);
    let user_rule = defaults
        .next()
        .context("missing first Claude collaboration default")?;
    let removed_rule = defaults
        .next()
        .context("missing second Claude collaboration default")?;
    let settings = &user_rule.file;
    directory(settings.parent().unwrap(), false)?;
    fs::write(
        settings,
        serde_json::to_vec(&serde_json::json!({
            "permissions": {"allow": [&user_rule.value]}
        }))?,
    )?;
    let installed = apply(&paths, &claude::definition(&paths)?, &[], Mode::Install)?;
    assert!(
        installed
            .entries
            .iter()
            .any(|entry| entry.id == user_rule.id && entry.preserved)
    );
    let bytes = fs::read(settings)?;
    let mut document = config::Document::parse(user_rule.format, Some(&bytes), settings)?;
    let allows = document
        .get(&user_rule.keys)?
        .context("missing Claude allows")?;
    let index = allows
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry == &removed_rule.value)
        .context("missing installed Claude default")?;
    document.array_change(&removed_rule.keys, Some(index), None)?;
    fs::write(settings, document.bytes())?;
    verify_record(&installed)?;
    let installed = apply(
        &paths,
        &claude::definition(&paths)?,
        &[installed],
        Mode::Update,
    )?;
    let installed = apply(
        &paths,
        &claude::definition(&paths)?,
        &[installed],
        Mode::Update,
    )?;
    let bytes = fs::read(settings)?;
    let document = config::Document::parse(user_rule.format, Some(&bytes), settings)?;
    let allows = document
        .get(&user_rule.keys)?
        .context("missing Claude allows")?;
    assert!(!allows.as_array().unwrap().contains(&removed_rule.value));
    apply(
        &paths,
        &claude::definition(&paths)?,
        &[installed],
        Mode::Uninstall,
    )?;
    let bytes = fs::read(settings)?;
    let document = config::Document::parse(user_rule.format, Some(&bytes), settings)?;
    let allows = document
        .get(&user_rule.keys)?
        .context("missing Claude allows")?;
    assert_eq!(
        allows.as_array().unwrap().as_slice(),
        std::slice::from_ref(&user_rule.value)
    );
    Ok(())
}

#[test]
fn foreign_registration_and_modified_owned_hooks_are_never_replaced() -> Result<()> {
    let (_root, paths) = fixture()?;
    let definition = claude::definition(&paths)?;
    let global = definition
        .entries
        .iter()
        .find(|entry| entry.id == "mcp")
        .context("missing Claude MCP definition")?
        .file
        .clone();
    directory(global.parent().unwrap(), false)?;
    let foreign = b"{\"mcpServers\":{\"bkpr\":{\"command\":\"user-owned\"}},\"keep\":true}\n";
    fs::write(&global, foreign)?;
    assert!(plan(&definition, &paths, &[], Mode::Install).is_err());
    assert!(check_unowned(&definition).is_err());
    assert_eq!(fs::read(&global)?, foreign);
    fs::remove_file(&global)?;
    let installed = apply(&paths, &definition, &[], Mode::Install)?;
    let entry = definition
        .entries
        .iter()
        .find(|entry| entry.array)
        .context("missing Claude hook")?;
    let original = fs::read(&entry.file)?;
    for command_edit in [false, true] {
        let mut document = config::Document::parse(entry.format, Some(&original), &entry.file)?;
        let mut value = entry.value.clone();
        if command_edit {
            let command = value["hooks"][0]["command"]
                .as_str()
                .context("missing native hook command")?;
            value["hooks"][0]["command"] =
                Value::String(command.replace("/fixture/bkpr-a", "/fixture/user-wrapper"));
        } else {
            value["user_modified"] = Value::Bool(true);
        }
        document.array_change(&entry.keys, Some(0), Some(&value))?;
        fs::write(&entry.file, document.bytes())?;
        let before = fs::read(&entry.file)?;
        assert!(
            plan(
                &definition,
                &paths,
                std::slice::from_ref(&installed),
                Mode::Update
            )
            .is_err()
        );
        assert_eq!(fs::read(&entry.file)?, before);
    }
    Ok(())
}

fn two_documents(paths: &Paths, suffix: &str) -> Result<Definition> {
    let mut definition = agy::definition(paths)?;
    definition.entries = vec![
        Entry::value(
            "first",
            paths.home.join("a.json"),
            Format::Json,
            &["bkpr"],
            Value::String(format!("first-{suffix}")),
        ),
        Entry::value(
            "second",
            paths.home.join("z.json"),
            Format::Json,
            &["bkpr"],
            Value::String(format!("second-{suffix}")),
        ),
    ];
    definition.removals.clear();
    Ok(definition)
}
#[test]
fn concurrent_native_writer_is_preserved_and_earlier_files_roll_back() -> Result<()> {
    let (_root, paths) = fixture()?;
    let definition = two_documents(&paths, "a")?;
    let first = paths.home.join("a.json");
    let second = paths.home.join("z.json");
    let original = b"{\n  // user comment\n  \"keep\": true\n}\n";
    fs::write(&first, original)?;
    let plan = plan(&definition, &paths, &[], Mode::Install)?;
    let concurrent = b"{\"native_writer\":true}\n";
    fs::write(&second, concurrent)?;
    let manifest = location(&paths, &definition);
    directory(manifest.parent().unwrap(), true)?;
    publish_bundle(&definition)?;
    assert!(commit(&plan, &manifest, false).is_err());
    assert_eq!(fs::read(&first)?, original);
    assert_eq!(fs::read(&second)?, concurrent);
    assert!(!manifest.exists());
    assert!(!manifest.with_file_name("pending.json").exists());
    Ok(())
}
#[test]
fn interrupted_multi_document_install_repairs_only_proven_owned_generations() -> Result<()> {
    let (_root, paths) = fixture()?;
    let first = two_documents(&paths, "a")?;
    let plan = plan(&first, &paths, &[], Mode::Install)?;
    let manifest = location(&paths, &first);
    directory(manifest.parent().unwrap(), true)?;
    publish_bundle(&first)?;
    write(
        &manifest.with_file_name("pending.json"),
        Some(&serde_json::to_vec(&plan.journal)?),
        0o600,
    )?;
    let change = &plan.changes[0];
    write(&change.path, change.after.as_deref(), change.mode)?;
    let pending = journal(&manifest.with_file_name("pending.json"), HarnessKind::Agy)?
        .context("missing interrupted ownership")?;
    let second = two_documents(&paths, "b")?;
    let installed = apply(&paths, &second, &pending.owners, Mode::Update)?;
    verify_record(&installed)?;
    assert!(!manifest.with_file_name("pending.json").exists());
    let first: Value = serde_json::from_slice(&fs::read(paths.home.join("a.json"))?)?;
    let second: Value = serde_json::from_slice(&fs::read(paths.home.join("z.json"))?)?;
    assert_eq!(first["bkpr"], "first-b");
    assert_eq!(second["bkpr"], "second-b");
    Ok(())
}
#[test]
fn bundle_cleanup_respects_live_launches_and_retains_modified_files() -> Result<()> {
    let (_root, paths) = fixture()?;
    let definition = agy::definition(&paths)?;
    publish_bundle(&definition)?;
    let lease = private_file(&definition.bundle.root.join(".lease"), false)?;
    FileExt::lock_shared(&lease)?;
    let lease = Lease(lease);
    gc::collect(&paths, HarnessKind::Agy, true)?;
    assert!(definition.bundle.root.join("registration.json").exists());
    drop(lease);
    let changed = definition.bundle.root.join("registration.json");
    fs::write(&changed, b"user-modified")?;
    assert!(!gc::collect(&paths, HarnessKind::Agy, true)?.is_empty());
    assert_eq!(fs::read(&changed)?, b"user-modified");
    fs::write(&changed, definition.bundle.files[0].bytes.as_ref())?;
    gc::collect(&paths, HarnessKind::Agy, true)?;
    assert!(!definition.bundle.root.exists());
    Ok(())
}
