WITH recent AS (SELECT * FROM listener_snapshot WHERE at >= unixepoch() - 86400 AND at <= unixepoch())
SELECT mount AS Mount,
 (SELECT listeners FROM recent AS latest WHERE latest.mount = s.mount ORDER BY at DESC LIMIT 1) AS Actuels,
 ROUND(AVG(listeners), 2) AS Moyenne, MAX(listeners) AS Pic,
 ROUND(100.0 * COUNT(listeners) / COUNT(*), 1) AS Collecte_pct,
 datetime(MAX(at), 'unixepoch') AS Dernier_UTC
FROM recent AS s GROUP BY mount ORDER BY Moyenne DESC, mount LIMIT 200
