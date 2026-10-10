, chosen AS (SELECT mount FROM listener_snapshot INDEXED BY listener_snapshot_mount_period, bounds WHERE at >= start_at AND at < end_at AND at <= unixepoch() AND (:mount = '' OR mount = :mount) ORDER BY mount LIMIT 1),
 scoped AS NOT MATERIALIZED (SELECT r.mount,r.at,r.listeners FROM listener_snapshot r JOIN chosen USING(mount),bounds WHERE r.at >= start_at AND r.at < end_at AND r.at <= unixepoch()),
 hourly_stats AS MATERIALIZED (SELECT mount,at/3600 AS hour,SUM(listeners) AS audience,COUNT(listeners) AS samples,COUNT(*) AS attempts,MAX(listeners) AS peak FROM scoped GROUP BY mount,hour),
 grain AS (SELECT CASE WHEN :grouping='Total' THEN CASE WHEN end_at-start_at<=172800 THEN 'Heure' WHEN end_at-start_at<=7776000 THEN 'Jour' ELSE 'Mois' END ELSE :grouping END AS name FROM bounds),
 bucketed AS (SELECT s.*, CASE name
 WHEN 'Heure' THEN at/3600 WHEN 'Jour' THEN at/86400 WHEN 'Semaine' THEN (at/86400+3)/7
 WHEN 'Mois' THEN strftime('%Y-%m',at,'unixepoch') WHEN 'Année' THEN strftime('%Y',at,'unixepoch')
 WHEN 'Heure du jour' THEN (at/3600)%24 WHEN 'Jour de semaine' THEN (at/86400+3)%7+1 END AS bucket FROM (SELECT *,hour*3600 AS at FROM hourly_stats) s,grain),
 summary AS (SELECT mount, 1.0*SUM(audience)/NULLIF(SUM(samples),0) AS mean, MAX(peak) AS peak, SUM(samples) AS samples,
 100.0*SUM(samples)/SUM(attempts) AS quality FROM hourly_stats GROUP BY mount),
 series_raw AS (SELECT mount, bucket, 1.0*SUM(audience)/NULLIF(SUM(samples),0) AS value, SUM(samples) AS samples FROM bucketed GROUP BY mount,bucket ORDER BY bucket DESC LIMIT 600),
 series AS (SELECT mount, CASE name WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00',bucket*3600,'unixepoch') WHEN 'Jour' THEN date(bucket*86400,'unixepoch') WHEN 'Semaine' THEN date((bucket*7-3)*86400,'unixepoch') WHEN 'Heure du jour' THEN printf('%02d:00',bucket) ELSE CAST(bucket AS TEXT) END AS bucket,value,samples FROM series_raw,grain),
 ranking AS (SELECT g.mount, COALESCE(NULLIF(g.country,''),'Inconnu') || CASE 'Pays' WHEN 'Région' THEN ' · ' || COALESCE(NULLIF(g.region,''),'Inconnue') WHEN 'Ville' THEN ' · ' || COALESCE(NULLIF(g.region,''),'Inconnue') || ' · ' || COALESCE(NULLIF(g.city,''),'Inconnue') ELSE '' END || CASE WHEN g.status='found' THEN '' ELSE ' · ' || g.status END AS label,
 1.0*SUM(g.listeners)/(SELECT samples FROM summary) AS value, (SELECT samples FROM summary) AS samples
 FROM listener_geo g JOIN chosen c ON c.mount=g.mount JOIN listener_snapshot s ON s.mount=g.mount AND s.at=(g.at+0) AND s.listeners IS NOT NULL CROSS JOIN bounds
 WHERE g.at>=start_at AND g.at<end_at AND g.at<=unixepoch()
 GROUP BY g.mount,label ORDER BY value DESC,label LIMIT 100),
 heat_raw AS (SELECT mount,(hour/24+3)%7+1 AS day,hour%24 AS hour_of_day,1.0*SUM(audience)/NULLIF(SUM(samples),0) AS value,SUM(samples) AS samples FROM hourly_stats GROUP BY mount,day,hour_of_day),
 heat AS (SELECT mount,CAST(day AS TEXT) AS day,printf('%02d',hour_of_day) AS hour,value,samples FROM heat_raw),
 output AS (
 SELECT 'summary' AS Section,mount AS Scope,'Audience moyenne' AS Label,'' AS Bucket,mean AS Value,'auditeurs' AS Unit,samples AS Samples FROM summary
 UNION ALL SELECT 'summary',mount,'Pic d’audience','',peak,'auditeurs',samples FROM summary
 UNION ALL SELECT 'summary',mount,'Relevés valides','',samples,'relevés',samples FROM summary
 UNION ALL SELECT 'summary',mount,'Collecte réussie','',quality,'%',samples FROM summary
 UNION ALL SELECT 'series',mount,'Audience moyenne',bucket,value,'auditeurs',samples FROM series
 UNION ALL SELECT 'ranking',mount,label,'',value,'auditeurs',samples FROM ranking
 UNION ALL SELECT 'heatmap',mount,day,hour,value,'auditeurs',samples FROM heat
 UNION ALL SELECT 'period',mount,datetime(start_at,'unixepoch'),datetime(end_at,'unixepoch'),NULL,'UTC',NULL FROM chosen,bounds
 UNION ALL SELECT 'note',mount,'600 dernières tranches / top 100 · Moyennes par relevé réussi · zéro inclus · absence de collecte = inconnue. Géographie : moyenne sur tous les relevés valides.','',NULL,'',NULL FROM chosen
)
SELECT * FROM output LIMIT 1000
