ALTER TABLE catalog_events ADD COLUMN attention_message_id TEXT;
ALTER TABLE catalog_events ADD COLUMN attention_sequence INTEGER;
ALTER TABLE catalog_events ADD COLUMN attention_intent TEXT;
ALTER TABLE catalog_events ADD COLUMN attention_posted_at_ms INTEGER;

-- The post already exists when channels.last_sequence advances in its transaction.
DROP TRIGGER catalog_channel_updated;
CREATE TRIGGER catalog_channel_updated
AFTER UPDATE OF workspace_wide,settings,last_sequence,last_attention ON channels
WHEN OLD.workspace_wide != NEW.workspace_wide OR OLD.settings != NEW.settings
  OR OLD.last_sequence != NEW.last_sequence OR OLD.last_attention != NEW.last_attention
BEGIN
    INSERT INTO catalog_events(workspace_id,channel_id,old_workspace_wide,old_settings,
                               old_last_sequence,old_last_attention,old_through,
                               attention_message_id,attention_sequence,attention_intent,attention_posted_at_ms)
    SELECT OLD.workspace_id,OLD.id,OLD.workspace_wide,OLD.settings,OLD.last_sequence,OLD.last_attention,
           COALESCE((SELECT p.through FROM positions p JOIN catalog_identity i ON p.participant_id=i.actor_id
                     WHERE p.channel_id=OLD.id),0),
           marked.id,marked.sequence,json_extract(marked.summary,'$.intent'),
           json_extract(marked.summary,'$.posted_at_ms')
    FROM (SELECT 1) LEFT JOIN posts marked
      ON OLD.last_sequence != NEW.last_sequence AND OLD.last_attention != NEW.last_attention
      AND NEW.last_attention = NEW.last_sequence
      AND marked.channel_id = NEW.id AND marked.sequence = NEW.last_sequence AND marked.attention = 1;
END;

-- No receiver could claim the legacy human placeholder, and it is not a catalog event.
DELETE FROM notification_work WHERE launch_id IS NULL;
