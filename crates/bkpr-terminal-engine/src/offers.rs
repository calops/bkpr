use super::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub const PASTE_OFFER_TIMEOUT: Duration = Duration::from_secs(30);
pub const PASTE_OFFER_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const PASTE_OFFER_MAX_COUNT: usize = 8;

pub(super) struct PasteOffer {
    pub created: Instant,
    pub location: ClipboardLocation,
    pub contents: Vec<ClipboardContent>,
}
impl PasteOffer {
    pub fn expired(&self) -> bool {
        self.created.elapsed() >= PASTE_OFFER_TIMEOUT
    }
    pub fn bytes(&self) -> usize {
        self.contents.iter().fold(0usize, |n, c| {
            n.saturating_add(c.mime.len()).saturating_add(c.data.len())
        })
    }
}
impl CallbackState {
    pub(super) fn prune_paste_offers(&mut self) {
        self.paste_offers.retain(|_, offer| !offer.expired());
    }
    pub(super) fn paste_offer(&mut self, id: u64, consume: bool) -> Option<Arc<PasteOffer>> {
        self.prune_paste_offers();
        if consume {
            self.paste_offers.remove(&id)
        } else {
            self.paste_offers.get(&id).cloned()
        }
    }
}
impl Terminal {
    /// Revoke bytes for a stale/disconnected originating view. A subsequent
    /// valid native password still correlates to the missing ID and is denied;
    /// it can never fall through to the current physical clipboard.
    pub fn cancel_paste_offer(&mut self, id: u64) -> bool {
        self.callbacks
            .borrow_mut()
            .paste_offers
            .remove(&id)
            .is_some()
    }
}

impl Terminal {
    /// Whether mode 5522 retained this offer and it is still within its lifetime.
    pub fn has_paste_offer(&self, id: u64) -> bool {
        self.callbacks
            .borrow()
            .paste_offers
            .get(&id)
            .is_some_and(|offer| !offer.expired())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn expired_offer_denies_its_valid_native_password_without_external_read() {
        let mut terminal = Terminal::new(Size {
            cols: 20,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        })
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        terminal.set_clipboard_handler(Box::new(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            ClipboardResponse {
                result: ClipboardResult::Success,
                contents: vec![],
                available: vec![],
                remember: false,
            }
        }));
        terminal.feed(b"\x1b[?5522h").unwrap();
        let event = String::from_utf8(
            terminal
                .paste_contents(
                    1,
                    vec![ClipboardContent {
                        mime: "image/png".into(),
                        data: vec![1, 2, 3],
                    }],
                    ClipboardLocation::Standard,
                    false,
                )
                .unwrap(),
        )
        .unwrap();
        let password = event
            .split(":pw=")
            .nth(1)
            .unwrap()
            .split([':', ';', '\x1b', '\x07'])
            .next()
            .unwrap();
        {
            let mut state = terminal.callbacks.borrow_mut();
            let offer = Arc::get_mut(state.paste_offers.get_mut(&1).unwrap()).unwrap();
            offer.created = Instant::now()
                .checked_sub(PASTE_OFFER_TIMEOUT)
                .expect("monotonic clock supports offer lifetime");
        }
        assert!(!terminal.has_paste_offer(1));
        let request =
            format!("\x1b]5522;type=read:id=expired:name=YXBw:pw={password};aW1hZ2UvcG5n\x1b\\");
        let response: Vec<u8> = terminal
            .feed(request.as_bytes())
            .unwrap()
            .into_iter()
            .filter_map(|e| {
                if let Effect::PtyWrite(bytes) = e {
                    Some(bytes)
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        assert!(
            String::from_utf8(response)
                .unwrap()
                .contains("status=EPERM")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
