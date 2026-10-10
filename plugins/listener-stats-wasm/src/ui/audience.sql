, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', at, 'unixepoch')
 WHEN 'Jour' THEN date(at, 'unixepoch')
 WHEN 'Semaine' THEN date(at, 'unixepoch', '-' || ((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7) || ' days')
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN strftime('%H:00', at, 'unixepoch')
 WHEN 'Jour de semaine' THEN CAST((CAST(strftime('%w', at, 'unixepoch') AS INTEGER) + 6) % 7 + 1 AS TEXT)
 ELSE 'Total' END AS Periode_UTC FROM recent)
SELECT mount AS Mount,
 (SELECT listeners FROM recent AS latest WHERE latest.mount = s.mount ORDER BY at DESC LIMIT 1) AS Dernier_effectif,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic,
 ROUND(100.0 * COUNT(listeners) / COUNT(*), 1) AS Collecte_pct,
 datetime(MAX(at), 'unixepoch') AS Dernier_UTC,
 MIN(listeners) AS Minimum, SUM(listeners) AS Observations_auditeurs,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs,
 datetime(MIN(at), 'unixepoch') AS Premier_UTC, Periode_UTC
FROM bucketed AS s GROUP BY mount, Periode_UTC ORDER BY Periode_UTC DESC, Moyenne DESC, mount LIMIT 1000