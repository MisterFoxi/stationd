, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN at / 3600
 WHEN 'Jour' THEN at / 86400
 WHEN 'Semaine' THEN (at / 86400 + 3) / 7
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN (at / 3600) % 24
 WHEN 'Jour de semaine' THEN (at / 86400 + 3) % 7 + 1
 ELSE 'Total' END AS Periode_UTC FROM recent)
SELECT CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', bucketed.Periode_UTC * 3600, 'unixepoch')
 WHEN 'Jour' THEN date(bucketed.Periode_UTC * 86400, 'unixepoch')
 WHEN 'Semaine' THEN date((bucketed.Periode_UTC * 7 - 3) * 86400, 'unixepoch')
 WHEN 'Heure du jour' THEN printf('%02d:00', bucketed.Periode_UTC)
 ELSE CAST(bucketed.Periode_UTC AS TEXT) END AS Periode_UTC, mount AS Mount,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic, MIN(listeners) AS Minimum,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs,
 SUM(listeners) AS Observations_auditeurs,
 ROUND(100.0 * COUNT(listeners) / COUNT(*), 1) AS Collecte_pct
FROM bucketed GROUP BY bucketed.Periode_UTC, mount ORDER BY bucketed.Periode_UTC DESC, mount LIMIT 1000