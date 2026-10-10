-- V1 play_count records selections with no dated history. Keep it unchanged.
CREATE TABLE media_play (
 play_id INTEGER PRIMARY KEY,
 media_key TEXT NOT NULL,
 media_path TEXT NOT NULL,
 title TEXT, artist TEXT, album TEXT, playlist_ref TEXT,
 started_at INTEGER NOT NULL CHECK(started_at >= 0),
 ended_at INTEGER CHECK(ended_at >= started_at),
 aired_seconds INTEGER CHECK(aired_seconds >= 0),
 played_to_end INTEGER CHECK(played_to_end IN (0, 1))
);
CREATE INDEX media_play_started ON media_play(started_at);
CREATE INDEX media_play_key_started ON media_play(media_key, started_at);
CREATE TABLE media_audience (
 play_id INTEGER NOT NULL, at INTEGER NOT NULL, listeners INTEGER NOT NULL CHECK(listeners >= 0),
 PRIMARY KEY(play_id, at)
);
CREATE INDEX media_audience_at ON media_audience(at);
CREATE TABLE media_active (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), play_id INTEGER, started_at INTEGER);
INSERT INTO media_active VALUES (1, NULL, NULL);
