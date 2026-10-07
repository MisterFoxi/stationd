# Webmin — connexion par nom de compte et mot de passe

Le parcours utilisateur utilise désormais un nom de compte et un mot de
passe. Aucun appareil à enregistrer, aucune demande Windows Hello.
Le serveur est accessible à :

https://remote.homestone.rp-radio.live/login

## Premier accès

Créer le compte depuis devstationd, dans le dépôt :

```sh
make ctl A="remote-user add Alice --role admin --station home-stone"
```

Ouvrir le lien temporaire affiché. La page demande de choisir un mot de
passe et de le confirmer. Les longueurs minimale et maximale viennent de
`password_min_length` et `password_max_length` dans `[plugin.config]`.
Le formulaire affiche ces valeurs et le serveur applique les mêmes bornes.
Les espaces et la ponctuation sont autorisés ; les caractères Unicode
sont comptés de la même façon dans le navigateur et sur le serveur.

Une fois le mot de passe enregistré, ouvrir la page de connexion,
saisir le nom exact du compte et le mot de passe. L'accueil liste les
stations autorisées. Les données en direct attendent la tranche C.

Le lien expire après 15 minutes et ne fonctionne qu'une seule fois.
Son secret est placé après `#`, puis retiré de la barre d'adresse par
la page. Il n'est pas envoyé dans les logs HTTP du proxy.

## Réinitialiser un mot de passe

```sh
make ctl A="remote-user enroll Alice"
```

Cette commande émet un nouveau lien, qui remplace le précédent. Ouvrir
ce lien pour définir le nouveau mot de passe. Le changement réussi
révoque toutes les sessions du compte et retire ses anciennes passkeys.
Le mot de passe courant fonctionne jusqu'à validation du nouveau.
Il n'existe pas encore de récupération autonome par email.

Pour le compte de test déjà créé :

```sh
make ctl A="remote-user enroll TestUser"
```

## Droits et révocation

Pour créer un compte autorisé sur plusieurs stations :

```sh
make ctl A="remote-user add Bob --role helper --station home-stone --station second-station"
```

Les IDs doivent figurer dans le catalogue du plugin. Chaque station a
son propre rôle : viewer, helper ou admin. Les actions métier et les
données en direct ne sont pas encore raccordées dans cette tranche.

```sh
make ctl A="remote-user list"
make ctl A="remote-user show Alice"
make ctl A="remote-user role Alice --station second-station viewer"
make ctl A="remote-user revoke Alice"
make ctl A="remote-session list"
make ctl A="remote-session revoke ID_SESSION"
make ctl A="remote-session revoke-user Alice"
make ctl A="remote-audit"
```

Changer un rôle révoque les sessions du compte. Révoquer un compte
invalide son mot de passe, ses liens et ses sessions de façon permanente.
Ces commandes passent uniquement par le gRPC opérateur de confiance.

Pour supprimer définitivement un utilisateur actif ou révoqué :

```sh
make ctl A="remote-user purge Alice"
```

`purge` retire le compte, son mot de passe, ses anciennes passkeys, ses
droits, son lien d'inscription, ses sessions et ses challenges en cours.
Le nom peut ensuite être réutilisé pour un nouveau compte, avec un nouvel
identifiant : les anciens liens et sessions restent invalides. Un nom
inconnu renvoie une erreur. L'historique d'audit est conservé selon sa
rétention normale, avec une trace `user.purge`.

## Configuration du plugin

```toml
[[plugin]]
name = "remote-supervision"
enabled = true
capabilities = ["db"]

[plugin.config]
bind = "127.0.0.1:8090"
public_url = "https://remote.homestone.rp-radio.live"
session_ttl_seconds = 43200
enrollment_ttl_seconds = 900
auth_requests_per_minute = 120
password_min_length = 12
password_max_length = 256

[[plugin.config.stations]]
id = "home-stone"
label = "Home Stone"
grpc_endpoint = "http://127.0.0.1:50051"
```

Les valeurs par défaut sont 12 et 256 caractères. Les bornes valides sont
`1 <= password_min_length <= password_max_length <= 256`.
Après modification de ces réglages, redémarrer StationD (`make restart`)
et recharger le formulaire. Les nouvelles bornes concernent la création
et la réinitialisation ; les mots de passe existants restent utilisables.

Les comptes existants sont conservés. Ils peuvent définir leur mot de
passe par un nouveau lien `remote-user enroll`. Le format de stockage
accepte aussi les anciennes identités sans mot de passe.

Compiler et tester avec `make webmin` et `make webmin-test`. Une nouvelle
version du code natif nécessite `make restart` pour être chargée.
Un simple `plugin reload` ne charge pas un nouveau binaire StationD.

## Protection des comptes

- Empreinte Argon2id avec sel aléatoire par mot de passe, jamais de mot de
  passe en clair dans la base ou l'audit. Argon2 effectue aussi une
  vérification factice pour les comptes inconnus ou révoqués.
- 10 tentatives de connexion par nom de compte et par minute ; limite
  supplémentaire par IP de connexion, partagée derrière le proxy.
- Traitement cryptographique hors du runtime HTTP, concurrence bornée.
- Cookies Secure, HttpOnly, SameSite=Strict, renouvelés à la connexion ;
  jetons stockés sous forme d'empreinte et sessions révocables.
- Mutations limitées à l'origine HTTPS configurée, CSRF pour le logout,
  droits contrôlés côté serveur pour chaque station.
- Base privée du plugin, transactions et audit sans secrets, rétention
  de 90 jours. Corps JSON limités à 64 KiB.

## Raccordement HTTPS sur le serveur de développement

Le domaine est couvert par le DNS wildcard vers `82.64.213.101`.
Traefik `192.168.1.94` publie ce nom en HTTPS avec le resolver Cloudflare.
Le fichier dynamique initial est sauvegardé sur Traefik sous
`config.yml.before-webmin-20261007T103641Z`.

Les fichiers de `docker/webmin/` définissent un relais systemd sur
`192.168.1.111:18090` vers le plugin `127.0.0.1:8090`. Le filtre IP
n'autorise que Traefik et localhost. `merge-traefik.py` ajoute uniquement
les entrées du fragment fourni, conserve les autres réglages et crée
une sauvegarde. Adapter les IP et le resolver pour une autre installation.

Vérifications du raccordement : `/login` répond 200 avec certificat
valide, HTTP redirige en HTTPS, `/api/session` sans connexion répond 401.
Le relais répond depuis Traefik et refuse l'accès depuis le poste Windows.

Pour retirer cet accès, désactiver `stationd-webmin.socket`, arrêter
`stationd-webmin.service`, puis retirer les deux routers et le service
`stationd-webmin` de la configuration dynamique Traefik.

Référence du hachage : [Argon2 RustCrypto](https://docs.rs/argon2/0.5.3/argon2/).
