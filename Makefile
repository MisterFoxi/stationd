# Raccourcis de dev — à lancer depuis la racine du dépôt, sur l'hôte
# (devstationd). Tout passe par le conteneur de dev (docker compose, README
# « Run (Docker) »). `make` ou `make help` liste les cibles.
#
#   make webmin                       # StationD avec le plugin natif Webmin
#   make webmin-test                  # tests ciblés du socle Webmin
#   make plugins                      # tous les plugins WASM
#   make plugins P=stop-when-idle-wasm
#   make ctl A="station state"        # n'importe quelle commande stationctl
#   make restart                      # cargo build + relance de stationd seul
#
# Profil : sur la branche git `main`, build / restart / tui compilent en
# release (optimisé, sans debug) ; ailleurs en debug. Forcer : PROFILE=release
# ou PROFILE=debug. `target/active` pointe sur le profil du dernier build :
# c'est lui que s6 lance ($STATIOND_BIN) et que `make ctl` appelle.

# Préfixe de recette « > » au lieu de la tabulation (copier-coller sûr).
.RECIPEPREFIX := >

DC      ?= docker compose
SVC     ?= station
# P = un plugin (nom du dossier sous plugins/), vide = tous.
P       ?=
# A = arguments de stationctl (make ctl A="…").
A       ?= status
# VM = cible SSH de livraison (alias ~/.ssh/config ou utilisateur@hôte).
VM      ?=
export VM
# T = arguments de stationd-tui (make tui T="…").
T       ?=
# Script Liquidsoap généré, vu du conteneur ([liquidsoap] script_path).
LIQ     ?= /src/data/station.liq
WASM    := wasm32-unknown-unknown
# Branche du dépôt (sur l'hôte) → profil de compilation.
BRANCH  := $(shell git rev-parse --abbrev-ref HEAD 2>/dev/null)
PROFILE ?= $(if $(filter main,$(BRANCH)),release,debug)
ifeq ($(filter $(PROFILE),release debug),)
$(error PROFILE=$(PROFILE) : release ou debug)
endif
CARGO_PROFILE = $(if $(filter release,$(PROFILE)),--release)

EXEC      = $(DC) exec -u dev $(SVC)
EXEC_ROOT = $(DC) exec $(SVC)
# stationctl du dépôt (target/active = dernier build), hors PATH dans l'image de dev.
CTL       = $(EXEC) sh -c '"$$STATIOND_BIN/stationctl" "$$@"' stationctl

.DEFAULT_GOAL := help
.PHONY: help up down image logs shell build release test clippy fmt plugins player-test uploader-test webmin webmin-test \
        all restart restart-ls restart-icecast restart-air check-liq tui ctl \
        state stop start package dist mrproper

help: ## Liste des cibles
> @grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | \
>   awk -F':.*## ' '{ printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2 }'

# --- conteneur ---------------------------------------------------------------
up: ## Démarre le conteneur de dev
> $(DC) up -d

down: ## Arrête le conteneur de dev
> $(DC) down

image: ## Reconstruit l'image de dev et recrée le conteneur (après docker/ ou .env)
> $(DC) build
> $(DC) up -d --force-recreate

logs: ## Suit les logs du conteneur
> $(DC) logs -f $(SVC)

shell: ## Shell dans le conteneur (utilisateur dev)
> $(DC) exec -it -u dev $(SVC) bash

# --- compilation / tests -----------------------------------------------------
build: ## cargo build (stationd + stationctl + plugins natifs, dont Webmin) : release sur main, debug ailleurs (PROFILE=…) ; target/active → ce profil
> @echo "== profil $(PROFILE) (branche $(or $(BRANCH),?))"
> $(EXEC) sh -c 'set -e; cargo build $(CARGO_PROFILE); ln -sfn $(PROFILE) /src/target/active'

webmin: build ## Compile Webmin et stationd-tui (sans redémarrage)
> $(EXEC) cargo build $(CARGO_PROFILE) -p stationd-tui

player-test: ## Tests du plugin natif station-player dans le conteneur de dev
> $(EXEC) cargo test --locked -p station-player

uploader-test: ## Tests de réception, tagging, scan et lecture seule du plugin média
> $(EXEC) cargo test --locked --lib upload -- --include-ignored

webmin-test: ## Tests ciblés Webmin dans le conteneur de dev
> $(EXEC) cargo test --locked --lib webmin_

release: ## cargo build --release --locked
> $(EXEC) cargo build --release --locked

test: player-test ## cargo test --locked (daemon et station-player)
> $(EXEC) cargo test --locked

clippy: ## cargo clippy --all-targets
> $(EXEC) cargo clippy --locked --all-targets

fmt: ## cargo fmt --check
> $(EXEC) cargo fmt --check

plugins: ## Plugins WASM (tous, ou P=<dossier>)
> $(EXEC) sh -c 'set -e; \
>   if [ -n "$(P)" ]; then set -- /src/plugins/$(P)/Cargo.toml; else set -- /src/plugins/*/Cargo.toml; fi; \
>   for m in "$$@"; do \
>     [ -f "$$m" ] || { echo "pas de plugin: $$m" >&2; exit 1; }; \
>     case "$$m" in */station-player/Cargo.toml) if [ -n "$(P)" ]; then echo "plugin natif: utiliser make build ou make player-test" >&2; exit 1; fi; continue;; esac; \
>     echo "== $${m%/Cargo.toml}"; \
>     cargo build --release --locked --target $(WASM) --manifest-path "$$m"; \
>   done; \
>   ls -l /src/plugins/$(or $(P),*)/target/$(WASM)/release/*.wasm'

all: build plugins test ## build + plugins + test

tui: ## Compile et lance la TUI (même profil que build) ; options : make tui T="--theme Nord"
> $(EXEC) cargo build $(CARGO_PROFILE) -p stationd-tui
> $(DC) exec -it -u dev $(SVC) ./target/$(PROFILE)/stationd-tui $(T)

# --- services s6 -------------------------------------------------------------
restart: build ## cargo build puis relance de stationd seul, attend qu'il soit prêt (l'antenne n'est pas coupée)
> $(EXEC_ROOT) s6-svc -T 30000 -wR -r /run/service/stationd

restart-ls: ## Relance Liquidsoap (après changement du .liq)
> $(EXEC_ROOT) s6-svc -r /run/service/liquidsoap

restart-icecast: ## Relance Icecast (après changement d'icecast.xml)
> $(EXEC_ROOT) s6-svc -r /run/service/icecast

restart-air: restart restart-icecast restart-ls ## stationd (réécrit .liq / icecast.xml) puis Icecast et Liquidsoap

check-liq: ## liquidsoap --check du script généré (LIQ=…)
> $(EXEC_ROOT) liquidsoap --check $(LIQ)

# --- stationctl --------------------------------------------------------------
ctl: ## stationctl A="…" (défaut : status)
> $(CTL) $(A)

state: ## stationctl station state
> $(CTL) station state

stop: ## Arrêt opérateur (stationctl station stop ; FORCE=1 pour --force)
> $(CTL) station stop $(if $(FORCE),--force)

start: ## Relance après un arrêt opérateur (stationctl station start)
> $(CTL) station start

# --- nettoyage --------------------------------------------------------------
mrproper: ## Grand ménage : cargo clean (dépôt + plugins), puis conteneur, image et volumes Docker du projet supprimés (make image pour repartir)
> @echo "== cargo clean (dépôt et plugins, dans le conteneur s'il tourne)"
> -$(EXEC) sh -c 'cargo clean; for m in /src/plugins/*/Cargo.toml; do [ -f "$$m" ] && cargo clean --manifest-path "$$m"; done; true'
> @echo "== docker : conteneur, image et volumes du projet (target, cargo-registry)"
> $(DC) down --volumes --rmi all --remove-orphans

# --- livraison ---------------------------------------------------------------
package: ## Package release puis transfert et installation si VM=<cible SSH> (ARGS=--allow-dirty pour un arbre modifié)
> docker/package.sh $(ARGS)
dist: package ## Alias de package : make dist VM=<cible SSH>

# --- V1 : Icecast commun, aucun démarrage de StationD -------------------------
ICECAST_DC = docker compose -f docker/icecast/compose.yaml
.PHONY: icecast-init icecast-up icecast-test icecast-logs icecast-down

icecast-init: ## Initialise les secrets locaux Icecast (idempotent)
> python3 docker/icecast/init-secrets.py

icecast-up: ## Démarre Icecast et son administration V1, sans StationD
> $(ICECAST_DC) up -d --build --wait admin

icecast-test: ## Deux sources et deux auditeurs synthétiques, sans StationD
> $(ICECAST_DC) --profile test run --build --rm smoke

icecast-logs: ## Suit les logs du serveur Icecast commun
> $(ICECAST_DC) logs -f icecastd

icecast-down: ## Arrête uniquement le banc Icecast V1
> $(ICECAST_DC) down

.PHONY: icecast-ctl icecast-reconfigure
icecast-ctl: ## Administration centrale gRPC : make icecast-ctl A="status"
> $(ICECAST_DC) exec -T admin stationctl icecast $(A)

icecast-reconfigure: ## Valide et recharge Icecast, admin et les services de test actifs
> python3 docker/icecast/apply-config.py --reload

.PHONY: icecast-continuity-test
icecast-continuity-test: ## Vérifie le flux pendant un arrêt/reprise de l'administration
> python3 tests/icecast-admin-continuity.py

.PHONY: icecast-live-up icecast-live-down
icecast-live-up: ## Lance deux sources synthétiques continues, sans StationD
> $(ICECAST_DC) --profile live up -d --build live-a live-b

icecast-live-down: ## Arrête les deux sources continues, garde Icecast et admin
> $(ICECAST_DC) --profile live stop live-a live-b

.PHONY: icecast-switch-up icecast-switch-down
icecast-switch-up: icecast-init ## Lance A/B et le routeur sur /radio.mp3, sans StationD
> $(ICECAST_DC) --profile live --profile switch up -d --build icecastd admin live-a live-b router

icecast-switch-down: ## Arrête le routeur de bascule et les deux sources de test
> $(ICECAST_DC) --profile live --profile switch stop router live-a live-b

.PHONY: icecast-switch-test
icecast-switch-test: ## Valide A/B sur le même auditeur et la continuité du routeur
> python3 tests/icecast-switch.py

# CONFIG = optional candidate TOML, separate from the versioned canonical file.
.PHONY: icecast-check icecast-apply icecast-geography
icecast-check: ## Valide schéma, secrets et MMDB sans redémarrer
> python3 docker/icecast/apply-config.py --check $(if $(CONFIG),--config "$(CONFIG)")

icecast-apply: ## Applique le TOML avec sauvegarde et retour arrière sur échec
> python3 docker/icecast/apply-config.py $(if $(CONFIG),--config "$(CONFIG)")

icecast-geography: ## Répartition géographique courante des auditeurs publics
> $(ICECAST_DC) exec -T admin stationctl icecast geography

.PHONY: icecast-geoip-update icecast-geoip-reload
icecast-geoip-update: ## Met à jour GeoLite2 City sous data/geoip (identifiants MaxMind interactifs)
> sh scripts/update-geolite2.sh ./data/geoip/GeoLite2-City.mmdb
> python3 docker/icecast/apply-config.py --reload

icecast-geoip-reload: ## Valide la base MMDB et recharge les services concernés
> python3 docker/icecast/apply-config.py --reload

.PHONY: icecast-geography-test
icecast-geography-test: ## Test proxy déclaré/non déclaré et GeoLite dans un projet isolé
> python3 tests/icecast-geography.py

.PHONY: icecast-traefik-test icecast-history
icecast-traefik-test: ## DNS/TLS/Traefik réels, IP effective et décodage MP3
> python3 tests/icecast-traefik.py

icecast-history: ## Historique géographique du plugin administratif
> $(ICECAST_DC) exec -T admin stationctl icecast history $(H)

.PHONY: icecast-history-test
icecast-history-test: ## Persistance réelle et continuité pendant une recréation administrative
> python3 tests/icecast-history.py

.PHONY: icecast-backup icecast-backup-test icecast-restore-test
icecast-backup: ## Sauvegarde privée, SQLite cohérent, sans interruption
> python3 docker/icecast/backup.py create

icecast-backup-test: ## Tests de validation et d'extraction des archives
> python3 -B tests/icecast-backup.py

icecast-restore-test: ## Restauration isolée sans ports publiés (BACKUP=archive.tar.gz)
> @test -n "$(BACKUP)" || { echo 'Fournir BACKUP=chemin/archive.tar.gz' >&2; exit 1; }
> python3 docker/icecast/backup.py test "$(BACKUP)" $(if $(STREAM_PROBES),--stream-probes)

.PHONY: icecast-recovery-image
icecast-recovery-image: ## Construit l'auxiliaire de sauvegarde/restauration
> docker build -f docker/icecast/Dockerfile --target recovery-tools -t stationd-v1-icecast-recovery:dev .


.PHONY: icecast-monitor-up icecast-monitor-status icecast-monitor-logs icecast-monitor-test
icecast-monitor-up: ## Construit et démarre uniquement la supervision centrale
> $(ICECAST_DC) up -d --build --no-deps monitor

icecast-monitor-status: ## État de supervision via gRPC administratif
> $(ICECAST_DC) exec -T admin stationctl icecast monitoring

icecast-monitor-logs: ## Alertes et retours à la normale JSON
> $(ICECAST_DC) logs -f monitor

icecast-monitor-test: ## Tests Rust de temporisation, reprise et seuils, sans daemon
> cargo test --locked --manifest-path crates/stationd-admin/Cargo.toml --bin station-watch

.PHONY: icecast-monitor-integration-test
icecast-monitor-integration-test: ## Alertes Docker isolees, sans emetteurs ni ports publies
> $(ICECAST_DC) -f docker/icecast/compose.standalone-test.yaml build icecastd
> python3 -B tests/icecast-monitor.py

.PHONY: icecast-monitor-direct-status
icecast-monitor-direct-status: ## Supervision gRPC directe si admin est inaccessible
> $(ICECAST_DC) exec -T monitor stationctl --addr http://127.0.0.1:50063 monitor-status


.PHONY: icecast-package icecast-dist icecast-dist-test
icecast-package: ## Paquet Icecast V1 ; livraison si VM=cible SSH (ARGS=--allow-dirty)
> python3 docker/icecast/package.py $(ARGS)

icecast-dist: icecast-package ## Alias de livraison Icecast V1 : make icecast-dist VM=cible

icecast-dist-test: ## Tests du paquet et de la conservation des configurations V1
> python3 -B tests/icecast-dist.py

.PHONY: icecast-dist-install-test
icecast-dist-install-test: ## Validation du vrai paquet sans lancer les services (BUNDLE=archive.tar)
> @test -n "$(BUNDLE)" || { echo "Fournir BUNDLE=dist/stationd-icecast-....tar" >&2; exit 1; }
> python3 -B tests/icecast-dist-install.py "$(BUNDLE)"

# StationDeck web: WASM frontend and narrow gRPC gateway, all in containers.
.PHONY: deck-build deck-check deck-smoke deck deck-down deck-token
DECK_DC = docker compose -f docker/stationdeck/compose.yaml

deck-build: ## Build the WebAssembly frontend and gRPC gateway images
> $(DECK_DC) build deck gateway control

deck-check: ## Run formatting, Rust/WASM tests, Clippy and gateway checks
> docker build -f docker/stationdeck/Dockerfile --target checks -t stationdeck-check:dev .
> docker build -f docker/stationdeck/Dockerfile --target gateway-checks -t stationdeck-gateway-check:dev .
> docker build -f docker/stationdeck/Dockerfile --target control-checks -t stationdeck-control-check:dev .

deck-smoke: ## Test Chromium against an isolated gRPC fixture; no public ports
> @set -eu; \
>   project="stationdeck-test-$$$$"; \
>   trap 'docker compose -p "$$project" -f docker/stationdeck/compose.yaml -f docker/stationdeck/compose.test.yaml down --volumes --remove-orphans' EXIT; \
>   docker compose -p "$$project" -f docker/stationdeck/compose.yaml -f docker/stationdeck/compose.test.yaml build; \
>   docker compose -p "$$project" -f docker/stationdeck/compose.yaml -f docker/stationdeck/compose.test.yaml run --rm browser

deck: ## Start the WebUI on port 18081 (STATIONDECK_PORT=...)
> $(DECK_DC) up -d --build --wait

deck-down: ## Stop only the StationDeck web project
> $(DECK_DC) --profile config-demo down

deck-token: ## Show the gateway access token in your own terminal; keep it private
> @$(DECK_DC) exec -T gateway cat /secrets/access-token
> @echo


.PHONY: deck-config-demo deck-author-token deck-reader-token
deck-config-demo: ## Start the configuration demo agent; no station changes
> $(DECK_DC) --profile config-demo up -d --build --wait

deck-author-token: ## Show the configuration editor token in your own terminal
> $(DECK_DC) exec -T control cat /secrets/authors/editor
> @echo

deck-reader-token: ## Show the read-only configuration token in your own terminal
> $(DECK_DC) exec -T control cat /secrets/authors/reader
> @echo

.PHONY: deck-config-smoke
deck-config-smoke: ## Isolated configuration authoring, discovery, offline cache and recovery
> sh tests/stationdeck-config-smoke.sh

# Real configuration adapters. Tests are deliberately separate targets.
.PHONY: deck-daemon-build deck-daemons deck-sd-agent deck-daemons-down
DECK_DAEMON_DC = $(DECK_DC) -f docker/stationdeck/compose.daemons.yaml
ICECAST_DECK_DC = docker compose -f docker/icecast/compose.yaml -f docker/icecast/compose.stationdeck.yaml
export STATIONDECK_SD_ENDPOINT

deck-daemon-build: ## Build the SD adapter and default icecastd image with its configuration agent
> $(DECK_DC) build control
> docker compose -f docker/icecast/compose.yaml build icecastd admin monitor

deck-daemons: ## Start StationDeck and icecastd; publication remains explicit
> $(DECK_DAEMON_DC) up -d --wait config-init control gateway deck
> $(ICECAST_DECK_DC) up -d --wait icecastd admin

deck-sd-agent: ## Connect an existing SD; requires STATIONDECK_SD_ENDPOINT=FQDN:port
> @test -n "$(STATIONDECK_SD_ENDPOINT)" || { echo 'Set STATIONDECK_SD_ENDPOINT to the existing StationD gRPC endpoint.' >&2; exit 1; }
> $(DECK_DAEMON_DC) --profile sd-config up -d --wait config-sd-agent

deck-daemons-down: ## Stop managed Icecast/admin and configuration adapters, retaining volumes
> $(ICECAST_DECK_DC) stop icecastd admin monitor
> $(DECK_DAEMON_DC) --profile sd-config --profile config-demo stop
