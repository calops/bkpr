CREATE TABLE direct_channels (
    channel_id TEXT PRIMARY KEY REFERENCES channels(id),
    first_participant TEXT NOT NULL,
    second_participant TEXT NOT NULL,
    display_name TEXT NOT NULL,
    CHECK(first_participant < second_participant),
    UNIQUE(first_participant,second_participant)
) STRICT;
CREATE INDEX direct_channels_by_first ON direct_channels(first_participant,channel_id);
CREATE INDEX direct_channels_by_second ON direct_channels(second_participant,channel_id);
