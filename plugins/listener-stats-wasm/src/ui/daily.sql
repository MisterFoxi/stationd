SELECT date(at, 'unixepoch') AS Jour_UTC, mount AS Mount,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic, MIN(listeners) AS Minimum,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs
FROM listener_snapshot WHERE at >= unixepoch() - 2592000 AND at <= unixepoch()
GROUP BY Jour_UTC, mount ORDER BY Jour_UTC DESC, mount LIMIT 1000
