#!/usr/bin/env bash
# Amorce d'une machine louée pour lire l'archive publique. Cette machine ne porte aucune clé
# et ne garde rien : elle compile le programme de lecture, l'installe, puis n'accepte plus
# que les demandes d'une seule machine, par le réseau privé.
#
# Usage : amorce.sh COMMIT ADRESSE CLIENT [CONNEXIONS]
#   COMMIT : le commit de cet atelier que la machine doit construire ; ce doit être celui du
#   dossier d'où ce script est lancé, sans retouche — contrôlé avant tout le reste ;
#   ADRESSE : l'adresse privée (IPv4) de cette machine, où le lecteur écoute ;
#   CLIENT : l'adresse privée (IPv4) de la seule machine admise à lui adresser des demandes ;
#   CONNEXIONS : le nombre de demandes servies à la fois, de 1 à 16 (2 si rien n'est dit).
# À lancer en administrateur, une seule fois, sur une machine neuve (Ubuntu 22.04), depuis un
# clone de cet atelier. Les commandes ne sont pas tracées.
set -euo pipefail

PORT=7411
CONSTRUCTEUR=/var/lib/constructeur

refuse() { echo "amorce.sh : $1" >&2; exit 2; }

# Une adresse IPv4, redite sans zéros de tête ; rien si le texte n'en est pas une.
ipv4() {
  local octet
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  local octets=("${BASH_REMATCH[@]:1}")
  for octet in "${octets[@]}"; do
    (( 10#$octet <= 255 )) || return 1
  done
  echo "$((10#${octets[0]})).$((10#${octets[1]})).$((10#${octets[2]})).$((10#${octets[3]}))"
}

(( $# == 3 || $# == 4 )) || refuse "attendu : COMMIT ADRESSE CLIENT [CONNEXIONS]"
(( EUID == 0 )) || refuse "à lancer en administrateur"
commit="$1"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || refuse "commit : quarante chiffres hexadécimaux attendus"
adresse="$(ipv4 "$2")" || refuse "adresse : une adresse IPv4 attendue"
client="$(ipv4 "$3")" || refuse "client : une adresse IPv4 attendue"
connexions="${4:-2}"
if ! [[ "$connexions" =~ ^[0-9]{1,2}$ ]] || (( 10#$connexions < 1 || 10#$connexions > 16 )); then
  refuse "connexions : de 1 à 16"
fi
atelier="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ "$(git -C "$atelier" rev-parse HEAD)" == "$commit" ]] \
  || refuse "ce dossier n'est pas au commit demandé"
[[ -z "$(git -C "$atelier" status --porcelain)" ]] \
  || refuse "ce dossier n'est pas celui du commit demandé, sans retouche"
outil="$(< "$atelier/recette/OUTIL")"
[[ "$outil" =~ ^[0-9a-f]{40}$ ]] || refuse "commit de l'outil illisible dans recette/OUTIL"
[[ ! -e "$CONSTRUCTEUR/atelier" ]] || refuse "cette machine a déjà été amorcée"

# Les paquets : ceux de la compilation, puis ceux de cette amorce.
bash "$atelier/recette/dependances.sh"
export DEBIAN_FRONTEND=noninteractive
apt-get -o DPkg::Lock::Timeout=300 install -y git curl ca-certificates ufw

# La compilation, sous un utilisateur sans droits : ce sont des programmes venus d'ailleurs
# qui s'exécutent là — le compilateur, l'outil et ses dépendances. Il travaille sur une copie
# de l'atelier qui est à lui.
id constructeur > /dev/null 2>&1 \
  || useradd --system --create-home --home-dir "$CONSTRUCTEUR" --shell /usr/sbin/nologin \
    constructeur
cp -a "$atelier" "$CONSTRUCTEUR/atelier"
chown -R constructeur:constructeur "$CONSTRUCTEUR/atelier"
runuser -u constructeur -- env HOME="$CONSTRUCTEUR" OUTIL="$outil" bash -s << 'CONSTRUIRE'
set -euo pipefail
cd "$HOME"
# Le compilateur : l'installeur officiel de Rust, sans rien choisir d'avance — la version
# est celle que l'outil demande, prise à la première compilation dans son dossier.
curl --proto '=https' --tlsv1.2 --silent --show-error --fail https://sh.rustup.rs \
  | sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
export PATH="$HOME/.cargo/bin:$PATH"
# L'outil, à son commit fixé et à lui seul.
git init --quiet outil
git -C outil remote add origin https://github.com/anza-xyz/jetstreamer
git -C outil fetch --quiet --depth 1 origin "$OUTIL"
git -C outil checkout --quiet --detach FETCH_HEAD
bash atelier/recette/construire.sh outil complete
CONSTRUIRE

# L'installation, par les scripts de ce dossier-ci — pas ceux de la copie, que des programmes
# venus d'ailleurs ont pu toucher.
bash "$atelier/lecteur/installer.sh" "$CONSTRUCTEUR/outil/target/release/examples/essai" \
  "$adresse" "$PORT" "$connexions"

# Le pare-feu : rien n'entre, sauf les demandes du client sur le port du lecteur.
ufw --force reset
ufw default deny incoming
ufw default allow outgoing
ufw allow proto tcp from "$client" to "$adresse" port "$PORT"
ufw --force enable
ufw status verbose

echo "amorce finie : le lecteur écoute sur $adresse:$PORT pour $client seul ; atelier $commit"
