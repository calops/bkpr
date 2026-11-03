//! Cold-start execution admission. Saved metadata is intent, never live authority.
use super::*;

struct Batch(Arc<Inner>);
impl Drop for Batch {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().restore_batch = false;
        self.0.changed.notify_all();
    }
}

impl Registry {
    /// Restore owned saved executions once, using only the current ordinary shell
    /// and fixed native resume adapters. A live daemon attachment never calls this.
    pub fn bootstrap(&self, spec: PaneSpawnSpec) -> Result<Value> {
        let saved = {
            let _transaction = self.inner.transaction.lock().unwrap();
            let mut state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            ensure!(
                !state.closing && !self.session_end_requested(),
                SessionClosing("session is closing")
            );
            if state.bootstrap_started
                || state.spawning != 0
                || state.live.values().any(|pane| {
                    !pane
                        .shared
                        .finished
                        .load(std::sync::atomic::Ordering::Acquire)
                })
            {
                return Ok(json!({"registry":state.record.registry}));
            }
            ensure!(
                !spec.argv.is_empty(),
                "automatic restore requires current shell argv"
            );
            state.bootstrap_started = true;
            state.restore_batch = true;
            state
                .record
                .registry
                .panes
                .iter()
                .filter(|pane| pane.workspace_id.is_some() && pane.window_id.is_some())
                .cloned()
                .map(|pane| {
                    let descriptor = state.record.automatic.get(&pane.id).cloned();
                    let cwd = state
                        .record
                        .directories
                        .get(&pane.id)
                        .map(|directory| directory.cwd.clone())
                        .unwrap_or_else(|| pane.cwd.clone());
                    (pane, descriptor, cwd)
                })
                .collect::<Vec<_>>()
        };
        let batch = Batch(self.inner.clone());
        if saved.is_empty() {
            let result = self.spawn(
                SpawnTarget::CreateWorkspace {
                    workspace_id: Uuid::new_v4().to_string(),
                    window_id: Uuid::new_v4().to_string(),
                    name: None,
                    bootstrap: true,
                },
                spec,
            );
            drop(batch);
            self.drain_completions()?;
            return result;
        }
        let mut restored = Vec::new();
        let mut failures = Vec::new();
        for (pane, descriptor, cwd) in saved {
            let result = (|| -> Result<Value> {
                let target = SpawnTarget::Restore {
                    workspace_id: pane
                        .workspace_id
                        .clone()
                        .context("saved workspace missing")?,
                    window_id: pane.window_id.clone().context("saved window missing")?,
                    pane_id: pane.id.clone(),
                };
                let (launch, attempt) = if let Some(descriptor) = descriptor {
                    recovery::validate(&descriptor)?;
                    let launch = self.native_resume_spec(
                        &descriptor,
                        pane.home.clone(),
                        pane.isolated_environment,
                        pane.title.clone(),
                        spec.size,
                    )?;
                    let attempt = recovery::ResumeAttempt {
                        source: pane.id.clone(),
                        expected: descriptor,
                        workspace: None,
                    };
                    (launch, Some(attempt))
                } else {
                    (
                        PaneSpawnSpec {
                            argv: spec.argv.clone(),
                            cwd,
                            home: pane.home.clone(),
                            isolated_environment: pane.isolated_environment,
                            title: Some(pane.title.clone()),
                            size: spec.size,
                        },
                        None,
                    )
                };
                // Never create a deleted CWD, nor silently enter HOME instead.
                ensure!(
                    launch.cwd.is_absolute() && launch.cwd.is_dir(),
                    "saved working directory is missing or unavailable: {:?}",
                    launch.cwd
                );
                let native = attempt.is_some();
                let mut result = self.spawn_inner(target, launch, attempt, None)?;
                if native {
                    result["binding"] = json!("pending_native_binding");
                    result["policy"] = json!("current_native_configuration");
                }
                Ok(result)
            })();
            match result {
                Ok(result) => restored.push(result),
                Err(error) => {
                    failures.push(json!({"pane_id":pane.id,"error":format!("{error:#}")}))
                }
            }
        }
        drop(batch);
        self.drain_completions()?;
        {
            let state = self.inner.state.lock().unwrap();
            healthy(&state)?;
            for id in &state.failed_restores {
                let exit = state
                    .record
                    .registry
                    .panes
                    .iter()
                    .find(|pane| &pane.id == id)
                    .and_then(|pane| pane.exit_code);
                failures.push(json!({"pane_id":id,
                    "error":format!("native resume exited before verified binding (exit {exit:?}); saved intent retained")}));
            }
        }
        Ok(json!({"registry":self.snapshot()?,"automatic_restore":true,
            "restored":restored,"failures":failures}))
    }
}
