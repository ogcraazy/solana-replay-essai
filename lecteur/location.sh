#!/bin/bash
# >>> AVANT DE CRÉER LA MACHINE : sur la ligne ci-dessous, remplace le texte entre guillemets
# >>> par l'adresse privée de la machine cliente. Rien d'autre à changer.
CLIENT="COLLE_ICI_L_ADRESSE_PRIVEE_DU_CLIENT"

# Script d'initialisation d'une machine louée pour lire l'archive publique : à coller dans le
# champ « script d'initialisation » (user data) du formulaire de création de la machine.
# Prévu pour Ubuntu 22.04, architecture x86-64. Aucun secret : cette machine ne porte aucune
# clé et ne garde rien.
#
# Il fait, une seule fois, au premier démarrage :
#   1. il contrôle l'adresse collée, et trouve l'adresse privée de cette machine : celle de
#      la carte dont le réseau contient le client ;
#   2. il rend son journal lisible sur le port 7412 de cette adresse — une connexion, une
#      lecture de la fin du journal, rien d'autre : c'est la seule façon de voir, du dehors,
#      ce qui se passe ici ;
#   3. il prend cet atelier au commit fixé ci-dessous et lance son amorce (lecteur/amorce.sh),
#      qui compile le programme de lecture, l'installe et ferme le pare-feu ;
#   4. il admet le client, et lui seul, à lire le journal.
# Si quelque chose échoue après le point 2, le pare-feu est fermé et le journal reste lisible
# par le client.
#
# Journal : /var/log/amorce-lecteur.log. Les commandes ne sont pas tracées.

# --- Fixé par l'atelier : ne pas modifier à la main ------------------------------------
ATELIER="https://github.com/ogcraazy/solana-replay-essai"
COMMIT="1b991add3eea3d76620825cf2b5e102161021fb5"
PORT_JOURNAL=7412
JOURNAL=/var/log/amorce-lecteur.log
SOURCES=/root/atelier

# --- Les fonctions. Sourcé, ce script ne fait que les définir (essais_location.sh). ----

# Une adresse IPv4, redite sans zéros de tête ; rien si le texte n'en est pas une.
ipv4() {
  local octet octets
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  octets=("${BASH_REMATCH[@]:1}")
  for octet in "${octets[@]}"; do
    ((10#$octet <= 255)) || return 1
  done
  echo "$((10#${octets[0]})).$((10#${octets[1]})).$((10#${octets[2]})).$((10#${octets[3]}))"
}

# Le nombre que fait une adresse IPv4 déjà contrôlée.
nombre() {
  local a b c d
  IFS=. read -r a b c d <<< "$1"
  echo "$(((a << 24) + (b << 16) + (c << 8) + d))"
}

# Vrai pour une adresse des trois plages privées : 10/8, 172.16/12, 192.168/16.
privee() {
  local n
  n="$(nombre "$1")"
  (((n >> 24) == 10 || (n >> 20) == 0xAC1 || (n >> 16) == 0xC0A8))
}

# L'adresse de cette machine dont le réseau contient le client. Lit, sur l'entrée, ce que
# dit « ip -4 -o addr show ». Code 1 si aucun réseau de la machine ne le contient, 2 si
# plusieurs le contiennent : il n'y a alors rien à choisir.
adresse_locale_pour() {
  local client ligne mot longueur adresse masque trouvee=""
  client="$(nombre "$1")"
  while read -r ligne; do
    # « 3: eth1    inet 10.114.0.3/20 brd … » : l'adresse et la longueur de son réseau.
    [[ "$ligne" =~ [[:space:]]inet[[:space:]]+([0-9.]+)/([0-9]+) ]] || continue
    mot="${BASH_REMATCH[1]}"
    longueur="${BASH_REMATCH[2]}"
    adresse="$(ipv4 "$mot")" || continue
    masque=$(((0xFFFFFFFF << (32 - longueur)) & 0xFFFFFFFF))
    (((client & masque) == ($(nombre "$adresse") & masque))) || continue
    [[ -z "$trouvee" || "$trouvee" == "$adresse" ]] || return 2
    trouvee="$adresse"
  done
  [[ -n "$trouvee" ]] || return 1
  echo "$trouvee"
}

# Vrai si le fichier (un « os-release ») dit Ubuntu 22.04.
systeme_admis() {
  grep -qx 'ID=ubuntu' "$1" 2> /dev/null || return 1
  grep -qx 'VERSION_ID="22.04"' "$1" 2> /dev/null || return 1
}

[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0

# --- Le premier démarrage ----------------------------------------------------------------

set -euo pipefail
export HOME=/root DEBIAN_FRONTEND=noninteractive

dire() { printf '\n[location %s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
echec() {
  dire "ÉCHEC : $*"
  exit 1
}

client="$(ipv4 "$CLIENT")" || echec "l'adresse du client est absente ou mal recopiée."
privee "$client" || echec "l'adresse du client n'est pas une adresse privée."
((EUID == 0)) || echec "ce script doit être lancé par root."

exec > >(tee -a "$JOURNAL") 2>&1
dire "client admis : $client"
adresse="$(ip -4 -o addr show | adresse_locale_pour "$client")" \
  || echec "aucune carte de cette machine, ou plus d'une, n'est sur le réseau du client."
dire "adresse privée de cette machine : $adresse"

dire "1/4 le journal, lisible sur $adresse:$PORT_JOURNAL"
cat > /etc/systemd/system/amorce-journal.socket << UNITE
# Le journal de l'amorce de cette machine, rendu à qui se connecte : une connexion, une
# lecture. Écrit par le script d'initialisation.
[Unit]
Description=Journal de l'amorce : attend une lecture

[Socket]
ListenStream=$adresse:$PORT_JOURNAL
Accept=yes
MaxConnections=4
# L'écoute s'ouvre même si l'adresse de la machine n'est pas encore en place.
FreeBind=yes

[Install]
WantedBy=sockets.target
UNITE
cat > /etc/systemd/system/amorce-journal@.service << UNITE
# Une lecture du journal de l'amorce : la fin du fichier, écrite dans la connexion.
[Unit]
Description=Journal de l'amorce : une lecture
CollectMode=inactive-or-failed

[Service]
ExecStart=/usr/bin/tail --bytes=262144 $JOURNAL
StandardInput=socket
StandardOutput=socket
StandardError=journal
DynamicUser=yes
ProtectSystem=strict
ProtectHome=yes
PrivateDevices=yes
PrivateTmp=yes
NoNewPrivileges=yes
CapabilityBoundingSet=
RuntimeMaxSec=30
UNITE
systemctl daemon-reload
systemctl enable --now amorce-journal.socket

# À tout échec, désormais : rien n'entre, sauf le client pour lire le journal.
fermer() {
  local code=$?
  ((code != 0)) || return 0
  dire "ÉCHEC (code $code) : le pare-feu est fermé, le journal reste lisible par le client."
  if command -v ufw > /dev/null; then
    ufw default deny incoming || true
    ufw default allow outgoing || true
    ufw allow proto tcp from "$client" to "$adresse" port "$PORT_JOURNAL" || true
    ufw --force enable || true
  fi
}
trap fermer EXIT

essayer() {
  local essai
  for essai in 1 2 3; do
    "$@" && return 0
    dire "échec ($essai/3), nouvel essai dans 15 s : $*"
    sleep 15
  done
  return 1
}

dire "2/4 le système et les paquets"
systeme_admis /etc/os-release || echec "Ubuntu 22.04 attendu."
# Au premier démarrage, le système fait ses propres mises à jour : on attend son tour.
essayer apt-get -o DPkg::Lock::Timeout=600 update
essayer apt-get -o DPkg::Lock::Timeout=600 install -y --no-install-recommends \
  ca-certificates git ufw

dire "3/4 l'atelier au commit $COMMIT"
[[ ! -e "$SOURCES" ]] || echec "cette machine a déjà été amorcée."
git init --quiet "$SOURCES"
git -C "$SOURCES" remote add origin "$ATELIER"
essayer git -C "$SOURCES" fetch --quiet --depth 1 origin "$COMMIT"
git -C "$SOURCES" checkout --quiet --detach FETCH_HEAD
[[ "$(git -C "$SOURCES" rev-parse HEAD)" == "$COMMIT" ]] \
  || echec "les sources prises ne sont pas au commit fixé."

dire "4/4 l'amorce du lecteur"
bash "$SOURCES/lecteur/amorce.sh" "$COMMIT" "$adresse" "$client"
# L'amorce a fermé le pare-feu au seul port du lecteur : le journal s'y ajoute.
ufw allow proto tcp from "$client" to "$adresse" port "$PORT_JOURNAL"

dire "terminé : le lecteur écoute sur $adresse pour $client seul ; atelier $COMMIT."
