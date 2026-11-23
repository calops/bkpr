CREATE TABLE channels (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    workspace_wide INTEGER NOT NULL CHECK(workspace_wide IN (0,1)),
    settings TEXT NOT NULL,
    last_sequence INTEGER NOT NULL DEFAULT 0,
    change_sequence INTEGER NOT NULL DEFAULT 0,
    last_attention INTEGER NOT NULL DEFAULT 0,
    UNIQUE(workspace_id,name)
) STRICT;
CREATE INDEX channel_workspace ON channels(workspace_id,id);
CREATE INDEX workspace_wide_channels ON channels(workspace_id,id) WHERE workspace_wide=1;
CREATE TABLE members (
    channel_id TEXT NOT NULL REFERENCES channels(id),
    participant_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    PRIMARY KEY(channel_id,participant_id)
) WITHOUT ROWID, STRICT;
CREATE INDEX visible_explicit_channels ON members(workspace_id,participant_id,channel_id);
CREATE TABLE posts (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL REFERENCES channels(id),
    sequence INTEGER NOT NULL,
    author_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL,
    summary TEXT NOT NULL,
    body TEXT NOT NULL,
    wake_count INTEGER NOT NULL,
    attention INTEGER NOT NULL CHECK(attention IN (0,1)),
    UNIQUE(channel_id,sequence),
    UNIQUE(author_id,request_key)
) STRICT;
CREATE TABLE positions (
    channel_id TEXT NOT NULL REFERENCES channels(id),
    participant_id TEXT NOT NULL,
    through INTEGER NOT NULL,
    PRIMARY KEY(channel_id,participant_id)
) WITHOUT ROWID, STRICT;
CREATE TABLE changes (
    channel_id TEXT NOT NULL REFERENCES channels(id),
    sequence INTEGER NOT NULL,
    kind INTEGER NOT NULL CHECK(kind BETWEEN 0 AND 3),
    participant_id TEXT,
    PRIMARY KEY(channel_id,sequence)
) WITHOUT ROWID, STRICT;
CREATE INDEX changes_visibility ON changes(channel_id,participant_id,sequence);
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL REFERENCES channels(id),
    descriptor TEXT NOT NULL,
    bytes BLOB NOT NULL CHECK(length(bytes) BETWEEN 1 AND 4194304)
) STRICT;
CREATE TABLE notification_work (
    channel_id TEXT NOT NULL REFERENCES channels(id),
    message_id TEXT NOT NULL REFERENCES posts(id),
    participant_id TEXT NOT NULL,
    launch_id TEXT,
    addressed INTEGER NOT NULL CHECK(addressed IN (0,1)),
    pending INTEGER NOT NULL DEFAULT 1 CHECK(pending IN (0,1)),
    PRIMARY KEY(message_id,participant_id),
    CHECK((launch_id IS NULL) = (participant_id = ''))
) WITHOUT ROWID, STRICT;
CREATE INDEX pending_notifications ON notification_work(channel_id,participant_id) WHERE pending=1;
