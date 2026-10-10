, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', at, 'unixepoch')
 WHEN 'Jour' THEN date(at, 'unixepoch')
 WHEN 'Semaine' THEN date(at, 'unixepoch', '-' || ((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7) || ' days')
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN strftime('%H:00', at, 'unixepoch')
 WHEN 'Jour de semaine' THEN CAST((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7 + 1 AS TEXT)
 ELSE 'Total' END AS Periode_UTC FROM recent),
totals AS (
 SELECT mount, Periode_UTC, COUNT(listeners) AS samples, SUM(listeners) AS audience
 FROM bucketed GROUP BY mount, Periode_UTC
), locations AS (
 SELECT g.mount, g.at, s.Periode_UTC, g.status, g.country,
 CASE WHEN :geography IN ('Région', 'Ville') THEN g.region ELSE '' END AS region,
 CASE WHEN :geography = 'Ville' THEN g.city ELSE '' END AS city,
 SUM(g.listeners) AS listeners
 FROM listener_geo AS g
 JOIN bucketed AS s ON s.mount = g.mount AND s.at = g.at AND s.listeners IS NOT NULL
 GROUP BY g.mount, g.at, s.Periode_UTC, g.status, g.country, 6, 7
)
SELECT g.mount AS Mount,
 COALESCE(NULLIF(g.country, ''), 'Inconnu') AS Pays,
 CASE WHEN :geography = 'Pays' THEN 'Toutes' ELSE COALESCE(NULLIF(g.region, ''), 'Inconnue') END AS Region,
 CASE WHEN :geography != 'Ville' THEN 'Toutes' ELSE COALESCE(NULLIF(g.city, ''), 'Inconnue') END AS Ville,
 ROUND(1.0 * SUM(g.listeners) / t.samples, 2) AS Moyenne,
 MAX(g.listeners) AS Pic,
 ROUND(100.0 * SUM(g.listeners) / NULLIF(t.audience, 0), 1) AS Part_pct,
 g.status AS Statut, g.Periode_UTC,
 SUM(g.listeners) AS Observations_auditeurs,
 CASE WHEN COUNT(*) < t.samples THEN 0 ELSE MIN(g.listeners) END AS Minimum,
 t.samples AS Releves_valides
FROM locations AS g
JOIN totals AS t ON t.mount = g.mount AND t.Periode_UTC = g.Periode_UTC
GROUP BY g.mount, g.Periode_UTC, g.status, g.country, g.region, g.city
ORDER BY g.Periode_UTC DESC, Moyenne DESC, g.mount, g.country, g.region, g.city, g.status LIMIT 1000