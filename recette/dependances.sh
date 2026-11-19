#!/usr/bin/env bash
# Les paquets qu'il faut pour compiler l'outil et son greffon, mesurer la lecture et comprimer
# les résultats. À lancer en administrateur.
set -euo pipefail

(( EUID == 0 )) || { echo "dependances.sh : à lancer en administrateur" >&2; exit 2; }

export DEBIAN_FRONTEND=noninteractive
# Sur une machine neuve, le système installe parfois lui-même des paquets à ce moment-là :
# attendre son tour plutôt qu'échouer.
attendre=(-o DPkg::Lock::Timeout=300)
apt-get "${attendre[@]}" update
apt-get "${attendre[@]}" install -y build-essential libudev-dev clang llvm-dev libclang-dev \
  pkg-config libssl-dev time zstd
clang --version | head -1
