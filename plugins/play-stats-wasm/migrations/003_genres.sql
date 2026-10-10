-- Unknown historical genres stay empty; no invented snapshot of past tags.
ALTER TABLE media_play ADD COLUMN genres_json TEXT NOT NULL DEFAULT '[]';
