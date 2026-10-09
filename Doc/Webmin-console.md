# Webmin — console admin (phase D)

La vue réseau propose directement **Ouvrir la TUI** sur chaque station autorisée :
un clic ouvre et connecte la console, sans passer par le dashboard détaillé.
La page d’une station propose aussi **Ouvrir la console de cette station** lorsque
la console est activée et que le compte possède le rôle admin sur cette station.
L’écran rappelle la station cible avant l’ouverture et pendant toute la session.
La console exécute directement `stationd-tui --addr ENDPOINT_CONFIGURÉ --lang LANGUE_VIEWER` sous Linux.

## Configuration

Dans la déclaration existante du plugin, placer cette table avant les stations :

```toml
[plugin.config.console]
enabled = true
command = "/usr/local/bin/stationd-tui"
max_sessions = 5
max_sessions_per_user = 1
idle_timeout_seconds = 900
max_duration_seconds = 14400

[[plugin.config.stations]]
# catalogue existant…
```

La console est désactivée par défaut. Le chemin doit être absolu et son dernier
composant doit être `stationd-tui`. L’opérateur installe ce binaire de confiance ;
aucun exécutable, argument, endpoint ni environnement ne provient du navigateur.
Le processus reçoit uniquement `TERM=xterm-256color` et `LANG=C.UTF-8` et démarre
depuis `/`. Il conserve l’UID du daemon, sans shell ni élévation de privilèges.
La TUI reste un client gRPC complet, réservé aux admins ; aucune console helper.

En développement : `make webmin` compile maintenant aussi la TUI. Utiliser
`command = "/src/target/active/stationd-tui"` dans le conteneur de développement.
Le paquet de production contient déjà `/usr/local/bin/stationd-tui`.
Appliquer les changements de configuration avec le mécanisme habituel de
rechargement du plugin. Cette phase ne modifie pas la configuration active et
ne redémarre pas automatiquement les stations.

## Transport et ressources

Le proxy HTTPS existant doit transmettre l’upgrade WebSocket pour
`/api/stations/ID/console/RESERVATION/ws`. Le navigateur utilise WSS, une origine
exacte et le cookie sécurisé déjà utilisé par le dashboard. La TUI vérifie aussi
les certificats gRPC HTTPS avec les racines natives ; le catalogue conserve les
restrictions HTTP privé/tunnel du dashboard.

Les ressources `@xterm/xterm` 5.5.0 et `@xterm/addon-fit` 0.10.0 sont intégrées
au binaire, avec leurs licences dans `vendor/`. Aucun CDN n’est sollicité.
La page console autorise les styles générés par xterm ; les scripts restent
limités à la même origine. Les autres pages gardent leur CSP précédente.

La création est un POST protégé par session, rôle de station, origin et CSRF.
Une réservation expire après 15 secondes ; elle compte dans les quotas.
L’upgrade exige la même session Web et la même station, puis consomme la
réservation une fois. Son identifiant seul ne permet jamais de se connecter.
Le quota par utilisateur couvre l’ensemble des stations et des sessions Web.

Les quotas sont bornés à 64 consoles. Les tailles acceptées sont 20..300 colonnes
et 5..120 lignes. Les messages WS sont limités à 8192 octets, les entrées clavier
à 4096 octets par message et l’ensemble des messages à 64 Kio/s par console.
Le navigateur accuse réception après le rendu de chaque bloc de sortie
(4096 octets maximum) : un client lent ne peut accumuler un flux sans limite.
Une écriture réseau ou PTY bloquée termine la console ; les pings, ACK,
redimensionnements et sorties de la TUI ne comptent pas comme activité clavier.

Les délais d’inactivité et de durée absolue valent chacun 1..86400 secondes.
La révocation, la déconnexion, l’expiration Web et l’arrêt du plugin ferment
le terminal. Les mutations d’identité réveillent les consoles immédiatement ;
une vérification périodique couvre aussi l’expiration. Le propriétaire du PTY
tue le groupe de processus et attend la terminaison de la TUI, y compris lors
de l’annulation de sa tâche. La sortie, le clavier et les secrets de session
ne sont pas enregistrés dans l’audit ; seuls l’acteur, la station et le résultat
de l’ouverture/fermeture sont conservés.

## Vérification

```sh
make webmin-test
docker compose exec -T -u dev station cargo test --locked -p stationd-tui
```

Les tests Linux utilisent des comptes et un faux programme TUI isolés dans des
répertoires temporaires. Ils couvrent origin/CSRF, rôle helper refusé, droits par
station, propriétaire, réservation à usage unique et expirée, quotas globaux et
par compte, ANSI/Unicode, redimensionnement, clavier, fermeture navigateur,
révocation, expiration inactive malgré des sorties périodiques, durée absolue,
arrêt et destruction des descendants. Aucun compte réel n’est créé ou modifié.

Le vrai binaire TUI a aussi été lancé dans un PTY avec son environnement minimal,
sa sortie ANSI observée et son redimensionnement vérifié. Le rendu navigateur a
été contrôlé sur ordinateur et à 360 px avec des données d’aperçu isolées.

Pour la recette sur l’installation HTTPS : activer la table console, ouvrir une
station avec un compte admin existant, vérifier l’interaction clavier puis fermer
la console. Un compte viewer/helper ne doit pas voir le lien ni accéder aux routes.

## Langue du viewer

Webmin négocie la langue depuis l’en-tête `Accept-Language` du navigateur,
en respectant les priorités (`q`) et les variantes régionales (`de-DE` → `de`).
Les langues disponibles sont le français, l’anglais et l’allemand ; le français
est utilisé lorsqu’aucune préférence prise en charge n’est disponible.

Cette langue est utilisée pour les pages de connexion, d’inscription, de réseau,
de station et de console, leurs messages dynamiques et le format des heures.
Elle est conservée dans la réservation de console et transmise à la TUI par
`--lang`, indépendamment de la locale du serveur. Les noms de stations, comptes
et médias restent les données originales. Après un changement de langue du
navigateur, recharger la page et ouvrir une nouvelle console.

Vérifications : `node tests/webmin-language.js`, `node tests/webmin-console.js`
(également avec `WEBMIN_TEST_LANGUAGE=fr` ou `de`), et `make webmin-test`.
