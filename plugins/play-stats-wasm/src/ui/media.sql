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
), totals AS (
 SELECT group_key, Periode_UTC, COUNT(*) AS Passages, MAX(started_at) AS last_at,
 SUM(aired_seconds) AS Duree_s, SUM(played_to_end = 1) AS Complets,
 SUM(played_to_end = 0) AS Coupes, SUM(ended_at IS NOT NULL AND played_to_end IS NULL) AS Issue_inconnue,
 SUM(ended_at IS NULL) AS Sans_fin_observee
 FROM recent GROUP BY group_key, Periode_UTC
), audience AS (
 SELECT p.group_key, p.Periode_UTC, ROUND(AVG(a.listeners),2) AS Moyenne, MAX(a.listeners) AS Pic, COUNT(*) AS Releves
 FROM recent p JOIN media_audience a ON a.play_id = p.play_id, bounds
 WHERE a.at >= start_at AND a.at < end_at AND a.at <= unixepoch()
 GROUP BY p.group_key, p.Periode_UTC
)
SELECT CASE :by WHEN 'Artiste' THEN COALESCE(NULLIF(p.artist,''),'Inconnu')
 WHEN 'Album' THEN COALESCE(NULLIF(p.album,''),'Inconnu') || ' · ' || COALESCE(NULLIF(p.artist,''),'Inconnu')
 WHEN 'Playlist' THEN COALESCE(NULLIF(p.playlist_ref,''),'Inconnue')
 ELSE COALESCE(NULLIF(p.title,''),p.media_path) END AS Element,
 t.Passages, datetime(t.last_at,'unixepoch') AS Dernier_UTC,
 t.Duree_s, COALESCE(t.Complets,0) AS Complets, COALESCE(t.Coupes,0) AS Coupes,
 t.Issue_inconnue, t.Sans_fin_observee, a.Moyenne AS Audience_moyenne, a.Pic AS Pic_audience,
 COALESCE(a.Releves,0) AS Releves_audience, t.Periode_UTC,
 CASE WHEN :by = 'Média' THEN p.media_path END AS Media,
 CASE WHEN :by IN ('Média','Album') THEN p.artist END AS Artiste,
 CASE WHEN :by = 'Média' THEN p.album END AS Album,
 CASE WHEN :by = 'Média' THEN p.playlist_ref END AS Playlist,
 t.group_key AS Identite
FROM totals t JOIN recent p ON p.play_id = (
 SELECT x.play_id FROM recent x WHERE x.group_key = t.group_key AND x.Periode_UTC = t.Periode_UTC
 ORDER BY x.started_at DESC, x.play_id DESC LIMIT 1)
LEFT JOIN audience a ON a.group_key = t.group_key AND a.Periode_UTC = t.Periode_UTC
ORDER BY t.Periode_UTC DESC,
 CASE :sorting WHEN 'Durée' THEN t.Duree_s WHEN 'Audience' THEN a.Moyenne ELSE t.Passages END DESC,
 t.Passages DESC, Element, t.group_key LIMIT 1000
