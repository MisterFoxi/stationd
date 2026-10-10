WITH RECURSIVE genre_filters(kind,genre,rest) AS (
 SELECT 'include','',replace(:include_genres,';',',') || ','
 UNION ALL SELECT 'exclude','',replace(:exclude_genres,';',',') || ','
 UNION ALL SELECT kind,trim(substr(rest,1,instr(rest,',')-1)),substr(rest,instr(rest,',')+1)
 FROM genre_filters WHERE rest!=''
), bounds AS (
 SELECT CASE :period
  WHEN 'Personnalisée' THEN unixepoch(:from)
  WHEN 'Aujourd’hui' THEN unixepoch(date('now'))
  WHEN '7 jours' THEN unixepoch() - 604800
  WHEN '30 jours' THEN unixepoch() - 2592000
  WHEN '90 jours' THEN unixepoch() - 7776000
  WHEN '365 jours' THEN unixepoch() - 31536000
  WHEN 'Tout' THEN 0 ELSE unixepoch() - 86400 END AS start_at,
 CASE WHEN :period = 'Personnalisée' THEN unixepoch(:to) ELSE unixepoch() + 1 END AS end_at
), recent AS (
 SELECT p.*, CASE :grouping
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', started_at, 'unixepoch')
 WHEN 'Jour' THEN date(started_at, 'unixepoch')
 WHEN 'Semaine' THEN date(started_at, 'unixepoch', '-' || ((CAST(strftime('%w', started_at, 'unixepoch') AS INTEGER) + 6) % 7) || ' days')
 WHEN 'Mois' THEN strftime('%Y-%m', started_at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', started_at, 'unixepoch')
 WHEN 'Heure du jour' THEN strftime('%H:00', started_at, 'unixepoch')
 WHEN 'Jour de semaine' THEN CAST((CAST(strftime('%w', started_at, 'unixepoch') AS INTEGER) + 6) % 7 + 1 AS TEXT)
 ELSE 'Total' END AS Periode_UTC,
 CASE :by WHEN 'Artiste' THEN 'artist:' || COALESCE(NULLIF(artist,''),'Inconnu')
 WHEN 'Album' THEN 'album:' || json_array(COALESCE(NULLIF(album,''),'Inconnu'), COALESCE(NULLIF(artist,''),'Inconnu'))
 WHEN 'Playlist' THEN 'playlist:' || COALESCE(NULLIF(playlist_ref,''),'Inconnue')
 ELSE media_key END AS group_key
 FROM media_play p, bounds WHERE started_at >= start_at AND started_at < end_at AND started_at <= unixepoch()
 AND (:media = '' OR instr(lower(media_path),lower(:media)) > 0 OR instr(lower(COALESCE(title,'')),lower(:media)) > 0
 OR instr(lower(COALESCE(artist,'')),lower(:media)) > 0 OR instr(lower(COALESCE(album,'')),lower(:media)) > 0)
 AND (NOT EXISTS (SELECT 1 FROM genre_filters WHERE kind='include' AND genre!='')
 OR EXISTS (SELECT 1 FROM json_each(p.genres_json) g JOIN genre_filters f
 ON trim(g.value)=f.genre COLLATE NOCASE WHERE f.kind='include' AND f.genre!=''))
 AND NOT EXISTS (SELECT 1 FROM json_each(p.genres_json) g JOIN genre_filters f
 ON trim(g.value)=f.genre COLLATE NOCASE WHERE f.kind='exclude' AND f.genre!='')
), bucketed AS (SELECT *, CASE (CASE WHEN :grouping='Total' THEN CASE WHEN end_at-start_at<=172800 THEN 'Heure' WHEN end_at-start_at<=7776000 THEN 'Jour' ELSE 'Mois' END ELSE :grouping END)
 WHEN 'Heure' THEN strftime('%Y-%m-%d %H:00', started_at, 'unixepoch')
 WHEN 'Jour' THEN date(started_at, 'unixepoch')
 WHEN 'Semaine' THEN date(started_at, 'unixepoch', '-' || ((CAST(strftime('%w', started_at, 'unixepoch') AS INTEGER)+6)%7) || ' days')
 WHEN 'Mois' THEN strftime('%Y-%m', started_at, 'unixepoch')
 WHEN 'Année' THEN strftime('%Y', started_at, 'unixepoch')
 WHEN 'Heure du jour' THEN strftime('%H:00', started_at, 'unixepoch')
 WHEN 'Jour de semaine' THEN CAST((CAST(strftime('%w', started_at, 'unixepoch') AS INTEGER)+6)%7+1 AS TEXT) END AS bucket FROM recent,bounds),
 samples AS (SELECT a.* FROM media_audience a JOIN recent p ON p.play_id=a.play_id,bounds
 WHERE a.at>=start_at AND a.at<end_at AND a.at<=unixepoch()),
 bucket_audience AS (SELECT p.bucket,AVG(a.listeners) AS mean,COUNT(*) AS samples FROM samples a JOIN bucketed p ON p.play_id=a.play_id GROUP BY p.bucket),
 group_audience AS (SELECT p.group_key,AVG(a.listeners) AS mean FROM samples a JOIN recent p ON p.play_id=a.play_id GROUP BY p.group_key),
 summary AS (SELECT COUNT(*) AS plays,SUM(aired_seconds) AS duration,SUM(ended_at IS NULL) AS missing FROM recent HAVING COUNT(*)>0),
 series AS (SELECT p.bucket, CASE :sorting WHEN 'Durée' THEN SUM(aired_seconds)
 WHEN 'Audience' THEN a.mean
 ELSE COUNT(*) END AS value,
 CASE :sorting WHEN 'Durée' THEN COUNT(aired_seconds) WHEN 'Audience' THEN COALESCE(a.samples,0) ELSE COUNT(*) END AS count
 FROM bucketed p LEFT JOIN bucket_audience a ON a.bucket=p.bucket GROUP BY p.bucket ORDER BY p.bucket DESC LIMIT 600),
 groups AS (SELECT group_key,COUNT(*) AS plays,SUM(aired_seconds) AS duration,MAX(started_at) AS last_at FROM recent GROUP BY group_key),
 ranking AS (SELECT CASE :by WHEN 'Artiste' THEN COALESCE(NULLIF(p.artist,''),'Inconnu')
 WHEN 'Album' THEN COALESCE(NULLIF(p.album,''),'Inconnu') || ' · ' || COALESCE(NULLIF(p.artist,''),'Inconnu')
 WHEN 'Playlist' THEN COALESCE(NULLIF(p.playlist_ref,''),'Inconnue') ELSE COALESCE(NULLIF(p.title,''),p.media_path) || ' · ' || p.media_path END AS label,
 CASE :sorting WHEN 'Durée' THEN t.duration WHEN 'Audience' THEN a.mean ELSE t.plays END AS value
 FROM groups t JOIN recent p ON p.play_id=(SELECT play_id FROM recent q WHERE q.group_key=t.group_key ORDER BY started_at DESC,play_id DESC LIMIT 1)
 LEFT JOIN group_audience a ON a.group_key=t.group_key
 ORDER BY value DESC,label,t.group_key LIMIT 100),
 heat AS (SELECT CAST((CAST(strftime('%w',started_at,'unixepoch') AS INTEGER)+6)%7+1 AS TEXT) AS day,
 strftime('%H',started_at,'unixepoch') AS hour,COUNT(*) AS value FROM recent GROUP BY day,hour),
 unit AS (SELECT CASE :sorting WHEN 'Durée' THEN 's' WHEN 'Audience' THEN 'auditeurs' ELSE 'passages' END AS name),
 output AS (
 SELECT 'summary' AS Section,'' AS Scope,'Diffusions' AS Label,'' AS Bucket,plays AS Value,'passages' AS Unit,plays AS Samples FROM summary
 UNION ALL SELECT 'summary','','Durée diffusée','',duration,'s',plays FROM summary
 UNION ALL SELECT 'summary','','Audience moyenne','',(SELECT AVG(listeners) FROM samples),'auditeurs',(SELECT COUNT(*) FROM samples) FROM summary
 UNION ALL SELECT 'summary','','Pic d’audience','',(SELECT MAX(listeners) FROM samples),'auditeurs',(SELECT COUNT(*) FROM samples) FROM summary
 UNION ALL SELECT 'series','',CASE :sorting WHEN 'Durée' THEN 'Durée diffusée' WHEN 'Audience' THEN 'Audience moyenne' ELSE 'Diffusions' END,bucket,value,name,count FROM series,unit
 UNION ALL SELECT 'ranking','',label,'',value,name,NULL FROM ranking,unit
 UNION ALL SELECT 'heatmap','',day,hour,value,'passages',value FROM heat
 UNION ALL SELECT 'period','',datetime(start_at,'unixepoch'),datetime(end_at,'unixepoch'),NULL,'UTC',NULL FROM bounds,summary
 UNION ALL SELECT 'note','','600 dernières tranches / top 100 · Durées entières rattachées au début · ' || missing || ' fin(s) non observée(s) · audience par relevé, pas d’auditeurs uniques.','',NULL,'',NULL FROM summary
)
SELECT * FROM output LIMIT 1000
