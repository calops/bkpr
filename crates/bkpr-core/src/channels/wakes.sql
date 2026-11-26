CREATE TABLE channel_batches (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL REFERENCES channels(id),
    participant_id TEXT NOT NULL,
    launch_id TEXT NOT NULL,
    binding_revision INTEGER NOT NULL,
    receiver_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    pending_count INTEGER NOT NULL CHECK(pending_count BETWEEN 1 AND 32),
    addressed_message_id TEXT REFERENCES posts(id),
    deadline INTEGER NOT NULL,
    outcome TEXT CHECK(outcome IN ('accepted','failed','unknown','revoked')),
    reason TEXT
) STRICT;
ALTER TABLE notification_work ADD COLUMN binding_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notification_work ADD COLUMN batch_id TEXT REFERENCES channel_batches(id);
CREATE INDEX claimed_wake_items ON notification_work(batch_id) WHERE batch_id IS NOT NULL;
