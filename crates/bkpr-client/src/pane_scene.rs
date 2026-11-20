use super::*;

/// Recently unreferenced generations retained per view; bounds memory while avoiding resends.
const RECENT_IMAGES: usize = 8;

impl Attached {
    /// Apply a canonical inline scene atomically. Every referenced image generation must be
    /// retained already or supplied by this scene; the backend omits generations in `held`.
    pub(super) fn scene(&mut self, update: scene_wire::DecodedScene) -> Result<()> {
        let retained = self
            .metadata
            .image_refs
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        // A generation dropped recently may be referenced again (scrollback, screens).
        for key in &retained {
            if !self.images.contains_key(key)
                && let Some(index) = self.recent.iter().position(|image| image.key() == *key)
            {
                let image = self.recent.remove(index).unwrap();
                self.images.insert(*key, image);
            }
        }
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
        let dropped: Vec<_> = self
            .images
            .extract_if(|key, _| !retained.contains(key))
            .map(|(_, image)| image)
            .collect();
        for image in dropped {
            self.recent.push_front(image);
        }
        self.recent.truncate(RECENT_IMAGES);
        *self.held.lock().unwrap() = self
            .images
            .keys()
            .copied()
            .chain(self.recent.iter().map(|image| image.key()))
            .collect();
        frame.images = self
            .metadata
            .image_refs
            .iter()
            .map(|key| {
                self.images
                    .get(key)
                    .cloned()
                    .expect("image preflight succeeded")
            })
            .collect();
        Ok(())
    }

    /// Install scene and clipboard metadata only after the scene's preflight succeeds.
    pub(super) fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        self.transports.extend(snapshot.transports);
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn frame(attached: &Attached, image: Option<Arc<Image>>) -> Frame {
        let mut frame = attached.frame.as_ref().unwrap().clone();
        frame.images = image.iter().cloned().collect();
        frame.placements = image
            .iter()
            .map(|image| bkpr_protocol::Placement {
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
            })
            .collect();
        frame
    }

    fn snapshot(attached: &Attached, frame: &Frame, revision: u64) -> Snapshot {
        let mut metadata = attached.metadata.clone();
        metadata.revision = revision;
        metadata.snapshot_revision += 1;
        metadata.image_refs = frame.image_keys();
        let held = attached.held.lock().unwrap().clone();
        let bytes = scene_wire::encode(frame, revision, None, 0, &held).unwrap();
        Snapshot {
            metadata,
            scene: Some(scene_wire::decode(&bytes).unwrap()),
            transports: Vec::new(),
        }
    }

    #[test]
    fn held_and_recently_dropped_generations_are_reused_without_resend() {
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        let shown = frame(attached, Some(image(1)));
        attached.apply(snapshot(attached, &shown, 2)).unwrap();
        assert_eq!(*attached.held.lock().unwrap(), [image(1).key()]);

        // The image disappears, then returns: the backend omits it (still held), and the
        // client restores it from its recent set instead of failing the scene.
        let hidden = frame(attached, None);
        attached.apply(snapshot(attached, &hidden, 3)).unwrap();
        assert!(attached.images.is_empty());
        assert_eq!(*attached.held.lock().unwrap(), [image(1).key()]);
        let again = frame(attached, Some(image(1)));
        let update = snapshot(attached, &again, 4);
        assert_eq!(update.scene.as_ref().unwrap().images().len(), 0);
        attached.apply(update).unwrap();
        assert_eq!(
            attached.frame.as_ref().unwrap().images[0].pixels,
            image(1).pixels
        );

        // A generation neither held nor supplied fails without replacing the frame.
        attached.held.lock().unwrap().clear();
        attached.images.clear();
        attached.recent.clear();
        let mut missing = snapshot(attached, &again, 5);
        missing.scene = Some(
            scene_wire::decode(&scene_wire::encode(&again, 5, None, 0, &[image(1).key()]).unwrap())
                .unwrap(),
        );
        assert!(attached.apply(missing).is_err());
        assert_eq!(attached.revision, 4);
    }
}
