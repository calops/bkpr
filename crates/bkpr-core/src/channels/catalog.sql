CREATE TABLE catalog_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    store_id TEXT NOT NULL,
    actor_id TEXT
) STRICT;
INSERT INTO catalog_identity(singleton,store_id) VALUES(1,lower(hex(randomblob(16))));

CREATE TABLE catalog_workspaces (
    id TEXT PRIMARY KEY
) WITHOUT ROWID, STRICT;

-- A before-image (NULL sequence for a creation), or a workspace tombstone
-- (NULL channel). Never retain membership, bodies or notification work here.
CREATE TABLE catalog_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL,
    channel_id TEXT,
    old_workspace_wide INTEGER,
    old_settings TEXT,
    old_last_sequence INTEGER,
    old_last_attention INTEGER,
    old_through INTEGER
) STRICT;
CREATE INDEX catalog_channel_events ON catalog_events(channel_id,sequence);

CREATE TRIGGER catalog_limit AFTER INSERT ON catalog_events BEGIN
    DELETE FROM catalog_events WHERE sequence <= NEW.sequence - 4096;
END;

CREATE TRIGGER catalog_channel_created AFTER INSERT ON channels BEGIN
    INSERT INTO catalog_events(workspace_id,channel_id) VALUES(NEW.workspace_id,NEW.id);
END;

CREATE TRIGGER catalog_channel_updated
AFTER UPDATE OF workspace_wide,settings,last_sequence,last_attention ON channels
WHEN OLD.workspace_wide != NEW.workspace_wide OR OLD.settings != NEW.settings
  OR OLD.last_sequence != NEW.last_sequence OR OLD.last_attention != NEW.last_attention
BEGIN
    INSERT INTO catalog_events(workspace_id,channel_id,old_workspace_wide,old_settings,
                               old_last_sequence,old_last_attention,old_through)
    VALUES(OLD.workspace_id,OLD.id,OLD.workspace_wide,OLD.settings,OLD.last_sequence,OLD.last_attention,
           COALESCE((SELECT p.through FROM positions p JOIN catalog_identity i ON p.participant_id=i.actor_id
                     WHERE p.channel_id=OLD.id),0));
END;

CREATE TRIGGER catalog_workspace_closed AFTER DELETE ON catalog_workspaces BEGIN
    INSERT INTO catalog_events(workspace_id) VALUES(OLD.id);
END;
