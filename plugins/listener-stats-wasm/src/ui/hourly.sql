, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', at, 'unixepoch')
 WHEN 'Jour' THEN date(at, 'unixepoch')
 WHEN 'Semaine' THEN date(at, 'unixepoch', '-' || ((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7) || ' days')
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN strftime('%H:00', at, 'unixepoch')
 WHEN 'Jour de semaine' THEN CAST((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7 + 1 AS TEXT)
 ELSE 'Total' END AS Periode_UTC FROM recent)
SELECT Periode_UTC, mount AS Mount,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic, MIN(listeners) AS Minimum,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs,
 SUM(listeners) AS Observations_auditeurs,
 ROUND(100.0 * COUNT(listeners) / COUNT(*), 1) AS Collecte_pct
FROM bucketed GROUP BY Periode_UTC, mount ORDER BY Periode_UTC DESC, mount LIMIT 1000