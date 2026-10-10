, bucketed AS (SELECT *, CASE :grouping
 WHEN 'Heure' THEN at / 3600
 WHEN 'Jour' THEN at / 86400
 WHEN 'Semaine' THEN (at / 86400 + 3) / 7
 WHEN 'Mois' THEN strftime('%Y-%m', at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', at, 'unixepoch')
 WHEN 'Heure du jour' THEN (at / 3600) % 24
 WHEN 'Jour de semaine' THEN (at / 86400 + 3) % 7 + 1
 ELSE 'Total' END AS Periode_UTC FROM recent)
SELECT mount AS Mount,
 (SELECT listeners FROM recent AS latest WHERE latest.mount = s.mount ORDER BY at DESC LIMIT 1) AS Dernier_effectif,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic,
 ROUND(100.0 * COUNT(listeners) / COUNT(*), 1) AS Collecte_pct,
 datetime(MAX(at), 'unixepoch') AS Dernier_UTC,
 MIN(listeners) AS Minimum, SUM(listeners) AS Observations_auditeurs,
 COUNT(listeners) AS Releves_valides, COUNT(*) - COUNT(listeners) AS Echecs,
 datetime(MIN(at), 'unixepoch') AS Premier_UTC, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', s.Periode_UTC * 3600, 'unixepoch')
 WHEN 'Jour' THEN date(s.Periode_UTC * 86400, 'unixepoch')
 WHEN 'Semaine' THEN date((s.Periode_UTC * 7 - 3) * 86400, 'unixepoch')
 WHEN 'Heure du jour' THEN printf('%02d:00', s.Periode_UTC)
 ELSE CAST(s.Periode_UTC AS TEXT) END AS Periode_UTC
FROM bucketed AS s GROUP BY mount, s.Periode_UTC ORDER BY s.Periode_UTC DESC, Moyenne DESC, mount LIMIT 1000