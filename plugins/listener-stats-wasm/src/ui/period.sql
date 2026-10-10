WITH bounds AS (
 SELECT CASE :period
  WHEN 'Personnalisée' THEN unixepoch(:from)
  WHEN 'Aujourd’hui' THEN unixepoch(date('now'))
  WHEN '7 jours' THEN unixepoch() - 604800
  WHEN '30 jours' THEN unixepoch() - 2592000
  WHEN '90 jours' THEN unixepoch() - 7776000
  WHEN '365 jours' THEN unixepoch() - 31536000
  WHEN 'Tout' THEN 0 ELSE unixepoch() - 86400 END AS start_at,
 CASE WHEN :period = 'Personnalisée' THEN unixepoch(:to) ELSE unixepoch() + 1 END AS end_at
), recent AS (
 SELECT * FROM listener_snapshot, bounds
 WHERE at >= start_at AND at < end_at AND at <= unixepoch()
 AND (:mount = '' OR mount = :mount)
)