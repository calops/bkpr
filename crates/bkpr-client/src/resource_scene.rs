use super::*;

fn validate_roster(manifest: &ImageManifest, keys: &[ImageKey]) -> Result<()> {
    anyhow::ensure!(
        keys.len() == manifest.descriptors().len(),
        "scene descriptor roster differs from metadata image references"
    );
    let mut seen = HashSet::with_capacity(keys.len());
    anyhow::ensure!(
        keys.iter()
            .all(|key| seen.insert(*key) && manifest.descriptor(*key).is_some()),
        "scene descriptor roster differs from metadata image references"
    );
    Ok(())
}

impl Attached {
    /// Apply canonical scene state atomically, admitting declared pending images in progressive mode.
    pub(super) fn scene(&mut self, mut update: scene_wire::DecodedScene) -> Result<()> {
        anyhow::ensure!(
            update.is_progressive() == self.progressive,
            "scene encoding differs from the attachment's explicit resource mode"
        );
        if !self.progressive {
            let retained = self
                .metadata
                .image_refs
                .iter()
                .copied()
                .collect::<HashSet<_>>();
            let incoming = update.image_keys().collect::<HashSet<_>>();
            anyhow::ensure!(
                retained
                    .iter()
                    .all(|key| self.images.contains_key(key) || incoming.contains(key)),
                "missing normalized image generation"
            );
            update.apply(&mut self.revision, &mut self.frame)?;
            let frame = self
                .frame
                .as_mut()
                .expect("a decoded scene supplies a frame");
            if frame.alternate_screen || frame.scrollback_offset == 0 {
                self.history = false;
            }
            for image in frame.images.drain(..) {
                self.images.insert(image.key(), image);
            }
            self.images.retain(|key, _| retained.contains(key));
            frame.images = self
                .metadata
                .image_refs
                .iter()
                .map(|key| {
                    self.images
                        .get(key)
                        .cloned()
                        .expect("resource preflight succeeded")
                })
                .collect();
            self.image_manifest = None;
            return Ok(());
        }

        anyhow::ensure!(
            update.image_keys().next().is_none(),
            "progressive scene supplied inline pixels"
        );
        let mut replacement = if self
            .image_manifest
            .as_ref()
            .is_some_and(|manifest| manifest.descriptors() == update.image_descriptors())
        {
            None
        } else {
            Some(ImageManifest::new(update.take_image_descriptors())?)
        };
        if let Some(manifest) = replacement.as_mut() {
            self.carry_stand_ins(manifest);
        }
        let manifest = replacement
            .as_ref()
            .or(self.image_manifest.as_ref())
            .expect("a progressive scene supplies a manifest");
        validate_roster(manifest, &self.metadata.image_refs)?;
        for descriptor in manifest.descriptors() {
            if let Some(previous) = self
                .image_manifest
                .as_ref()
                .and_then(|previous| previous.descriptor(descriptor.key))
            {
                anyhow::ensure!(
                    previous == descriptor,
                    "immutable scene image descriptor changed"
                );
            }
            if let Some(image) = self.images.get(&descriptor.key) {
                descriptor.validate_image(image)?;
            }
        }
        update.apply(&mut self.revision, &mut self.frame)?;
        if let Some(manifest) = replacement {
            self.image_manifest = Some(manifest);
        }
        let manifest = self.image_manifest.as_ref().unwrap();
        self.images
            .retain(|key, _| manifest.descriptor(*key).is_some());
        let frame = self
            .frame
            .as_mut()
            .expect("a decoded scene supplies a frame");
        if frame.alternate_screen || frame.scrollback_offset == 0 {
            self.history = false;
        }
        frame.images = manifest
            .descriptors()
            .iter()
            .filter_map(|descriptor| self.images.get(&descriptor.key).cloned())
            .collect();
        Ok(())
    }

    /// Keep presenting each replaced image's newest resident (or presented) generation
    /// while its replacement is pending, instead of leaving the placement blank.
    fn carry_stand_ins(&self, manifest: &mut ImageManifest) {
        let pending: Vec<ImageKey> = manifest
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.key)
            .filter(|key| !self.images.contains_key(key))
            .collect();
        for key in pending {
            let resident = self
                .images
                .values()
                .filter(|image| image.id == key.id && image.generation < key.generation);
            let presented = self.image_manifest.iter().flat_map(|previous| {
                previous
                    .descriptors()
                    .iter()
                    .filter(|descriptor| descriptor.key.id == key.id)
                    .filter_map(|descriptor| previous.stand_in(descriptor.key))
            });
            let newest = resident
                .chain(presented)
                .filter(|image| image.generation < key.generation)
                .max_by_key(|image| image.generation)
                .cloned();
            if let Some(image) = newest {
                // Dimension or format changes keep the honest pending presentation.
                let _ = manifest.set_stand_in(key, image);
            }
        }
    }

    /// Install scene and clipboard metadata only after the scene's preflight succeeds.
    pub(super) fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        if self.progressive && snapshot.scene.is_none() {
            if let Some(manifest) = &self.image_manifest {
                validate_roster(manifest, &snapshot.metadata.image_refs)?;
            } else {
                anyhow::ensure!(
                    snapshot.metadata.image_refs.is_empty(),
                    "image references changed without a scene manifest"
                );
            }
        }
        let previous_metadata = std::mem::replace(&mut self.metadata, snapshot.metadata);
        if let Some(scene) = snapshot.scene
            && let Err(error) = self.scene(scene)
        {
            self.metadata = previous_metadata;
            return Err(error);
        }
        self.clipboard_id = self
            .metadata
            .clipboard
            .as_ref()
            .map(|pending| pending.request_id.clone());
        Ok(())
    }

    /// Share newly complete current-view images without advancing the backend scene revision.
    pub(super) fn sync_resources(
        &mut self,
        resources: &crate::resources::Resources,
    ) -> Result<bool> {
        let Some(manifest) = self.image_manifest.as_ref().filter(|_| self.progressive) else {
            return Ok(false);
        };
        if self.frame.is_none() || self.retirement_requested {
            return Ok(false);
        }
        let received = resources.received(&self.metadata.pane_id, &self.view);
        let mut arrived = Vec::new();
        for key in received {
            let Some(descriptor) = manifest.descriptor(key) else {
                continue;
            };
            if self.images.contains_key(&key) {
                continue;
            }
            if let Some(image) = resources.image(&self.metadata.pane_id, key) {
                descriptor.validate_image(&image)?;
                arrived.push((key, image));
            }
        }
        if arrived.is_empty() {
            return Ok(false);
        }
        let manifest = self.image_manifest.as_mut().unwrap();
        for (key, _) in &arrived {
            manifest.clear_stand_in(*key);
        }
        self.images.extend(arrived);
        let frame = self.frame.as_mut().unwrap();
        frame.images.clear();
        frame.images.extend(
            manifest
                .descriptors()
                .iter()
                .filter_map(|descriptor| self.images.get(&descriptor.key).cloned()),
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResourcePhase, Resources};
    use crate::transport::test_wire::{read_request, write_response};
    use std::{os::unix::net::UnixListener, thread};

    fn image(generation: u64) -> Arc<Image> {
        Arc::new(Image {
            id: 7,
            generation,
            width: 1,
            height: 1,
            format: PixelFormat::Rgb,
            pixels: vec![generation as u8, 2, 3],
            source: None,
        })
    }

    fn frame(attached: &Attached, image: Arc<Image>, text: &str) -> Frame {
        let mut frame = attached.frame.as_ref().unwrap().clone();
        frame.rows[0].cells[0].text = text.into();
        frame.cursor.x = 1;
        frame.images = vec![image.clone()];
        frame.placements = vec![bkpr_protocol::Placement {
            image_id: image.id,
            generation: image.generation,
            placement_id: 1,
            col: 0,
            row: 0,
            offset_x: 0,
            offset_y: 0,
            width: 8,
            height: 16,
            source_x: 0,
            source_y: 0,
            source_width: 1,
            source_height: 1,
            layer: ImageLayer::AboveText,
            z: 0,
        }];
        frame
    }

    fn snapshot(attached: &Attached, frame: &Frame, revision: u64) -> Snapshot {
        let mut metadata = attached.metadata.clone();
        metadata.revision = revision;
        metadata.snapshot_revision += 1;
        metadata.image_refs = frame.image_keys();
        let bytes = scene_wire::encode_progressive(frame, revision, None, 0, &[]).unwrap();
        Snapshot {
            metadata,
            scene: Some(scene_wire::decode(&bytes).unwrap()),
        }
    }

    fn until(resources: &mut Resources, phase: ResourcePhase) {
        loop {
            resources.poll().unwrap();
            if resources
                .statuses("focused-pane", "focused-view")
                .iter()
                .any(|status| status.phase == phase)
            {
                return;
            }
            let mut fd = libc::pollfd {
                fd: resources.wake().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: fd is one initialized pollfd retained for the call.
            assert_eq!(
                unsafe { libc::poll(&mut fd, 1, 3000) },
                1,
                "image resource wake timed out"
            );
        }
    }

    fn send(stream: &mut std::os::unix::net::UnixStream, resource: ImageResource) {
        write_response(
            stream,
            &Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(Payload::ImageResource(resource)),
                error: None,
                failure: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn explicit_scene_mode_is_required_even_for_an_empty_image_roster() {
        for progressive in [false, true] {
            for encoded_progressive in [false, true] {
                let (mut app, _capture) = super::super::tests::fixture();
                super::super::tests::attach_fixture(&mut app);
                let attached = app.views.get_mut("focused-pane").unwrap();
                attached.progressive = progressive;
                let before = attached.frame.as_ref().unwrap().clone();
                let before_metadata = serde_json::to_value(&attached.metadata).unwrap();
                let mut next = before.clone();
                next.rows[0].cells[0].text = "E".into();
                let bytes = if encoded_progressive {
                    scene_wire::encode_progressive(&next, 2, None, 0, &[])
                } else {
                    scene_wire::encode(&next, 2, None, 0, &[])
                }
                .unwrap();
                let mut metadata = attached.metadata.clone();
                metadata.revision = 2;
                metadata.snapshot_revision += 1;
                let result = attached.apply(Snapshot {
                    metadata,
                    scene: Some(scene_wire::decode(&bytes).unwrap()),
                });
                if progressive == encoded_progressive {
                    result.unwrap();
                    assert_eq!(attached.frame.as_ref(), Some(&next));
                    assert_eq!(attached.revision, 2);
                    assert_eq!(attached.image_manifest.is_some(), progressive);
                } else {
                    assert!(result.is_err());
                    assert_eq!(attached.frame.as_ref(), Some(&before));
                    assert_eq!(attached.revision, 1);
                    assert_eq!(
                        serde_json::to_value(&attached.metadata).unwrap(),
                        before_metadata
                    );
                    assert!(attached.image_manifest.is_none());
                    assert!(attached.images.is_empty());
                }
            }
        }
    }

    #[test]
    fn pending_scene_applies_text_cursor_and_placements_and_reuses_manifest() {
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        attached.progressive = true;
        let expected = frame(attached, image(1), "λ");
        attached.apply(snapshot(attached, &expected, 2)).unwrap();
        let actual = attached.frame.as_ref().unwrap();
        assert_eq!(actual.rows, expected.rows);
        assert_eq!(actual.cursor, expected.cursor);
        assert_eq!(actual.placements, expected.placements);
        assert!(actual.images.is_empty());
        assert!(attached.images.is_empty());
        let descriptors = attached
            .image_manifest
            .as_ref()
            .unwrap()
            .descriptors()
            .as_ptr();
        let mut changed = expected.clone();
        changed.rows[0].cells[0].text = "N".into();
        let mut update = snapshot(attached, &changed, 3);
        let baseline = (2, Arc::new(expected));
        let bytes = scene_wire::encode_progressive(&changed, 3, Some(&baseline), 2, &[]).unwrap();
        update.scene = Some(scene_wire::decode(&bytes).unwrap());
        attached.apply(update).unwrap();
        assert_eq!(attached.revision, 3);
        assert_eq!(attached.frame.as_ref().unwrap().rows, changed.rows);
        assert_eq!(
            attached
                .image_manifest
                .as_ref()
                .unwrap()
                .descriptors()
                .as_ptr(),
            descriptors
        );
    }

    #[test]
    fn rejected_manifest_or_baseline_keeps_all_scene_state_unchanged() {
        for invalid in 0..4 {
            let (mut app, _capture) = super::super::tests::fixture();
            super::super::tests::attach_fixture(&mut app);
            let attached = app.views.get_mut("focused-pane").unwrap();
            attached.progressive = true;
            let initial = frame(attached, image(1), "A");
            attached.apply(snapshot(attached, &initial, 2)).unwrap();
            attached.images.insert(image(1).key(), image(1));
            attached.frame.as_mut().unwrap().images.push(image(1));
            attached.clipboard_id = Some("untouched".into());
            attached.history = true;
            let before = attached.frame.as_ref().unwrap().clone();
            let metadata = serde_json::to_value(&attached.metadata).unwrap();
            let manifest = attached
                .image_manifest
                .as_ref()
                .unwrap()
                .descriptors()
                .as_ptr();
            let mut next = frame(attached, image(1), "B");
            let mut update = snapshot(attached, &next, 3);
            match invalid {
                0 => update.metadata.image_refs.clear(),
                1 => update.metadata.image_refs.push(image(1).key()),
                2 => {
                    next.images = vec![Arc::new(Image {
                        width: 2,
                        pixels: vec![9; 6],
                        ..(*image(1)).clone()
                    })];
                    update = snapshot(attached, &next, 3);
                }
                _ => {
                    let baseline = (1, Arc::new(initial));
                    let bytes =
                        scene_wire::encode_progressive(&next, 3, Some(&baseline), 1, &[]).unwrap();
                    update.scene = Some(scene_wire::decode(&bytes).unwrap());
                }
            }
            assert!(attached.apply(update).is_err());
            assert_eq!(serde_json::to_value(&attached.metadata).unwrap(), metadata);
            assert_eq!(attached.frame.as_ref(), Some(&before));
            assert_eq!(attached.revision, 2);
            assert_eq!(attached.images.len(), 1);
            assert_eq!(attached.images[&image(1).key()].pixels, image(1).pixels);
            assert_eq!(
                attached
                    .image_manifest
                    .as_ref()
                    .unwrap()
                    .descriptors()
                    .as_ptr(),
                manifest
            );
            assert_eq!(attached.clipboard_id.as_deref(), Some("untouched"));
            assert!(attached.history);
        }
    }

    #[test]
    fn complete_resources_arrive_without_scene_revision_and_replacements_never_reuse_old_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("scene-resources.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (finish, finish_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            for generation in 1..=2 {
                let expected = image(generation);
                let descriptor = ImageDescriptor::from_image(&expected).unwrap();
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                assert!(matches!(read_request(&mut stream).unwrap().operation,
                    Operation::Pane { pane_id, request: PaneRequest::ImageResource { view_id, key, .. } }
                    if pane_id == "focused-pane" && view_id == "focused-view" && key == descriptor.key));
                send(
                    &mut stream,
                    ImageResource::Start {
                        descriptor,
                        encoding: ImageEncoding::Raw,
                        encoded_len: 3,
                    },
                );
                send(
                    &mut stream,
                    ImageResource::Data {
                        key: descriptor.key,
                        offset: 0,
                        data: expected.pixels.clone().into(),
                    },
                );
                finish_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                send(
                    &mut stream,
                    ImageResource::End {
                        key: descriptor.key,
                        encoded_len: 3,
                    },
                );
            }
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        attached.progressive = true;
        for generation in 1..=2 {
            let expected = frame(
                attached,
                image(generation),
                if generation == 1 { "A" } else { "B" },
            );
            attached
                .apply(snapshot(attached, &expected, generation + 1))
                .unwrap();
            assert!(attached.images.is_empty());
            assert!(attached.frame.as_ref().unwrap().images.is_empty());
            let descriptors = attached.image_manifest.as_ref().unwrap().descriptors();
            resources
                .sync_view("focused-pane", "focused-view", descriptors, |_| None)
                .unwrap();
            until(&mut resources, ResourcePhase::Receiving);
            assert!(
                resources
                    .received("focused-pane", "focused-view")
                    .is_empty()
            );
            assert!(!attached.sync_resources(&resources).unwrap());
            let revision = attached.revision;
            let cursor = attached.frame.as_ref().unwrap().cursor.clone();
            let rows = attached.frame.as_ref().unwrap().rows.as_ptr();
            finish.send(()).unwrap();
            until(&mut resources, ResourcePhase::Ready);
            assert!(attached.sync_resources(&resources).unwrap());
            let retained = resources
                .image("focused-pane", image(generation).key())
                .unwrap();
            let actual = attached.frame.as_ref().unwrap();
            assert!(Arc::ptr_eq(&actual.images[0], &retained));
            assert!(Arc::ptr_eq(&attached.images[&retained.key()], &retained));
            assert_eq!(actual.rows.as_ptr(), rows);
            assert_eq!(actual.cursor, cursor);
            assert_eq!(attached.revision, revision);
            assert_eq!(
                resources.received("focused-pane", "focused-view"),
                [retained.key()]
            );
            assert_eq!(actual.images[0].pixels, image(generation).pixels);
            assert!(!attached.sync_resources(&resources).unwrap());
        }
        server.join().unwrap();
        resources.retire_view("focused-pane", "focused-view");
        assert!(
            resources
                .received("focused-pane", "focused-view")
                .is_empty()
        );
        attached.images.clear();
        attached.frame.as_mut().unwrap().images.clear();
        assert!(!attached.sync_resources(&resources).unwrap());
        assert!(attached.frame.as_ref().unwrap().images.is_empty());
    }

    #[test]
    fn resource_failure_leaves_valid_text_and_revision_usable() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("failed-resource.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream).unwrap();
            let descriptor = ImageDescriptor::from_image(&image(1)).unwrap();
            send(
                &mut stream,
                ImageResource::Start {
                    descriptor,
                    encoding: ImageEncoding::Raw,
                    encoded_len: 3,
                },
            );
        });
        let mut resources = Resources::new(Endpoint::unmanaged(&socket)).unwrap();
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        attached.progressive = true;
        let expected = frame(attached, image(1), "λ");
        attached.apply(snapshot(attached, &expected, 2)).unwrap();
        resources
            .sync_view(
                "focused-pane",
                "focused-view",
                attached.image_manifest.as_ref().unwrap().descriptors(),
                |_| None,
            )
            .unwrap();
        until(&mut resources, ResourcePhase::Failed);
        assert!(!attached.sync_resources(&resources).unwrap());
        assert_eq!(attached.revision, 2);
        assert_eq!(attached.frame.as_ref().unwrap().rows, expected.rows);
        assert_eq!(attached.frame.as_ref().unwrap().cursor, expected.cursor);
        assert!(
            resources
                .received("focused-pane", "focused-view")
                .is_empty()
        );
        server.join().unwrap();
    }
}
