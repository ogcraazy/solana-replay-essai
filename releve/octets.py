"""Octets de l'archive par slot, d'après l'index des plages d'une époque.

Usage : octets.py FICHIER_INDEX ÉPOQUE [DÉBUT FIN]
        octets.py FICHIER_INDEX ÉPOQUE avant SLOT

L'index porte, pour chaque slot de l'époque et dans l'ordre, douze octets petit-boutistes :
la position (8 octets) et la longueur (4 octets) de ce qui appartient au slot dans le fichier
de l'époque ; un slot sans bloc est tout à zéro. Le script écrit une ligne JSON : pour
l'époque entière, ou pour la part de la plage « DÉBUT FIN » (bornes comprises) qui s'y trouve.

Avec « avant SLOT », il écrit le dernier slot à bloc strictement avant SLOT dans l'époque, ou
« - » quand aucun bloc ne l'y précède ; SLOT peut être le premier slot de l'époque suivante.
C'est de là qu'une lecture doit partir pour connaître le parent du premier bloc d'une plage.
"""

import json
import struct
import sys

SLOTS = 432_000  # slots par époque


def longueurs(chemin: str) -> list[int]:
    """La longueur de chaque slot de l'époque, dans l'ordre."""
    with open(chemin, "rb") as fichier:
        donnees = fichier.read()
    if not donnees or len(donnees) % 12:
        raise SystemExit(f"index illisible : {len(donnees)} octets")
    return [longueur for _position, longueur in struct.iter_unpack("<QI", donnees)]


def resume(lues: list[int], epoque: int, debut: int | None = None, fin: int | None = None) -> dict:
    """Les slots, les slots avec bloc et les octets de l'époque, ou de sa part dans la plage."""
    premier = epoque * SLOTS
    dernier = premier + len(lues) - 1
    debut = premier if debut is None else max(debut, premier)
    fin = dernier if fin is None else min(fin, dernier)
    choisies = lues[debut - premier : fin - premier + 1] if debut <= fin else []
    return {
        "epoque": epoque,
        "debut": debut,
        "fin": fin,
        "slots": len(choisies),
        "slots_avec_bloc": sum(1 for longueur in choisies if longueur),
        "octets": sum(choisies),
    }


def avant(lues: list[int], epoque: int, slot: int) -> int | None:
    """Le dernier slot à bloc strictement avant `slot` dans l'époque, s'il y en a un."""
    premier = epoque * SLOTS
    if len(lues) != SLOTS:
        raise SystemExit(f"index incomplet : {len(lues)} slots au lieu de {SLOTS}")
    if not premier <= slot <= premier + SLOTS:
        raise SystemExit(f"slot {slot} hors de l'époque {epoque}")
    for rang in range(slot - premier - 1, -1, -1):
        if lues[rang]:
            return premier + rang
    return None


def nombre(texte: str) -> int:
    """Un nombre écrit en chiffres, et rien d'autre."""
    if not (texte.isascii() and texte.isdigit()):
        raise SystemExit(f"nombre attendu : {texte!r}")
    return int(texte)


if __name__ == "__main__":
    arguments = sys.argv[1:]
    if len(arguments) == 4 and arguments[2] == "avant":
        trouve = avant(longueurs(arguments[0]), nombre(arguments[1]), nombre(arguments[3]))
        print("-" if trouve is None else trouve)
    elif len(arguments) in (2, 4):
        bornes = [nombre(texte) for texte in arguments[2:4]]
        print(json.dumps(resume(longueurs(arguments[0]), nombre(arguments[1]), *bornes)))
    else:
        raise SystemExit(__doc__)
