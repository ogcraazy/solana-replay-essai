#!/usr/bin/env bash
# Construit le programme de lecture : l'outil libre Jetstreamer, pris à un commit fixé, où sont
# posés le greffon de cet atelier et deux correctifs.
#
# Usage : construire.sh DOSSIER_DE_L_OUTIL OPTIMISATION
#   DOSSIER_DE_L_OUTIL : les sources de l'outil, au commit que fixe le fichier OUTIL d'à côté
#   (anza-xyz/jetstreamer, étiquette jetstreamer@v0.8.1 du 9/10/2026) ;
#   OPTIMISATION : « rapide » compile vite, « complete » compile comme l'outil le prévoit
#   (plus long, lecture plus vive).
# Le programme : DOSSIER_DE_L_OUTIL/target/release/examples/essai.
#
# Le programme redit, dans l'événement « debut » de chaque flux, le commit de l'atelier dont il
# est construit et celui de l'outil. Ils sont lus ici, dans les deux dépôts ; et le greffon
# compilé doit être celui de ce commit, sans retouche.
set -euo pipefail

refuse() { echo "construire.sh : $1" >&2; exit 2; }

(( $# == 2 )) || refuse "attendu : DOSSIER_DE_L_OUTIL OPTIMISATION"
optimisation="$2"
[[ "$optimisation" == rapide || "$optimisation" == complete ]] \
  || refuse "optimisation : « rapide » ou « complete »"
[[ -d "$1/.git" ]] || refuse "les sources de l'outil ne sont pas dans ce dossier"
outil="$(cd "$1" && pwd)"
atelier="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OUTIL="$(< "$atelier/recette/OUTIL")"
[[ "$OUTIL" =~ ^[0-9a-f]{40}$ ]] || refuse "commit de l'outil illisible dans recette/OUTIL"
[[ "$(git -C "$outil" rev-parse HEAD)" == "$OUTIL" ]] \
  || refuse "l'outil n'est pas au commit fixé ($OUTIL)"
commit="$(git -C "$atelier" rev-parse HEAD)"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || refuse "commit de l'atelier illisible"
[[ -z "$(git -C "$atelier" status --porcelain -- greffon recette)" ]] \
  || refuse "le greffon ou la recette ne sont pas ceux du commit $commit"

# Les modules du greffon qui ne dépendent que de la bibliothèque standard portent leurs
# propres essais : ils passent ici, avec le compilateur que l'outil demande (d'où le dossier
# de l'outil), avant toute compilation de l'outil.
essais="$(mktemp -d)"
(
  cd "$outil"
  for module in flux outils suivi demande; do
    rustc --edition 2024 --test "$atelier/greffon/$module.rs" -o "$essais/$module"
    "$essais/$module"
  done
)

# Deux correctifs, et rien d'autre. Le premier : depuis l'époque 1064, le premier bloc de
# chaque époque porte des récompenses d'un type que cette version de l'outil refuse, et la
# lecture n'en sort pas (constaté à l'exécution n° 44329878008) ; le greffon ne lit aucune
# récompense, le correctif les fait passer sans les lire. Le second : l'outil rend au
# greffon, en plus du compte rendu d'exécution qu'il a décodé, les octets de ce compte rendu
# tels que l'archive les garde, et la transaction signée au format du flux par la conversion
# de la bibliothèque de Solana ; il ne change rien à ce que l'outil lit.
poser() {
  if git -C "$outil" apply --reverse --check "$1" 2> /dev/null; then
    echo "correctif déjà posé : $(basename "$1")"
  else
    git -C "$outil" apply --verbose "$1"
  fi
}
poser "$atelier/greffon/recompenses.patch"
poser "$atelier/greffon/octets.patch"
mkdir -p "$outil/examples/essai"
cp "$atelier"/greffon/{main,flux,outils,suivi,demande}.rs "$outil/examples/essai/"

# « --locked » : les versions de dépendances que l'outil a lui-même éprouvées, et pas
# d'autres. Le compilateur déborde de sa pile sur l'une des bibliothèques de Solana et
# conseille lui-même de l'agrandir (constaté à la première exécution).
export RUST_MIN_STACK=134217728 OPENSSL_NO_VENDOR=1
export ESSAI_ATELIER="$commit" ESSAI_OUTIL="$OUTIL"
if [[ "$optimisation" == rapide ]]; then
  export CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
fi
depart=$SECONDS
(cd "$outil" && cargo build --release --locked --example essai)
echo "compilation : $((SECONDS - depart)) s, atelier $commit, outil $OUTIL"
ls -l "$outil/target/release/examples/essai"
du -sh "$outil/target"
