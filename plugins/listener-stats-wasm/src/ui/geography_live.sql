WITH mounts AS (
 SELECT DISTINCT mount FROM listener_snapshot WHERE (:mount='' OR mount=:mount)
), latest AS (
 SELECT s.mount,s.at,s.listeners FROM mounts m
 JOIN listener_snapshot s ON s.mount=m.mount AND s.at=(
 SELECT at FROM listener_snapshot WHERE mount=m.mount AND at<=unixepoch() ORDER BY at DESC LIMIT 1)
)
SELECT CASE WHEN s.listeners=0 THEN '—' ELSE COALESCE(NULLIF(g.country,''),'Inconnu') END AS Pays,
 CASE WHEN s.listeners=0 THEN '—' ELSE COALESCE(NULLIF(g.city,''),'Inconnue') END AS Ville,
 CASE WHEN s.listeners IS NULL THEN NULL ELSE COALESCE(SUM(g.listeners),s.listeners) END AS Auditeurs,
 s.mount AS Flux,datetime(s.at,'unixepoch') AS Releve_UTC,
 unixepoch()-s.at AS Age_s,
 CASE WHEN s.listeners IS NULL THEN 'Collecte échouée'
 WHEN s.listeners=0 THEN 'Aucun auditeur'
 ELSE COALESCE(g.status,'unavailable') END AS Statut
FROM latest s LEFT JOIN listener_geo g ON g.mount=s.mount AND g.at=(s.at+0) AND s.listeners>0
GROUP BY s.mount,s.at,g.country,g.city,g.status
ORDER BY Auditeurs DESC,Pays,Ville,Flux,Statut LIMIT 1000
