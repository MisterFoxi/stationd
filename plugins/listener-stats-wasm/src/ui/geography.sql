, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN at / 3600
 WHEN 'Jour' THEN at / 86400
 WHEN 'Semaine' THEN (at / 86400 + 3) / 7
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN (at / 3600) % 24
 WHEN 'Jour de semaine' THEN (at / 86400 + 3) % 7 + 1
 ELSE 'Total' END AS Periode_UTC FROM recent),
totals AS (
 SELECT mount, Periode_UTC, COUNT(listeners) AS samples, SUM(listeners) AS audience
 FROM bucketed GROUP BY mount, Periode_UTC
), locations AS (
 SELECT g.mount, g.at, CASE :grouping
 WHEN 'Heure' THEN g.at / 3600
 WHEN 'Jour' THEN g.at / 86400
 WHEN 'Semaine' THEN (g.at / 86400 + 3) / 7
 WHEN 'Mois' THEN strftime('%Y-%m', g.at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', g.at, 'unixepoch')
 WHEN 'Heure du jour' THEN (g.at / 3600) % 24
 WHEN 'Jour de semaine' THEN (g.at / 86400 + 3) % 7 + 1
 ELSE 'Total' END AS Periode_UTC, g.status, g.country,
 CASE WHEN :geography IN ('Région', 'Ville') THEN g.region ELSE '' END AS region,
 CASE WHEN :geography = 'Ville' THEN g.city ELSE '' END AS city,
 SUM(g.listeners) AS listeners
 FROM listener_geo AS g
 CROSS JOIN bounds
 -- Keep the timestamp lookup exact; avoid propagating period ranges into this join.
 CROSS JOIN listener_snapshot AS s INDEXED BY listener_snapshot_mount_period
 WHERE s.mount = g.mount AND s.at = (g.at + 0) AND s.listeners IS NOT NULL
 AND g.at >= start_at AND g.at < end_at AND g.at <= unixepoch()
 AND (:mount = '' OR g.mount = :mount)
 GROUP BY g.mount, g.at, 3, g.status, g.country, 6, 7
)
SELECT g.mount AS Mount,
 COALESCE(NULLIF(g.country, ''), 'Inconnu') AS Pays,
 CASE WHEN :geography = 'Pays' THEN 'Toutes' ELSE COALESCE(NULLIF(g.region, ''), 'Inconnue') END AS Region,
 CASE WHEN :geography != 'Ville' THEN 'Toutes' ELSE COALESCE(NULLIF(g.city, ''), 'Inconnue') END AS Ville,
 ROUND(1.0 * SUM(g.listeners) / t.samples, 2) AS Moyenne,
 MAX(g.listeners) AS Pic,
 ROUND(100.0 * SUM(g.listeners) / NULLIF(t.audience, 0), 1) AS Part_pct,
 g.status AS Statut, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', g.Periode_UTC * 3600, 'unixepoch')
 WHEN 'Jour' THEN date(g.Periode_UTC * 86400, 'unixepoch')
 WHEN 'Semaine' THEN date((g.Periode_UTC * 7 - 3) * 86400, 'unixepoch')
 WHEN 'Heure du jour' THEN printf('%02d:00', g.Periode_UTC)
 ELSE CAST(g.Periode_UTC AS TEXT) END AS Periode_UTC,
 SUM(g.listeners) AS Observations_auditeurs,
 CASE WHEN COUNT(*) < t.samples THEN 0 ELSE MIN(g.listeners) END AS Minimum,
 t.samples AS Releves_valides
FROM locations AS g
JOIN totals AS t ON t.mount = g.mount AND t.Periode_UTC = g.Periode_UTC
GROUP BY g.mount, g.Periode_UTC, g.status, g.country, g.region, g.city
ORDER BY g.Periode_UTC DESC, Moyenne DESC, g.mount, g.country, g.region, g.city, g.status LIMIT 1000