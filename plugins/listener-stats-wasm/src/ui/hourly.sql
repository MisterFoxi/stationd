SELECT strftime('%Y-%m-%d %H:00', at, 'unixepoch') AS Heure_UTC, mount AS Mount,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic, MIN(listeners) AS Minimum,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs
FROM listener_snapshot WHERE at >= unixepoch() - 86400 AND at <= unixepoch()
GROUP BY Heure_UTC, mount ORDER BY Heure_UTC DESC, mount LIMIT 1000
