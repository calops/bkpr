-- The marker classifies retained history. No holder, launch, grant or membership is restored.
CREATE TABLE role_threads (
    channel_id TEXT PRIMARY KEY REFERENCES channels(id),
    archive_reported INTEGER NOT NULL DEFAULT 0 CHECK(archive_reported IN (0,1))
) WITHOUT ROWID, STRICT;
CREATE INDEX role_threads_pending_archive ON role_threads(channel_id) WHERE archive_reported=0;

ALTER TABLE catalog_events ADD COLUMN role_archive INTEGER NOT NULL DEFAULT 0;
CREATE TRIGGER catalog_role_thread_archived AFTER UPDATE OF archive_reported ON role_threads
WHEN OLD.archive_reported=0 AND NEW.archive_reported=1
BEGIN
    INSERT INTO catalog_events(workspace_id,channel_id,old_workspace_wide,old_settings,
                               old_last_sequence,old_last_attention,old_through,role_archive)
    SELECT c.workspace_id,c.id,c.workspace_wide,c.settings,c.last_sequence,c.last_attention,
           COALESCE((SELECT p.through FROM positions p JOIN catalog_identity i ON p.participant_id=i.actor_id
                     WHERE p.channel_id=c.id),0),1
    FROM channels c WHERE c.id=NEW.channel_id;
    UPDATE notification_work SET pending=0
    WHERE channel_id=NEW.channel_id AND launch_id IS NOT NULL AND pending=1;
    UPDATE channel_batches SET revoked=1
    WHERE channel_id=NEW.channel_id AND revoked=0;
END;
