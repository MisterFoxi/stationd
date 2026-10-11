-- Preserve the last positive audience observation when broadcast state changes.
ALTER TABLE broadcast_state ADD COLUMN last_listener_at INTEGER;
