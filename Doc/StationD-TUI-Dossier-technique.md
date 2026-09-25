# StationD — Dossier technique de la TUI

Version 1.0 — 25 septembre 2026  
Statut : spécification de conception, écran par écran  
Socle retenu : Rust et Ratatui

## Sommaire

- [Objet et principes](#1-objet-et-statut-du-document)
- [Carte des écrans](#2-carte-des-écrans)
- [S00 — Structure commune et bandeau](#3-s00--structure-commune)
- [S01 — Direct](#4-s01--direct)
- [S02 — Agenda jour](#5-s02--agenda-jour)
- [S03 — Agenda semaine](#6-s03--agenda-semaine)
- [S04 — Règle de programmation](#7-s04--détail-et-édition-dune-règle)
- [S05 — Playlists](#8-s05--playlists)
- [S06 — Formulaire de playlist](#9-s06--création-et-édition-dune-playlist)
- [S07 — Groupe](#10-s07--édition-dun-groupe)
- [S08 — Médias](#11-s08--médias)
- [S09 — Fiche média](#12-s09--fiche-média)
- [S10 — Diagnostic](#13-s10--diagnostic)
- [S11 — Détail d’erreur](#14-s11--détail-derreur)
- [S12 — Aide et dialogues](#15-s12--aide-et-dialogues-communs)
- [Architecture logicielle](#16-architecture-logicielle-proposée)
- [Contrats de données](#17-contrats-de-données-à-raccorder)
- [Actualisation et persistance](#18-actualisation-résilience-et-persistance)
- [Lots de réalisation](#19-découpage-de-réalisation)
- [Recette](#20-plan-de-recette-transversal)
- [Inventaire préalable du dépôt](#21-points-à-vérifier-dans-le-dépôt-avant-développement)

## 1. Objet et statut du document

Ce dossier décrit la TUI de StationD pour préparer son implémentation : organisation des écrans, composants, navigation, données, événements, états dégradés et critères de recette.

Il reprend les choix discutés dans cette conversation : cinq pages principales, un bandeau permanent avec auditeurs courants, la vue Direct, l’agenda jour/semaine, les playlists et groupes, les médias et le diagnostic. Les maquettes sont des illustrations de conception avec des données fictives. Le présent document fait foi pour le comportement attendu.

**Ce document n’est pas un audit du dépôt courant.** Aucun code source actuel n’a été examiné pour sa rédaction. Les noms de structures, d’opérations et de modules ci-dessous sont des propositions de contrat et d’organisation ; ils ne prouvent pas l’existence d’une API. Les champs TOML doivent être raccordés aux types et au parseur réels, sans créer une grammaire parallèle.

Trois niveaux sont distingués :

| Niveau | Signification |
|---|---|
| Retenu | Choix fonctionnel issu des échanges : TOML, CLI, Ratatui, écrans et bandeau |
| Spécifié | Proposition détaillée par ce dossier pour rendre ces choix implémentables |
| À raccorder | Donnée ou commande dont la disponibilité doit être vérifiée dans StationD |

### 1.1 Principes structurants

- Les TOML restent la source de vérité de la configuration.
- La TUI et `stationctl` emploient les mêmes opérations métier et les mêmes validations.
- Le moteur décide de la programmation, des priorités et des médias à diffuser.
- SQLite conserve son rôle de persistance interne ; la TUI ne contourne pas les opérations métier par des écritures SQL.
- La navigation, les recherches et les aperçus sont sans effet sur la diffusion.
- Les fonctions facultatives restent fournies par des services ou plugins adaptés. Leur absence n’empêche pas d’administrer la station.
- La TUI distingue configuration enregistrée, configuration reconnue par le moteur, prévision et exécution réelle.

### 1.2 Périmètre

Inclus : suivi de diffusion, file préparée, historique, agenda, définition des playlists et groupes, consultation des médias, validation et diagnostic.

Hors périmètre de cette version : publication de podcasts, site public, gestion éditoriale des épisodes, édition audio, console de mixage, géolocalisation et analyses d’audience historiques. Le compteur courant d’auditeurs est inclus, avec un fournisseur de mesure facultatif.

## 2. Carte des écrans

| ID | Écran | Accès | Rôle |
|---|---|---|---|
| S00 | Structure commune | Permanent | Bandeau, onglets, statut, aide contextuelle |
| S01 | Direct | `1` | Exécution réelle et file préparée |
| S02 | Agenda jour | `2` | Chronologie prévisionnelle détaillée |
| S03 | Agenda semaine | `v` dans Agenda | Vue d’ensemble et accès à un jour |
| S04 | Détail / édition d’une règle | Depuis Agenda | Lire ou modifier la programmation |
| S05 | Playlists | `3` | Parcourir les définitions et leur contenu |
| S06 | Création / édition d’une playlist | `n` / `e` | Formulaire et aperçu TOML |
| S07 | Édition d’un groupe | Depuis S05/S06 | Membres, ordre et validation |
| S08 | Médias | `4` | Catalogue, recherche et filtres |
| S09 | Fiche média | Depuis S08 ou Direct | Métadonnées et références |
| S10 | Diagnostic | `5` | État, validations et événements |
| S11 | Détail d’une erreur | Depuis S10 | Cause, localisation et action possible |
| S12 | Aide et dialogues communs | `?` / action contextuelle | Aide, sélection, confirmation, conflits |

Les sous-écrans conservent l’écran d’origine, la sélection et le défilement. `Esc` revient exactement à ce contexte. Les formulaires avec modifications passent par le dialogue de sortie avant de changer de page.

## 3. S00 — Structure commune

### 3.1 Organisation

L’écran se compose, de haut en bas, du bandeau de station, des cinq onglets, de la zone de travail, d’une ligne de statut et d’une ligne de raccourcis contextuels. Les bordures et espacements doivent rester économes en lignes.

| Zone | Hauteur cible | Contenu |
|---|---:|---|
| Bandeau | 2 lignes de contenu | Station, connexion, uptime, auditeurs, horloge, fuseau |
| Onglets | 1 ligne de contenu | Direct, Agenda, Playlists, Médias, Diagnostic |
| Travail | Espace restant | Panneaux de la page active |
| Statut | 1 ligne | Synthèse santé, opération en cours ou résultat |
| Raccourcis | 1 ligne | Actions disponibles pour le panneau actif |

Sur les autres pages que Direct, une ligne compacte « En direct : artiste — titre » peut compléter le bandeau si la hauteur disponible le permet. La vue Direct possède déjà son panneau En cours.

### 3.2 Données permanentes

| Champ | Règle d’affichage |
|---|---|
| Station | Nom de la station ciblée ; ne dépend pas de la sélection d’une playlist |
| Connexion | Connexion au service StationD : connexion, connecté, déconnecté, erreur |
| Uptime | Durée de fonctionnement du service, par exemple `2j 04h 18m` ; pas l’âge de la TUI |
| Auditeurs | Nombre courant fourni par la source de mesure, ou `—` si indisponible |
| Horloge | Heure courante dans le fuseau de station, format 24 heures |
| Fuseau | Identifiant explicite, par exemple `America/Los_Angeles` |
| Diffusion | État distinct de la connexion au service ; connecté ne signifie pas en diffusion |

**Sémantique du compteur.** Le libellé « Auditeurs » représente ici les connexions d’écoute courantes de la station. Il ne promet pas un nombre de personnes uniques. Le fournisseur documente son périmètre : point de montage ou agrégat de points de montage. La TUI ne somme pas arbitrairement plusieurs sources et ne déduplique pas par adresse IP.

La mesure possède son propre horodatage. Une valeur périmée peut rester visible avec `~` et la mention « ancienne » ; une valeur jamais reçue affiche `—`. Zéro signifie une mesure valide de zéro connexion. Une panne du fournisseur d’audience ne change pas l’état de connexion au moteur.

### 3.3 Navigation globale

| Touche | Action hors saisie |
|---|---|
| `1` à `5` | Ouvrir la page correspondante |
| `Tab` / `Shift+Tab` | Panneau suivant / précédent |
| Flèches | Déplacer la sélection dans le panneau actif |
| `PageUp` / `PageDown` | Défilement par page |
| `Home` / `End` | Première / dernière ligne chargée |
| `Entrée` | Ouvrir le détail ou valider le choix courant |
| `Esc` | Fermer le niveau courant ou annuler une saisie |
| `/` | Ouvrir la recherche de la vue |
| `?` | Ouvrir l’aide du contexte |
| `q` | Quitter depuis une vue de consultation |

En saisie, les caractères ordinaires, y compris chiffres et `q`, sont insérés dans le champ. Une boîte modale capture les entrées. Les raccourcis de consultation ne traversent jamais un formulaire actif. Quitter la TUI ne stoppe pas StationD.

### 3.4 Dimensions et style

Objectif de confort : 120 colonnes × 35 lignes. Minimum fonctionnel : 80 × 24. Les seuils exacts dépendent de la place réelle après bordures et doivent être validés au rendu.

- Large : liste et inspecteur visibles ; trois panneaux possibles pour Playlists.
- Compact : inspecteur ouvert en sous-écran ; tableaux réduits aux colonnes essentielles.
- Sous le minimum : message de redimensionnement et possibilité de quitter, sans panique de rendu.
- Redimensionnement : conserver sélection, brouillon et contexte.

Fond sombre, texte clair, cyan pour le focus, vert pour un état sain, ambre pour une projection ou alerte, rouge pour une erreur. Chaque couleur est doublée d’un texte ou d’un marqueur. Aucune police à pictogrammes n’est obligatoire. Les largeurs sont calculées en cellules terminal, pas en octets UTF-8. Un texte tronqué reste consultable dans le détail.

## 4. S01 — Direct

### 4.1 Finalité et disposition

La page montre ce qui se passe réellement : élément diffusé, file préparée et historique des débuts effectifs. Elle n’exécute aucune sélection musicale pour fabriquer un affichage.

| Panneau | Position en mode large | Fonction |
|---|---|---|
| En cours | Pleine largeur, en haut | Élément diffusé et progression |
| À suivre | Gauche, milieu | File préparée dans l’ordre du moteur |
| Historique récent | Gauche, bas | Dernières diffusions, plus récente en premier |
| Détails de sélection | Droite | Explication de l’élément sélectionné |

En mode compact, En cours reste affiché, puis un sélecteur donne accès à la file ou à l’historique. `Entrée` ouvre les détails en pleine zone de travail.

### 4.2 En cours

Champs : artiste, titre, identifiant d’instance de lecture, source active, durée connue, position, temps restant, origine playlist, groupe et membre éventuels, déclencheur de programmation lorsqu’il est exposé par le moteur.

Une source externe peut ne fournir ni durée ni position : afficher « flux continu », sans barre ni reste inventé. Un titre absent utilise le nom de fichier comme repli avec « titre non renseigné ». Aucune métadonnée de l’ancien morceau ne doit être réutilisée silencieusement pour le nouveau.

La progression peut être interpolée localement depuis une position datée avec une horloge monotone. Chaque nouvel état moteur corrige cette estimation. Après déconnexion ou expiration, figer l’affichage et le signaler. La TUI n’anticipe pas elle-même le changement de titre lorsque la barre atteint sa fin.

### 4.3 À suivre

| Colonne | Description |
|---|---|
| Début estimé | Facultatif, précédé de `~` |
| Artiste / titre | Désignation du média |
| Durée | Valeur connue ou `—` |
| Origine | Playlist ; groupe si applicable |
| État | Préparé, indisponible ou autre état réellement exposé |

Les heures proviennent d’une estimation du moteur si elle existe. Une estimation locale n’est permise qu’avec une chaîne complète de durées et des transitions connues ; sinon afficher `—` pour l’élément concerné et les suivants. Ne pas assimiler systématiquement la somme des durées à l’heure de départ en présence de recouvrements ou de flux continus.

La sélection suit l’identifiant d’entrée de file, pas l’indice de ligne. Lorsqu’une entrée disparaît, sélectionner la voisine et annoncer la mise à jour. Aucun rafraîchissement ne replace arbitrairement l’utilisateur en haut.

### 4.4 Historique et explication

L’historique distingue début réel et diffusion terminée si cette information existe. Une entrée ayant démarré puis été interrompue ne doit pas être présentée comme intégralement jouée.

L’inspecteur affiche : titre, état, durée, chemin, playlist, groupe, rang du membre, règle source et motif de sélection. Le texte « Pourquoi ce titre ? » est dérivé des informations de décision fournies par le moteur. S’il manque une trace, afficher « motif non disponible » ; ne pas reconstruire a posteriori un motif plausible.

### 4.5 Actions et états

`Entrée` ouvre le détail ; `p` rejoint la playlist ; `m` rejoint la fiche média. Le retour restaure le contexte Direct.

Le passage au titre suivant est une capacité optionnelle, désactivée tant que le contrat moteur n’est pas disponible. Son dialogue nomme l’instance de lecture visée. La requête inclut cette identité ; si le morceau a changé entre-temps, le serveur refuse l’action obsolète. Un acquittement reçu signifie « commande acceptée » ; seul l’état moteur confirme le changement. Aucun réessai automatique d’une commande mutante.

États particuliers : station arrêtée, file vide, source externe active, historique indisponible, données anciennes. « File vide » ne signifie pas nécessairement « silence » : le moteur peut diffuser un flux externe ou une source de secours.

### 4.6 Recette

- Le compteur d’auditeurs reste visible et indépendant de l’état de la file.
- Un groupe affiche le membre courant et l’origine des membres préparés.
- Un flux sans durée ne produit ni faux compte à rebours ni fausse heure de fin.
- Le rafraîchissement conserve la sélection par identité.
- Naviguer dans cette page n’ajoute aucun média à la file et n’avance aucun curseur.

## 5. S02 — Agenda jour

### 5.1 Disposition

Barre de contexte : date, fuseau, mode Jour/Semaine, plage visible et mention « Prévision ». Chronologie à gauche ; inspecteur de règle à droite ; zone indicative distincte sous la chronologie. En mode compact, les détails s’ouvrent séparément.

La chronologie présente les changements de programme et rendez-vous utiles. Une même base n’est pas répétée à chaque ligne si aucun changement significatif ne le justifie. Une reprise après événement peut être indiquée sans réimprimer toute sa définition.

### 5.2 Sémantique temporelle

| Élément | Représentation |
|---|---|
| Base / plage horaire | Fond de programmation ou segment de validité |
| Horaire fixe | Repère daté avec heure prévue |
| `every` temporel | Repère projeté marqué `~` |
| `every` au compteur de titres | Liste indicative séparée, sans heure artificielle |
| Groupe | Départ prévu et enchaînement ; fin inconnue si non fournie |

La priorité et la résolution des conflits proviennent du moteur. La TUI ne duplique pas son algorithme. Les éventuels conflits ou masquages ne sont expliqués que si le service les expose.

Une plage de validité de règle n’est pas la durée effective de lecture. Une durée totale de pool n’est pas une durée d’émission. Aucun rectangle ne doit suggérer une fin certaine pour un groupe de durée indéterminée : utiliser un repère de départ et un libellé « fin selon contenu ».

La projection `every` dépend du modèle du moteur et de son état initial. Le détail indique les hypothèses reçues. Si l’aperçu sème la cadence au début de la fenêtre, le premier repère est relatif à cette fenêtre ; ne pas le présenter comme une promesse de diffusion à cet instant.

### 5.3 Inspecteur et statistiques de pool

Champs : playlist, type, règle, déclencheur, priorité/poids si applicable, fichier TOML source, position de règle, fenêtre de validité, hypothèses de projection, nombre de médias sélectionnables et somme de leurs durées connues.

Les statistiques sont obtenues en lecture seule : réutiliser les chemins de matérialisation statique/dynamique du domaine et l’agrégation des durées, sans appel à un résolveur de lecture. Aucun changement de curseur, d’état de groupe, de dernier passage ou appel à un plugin à effet de bord.

| Situation | Affichage attendu |
|---|---|
| Pool connu et complet | `128 médias · 08h 12m` |
| Durées partiellement inconnues | `128 médias · ≥ 07h 50m · 4 durées inconnues` |
| Pool vide | `0 média sélectionnable`, alerte explicite |
| Calcul en cours | Indicateur local, navigation toujours disponible |
| Calcul impossible | `Pool indisponible` et détail d’erreur |

Pour un groupe, proposer le détail par membre. Le total global représente l’union des médias par identité, afin d’éviter de compter deux fois un média présent chez plusieurs membres. Si l’API ne fournit que des sommes par membre, les afficher comme telles, sans les rebaptiser total de médias uniques. Ce choix d’agrégation doit être fixé dans le contrat de pool avant raccordement.

### 5.4 Actions et recette

`v` bascule Jour/Semaine ; `[` / `]` passe au jour précédent/suivant ; `t` revient à aujourd’hui ; `g` ouvre le choix de date ; `Entrée` ouvre S04 ; `p` rejoint la playlist ; `e` ouvre l’édition si disponible ; `r` renouvelle l’aperçu.

Recette : règles au compteur toujours hors timeline ; statistiques de pool sans effets de bord ; origine TOML accessible pour une erreur ; groupe sans fausse fin ; réponse d’une ancienne date ignorée après changement de date ; fuseau explicitement visible.

## 6. S03 — Agenda semaine

Sept colonnes représentent les jours de la semaine dans le fuseau de station. Un axe vertical commun représente les heures ; un zoom propose des pas de 15, 30 ou 60 minutes. Seule une plage verticale lisible est affichée, avec défilement.

Les rendez-vous courts sont des repères sélectionnables, même lorsque leur durée est inférieure à une cellule. Plusieurs repères dans la même cellule affichent un compteur et s’ouvrent dans une liste, sans disparition silencieuse. Les libellés tronqués sont développés dans l’inspecteur.

Les règles au compteur sont regroupées sous la grille ou consultables dans un sous-panneau « Indicatif ». Elles ne deviennent pas des rendez-vous du calendrier.

| Action | Effet |
|---|---|
| Gauche / droite | Changer de jour sélectionné |
| Haut / bas | Parcourir les repères ou tranches horaires |
| `Entrée` sur jour | Ouvrir S02 à la date et à l’heure visées |
| `Entrée` sur événement | Ouvrir son détail S04 |
| `[` / `]` | Semaine précédente / suivante |
| `+` / `-` | Modifier le pas de la grille |

En terminal étroit, afficher une liste des sept jours avec synthèse, puis ouvrir le jour choisi. Ne pas forcer sept colonnes illisibles.

Les journées de changement d’heure peuvent avoir 23 ou 25 heures. Le service fournit des instants non ambigus ; l’affichage distingue les heures répétées par leur décalage UTC. La TUI ne fabrique pas sept journées de 24 heures par addition naïve.

Recette : repères simultanés tous accessibles ; changement de semaine stable ; heures ambiguës distinguées ; passage semaine/jour conservant le contexte ; règle sans fin connue affichée sans durée inventée.

## 7. S04 — Détail et édition d’une règle

### 7.1 Consultation

Présenter un résumé lisible, la playlist cible, le déclencheur, les contraintes de calendrier, les paramètres de répétition, la provenance TOML et un aperçu du résultat. Une référence de règle doit rester stable lors d’une simple actualisation ; une position de ligne seule n’est pas une identité durable.

Le détail est accessible même si l’édition n’est pas raccordée. Les champs absents du modèle courant n’apparaissent pas comme des options fonctionnelles.

### 7.2 Formulaire proposé

| Ensemble | Champs selon le type réel de règle |
|---|---|
| Cible | Playlist ou groupe référencé par identité |
| Calendrier | Jours, période, date ou plages autorisées |
| Horaire fixe | Heure dans le fuseau de station |
| Répétition temporelle | Intervalle et ancrage, si pris en charge |
| Répétition par titres | Nombre de titres et règles de comptage du moteur |
| Priorité | Poids ou priorité applicable au modèle |

Le besoin exprimé pour les poids est une valeur par défaut de 15 sur une plage de 0 à 50, 50 étant la valeur la plus haute. Ce domaine doit être confirmé dans le modèle actuel, puis partagé entre CLI et TUI. Ne pas renverser le sens en triant l’affichage.

Les variantes « toutes les 15 minutes » et « à partir de l’heure pleine » sont des intentions distinctes ; le formulaire ne les fusionne pas. Leur encodage est celui du parseur réel. Une borne de validité de règle est explicitement nommée comme telle et ne signifie pas fin garantie du programme.

### 7.3 Validation et sauvegarde

Validation syntaxique, références, valeurs de domaine, puis validation métier par le composant partagé. L’aperçu du brouillon doit valider ce brouillon sans l’appliquer au moteur. Si ce service n’existe pas encore, indiquer que seul l’aperçu de la configuration enregistrée est disponible.

`Ctrl+S` valide et enregistre. Une erreur positionne le focus sur le champ concerné et conserve toutes les saisies. Une sauvegarde réussie invalide les aperçus concernés ; leur recalcul ne vaut pas confirmation d’application moteur.

Recette : encodage relu par le parseur réel ; aucune modification sur `Esc` sans sauvegarde ; conflit externe traité ; absence d’heure de fin obligatoire inventée ; prévisualisation sans mutation de la programmation active.

## 8. S05 — Playlists

### 8.1 Organisation

Trois panneaux en mode large : liste des playlists, contenu ou membres, inspecteur. Deux panneaux sur largeur intermédiaire ; liste puis détails séparés en mode compact.

La liste affiche nom, type, état de validation et éventuellement taille du pool. Les calculs coûteux sont différés ; l’ouverture de la liste ne matérialise pas immédiatement toutes les playlists.

| Type | Panneau central |
|---|---|
| Statique | Médias et ordre déclaré |
| Dynamique | Sources et aperçu des médias sélectionnables |
| Smartblock | Critères et aperçu de leur résultat |
| Groupe | Membres dans leur ordre d’exécution |

Ces catégories sont des présentations fonctionnelles. Leur correspondance aux variantes et discriminants du modèle Rust/TOML doit être vérifiée ; la TUI n’impose pas quatre variantes si le domaine en utilise une autre organisation.

### 8.2 Inspecteur

Nom, identité, fichier source, mode de sélection, critères ou sources, ordre éventuel, validation, statistiques datées du pool et état de configuration. Un groupe montre le lien vers ses membres ; aucune action d’inspection ne déclenche leur résolution en lecture.

États de configuration affichables : brouillon, enregistré, application en attente, appliqué à une révision donnée, rejeté par le moteur, application inconnue. Le statut « appliqué » exige une preuve du service.

### 8.3 Actions

| Touche | Action |
|---|---|
| `/` | Filtrer par nom ou type |
| `n` | Créer une définition |
| `e` | Modifier la sélection |
| `Entrée` | Ouvrir le contenu ou le membre sélectionné |
| `p` | Calculer un aperçu en lecture seule |
| `E` | Ouvrir le TOML dans l’éditeur externe local |

La suppression, si raccordée, est proposée dans un menu explicite. Avant confirmation, afficher les références par groupes et règles connues du domaine. Ne pas supprimer automatiquement les médias associés ni les règles qui la référencent.

États : aucune définition, fichier invalide, référence manquante, pool vide, fichier illisible, fonctionnalité non prise en charge. Un TOML invalide doit rester visible dans la liste avec son chemin pour permettre sa correction.

Recette : groupes visibles ; distinction playlist vide/fichier invalide ; filtrage sans perte de contexte ; aperçu sans effet de bord ; fichier source accessible ; aucun écrasement de champs inconnus.

## 9. S06 — Création et édition d’une playlist

### 9.1 Disposition et cycle

Vue dédiée avec formulaire à gauche et aperçu TOML ou diagnostic à droite. En mode compact, alterner Formulaire/Aperçu. Le bandeau local affiche le fichier cible et l’état du brouillon.

Séquence : choisir un type pris en charge, renseigner les propriétés, définir le contenu, valider, enregistrer, puis afficher séparément l’état de prise en compte par le moteur. Aucun enregistrement implicite lors d’un changement de champ.

### 9.2 Champs

| Partie | Règle |
|---|---|
| Identité | Nom affiché et identité selon le domaine ; ne pas confondre nom et chemin |
| Destination | Chemin TOML dans la racine de configuration autorisée |
| Type | Choix limités aux types acceptés par le modèle actuel |
| Sélection | Mode de lecture pris en charge par ce type |
| Contenu | Médias, sources ou critères selon le type |
| Validation | Erreurs par champ et erreurs globales avec provenance |

Les chemins absolus arbitraires et sorties de racine ne sont pas permis par le simple choix d’un nom. Une définition existante conserve son identité lors d’un changement de libellé, selon les possibilités réelles du domaine.

### 9.3 Éditeur de critères

Chaque critère expose : champ, opérateur, valeur et, lorsque le modèle l’exige, type de valeur. Les choix sont issus du schéma métier. Pour le champ métier `type`, les valeurs proposées proviennent de l’ensemble des types défini dans StationD, pas d’une liste codée dans la TUI.

Les valeurs restent typées : un chemin est une chaîne, un nombre reste numérique, une collection conserve ses éléments. L’interface ne concatène pas une expression libre à partir de valeurs non échappées.

Les opérateurs comme `has` et `in` ne sont pas considérés interchangeables. Leur présence et leur signification dépendent du champ et du parseur partagé. En cas de syntaxe acceptée mais d’exécution non prise en charge, présenter une erreur métier explicite avec le fichier et le critère ; ne pas convertir silencieusement l’opérateur.

L’aperçu affiche le nombre de correspondances, la durée connue et une page de résultats. Toute requête précédente devenue obsolète est ignorée si l’utilisateur modifie le critère entre-temps.

### 9.4 Conservation du TOML et éditeur externe

L’édition structurée doit préserver les commentaires et les champs qu’elle ne modifie pas. Si la stratégie de sérialisation retenue ne permet pas cette conservation pour un document donné, bloquer la réécriture destructive et proposer l’éditeur externe.

L’éditeur externe suspend puis restaure correctement le terminal. À son retour, relire et valider le document. Il s’agit d’un accès local : un chemin du serveur distant ne doit pas être interprété comme un chemin local. L’édition distante attend un contrat explicite de téléchargement/version/sauvegarde et ne fait pas partie du repli local.

Recette : sauvegarde atomique ; relecture par le parseur réel ; conservation des champs non édités ; saisies maintenues après erreur ; formulaire compatible 80 × 24 ; aucun écrasement d’un fichier modifié par ailleurs.

## 10. S07 — Édition d’un groupe

Le groupe contient une liste ordonnée de références vers des playlists. Pour « Homestone Chronicles », l’exemple est Intro → Current episode → Outro. Les noms de cet exemple ne sont pas des valeurs intégrées au programme.

Le panneau central présente rang, nom du membre, type et validité. L’inspecteur détaille le membre sélectionné et son pool. Ajouter ouvre un sélecteur de playlists ; retirer enlève une référence du brouillon, pas le fichier de playlist.

| Action | Effet |
|---|---|
| `a` | Ajouter une référence à la position choisie |
| `d` | Retirer la référence du brouillon |
| `u` / `j` | Monter / descendre le membre sélectionné |
| `Entrée` | Inspecter le membre, sans modifier son contenu |
| `Ctrl+S` | Valider et enregistrer le groupe |

Valider références manquantes, membres vides, contraintes d’imbrication et cycles. Les doublons ou groupes imbriqués ne sont ni autorisés ni interdits arbitrairement par la TUI : appliquer les règles du domaine. Si des groupes imbriqués sont autorisés, détecter les cycles indirects.

Le nombre de médias sélectionnables et la somme de durées ne prédisent pas la durée jouée du groupe. Les informations par membre sont préférables à un total ambigu.

Une modification du groupe pendant sa diffusion ne réécrit pas la file déjà préparée depuis l’interface. L’effet exact dépend du contrat de rechargement moteur ; le résultat d’application doit préciser sa portée lorsqu’elle est connue.

Recette : ordre exact après sauvegarde/relecture ; retrait sans suppression du membre ; référence cassée localisée ; aucun changement de `group_state` pendant édition ou aperçu ; retour à la sélection d’origine.

## 11. S08 — Médias

### 11.1 Catalogue

Barre de recherche et filtres en haut, tableau au centre, inspecteur à droite en mode large. Champs proposés : artiste, titre, durée, type métier, genre, chemin et état connu. La colonne chemin peut être masquée en mode compact.

La recherche porte sur les métadonnées et éventuellement le chemin selon les capacités du catalogue. Les filtres ciblent notamment type, genre, dossier, titre manquant, artiste manquant et fichier signalé indisponible. Les choix de type et de genre viennent du catalogue ou de ses référentiels.

Un compteur distingue nombre total de résultats et nombre de lignes chargées. Pagination et tri s’appliquent à l’ensemble de la requête côté service, pas uniquement à la page courante. Un second critère stable, comme l’identifiant média, évite les déplacements aléatoires entre pages de résultats équivalents.

### 11.2 Actions et limites

`/` ouvre la recherche ; `f` ouvre les filtres ; `s` choisit le tri ; `Entrée` ouvre S09. La sélection ne lance pas de lecture. La sélection d’un média depuis un formulaire peut renvoyer sa référence au brouillon appelant.

Cette version consulte les métadonnées. Elle ne modifie pas les tags MP3 et ne lance pas automatiquement de scan des fichiers. Une future action de réindexation devra exposer sa portée, son avancement et son résultat par le service dédié.

La disponibilité est celle de la dernière observation du catalogue, avec son âge. Éviter de parcourir le stockage NFS ou de tester chaque fichier à chaque rafraîchissement de tableau.

Recette : recherche réactive sur un grand catalogue ; chaîne vide distincte d’une valeur absente si le domaine le permet ; texte long consultable ; fichier manquant non supprimé automatiquement ; pagination stable ; références correctes au retour d’un sélecteur.

## 12. S09 — Fiche média

Vue de détail structurée, défilable :

| Bloc | Champs |
|---|---|
| Identité | ID média, artiste, titre, album si connu |
| Technique | Durée, format, autres propriétés réellement indexées |
| Classement | Type métier, genres et tags exposés |
| Fichier | Chemin complet, disponibilité observée et date de contrôle |
| Références | Appartenances statiques connues et origine de la consultation |

Les valeurs présentées sont celles du catalogue StationD. La fiche ne prétend pas montrer séparément ID3v1 et ID3v2 si cette provenance n’est pas stockée.

L’appartenance statique et la correspondance à une règle dynamique sont deux informations différentes. Pour une playlist dynamique, une correspondance n’est affichée qu’après évaluation explicite en lecture seule, avec la définition utilisée. Ne pas annoncer « aucune playlist » en se fondant uniquement sur l’absence d’association statique.

Si la fiche vient de Direct, conserver un bloc de contexte de diffusion : entrée de file ou d’historique, origine et règle connues. Si elle vient du catalogue, ce bloc peut être absent.

Recette : titre absent identifié clairement ; chemin complet accessible ; aucune lecture audio automatique ; retour à la même ligne du catalogue ou de la file ; références dynamiques non déduites d’associations statiques.

## 13. S10 — Diagnostic

### 13.1 Organisation

Trois sous-vues : État, Validation, Événements. Les actions de diagnostic sont des lectures ; démarrer ou arrêter un service n’est pas une conséquence d’un changement de sous-vue.

| Sous-vue | Contenu |
|---|---|
| État | Service StationD, diffusion, stockage/catalogue et fournisseur d’audience, selon les sondes disponibles |
| Validation | Fichiers valides/invalides, références cassées, révisions enregistrées et appliquées |
| Événements | Journal borné avec filtres de niveau, composant et recherche |

Chaque ligne d’état possède : composant, état, message court et horodatage. Les états inconnus et non configurés sont distingués des pannes. Un composant facultatif absent peut être « non configuré » sans dégrader artificiellement toute la station.

### 13.2 Événements

Afficher heure, niveau, composant et message. Le suivi automatique du bas de liste est actif à l’ouverture. Remonter manuellement suspend ce suivi ; un compteur annonce les nouveaux événements. Une action explicite réactive le suivi.

Le tampon est borné ; son écrêtage est signalé. Une coupure de flux ajoute une indication de discontinuité. Après reconnexion, ne pas inventer les événements perdus. Les données sensibles déjà identifiées par le service sont masquées, notamment secrets, mots de passe et jetons présents dans des URL.

### 13.3 Actions et recette

`Entrée` ouvre S11 ; `/` filtre ; `r` relit l’état ou relance la validation en lecture seule ; `f` active/désactive le suivi dans Événements ; `E` ouvre localement le fichier source si la localisation est disponible.

Recette : moteur connecté et diffuseur en panne affichés séparément ; fichier invalide identifiable ; historique d’événements borné ; erreurs sans secrets ; échec d’une sonde n’empêchant pas la consultation des autres.

## 14. S11 — Détail d’erreur

Le détail doit permettre d’agir sans deviner quel TOML est concerné. Les informations attendues sont : catégorie, message, opération, fichier, chemin de propriété ou identifiant de règle, ligne/colonne si connues, cause technique et instant du constat.

| Catégorie | Présentation |
|---|---|
| Syntaxe | Localisation et message du parseur |
| Valeur invalide | Champ, valeur attendue, valeur reçue si affichable |
| Référence | Identité recherchée et définition qui la référence |
| Fonction non prise en charge | Opération ou opérateur concerné, sans le présenter comme panne réseau |
| Infrastructure | Lecture disque, accès, transport ou service indisponible |
| Conflit | Révision éditée et révision désormais présente |

Exemple de présentation, sans imposer un format de message au moteur : fichier `playlists/evening.toml`, critère n°2, champ `path`, erreur « chaîne attendue ». Une ligne exacte inconnue reste inconnue ; la TUI ne fabrique pas une position.

Une erreur longue défile. L’action « Ouvrir la source » n’est proposée que pour une source connue et accessible. Revenir conserve les filtres et la sélection de Diagnostic.

Recette : toute erreur de validation associée à une définition conserve sa provenance ; distinctions validation/infrastructure/non pris en charge ; copie ou consultation complète du message possible sans tronquage irréversible.

## 15. S12 — Aide et dialogues communs

### 15.1 Aide

L’aide affiche les raccourcis du contexte et les conventions : `~` prévision ou ancienne mesure selon son libellé, `—` inconnu, distinction pool/durée de programme, enregistré/appliqué. Les raccourcis indisponibles sont masqués ou expliqués comme indisponibles.

### 15.2 Dialogues

| Dialogue | Informations et résultat |
|---|---|
| Sélecteur | Recherche, liste, choix unique ou multiple selon l’appelant |
| Sortie de brouillon | Enregistrer, abandonner les modifications, continuer l’édition |
| Action sur la diffusion | Objet exact et effet ; annulation sélectionnée par défaut |
| Suppression | Définition visée et références connues ; aucun média supprimé |
| Conflit de fichier | Modifié depuis l’ouverture ; comparer, recharger ou conserver le brouillon |
| Résultat de sauvegarde | Révision enregistrée et état d’application connu |

Un conflit ne propose pas un écrasement automatique. Le brouillon peut être conservé pour comparaison ; recharger nécessite d’expliciter la perte des modifications locales. Les notifications de succès disparaissent après un court délai proposé de quatre secondes ; une erreur importante reste consultable dans le contexte et le diagnostic.

Une modale possède un focus unique et un ordre de tabulation défini. `Esc` annule l’action en cours. Aucun clic ou raccourci ne traverse la modale vers une action de diffusion sous-jacente.

## 16. Architecture logicielle proposée

### 16.1 Répartition des responsabilités

| Couche | Responsabilité | Interdiction |
|---|---|---|
| Rendu Ratatui | Transformer l’état en composants visuels | Accès réseau/disque dans la fonction de rendu |
| État d’interface | Page, focus, sélection, brouillons, cache d’affichage | Résoudre le prochain média |
| Gestion des actions | Transformer une entrée en action de lecture ou commande explicite | Déclencher une mutation lors d’un simple rafraîchissement |
| Adaptateurs | Transport, fichiers locaux, conversion en modèles de vue | Dupliquer la grammaire et les validations |
| Domaine partagé / services | Validation, persistance, programmation, décisions | Dépendre du dessin des widgets |

L’appel CLI ou TUI doit arriver au même cas d’usage métier. La TUI ne parse pas la sortie humaine de `stationctl status` : elle consomme la structure dont cette sortie est issue, directement ou par transport structuré.

Le repli initial d’écriture locale des TOML est possible tant que le chemin de sauvegarde distant n’existe pas. Il réutilise les types et validateurs partagés et annonce honnêtement « enregistré ; application moteur inconnue » si aucun acquittement n’est disponible.

### 16.2 Modules suggérés

Les noms suivants sont indicatifs ; ils seront adaptés à l’arborescence existante :

| Module | Contenu |
|---|---|
| `app` | État global, routage des pages, cycle de vie du terminal |
| `actions` | Actions typées et règles de dispatch |
| `screens/direct` | En cours, file, historique et inspecteur |
| `screens/agenda` | Jour, semaine, détail et édition de règle |
| `screens/playlists` | Catalogue de définitions et formulaires |
| `screens/media` | Recherche et fiche |
| `screens/diagnostics` | Santé, validation et événements |
| `components` | Bandeau, tableaux, champs, sélecteurs, dialogues |
| `data` | Modèles de vue et adaptateurs aux contrats existants |
| `theme` | Couleurs, densité et variantes compactes |

État global minimal : page active, pile de retour, connexion, données du bandeau, états par écran, brouillon actif, modale, notifications et requêtes en cours. Les identités de sélection sont séparées de leurs positions visuelles.

### 16.3 Boucle d’événements

Les entrées clavier, redimensionnements, réponses du service et temporisations arrivent dans une boucle qui met à jour l’état. Les opérations bloquantes sont exécutées hors rendu. Le rendu est déclenché par un changement visible ou par le tick nécessaire à l’horloge/progression.

Chaque requête de consultation porte un identifiant et une clé de contexte : station, écran, filtre, intervalle temporel et révision utile. Une réponse à une ancienne recherche ne remplace pas la recherche actuelle. La fermeture d’un écran peut annuler une lecture ; elle ne doit pas rejouer ou inverser une commande déjà envoyée.

Les abonnements nécessaires au bandeau restent actifs. Les lectures coûteuses de pages masquées sont suspendues, tout en conservant leur dernier état visible et sa date.

## 17. Contrats de données à raccorder

Les noms ci-dessous sont des **modèles de vue proposés**, pas des messages protobuf déclarés existants. Avant implémentation, produire une table de correspondance avec les structures Rust, commandes et RPC présents.

| Modèle proposé | Champs utiles |
|---|---|
| `StationSnapshot` | Identité/nom, fuseau, uptime, état du service, état de diffusion, instant d’observation |
| `AudienceSnapshot` | Compteur optionnel, périmètre, fournisseur, instant d’observation, état de fraîcheur |
| `PlaybackSnapshot` | ID d’instance, média ou flux, position datée, durée optionnelle, origine, groupe/membre, état |
| `QueueSnapshot` | Révision, entrées avec ID stable, média, durée, origine et départ estimé optionnel |
| `HistoryPage` | Entrées de diffusion, début réel, fin/issue si connues, curseur de pagination |
| `AgendaView` | Fenêtre, fuseau, révision, occurrences, indicatif, hypothèses et erreurs localisées |
| `PoolStats` | Compte, durée connue, durées inconnues, caractère complet, sémantique d’agrégation, révision |
| `DefinitionDocument` | Identité, chemin, texte TOML, révision, validation et état d’application |
| `MediaPage` | Critères, tri stable, résultats, total si connu et pagination |
| `DiagnosticEvent` | Identité, horodatage, niveau, composant, message et provenance |
| `Capabilities` | Opérations disponibles et limites du service raccordé |

Le contrat d’aperçu discuté précédemment, `PreviewResponse { occurrences, indicative }`, est la référence fonctionnelle à vérifier dans le dépôt. Les occurrences forment une timeline monotone ; l’indicatif contient notamment les règles au compteur. L’interface ne fusionne pas ces collections pour leur attribuer à toutes une heure.

### 17.1 Typage et absence de données

- Durées exprimées dans une unité explicite, idéalement millisecondes au transport ; conversion seulement à l’affichage.
- Instants absolus et fuseau explicite pour les conversions de calendrier.
- Identités stables pour média, entrée de file, diffusion, règle et définition lorsque disponibles.
- Valeur optionnelle pour l’inconnu : ne pas surcharger zéro ou chaîne vide.
- Erreur structurée avec provenance ; éviter de devoir extraire le chemin d’un message libre.
- Fraîcheur propre à chaque source ; un bandeau récent ne garantit pas un historique récent.

### 17.2 Opérations fonctionnelles

| Opération | Mode | Consommateurs |
|---|---|---|
| Lire/suivre état de station | Lecture | Bandeau, Direct, Diagnostic |
| Lire audience courante | Lecture facultative | Bandeau |
| Lire/suivre lecture, file et historique | Lecture | Direct |
| Prévisualiser la programmation | Lecture sans effets métier | Agenda |
| Matérialiser un pool / lire ses statistiques | Lecture sans effets métier | Agenda, Playlists |
| Lister/lire/valider une définition | Lecture | Agenda, Playlists, Diagnostic |
| Enregistrer une définition avec révision attendue | Mutation explicite | Formulaires |
| Lire l’état d’application d’une révision | Lecture | Formulaires, Diagnostic |
| Chercher/lire les médias | Lecture | Médias et sélecteurs |
| Lire/suivre les événements | Lecture | Diagnostic |
| Passer le titre courant | Mutation optionnelle protégée par ID d’instance | Direct |

Les écritures utilisent le chemin prévu par StationD. Si le moteur surveille automatiquement les TOML, la TUI observe le résultat de ce rechargement ; elle n’ajoute pas une deuxième application concurrente. Si une commande d’application explicite est requise, sa sémantique doit être exposée et partagée avec la CLI.

## 18. Actualisation, résilience et persistance

### 18.1 Valeurs de départ proposées

Ces valeurs sont configurables et ne constituent pas des caractéristiques mesurées du système actuel.

| Donnée | Stratégie initiale |
|---|---|
| Horloge / progression visible | Tick local de 1 seconde |
| État moteur / lecture / file | Abonnement si disponible ; sinon lecture toutes les 2 secondes |
| Audience | Lecture toutes les 5 secondes ; ancienne après 15 secondes sans observation valide |
| Santé générale | Lecture toutes les 5 secondes |
| Recherche média | Déclenchement après 250 ms sans nouvelle frappe |
| Agenda | Sur changement de fenêtre/révision ou actualisation demandée |
| Pools | À la demande, cache par définition et révision du catalogue |
| Historique | Chargement initial puis actualisation sur événement de lecture ou lecture périodique modérée |

Les intervalles d’audience tiennent compte de la fréquence réelle du fournisseur. La date d’observation distante, et pas seulement la date de réception, sert à déterminer si une mesure est ancienne.

Pas de requêtes identiques concurrentes. Les timeouts restent configurables ; point de départ proposé : cinq secondes pour une lecture simple. Les calculs de pool plus longs disposent d’un état d’avancement ou d’attente distinct.

### 18.2 Déconnexion

Conserver les dernières données avec leur âge, désactiver les commandes de diffusion et retenter les lectures selon un délai progressif plafonné. Une reprise recharge un état cohérent avant de réactiver les commandes. L’édition d’un fichier local accessible peut rester possible ; son application au moteur est alors inconnue.

L’état « demande envoyée, réponse perdue » d’une mutation exige une réconciliation par lecture ou identifiant d’opération. Ne pas rejouer la sauvegarde ou le saut de titre simplement parce que le transport a expiré.

### 18.3 Sauvegarde des définitions

Conserver une révision initiale ou empreinte à l’ouverture. Au moment d’écrire, vérifier la précondition puis effectuer une écriture atomique dans le même système de fichiers, avec permissions adaptées. Une erreur conserve le brouillon.

Le remplacement atomique évite un fichier partiellement écrit, mais ne suffit pas à prévenir deux auteurs concurrents. La vérification de révision et l’écriture doivent être sérialisées par le service ou par un verrou partagé avec tous les écrivains concernés. Si cette garantie manque dans le repli local, la limite doit être explicitée et un conflit détecté doit bloquer l’écrasement.

Le statut appliqué fait référence à la révision que le moteur a reconnue. Si une édition plus récente existe déjà, l’acquittement d’une ancienne révision ne valide pas la nouvelle.

## 19. Découpage de réalisation

Les lots adaptent l’existant ; ils ne présument pas qu’il faut réécrire les écrans déjà présents.

| Lot | Livrable | Condition de fin |
|---|---|---|
| 0 | Inventaire du code et table de raccordement | Chaque donnée/action classée existante, à enrichir ou indisponible |
| 1 | Structure commune, focus, dimensions, bandeau et audience | Navigation stable et états inconnus correctement rendus |
| 2 | Playlists, création/édition, groupes, validation locale | Sauvegarde/relecture fidèle, conflits traités |
| 3 | Agenda jour/semaine et inspecteur | Projections distinctes, pools en lecture seule, dates/fuseaux corrects |
| 4 | Direct raccordé aux états réels | File, historique et progression cohérents ; commandes optionnelles séparées |
| 5 | Médias et diagnostic | Catalogue paginé, erreurs localisées, journal borné |
| 6 | Intégration et recette terminal | Modes compact/large, déconnexions et modifications concurrentes validés |

L’édition de règles de l’agenda dépend de la disponibilité du contrat de sauvegarde de grille. Elle peut être livrée après la consultation sans bloquer celle-ci. Le compteur d’audience peut être raccordé indépendamment par son fournisseur.

## 20. Plan de recette transversal

### 20.1 Tests de logique ciblés

| Risque | Vérification |
|---|---|
| Raccourci interceptant une saisie | `1`, `q` et `/` restent du texte dans un champ |
| Sélection déplacée par rafraîchissement | Conservation par ID malgré insertion/retrait de lignes |
| Réponse ancienne écrasant un filtre récent | Rejet d’une réponse à contexte obsolète |
| Lecture avec effets de bord | Aperçu laissant inchangés curseurs, groupe et dernier passage |
| Groupe compté plusieurs fois | Sémantique du pool respectée pour médias partagés |
| Inconnu présenté comme zéro | Audience/durée/position absentes rendues explicitement |
| Sauvegarde écrasant une édition externe | Précondition de révision et conflit correctement traités |
| Révision mal acquittée | Ancien acquittement ne validant pas un brouillon plus récent |
| Mauvaise commande après changement de titre | Refus d’un saut portant un ID d’instance périmé |
| Journée civile atypique | Aperçus autour des changements d’heure et à cheval sur minuit |

### 20.2 Recette intégrée et visuelle

Scénario représentatif : consulter une station active, modifier une playlist, valider un groupe Intro/Épisode/Outro, consulter son aperçu, vérifier les statistiques de pool, revenir au Direct et retrouver la file réelle. Ouvrir ensuite un TOML volontairement invalide et retrouver immédiatement sa provenance dans Diagnostic.

Exécuter ce scénario en terminal 120 × 35 puis 80 × 24, avec noms longs et accents, redimensionnement pendant une saisie, perte de connexion, fournisseur d’audience absent, pool vide et flux externe sans durée. Vérifier enfin que quitter la TUI restaure le terminal et laisse StationD fonctionner.

Le contrôle visuel vérifie lisibilité, absence de recouvrement, focus visible et raccourcis exacts. Les tests du domaine restent la référence pour la grammaire et les priorités ; la TUI teste son raccordement et ne réimplémente pas ces règles dans ses propres fixtures.

## 21. Points à vérifier dans le dépôt avant développement

1. Modules TUI actuels, backend terminal et organisation des commandes de `stationctl`.
2. Source structurée de `stationctl status`, uptime, état réel de diffusion et fuseau.
3. Source du compteur courant d’auditeurs, périmètre de mesure et horodatage.
4. Identifiants et contrats existants pour lecture en cours, file et historique.
5. Trace disponible du motif de sélection et des membres de groupe.
6. Forme actuelle de `PreviewResponse`, hypothèses des règles `every` et diagnostics de provenance.
7. Matérialisation en lecture seule des pools et stratégie d’agrégation des groupes.
8. Modèle TOML réel, opérateurs de filtre, référentiel des types et préservation des champs/commentaires.
9. Règles d’imbrication des groupes, portée d’une modification pendant une diffusion.
10. Mode réel de rechargement, preuve d’application et mécanisme de révision/concurrence.
11. Recherche paginée des médias, erreurs structurées et source des événements.
12. Distinction des capacités locales/distantes et comportement en cas de fonction non prise en charge.

Le résultat attendu de cet inventaire est une table de raccordement concrète : écran → donnée/action → symbole Rust ou RPC → extension nécessaire → critère de recette. Les propositions de ce dossier deviennent alors des tâches d’implémentation sans inventer d’API ni modifier implicitement les règles de StationD.
