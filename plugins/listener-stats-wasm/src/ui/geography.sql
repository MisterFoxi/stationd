WITH recent AS (SELECT * FROM listener_snapshot WHERE at >= unixepoch() - 86400 AND at <= unixepoch()),
totals AS (SELECT mount, COUNT(listeners) AS samples, SUM(listeners) AS audience FROM recent GROUP BY mount)
SELECT g.mount AS Mount,
 COALESCE(NULLIF(g.country, ''), 'Inconnu') AS Pays,
 COALESCE(NULLIF(g.region, ''), 'Inconnue') AS Region,
 COALESCE(NULLIF(g.city, ''), 'Inconnue') AS Ville,
 ROUND(1.0 * SUM(g.listeners) / t.samples, 2) AS Moyenne,
 MAX(g.listeners) AS Pic,
 ROUND(100.0 * SUM(g.listeners) / NULLIF(t.audience, 0), 1) AS Part_pct,
 g.status AS Statut
FROM listener_geo AS g
JOIN recent AS s ON s.mount = g.mount AND s.at = g.at AND s.listeners IS NOT NULL
JOIN totals AS t ON t.mount = g.mount
GROUP BY g.mount, g.status, g.country, g.region, g.city
ORDER BY Moyenne DESC, g.mount, g.country, g.region, g.city, g.status LIMIT 200
