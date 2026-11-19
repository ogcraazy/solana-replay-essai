#!/usr/bin/env bash
# Installe le lecteur sur cette machine : le programme, le script qui le lance et les deux
# unités du système, puis ouvre l'écoute.
#
# Usage : installer.sh PROGRAMME ADRESSE PORT CONNEXIONS
#   PROGRAMME : le programme de lecture, construit par la recette ;
#   ADRESSE : l'adresse IPv4 de cette machine où écouter (127.0.0.1, ou son adresse privée) ;
#   PORT : de 1024 à 65535 ;
#   CONNEXIONS : le nombre de demandes servies à la fois, de 1 à 16.
# À lancer en administrateur. Relancé, il remplace ce qu'il avait posé.
set -euo pipefail

refuse() { echo "installer.sh : $1" >&2; exit 2; }

(( $# == 4 )) || refuse "attendu : PROGRAMME ADRESSE PORT CONNEXIONS"
(( EUID == 0 )) || refuse "à lancer en administrateur"
programme="$1"
[[ -f "$programme" && -x "$programme" ]] || refuse "programme introuvable, ou qui ne s'exécute pas"
[[ "$2" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] \
  || refuse "adresse : une adresse IPv4 attendue"
octets=("${BASH_REMATCH[@]:1}")
for octet in "${octets[@]}"; do
  (( 10#$octet <= 255 )) || refuse "adresse : une adresse IPv4 attendue"
done
adresse="$((10#${octets[0]})).$((10#${octets[1]})).$((10#${octets[2]})).$((10#${octets[3]}))"
if ! [[ "$3" =~ ^[0-9]{4,5}$ ]] || (( 10#$3 < 1024 || 10#$3 > 65535 )); then
  refuse "port : de 1024 à 65535"
fi
port=$((10#$3))
if ! [[ "$4" =~ ^[0-9]{1,2}$ ]] || (( 10#$4 < 1 || 10#$4 > 16 )); then
  refuse "connexions : de 1 à 16"
fi
connexions=$((10#$4))
ici="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

install -D -m 0755 -o root -g root "$programme" /usr/local/lib/lecteur/essai
install -D -m 0755 -o root -g root "$ici/servir" /usr/local/lib/lecteur/servir
install -D -m 0644 -o root -g root "$ici/lecteur.socket" /etc/systemd/system/lecteur.socket
install -D -m 0644 -o root -g root "$ici/lecteur@.service" "/etc/systemd/system/lecteur@.service"
install -d -m 0755 -o root -g root /etc/systemd/system/lecteur.socket.d
reglage=/etc/systemd/system/lecteur.socket.d/ecoute.conf
{
  echo "# Écrit par installer.sh : où écouter, et combien de demandes servir à la fois."
  echo "[Socket]"
  echo "ListenStream=$adresse:$port"
  echo "MaxConnections=$connexions"
} > "$reglage"
chmod 0644 "$reglage"

systemctl daemon-reload
systemctl enable lecteur.socket
systemctl restart lecteur.socket
systemctl is-active --quiet lecteur.socket || refuse "l'écoute n'a pas démarré"
echo "lecteur installé : écoute sur $adresse:$port, $connexions demande(s) à la fois"
