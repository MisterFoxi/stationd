-- Keep a randomly selected track quota stable across turns and restarts.
ALTER TABLE group_state ADD COLUMN random_take INTEGER
    CHECK (random_take IS NULL OR random_take BETWEEN 1 AND 4294967295);
