"""Octets de l'archive par slot, d'après l'index des plages d'une époque.

Usage : octets.py FICHIER_INDEX ÉPOQUE [DÉBUT FIN]

L'index porte, pour chaque slot de l'époque et dans l'ordre, douze octets petit-boutistes :
la position (8 octets) et la longueur (4 octets) de ce qui appartient au slot dans le fichier
de l'époque ; un slot sans bloc est tout à zéro. Le script écrit une ligne JSON : pour
l'époque entière, ou pour la part de la plage « DÉBUT FIN » (bornes comprises) qui s'y trouve.
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


if __name__ == "__main__":
    if len(sys.argv) not in (3, 5):
        raise SystemExit(__doc__)
    bornes = [int(texte) for texte in sys.argv[3:5]]
    print(json.dumps(resume(longueurs(sys.argv[1]), int(sys.argv[2]), *bornes)))
