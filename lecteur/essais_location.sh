#!/usr/bin/env bash
# Essais du script d'initialisation d'une machine louée (location.sh) : ses fonctions, lues
# sans rien exécuter, puis ses refus quand il est lancé sans droits. Rien n'est installé ni
# modifié sur la machine.
#
# Usage : essais_location.sh
# Chaque essai dit CONFORME ou ÉCHEC ; le code de sortie est le nombre d'échecs.
set -uo pipefail

ici="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$ici/location.sh"
travail="$(mktemp -d)"
chmod 755 "$travail"
echecs=0

# egal NOM ATTENDU OBTENU
egal() {
  if [[ "$2" == "$3" ]]; then
    echo "essai — $1 : CONFORME"
  else
    echo "essai — $1 : ÉCHEC — attendu « $2 », obtenu « $3 »"
    echecs=$((echecs + 1))
  fi
}

# Ce qu'une fonction dit et rend : « sortie|code ».
rend() {
  local sortie code
  sortie="$("$@" 2> /dev/null)"
  code=$?
  echo "$sortie|$code"
}

# Sourcé, le script ne fait que définir ses fonctions.
# shellcheck source=lecteur/location.sh
source "$script"

egal "une adresse est redite sans zéros de tête" "10.1.0.255|0" "$(rend ipv4 010.001.000.255)"
egal "un octet au-delà de 255 est refusé" "|1" "$(rend ipv4 256.1.1.1)"
egal "trois octets sont refusés" "|1" "$(rend ipv4 1.2.3)"
egal "une espace est refusée" "|1" "$(rend ipv4 ' 10.0.0.1')"
egal "le texte d'origine est refusé" "|1" "$(rend ipv4 "$CLIENT")"

for adresse in 10.0.0.0 10.255.255.255 172.16.0.0 172.31.255.255 192.168.0.0 192.168.255.255; do
  egal "$adresse est une adresse privée" "|0" "$(rend privee "$adresse")"
done
for adresse in 9.255.255.255 11.0.0.0 172.15.255.255 172.32.0.0 192.167.255.255 192.169.0.0 \
  8.8.8.8 127.0.0.1 169.254.169.254; do
  egal "$adresse n'est pas une adresse privée" "|1" "$(rend privee "$adresse")"
done

# Une machine de l'hébergeur : une adresse publique et une adresse d'ancrage sur la première
# carte, l'adresse du réseau privé sur la seconde.
cartes='1: lo    inet 127.0.0.1/8 scope host lo\       valid_lft forever preferred_lft forever
2: eth0    inet 203.0.113.7/20 brd 203.0.127.255 scope global eth0\       valid_lft forever
2: eth0    inet 10.19.0.5/16 brd 10.19.255.255 scope global eth0\       valid_lft forever
3: eth1    inet 10.114.0.3/20 brd 10.114.15.255 scope global eth1\       valid_lft forever
3: eth1    inet6 fe80::1/64 scope link \       valid_lft forever preferred_lft forever
9: faux    inet 10.114.0.300/20 scope global faux\       valid_lft forever'
locale() { printf '%s\n' "$2" | rend adresse_locale_pour "$1"; }

egal "le réseau privé du client donne l'adresse de sa carte" "10.114.0.3|0" \
  "$(locale 10.114.0.2 "$cartes")"
egal "la dernière adresse du réseau en est encore" "10.114.0.3|0" \
  "$(locale 10.114.15.255 "$cartes")"
egal "la première adresse d'après n'en est plus" "|1" "$(locale 10.114.16.0 "$cartes")"
egal "un client du réseau d'ancrage donne l'adresse d'ancrage" "10.19.0.5|0" \
  "$(locale 10.19.200.1 "$cartes")"
egal "aucun réseau de la machine ne contient le client" "|1" "$(locale 192.168.1.1 "$cartes")"
egal "une machine sans carte" "|1" "$(locale 10.114.0.2 "")"
egal "deux réseaux contiennent le client : aucun choix" "|2" \
  "$(locale 10.114.0.2 "$cartes
4: eth2    inet 10.0.0.9/8 brd 10.255.255.255 scope global eth2\       valid_lft forever")"
egal "la même adresse dite deux fois reste une adresse" "10.114.0.3|0" \
  "$(locale 10.114.0.2 "$cartes
3: eth1    inet 10.114.0.3/20 brd 10.114.15.255 scope global secondary eth1\       valid_lft forever")"

printf 'NAME="Ubuntu"\nVERSION_ID="22.04"\nID=ubuntu\nID_LIKE=debian\n' > "$travail/admis"
printf 'NAME="Ubuntu"\nVERSION_ID="24.04"\nID=ubuntu\nID_LIKE=debian\n' > "$travail/plus-recent"
printf 'NAME="Debian"\nVERSION_ID="22.04"\nID=debian\n' > "$travail/autre"
egal "Ubuntu 22.04 est admis" "|0" "$(rend systeme_admis "$travail/admis")"
egal "une autre version d'Ubuntu ne l'est pas" "|1" "$(rend systeme_admis "$travail/plus-recent")"
egal "un autre système ne l'est pas" "|1" "$(rend systeme_admis "$travail/autre")"
egal "un fichier absent ne l'est pas" "|1" "$(rend systeme_admis "$travail/absent")"

egal "la ligne à remplir est la quatrième" 'CLIENT="COLLE_ICI_L_ADRESSE_PRIVEE_DU_CLIENT"' \
  "$(sed -n 4p "$script")"
egal "le texte à remplacer n'y est qu'une fois" 1 "$(grep -c 'COLLE_ICI' "$script")"
egal "aucune commande n'est tracée" 0 "$(grep -c 'set -x' "$script")"
egal "le commit fixé est entier" 1 "$(grep -Ec '^COMMIT="[0-9a-f]{40}"$' "$script")"
egal "le script se lit" 0 "$(bash -n "$script" > /dev/null 2>&1; echo $?)"

# Le script lancé pour de bon, mais sans droits : il s'arrête avant de rien faire. En
# administrateur, il est lancé sous un compte sans droits.
lance() {
  local copie sortie code
  copie="$travail/location-$1.sh"
  sed "4s/.*/CLIENT=\"$1\"/" "$script" > "$copie"
  chmod 644 "$copie"
  if ((EUID == 0)); then
    sortie="$(runuser -u nobody -- bash "$copie" 2>&1)"
  else
    sortie="$(bash "$copie" 2>&1)"
  fi
  code=$?
  echo "$(printf '%s\n' "$sortie" | grep 'ÉCHEC' | tail -n 1 | sed 's/^\[[^]]*\] //')|$code"
}

egal "le texte d'origine laissé arrête le script" \
  "ÉCHEC : l'adresse du client est absente ou mal recopiée.|1" \
  "$(lance COLLE_ICI_L_ADRESSE_PRIVEE_DU_CLIENT)"
egal "une adresse publique arrête le script" \
  "ÉCHEC : l'adresse du client n'est pas une adresse privée.|1" "$(lance 8.8.8.8)"
egal "sans droits, le script s'arrête avant de rien faire" \
  "ÉCHEC : ce script doit être lancé par root.|1" "$(lance 10.114.0.2)"

echo "essais de location.sh : $echecs échec(s)"
exit "$echecs"
