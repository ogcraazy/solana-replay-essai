#!/usr/bin/env bash
# Contrôles d'un lecteur installé : ce qu'il fait d'une demande refusée, d'une ligne qui ne
# vient pas, d'un client qui s'en va, et de plus de demandes que sa borne.
#
# Usage : controles.sh ADRESSE PORT PROGRAMME LIGNE
#         controles.sh ADRESSE PORT PROGRAMME borne
#   PROGRAMME : le chemin du programme installé, pour compter ses processus ;
#   LIGNE : une demande juste, dont le flux fait bien plus d'un million d'octets ;
#   « borne » : le lecteur vient d'être installé pour une seule demande à la fois.
# Chaque contrôle dit CONFORME ou ÉCHEC ; le code de sortie est le nombre d'échecs.
set -uo pipefail

if (( $# != 4 )); then
  echo "controles.sh : attendu ADRESSE PORT PROGRAMME LIGNE (ou « borne »)" >&2
  exit 2
fi
adresse="$1"; port="$2"; programme="$3"; ligne="$4"
ici="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
travail="$(mktemp -d)"
echecs=0

dire() {
  local verdict=CONFORME
  if (( ! $2 )); then
    verdict=ÉCHEC
    echecs=$((echecs + 1))
  fi
  echo "contrôle — $1 : $verdict — $3"
}

# L'événement « refus » tel qu'il doit arriver, seul : un enregistrement de genre 2, sa
# longueur sur quatre octets, puis son texte.
refus_attendu() {
  python3 -c '
import struct, sys
texte = ("{\"e\":\"refus\",\"motif\":\"" + sys.argv[1] + "\"}").encode()
sys.stdout.buffer.write(b"\x02" + struct.pack(">I", len(texte)) + texte)' "$1"
}

# Une connexion où l'on ne dit rien : rend le nombre d'octets reçus avant sa fermeture.
muette() {
  (exec 5<> "/dev/tcp/$adresse/$port" && timeout 30 cat <&5) 2> /dev/null | wc -c
}

# Une demande refusée reçoit pour tout flux l'événement « refus » et son motif.
refusee() {
  local nom="$1" motif="$2"
  refus_attendu "$motif" > "$travail/voulu"
  if cmp --silent "$travail/recu" "$travail/voulu"; then
    dire "$nom" 1 "un seul événement, « refus : $motif »"
  else
    dire "$nom" 0 \
      "$(wc -c < "$travail/recu") octet(s) reçu(s), $(wc -c < "$travail/voulu") attendus"
  fi
}

if [[ "$ligne" == borne ]]; then
  # Une première connexion, muette, que le lecteur garde dix secondes ; pendant ce temps une
  # seconde arrive : elle est de trop, et se ferme aussitôt sans rien recevoir.
  depart=$SECONDS
  muette > "$travail/premiere" &
  premiere=$!
  sleep 2
  avant=$SECONDS
  octets=$(muette)
  duree=$((SECONDS - avant))
  dire "connexion de trop" $(( octets == 0 && duree <= 3 )) \
    "fermée après $duree s, $octets octet(s) reçu(s)"
  wait "$premiere"
  duree=$((SECONDS - depart))
  octets=$(< "$travail/premiere")
  dire "la connexion en cours n'est pas troublée" $(( octets == 0 && duree >= 9 && duree <= 14 )) \
    "tenue $duree s, $octets octet(s) reçu(s)"
  bash "$ici/demander.sh" "$adresse" "$port" "v2" > "$travail/recu" 2> /dev/null
  refusee "après elles, le lecteur répond" "version inconnue : ce lecteur ne sert que « v1 »"
  exit "$echecs"
fi

# Une demande d'une version inconnue.
bash "$ici/demander.sh" "$adresse" "$port" "v2 ${ligne#v1 }" > "$travail/recu" 2> /dev/null
refusee "demande refusée" "version inconnue : ce lecteur ne sert que « v1 »"

# Une ligne qui n'en finit pas : 1 100 octets sans saut de ligne. Le lecteur n'en lit que
# 1 025 et refuse ; les octets qu'il n'a pas lus peuvent faire finir la connexion par une
# coupure, que l'on tolère ici.
printf -v longue '%*s' 1100 ''
longue="${longue// /x}"
(exec 5<> "/dev/tcp/$adresse/$port" && printf '%s' "$longue" >&5 && cat <&5) \
  > "$travail/recu" 2> /dev/null
refusee "ligne trop longue" "ligne de plus de 1024 octets"

# Une ligne qui ne vient pas : la connexion est close au bout de dix secondes, sans un octet.
depart=$SECONDS
octets=$(muette)
duree=$((SECONDS - depart))
dire "ligne qui ne vient pas" $(( octets == 0 && duree >= 9 && duree <= 14 )) \
  "connexion close après $duree s, $octets octet(s) reçu(s)"

# Le client s'en va après le premier million d'octets : dix secondes plus tard au plus, plus
# aucun processus du programme ne lit l'archive pour personne.
pris=$( (exec 5<> "/dev/tcp/$adresse/$port" && printf '%s\n' "$ligne" >&5 && head -c 1000000 <&5) \
  2> /dev/null | wc -c)
fin=$((SECONDS + 10))
while (( SECONDS < fin )) && pgrep -f -x "$programme" > /dev/null; do
  sleep 1
done
restants=$(pgrep -f -x -c "$programme" || true)
dire "client parti au milieu du flux" $(( pris == 1000000 && restants == 0 )) \
  "$pris octets pris, puis $restants processus restant(s)"

exit "$echecs"
