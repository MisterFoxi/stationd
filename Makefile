# Raccourcis de dev — à lancer depuis la racine du dépôt, sur l'hôte
# (devstationd). Tout passe par le conteneur de dev (docker compose, README
# « Run (Docker) »). `make` ou `make help` liste les cibles.
#
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
.PHONY: help up down image logs shell build release test clippy fmt plugins \
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
build: ## cargo build (stationd + stationctl) : release sur main, debug ailleurs (PROFILE=…) ; target/active → ce profil
> @echo "== profil $(PROFILE) (branche $(or $(BRANCH),?))"
> $(EXEC) sh -c 'set -e; cargo build $(CARGO_PROFILE); ln -sfn $(PROFILE) /src/target/active'

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
