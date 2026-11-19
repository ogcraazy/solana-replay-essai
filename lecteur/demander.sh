#!/usr/bin/env bash
# Adresse une demande au lecteur, et rend sur la sortie standard le flux reçu, jusqu'à ce que
# le lecteur ferme la connexion.
#
# Usage : demander.sh ADRESSE PORT LIGNE
#   LIGNE : la demande, sans son saut de ligne —
#   « v1 <premier>:<dernier> <avant> <programmes> ».
# Rien d'autre ne doit suivre la ligne : des octets que le lecteur ne lit pas feraient se
# fermer la connexion par une coupure, pas par une fin.
set -euo pipefail

(( $# == 3 )) || { echo "demander.sh : attendu ADRESSE PORT LIGNE" >&2; exit 2; }

exec 5<> "/dev/tcp/$1/$2"
printf '%s\n' "$3" >&5
# Ce script laisse la place à la lecture : qui l'arrête arrête la lecture, et la connexion.
exec cat <&5
