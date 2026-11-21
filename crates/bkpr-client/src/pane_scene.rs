use super::*;
use std::collections::VecDeque;

/// Recently unreferenced generations kept per view so a reappearing one needs no resend,
/// bounded by count and by their decoded plus original bytes.
const RECENT_IMAGES: usize = 8;
const RECENT_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// A view's image generations, and what its poll worker reports to the backend as held.
pub(super) struct ViewImages {
    /// Generations the current scene references.
    live: HashMap<ImageKey, Arc<Image>>,
    /// Recently unreferenced generations, newest first; reported as held.
    recent: VecDeque<Arc<Image>>,
    recent_bytes: usize,
    /// Generations dropped from `recent` while applying snapshot `.0`. A poll sent before that
    /// apply may still have reported them, so the backend may omit them from its snapshot.
    evicted: Vec<(u64, Arc<Image>)>,
    held: Arc<updates::HeldImages>,
}

fn residency(image: &Image) -> usize {
    image.pixels.len() + image.source.as_ref().map_or(0, |source| source.bytes.len())
}

impl ViewImages {
    pub(super) fn new(held: Arc<updates::HeldImages>) -> Self {
        Self {
            live: HashMap::new(),
            recent: VecDeque::new(),
            recent_bytes: 0,
            evicted: Vec::new(),
            held,
        }
    }

    /// Release every generation, as when the view's frame is discarded.
    pub(super) fn clear(&mut self) {
        self.live.clear();
        self.recent.clear();
        self.recent_bytes = 0;
        self.evicted.clear();
    }

    /// Make an unreferenced generation live again, if it is still retained.
    fn restore(&mut self, key: ImageKey) {
        if self.live.contains_key(&key) {
            return;
        }
        let image = if let Some(index) = self.recent.iter().position(|image| image.key() == key) {
            let image = self.recent.remove(index).unwrap();
            self.recent_bytes -= residency(&image);
            image
        } else if let Some(index) = self
            .evicted
            .iter()
            .position(|(_, image)| image.key() == key)
        {
            self.evicted.swap_remove(index).1
        } else {
            return;
        };
        self.live.insert(key, image);
    }

    /// After applying snapshot `sequence`, keep only `referenced` generations live, move the
    /// rest to `recent`, and publish what the next poll may report.
    fn settle(&mut self, referenced: &HashSet<ImageKey>, sequence: u64, held_as_of: u64) {
        for (_, image) in self.live.extract_if(|key, _| !referenced.contains(key)) {
            self.recent_bytes += residency(&image);
            self.recent.push_front(image);
        }
        while self.recent.len() > RECENT_IMAGES || self.recent_bytes > RECENT_IMAGE_BYTES {
            let image = self.recent.pop_back().unwrap();
            self.recent_bytes -= residency(&image);
            self.evicted.push((sequence, image));
        }
        // Polls are applied in order: once a snapshot's poll read a publish made after an
        // eviction, no later poll reports that generation.
        self.evicted
            .retain(|(evicted_in, _)| *evicted_in > held_as_of);
        self.held.publish(
            sequence,
            self.live
                .keys()
                .copied()
                .chain(self.recent.iter().map(|image| image.key()))
                .collect(),
        );
    }
}

impl Attached {
    /// Apply a canonical inline scene atomically. Every referenced image generation must be
    /// retained already or supplied by this scene; the backend omits those reported held.
    fn scene(
        &mut self,
        update: scene_wire::DecodedScene,
        sequence: u64,
        held_as_of: u64,
    ) -> Result<()> {
        let referenced = self
            .metadata
            .image_refs
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        // A generation dropped recently may be referenced again (scrollback, screens).
        for key in &referenced {
            self.images.restore(*key);
        }
        let incoming = update.image_keys().collect::<HashSet<_>>();
        anyhow::ensure!(
            referenced
                .iter()
                .all(|key| self.images.live.contains_key(key) || incoming.contains(key)),
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
            self.images.live.insert(image.key(), image);
        }
        self.images.settle(&referenced, sequence, held_as_of);
        frame.images = self
            .metadata
            .image_refs
            .iter()
            .map(|key| {
                self.images
                    .live
                    .get(key)
                    .cloned()
                    .expect("image preflight succeeded")
            })
            .collect();
        Ok(())
    }

    /// Install scene and clipboard metadata, and the scene's image payloads, only after the
    /// scene's preflight succeeds.
    pub(super) fn apply(&mut self, snapshot: Snapshot) -> Result<()> {
        let previous_metadata = std::mem::replace(&mut self.metadata, snapshot.metadata);
        if let Some(scene) = snapshot.scene
            && let Err(error) = self.scene(scene, snapshot.sequence, snapshot.held_as_of)
        {
            self.metadata = previous_metadata;
            return Err(error);
        }
        self.transports.extend(snapshot.transports);
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

    /// The backend's snapshot for a poll that reported `held` after `held_as_of` was applied.
    fn snapshot(
        attached: &Attached,
        frame: &Frame,
        revision: u64,
        (held_as_of, held): (u64, Vec<ImageKey>),
    ) -> Snapshot {
        let mut metadata = attached.metadata.clone();
        metadata.revision = revision;
        metadata.snapshot_revision += 1;
        metadata.image_refs = frame.image_keys();
        let bytes = scene_wire::encode(frame, revision, None, 0, &held).unwrap();
        Snapshot {
            metadata,
            scene: Some(scene_wire::decode(&bytes).unwrap()),
            transports: Vec::new(),
            sequence: revision,
            held_as_of,
        }
    }

    #[test]
    fn held_and_recently_dropped_generations_are_reused_without_resend() {
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        let shown = frame(attached, Some(image(1)));
        let held = attached.images.held.read();
        attached.apply(snapshot(attached, &shown, 2, held)).unwrap();
        assert_eq!(attached.images.held.read(), (2, vec![image(1).key()]));

        // The image disappears, then returns: the backend omits it (still held), and the
        // client restores it from its recent set instead of failing the scene.
        let hidden = frame(attached, None);
        let held = attached.images.held.read();
        attached
            .apply(snapshot(attached, &hidden, 3, held))
            .unwrap();
        assert!(attached.images.live.is_empty());
        assert_eq!(attached.images.held.read(), (3, vec![image(1).key()]));
        let again = frame(attached, Some(image(1)));
        let update = snapshot(attached, &again, 4, attached.images.held.read());
        assert_eq!(update.scene.as_ref().unwrap().images().len(), 0);
        attached.apply(update).unwrap();
        assert_eq!(
            attached.frame.as_ref().unwrap().images[0].pixels,
            image(1).pixels
        );

        // A generation neither held nor supplied fails without replacing the frame.
        attached.images.clear();
        let missing = snapshot(attached, &again, 5, (4, vec![image(1).key()]));
        assert!(attached.apply(missing).is_err());
        assert_eq!(attached.revision, 4);
    }

    #[test]
    fn a_generation_evicted_after_its_poll_reported_it_held_is_still_restored() {
        // Each poll is sent before the UI applies the previous snapshot, as the worker does,
        // so it reports the held set from two applies earlier plus what it just delivered.
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        let mut stale = attached.images.held.read();
        let mut delivered = None::<ImageKey>;
        // Cycle through more generations than `recent` keeps, then show the oldest again.
        let shown = (1..=RECENT_IMAGES as u64 + 2).chain([1, 2, 3]);
        for (revision, generation) in (2..).zip(shown) {
            let (applied, mut held) = stale.clone();
            held.extend(delivered);
            let scene = frame(attached, Some(image(generation)));
            let update = snapshot(attached, &scene, revision, (applied, held));
            delivered = update.scene.as_ref().unwrap().image_keys().next();
            stale = attached.images.held.read();
            attached.apply(update).unwrap_or_else(|error| {
                panic!("generation {generation} at revision {revision}: {error}")
            });
            assert_eq!(
                attached.frame.as_ref().unwrap().images[0].pixels,
                image(generation).pixels
            );
        }
        assert!(attached.images.recent.len() <= RECENT_IMAGES);
        assert!(attached.images.evicted.len() <= 2);
    }

    #[test]
    fn recently_dropped_generations_are_bounded_by_bytes() {
        let (mut app, _capture) = super::super::tests::fixture();
        super::super::tests::attach_fixture(&mut app);
        let attached = app.views.get_mut("focused-pane").unwrap();
        let large = |generation| {
            Arc::new(Image {
                id: 9,
                generation,
                width: 2048,
                height: 2048,
                format: PixelFormat::Rgba,
                pixels: vec![generation as u8; 2048 * 2048 * 4],
                source: None,
            })
        };
        for generation in 1..=6 {
            let mut scene = frame(attached, Some(large(generation)));
            scene.placements[0].image_id = 9;
            scene.placements[0].generation = generation;
            scene.placements[0].source_width = 2048;
            scene.placements[0].source_height = 2048;
            let held = attached.images.held.read();
            let revision = attached.revision + 1;
            attached
                .apply(snapshot(attached, &scene, revision, held))
                .unwrap();
        }
        assert!(attached.images.recent_bytes <= RECENT_IMAGE_BYTES);
        assert_eq!(attached.images.recent.len(), 4);
    }
}
