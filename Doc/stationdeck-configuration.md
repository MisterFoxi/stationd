# StationDeck — autorité de configuration et découverte V1

Décision du 17 octobre 2026. **Première verticale implémentée** : backend
persistant, authoring par schéma, publication, pull/report gRPC, découverte IPv4,
cache autonome et consommateur de démonstration. Les premiers adaptateurs réels
sont câblés (SD listener-stats et réglages Icecast), recette à effectuer :
[câblage et limites](stationdeck-daemon-adapters.md). Guide :
[exploitation et recette](stationdeck-config-dev.md). Ce document complète le [contrat](stationdeck-contract.md) et
le [plan](stationdeck-plan.md).

## Noms et responsabilités

- **StationDeck** assure l'authoring : édition, validation, publication et suivi
  des configurations, avec les contributions de ses modules/plugins.
- **Backend StationDeck** : service permanent en container, persistance des
  configurations et distribution gRPC. Fermer le navigateur ne l'arrête pas.
- **StationD 0.5** : référence réutilisée telle quelle, sans réécriture imposée.
- **icecastd** : nom retenu pour le service/container Icecast V1, distinct de
  l'Icecast de la 0.5. Le daemon administratif `station-admin` reste distinct.

Le Compose V1 utilise désormais le service/DNS **`icecastd`** ; les commandes
`make icecast-*`, les chemins de configuration et les tags d'images restent stables.
Le projet Docker reste `stationd-v1-icecast`, pour conserver ses volumes.
La migration d'un banc existant interrompt brièvement les flux de test : voir
[la procédure](v1-icecast.md#migration-du-service-v1-vers-icecastd).
Le Compose historique et les données de la 0.5 ne sont pas modifiés.

## Circuit de configuration

```text
Modules/plugins StationDeck dans la WebUI
                  | authoring autorisé
                  v
Backend StationDeck : brouillons -> validation -> publication versionnée
                  ^
                  | gRPC : configuration propre à la cible,
                  | notification des versions, résultat d'application
         Démon ou adaptateur identifié
                  |
                  +-- StationD 0.5 et ses plugins existants
                  +-- icecastd (configuration Icecast générée)
```

Les démons viennent chercher leur configuration auprès du backend. Ils déclarent
leur identité et leurs capacités ; le serveur détermine leur périmètre autorisé.
Un identifiant déclaré ou un service découvert ne constitue pas une authentification.
La WebUI passe par une API web autorisée ; aucun client tonic natif dans le navigateur.

Le backend conserve durablement les brouillons et versions publiées. Les TOML/Git
existants constituent la source initiale et un format d'import/export ; ils ne
doivent pas devenir un second auteur concurrent de la configuration gérée.
Le stockage initial est un état JSON versionné et remplacé atomiquement dans un
volume privé, protégé par verrou mono-écrivain. L'API permet l'export de l'état et
la soumission de documents validés. L'import automatique des TOML/Git reste à
implémenter ; aucun deuxième auteur concurrent n'est activé.
Les préférences de navigateur restent locales, distinctes des configurations métier.

## Découverte et amorçage

Extension générique préparée le 18 octobre : [découverte des services](stationdeck-service-discovery.md). Elle couvre les interfaces versionnées de tous les services et les futurs services, avec annonces authentifiées, bindings et diagnostics. Son activation réseau et le provisionnement des clés restent soumis à approbation ; le protocole de configuration historique décrit ci-dessous reste actif et compatible.

Priorité : **endpoint explicite FQDN:port**, sinon découverte automatique.
La connexion choisit un profil de transport explicite (TLS et autorité de confiance) ;
le seul nom découvert ne permet pas de faire confiance à un serveur.

La découverte trouve le backend de configuration ; l'échange suivant se fait en
gRPC. gRPC n'offre pas de broadcast natif. Un mécanisme séparé de découverte locale
(broadcast IPv4 versionné, implémenté) annonce le service. Les annonces
ne transportent aucune configuration ni aucun secret. Le FQDN explicite permet de
fonctionner sans découverte locale, notamment entre réseaux Docker ou machines.
La recette en containers inclut un réseau Docker interne sans route par défaut :
l'agent utilise les broadcasts des sous-réseaux connectés. Le backend se découvre
sur le port UDP 50071 puis se contacte en gRPC sur 50070. L'endpoint peut être
fourni par bootstrap ou STATIONDECK_CONFIG_ENDPOINT, prioritaire ; plusieurs
autorités correspondantes bloquent le choix automatique.

Le démon conserve uniquement le minimum d'amorçage local : identité stable,
endpoint optionnel, mode de découverte, références d'identifiants et confiance,
chemin du cache de dernière configuration valide. Ces données sont montées dans
le container ; elles ne dépendent pas d'un navigateur ouvert.
Plusieurs autorités découvertes sans sélection explicite : diagnostic et absence
de choix arbitraire ; conserver la configuration valide déjà appliquée.

## Contrat commun pour tous les plugins

Cette architecture couvre les modules StationDeck et les plugins des démons,
avec des responsabilités distinctes. Un plugin serveur reste exécuté par son
host ; son code ne migre pas dans le navigateur. Ses vues déclaratives existantes
ou un module UI dédié contribuent à l'authoring via les services autorisés.

Chaque domaine de configuration doit disposer de :

- une cible stable (instance de démon, plugin et instance du plugin), un schéma
  versionné et les règles de validation ;
- une contribution d'édition ou un rendu générique lorsque le schéma le permet ;
- permissions de lecture, édition et publication vérifiées côté serveur ;
- dépendances, versions compatibles et capacités nécessaires, vérifiées avant
  publication puis avant application ;
- un adaptateur d'application, avec indication explicite : à chaud, rechargement
  ou redémarrage nécessaire ;
- un retour d'état : version souhaitée, version appliquée, date et erreur expurgée.

Le module gRPC transporte ces contrats ; les règles métier restent dans les
plugins/services concernés. Le backend reste générique grâce à des extensions
validatrices autorisées côté serveur : une validation UI seule ne suffit pas.
Déclarer une capacité ou une permission ne l'accorde pas ; les règles du contrat
StationDeck continuent de s'appliquer à l'authoring et aux identités de service.
Les secrets sont référencés et délivrés aux seules cibles autorisées, jamais
inclus dans une annonce de découverte ou un export public.

Le schéma de configuration d'un plugin serveur et le manifeste d'un module
StationDeck sont deux contrats distincts. Leurs versions doivent être vérifiées ;
les exemples de manifestes actuels ne définissent pas encore ce nouveau protocole.
Un plugin sans schéma/adaptateur compatible est signalé comme non pris en charge,
jamais présenté comme entièrement configurable. La compatibilité complète est
l'objectif, pas une propriété déjà livrée du catalogue actuel.

## Publication, application et continuité

Une édition crée un brouillon. Une publication valide le schéma, les références,
les dépendances et les droits, puis produit une révision immuable. Une édition
concurrente vérifie la révision de départ et signale un conflit.
Chaque cible reçoit uniquement sa configuration autorisée, valide localement,
l'applique puis rapporte la révision effectivement appliquée. Un refus ne doit
pas effacer la dernière configuration valide. Les états souhaité et appliqué
restent distincts ; aucune atomicité globale entre plusieurs démons n'est promise.

Une reconnexion resynchronise les révisions : une notification peut être perdue
sans perdre la configuration publiée. Les délais, flux et reprises sont bornés.
Une configuration publiée ne déclenche pas implicitement une commande métier
comme une bascule ou un arrêt d'antenne.

Backend indisponible : le démon continue avec sa dernière configuration valide,
persistée localement. Au premier démarrage sans configuration valide, il signale
son attente et n'invente aucune configuration de diffusion. Un retour arrière
republie une configuration compatible sous une nouvelle révision traçable.

Pour StationD 0.5, un adaptateur extérieur produit la configuration attendue et
utilise uniquement les mécanismes d'application existants. Même principe pour
icecastd : génération du fichier Icecast et application selon ses possibilités.
Ni le pull gRPC natif par ces programmes ni le rechargement à chaud de tous leurs
plugins ne sont supposés disponibles.

## Validation du renommage icecastd

Le 17 octobre 2026 : images construites en containers, 20 tests ciblés (sauvegarde,
livraison et application), recette isolée avec deux sources/deux auditeurs MP3,
API native et gRPC authentifié. Banc stationdev1 migré : A/B et /radio.mp3 connectés,
sélection B conservée, volumes inchangés. Les tests de compatibilité des anciens
noms d'archives ne constituent pas une restauration complète de chaque archive.
La distribution centrale et le premier agent sont livrés ; cette validation du
renommage reste distincte de la recette de configuration décrite dans le guide.

## Livraison et recette à prévoir

1. Livré : contrats, identités de service/opérateur, schémas simples et bootstrap.
2. Livré : backend persistant, authoring modulaire, validation/publication autorisées.
3. Livré : consommateur de démonstration, cache, version confirmée et rapport d'essai.
4. Livré : broadcast IPv4, endpoint explicite prioritaire, refus d'autorités ambiguës.
5. Câblé, à tester : adaptateurs SD listener-stats et icecastd (guide dédié).
6. À poursuivre : autres réglages/adaptateurs, import des schémas existants,
   champs complexes/secrets, comptes utilisateurs et registre complet.

Les agents resynchronisent actuellement par pull/report toutes les trois secondes,
avec backoff borné après panne ; le flux de notifications reste une évolution.
L'historique est limité sans suppression silencieuse. L'agent ne prétend appliquer
un plugin réel que si son programme local d'application le confirme. Le code
d'erreur cache_failed ne garantit pas l'annulation d'un effet externe déjà réalisé.

Recette en containers : navigateur fermé ; backend coupé puis rétabli ; redémarrage
avec cache et premier démarrage sans cache ; version incompatible ; refus de droits ;
publication concurrente ; erreur d'application et retour arrière ; notification
perdue ; plugin absent ; redémarrage nécessaire ; plusieurs autorités découvertes ;
aucune régression ni modification imposée à StationD 0.5.
