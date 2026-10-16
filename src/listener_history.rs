//! Shared audience queries; sample means are not unique listeners.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum Period { Hour, Day, Week }

pub fn parse_window(value: &str) -> Result<u64, String> {
    let split = value.find(|c: char| !c.is_ascii_digit()).ok_or("Use a duration such as 24h, 7d or 4w")?;
    let amount: u64 = value[..split].parse().map_err(|_| "Invalid duration")?;
    let unit = match &value[split..] { "h" => 3600, "d" => 86400, "w" => 604800, _ => return Err("Use h, d or w".into()) };
    let seconds = amount.checked_mul(unit).ok_or("Duration too large")?;
    if seconds == 0 || seconds > 365 * 86400 { return Err("Window must be between 1h and 365d".into()); }
    Ok(seconds)
}

pub fn regions_query(mount: Option<&str>) -> String {
    // The existing DB RPC has no bind parameters. Quote a single SQL literal;
    // never interpolate a mount as SQL syntax or as a LIKE pattern.
    let filter = mount.map(|m| format!("WHERE mount = '{}'", m.replace('\'', "''"))).unwrap_or_default();
    format!(r#"WITH latest AS (
        SELECT mount, MAX(at) AS at FROM listener_snapshot {filter} GROUP BY mount
    ), regional AS (
        SELECT g.mount, g.at, g.status, g.country, g.region, SUM(g.listeners) AS listeners
        FROM listener_geo g JOIN latest l ON l.mount = g.mount AND l.at = g.at
        GROUP BY g.mount, g.at, g.status, g.country, g.region
    )
    SELECT s.mount, s.at, COALESCE(g.country, '') AS country,
        COALESCE(g.region, '') AS region,
        CASE WHEN s.listeners IS NULL THEN NULL ELSE COALESCE(g.listeners, s.listeners) END AS listeners,
        CASE WHEN s.listeners IS NULL THEN 'collecte_inconnue'
             WHEN s.listeners = 0 THEN 'vide'
             ELSE COALESCE(g.status, 'details_absents') END AS status
    FROM latest l JOIN listener_snapshot s ON s.mount = l.mount AND s.at = l.at
    LEFT JOIN regional g ON g.mount = s.mount AND g.at = s.at
    WHERE s.listeners IS NULL OR s.listeners > 0
    ORDER BY s.mount, listeners DESC, country, region, status"#)
}

pub fn stats_query(period: Period, since: i64, until: i64, mount: Option<&str>) -> String {
    let bucket = match period {
        Period::Hour => "at - (at % 3600)",
        Period::Day => "at - (at % 86400)",
        // Unix epoch was a Thursday; calendar weeks begin on Monday UTC.
        Period::Week => "at - ((at + 259200) % 604800)",
    };
    let mount = mount.map(|m| format!("AND mount = '{}'", m.replace('\'', "''"))).unwrap_or_default();
    format!(r#"WITH observations AS (
        SELECT mount, at, listeners, {bucket} AS bucket
        FROM listener_snapshot WHERE at >= {since} AND at <= {until} {mount}
    ), coverage AS (
        SELECT mount, bucket,
            SUM(CASE WHEN listeners IS NOT NULL THEN 1 ELSE 0 END) AS samples,
            SUM(CASE WHEN listeners IS NULL THEN 1 ELSE 0 END) AS failures
        FROM observations GROUP BY mount, bucket
    ), counts AS (
        SELECT o.mount, o.bucket, g.status, g.country, g.region, g.city,
            SUM(g.listeners) AS total, MAX(g.listeners) AS peak
        FROM observations o JOIN listener_geo g ON g.mount = o.mount AND g.at = o.at
        WHERE o.listeners IS NOT NULL
        GROUP BY o.mount, o.bucket, g.status, g.country, g.region, g.city
        HAVING SUM(g.listeners) > 0
    )
    SELECT strftime('%Y-%m-%d %H:%M', g.bucket, 'unixepoch') AS periode_utc,
        g.mount, g.country AS pays, g.region, g.city AS ville,
        CAST(g.total AS REAL) / c.samples AS moyenne,
        g.peak AS pic, c.samples AS releves, c.failures AS echecs, g.status AS etat_geoip
    FROM counts g JOIN coverage c ON c.mount = g.mount AND c.bucket = g.bucket
    ORDER BY g.bucket DESC, g.mount, moyenne DESC, pays, region, ville, etat_geoip"#)
}

