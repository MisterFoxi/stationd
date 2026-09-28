# Raccourcis de dev — à lancer depuis la racine du dépôt, sur l'hôte
# (devstationd). Tout passe par le conteneur de dev (docker compose, README
# « Run (Docker) »). `make` ou `make help` liste les cibles.
#
#   make plugins                      # tous les plugins WASM
#   make plugins P=stop-when-idle-wasm
#   make ctl A="station state"        # n'importe quelle commande stationctl
#   make restart                      # cargo build + relance de stationd seul

# Préfixe de recette « > » au lieu de la tabulation (copier-coller sûr).
.RECIPEPREFIX := >

DC      ?= docker compose
SVC     ?= station
# P = un plugin (nom du dossier sous plugins/), vide = tous.
P       ?=
# A = arguments de stationctl (make ctl A="…").
A       ?= status
# Script Liquidsoap généré, vu du conteneur ([liquidsoap] script_path).
LIQ     ?= /src/data/station.liq
WASM    := wasm32-unknown-unknown

EXEC      = $(DC) exec -u dev $(SVC)
EXEC_ROOT = $(DC) exec $(SVC)
# stationctl du dépôt (target/debug), hors PATH dans l'image de dev.
CTL       = $(EXEC) sh -c '"$$STATIOND_BIN/stationctl" "$$@"' stationctl

.DEFAULT_GOAL := help
.PHONY: help up down image logs shell build release test clippy fmt plugins \
        all restart restart-ls restart-icecast restart-air check-liq tui ctl \
        state stop start package

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
build: ## cargo build (stationd + stationctl, debug)
> $(EXEC) cargo build

release: ## cargo build --release --locked
> $(EXEC) cargo build --release --locked

test: ## cargo test --locked
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
>     echo "== $${m%/Cargo.toml}"; \
>     cargo build --release --locked --target $(WASM) --manifest-path "$$m"; \
>   done; \
>   ls -l /src/plugins/$(or $(P),*)/target/$(WASM)/release/*.wasm'

all: build plugins test ## build + plugins + test

tui: ## Compile et lance la TUI
> $(EXEC) cargo build --features tui
> $(DC) exec -it -u dev $(SVC) ./target/debug/stationd-tui

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

# --- livraison ---------------------------------------------------------------
package: ## docker/package.sh (arbre git propre ; ARGS=--allow-dirty sinon)
> docker/package.sh $(ARGS)