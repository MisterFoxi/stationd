//! Pure ingestion plan; no guest/host calls, and no counts of mere selections.
use serde_json::{Value, json};
pub const LEGACY_MIGRATION: &str = "CREATE TABLE play_count (
             media   TEXT    PRIMARY KEY,
             plays   INTEGER NOT NULL,
             last_at INTEGER NOT NULL
         )";
fn statement(sql: &str, params: Value) -> Value {
    json!({"sql":sql, "params":params})
}
fn integer(value: &Value, key: &str) -> Result<i64, String> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .ok_or_else(|| format!("invalid {key}"))
}
fn text(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("invalid {key}"))
}
fn optional(value: &Value, key: &str) -> Result<Value, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(s)) => Ok(json!(s)),
        _ => Err(format!("invalid {key}")),
    }
}
pub fn statements(event: &Value, retention_days: u32) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let (kind, body) = if let Some(v) = event.get("TrackStarted") {
        ("start", v)
    } else if let Some(v) = event.get("TrackFinished") {
        ("finish", v)
    } else if let Some(v) = event.get("ListenersSampled") {
        let at = integer(v, "at")?;
        let count = integer(v, "count")?;
        out.push(statement("INSERT OR IGNORE INTO media_audience (play_id, at, listeners)
                SELECT p.play_id, :at, :count FROM media_active a JOIN media_play p ON p.play_id = a.play_id
                WHERE a.singleton = 1 AND p.started_at <= :at AND p.ended_at IS NULL", json!({"at":at,"count":count})));
        return Ok(out);
    } else if event.get("LiveStarted").is_some()
        || event
            .get("BroadcastStateChanged")
            .and_then(|v| v.get("to"))
            .and_then(Value::as_str)
            .is_some_and(|s| matches!(s, "paused" | "stopped" | "sleeping"))
    {
        return Ok(vec![statement(
            "UPDATE media_active SET play_id = NULL, started_at = NULL WHERE singleton = 1",
            json!({}),
        )]);
    } else {
        return Ok(out);
    };
    let play = body.get("playback").ok_or("missing playback")?;
    let id = integer(play, "play_id")?;
    if id == 0 {
        return Err("invalid play_id".into());
    }
    let at = integer(play, "at")?;
    let path = text(play, "media_path")?;
    let uuid = optional(play, "media_uuid")?;
    let key = match uuid.as_str().filter(|s| !s.is_empty()) {
        Some(s) => format!("uuid:{s}"),
        None => format!("path:{path}"),
    };
    let mut params = json!({"id":id,"key":key,"path":path,"title":optional(play,"title")?,
        "artist":optional(play,"artist")?,"album":optional(play,"album")?,"playlist":optional(play,"playlist_ref")?,"at":at});
    let insert = "INSERT INTO media_play (play_id, media_key, media_path, title, artist, album, playlist_ref, started_at)
        VALUES (:id, :key, :path, :title, :artist, :album, :playlist, :at) ON CONFLICT(play_id) DO NOTHING";
    if kind == "start" {
        out.push(statement(insert, params));
        out.push(statement(
            "UPDATE media_active SET play_id = :id, started_at = :at WHERE singleton = 1
            AND (started_at IS NULL OR :at > started_at OR (:at = started_at AND :id >= play_id))
            AND EXISTS (SELECT 1 FROM media_play WHERE play_id = :id AND ended_at IS NULL)",
            json!({"id":id,"at":at}),
        ));
    } else {
        let end = integer(body, "at")?;
        if end < at {
            return Err("finish precedes start".into());
        }
        let seconds = integer(body, "aired_seconds")?;
        let verdict = match body.get("played_to_end") {
            None | Some(Value::Null) => Value::Null,
            Some(Value::Bool(b)) => json!(if *b { 1 } else { 0 }),
            _ => return Err("invalid played_to_end".into()),
        };
        params["end"] = json!(end);
        params["seconds"] = json!(seconds);
        params["verdict"] = verdict;
        out.push(statement("INSERT INTO media_play (play_id, media_key, media_path, title, artist, album, playlist_ref, started_at, ended_at, aired_seconds, played_to_end)
            VALUES (:id,:key,:path,:title,:artist,:album,:playlist,:at,:end,:seconds,:verdict)
            ON CONFLICT(play_id) DO UPDATE SET media_path = excluded.media_path, title = excluded.title,
            artist = excluded.artist, album = excluded.album, playlist_ref = excluded.playlist_ref,
            ended_at = excluded.ended_at, aired_seconds = excluded.aired_seconds, played_to_end = excluded.played_to_end
            WHERE media_play.ended_at IS NULL", params));
        out.push(statement("UPDATE media_active SET play_id = NULL, started_at = NULL WHERE singleton = 1 AND play_id = :id", json!({"id":id})));
    }
    let cutoff = at.saturating_sub(i64::from(retention_days) * 86400);
    out.push(statement("DELETE FROM media_audience WHERE at < :cutoff OR play_id IN (SELECT play_id FROM media_play WHERE started_at < :cutoff)", json!({"cutoff":cutoff})));
    out.push(statement(
        "DELETE FROM media_play WHERE started_at < :cutoff",
        json!({"cutoff":cutoff}),
    ));
    Ok(out)
}
