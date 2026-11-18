"""Empreintes, champ par champ, d'un flux de transactions Solana au format Yellowstone.

Deux sources qui disent rendre les mêmes transactions (un flux reçu en direct, une archive
relue) ne se comparent pas octet à octet : deux encodages du même message diffèrent. Ce
programme lit des messages ``SubscribeUpdate``, les resérialise de façon déterministe et
range ce qu'ils contiennent dans des colonnes. Lancé des deux côtés, sur la même plage de
slots, il rend des sommes d'empreintes : deux sources égales rendent les mêmes sommes,
colonne par colonne, et une colonne qui diffère dit où chercher.

Entrée : une suite d'enregistrements ::

    genre (1 octet) | longueur (4 octets, gros-boutiste) | contenu

- genre 1 : un message ``SubscribeUpdate`` (une transaction, ou l'en-tête d'un bloc) ;
- genre 2 : un événement de celui qui écrit le flux, objet JSON dont la clé ``e`` dit la
  nature.

Définitions (format 2)
----------------------
**Colonnes.** Trois familles, dans cet ordre.

- ``b.`` : les feuilles (champs qui ne sont pas des messages) de l'en-tête de bloc, sans
  ses récompenses ni son numéro de banque ; un champ de type message a en plus une colonne
  de forme, ``chemin#`` ; ``b.inconnus`` reçoit les champs que les définitions ne
  connaissent pas.
- ``e.`` : sept groupes qui, ensemble, prennent tout d'une transaction, et sont calculés
  pour chacune. ``e.identite`` : un octet de présence (1 si la transaction signée est là,
  plus 2 si le compte rendu d'exécution est là), puis l'enveloppe sans ces deux champs
  (signature, rang, vote, champs inconnus). ``e.transaction`` : la transaction signée. Le
  compte rendu d'exécution, lui, est partagé en cinq parts selon les numéros de ses
  champs : ``e.debut`` (1 à 4 : erreur, frais, soldes), ``e.internes`` (5 : instructions
  internes), ``e.journaux`` (6), ``e.jetons`` (7 et 8 : soldes de jetons) et ``e.fin`` (9
  et au-delà, champs inconnus compris). Le résumé redit, sous ``groupes``, les champs de
  chaque part d'après les définitions lues.
- ``t.`` : les feuilles de la transaction et de son compte rendu, colonnes de forme et
  ``t.inconnus`` comprises, calculées pour les seules transactions des tranches de slots
  dont le rang est multiple de ``echantillon`` : le détail qui nomme un champ, là où un
  groupe ne nomme qu'une part.

**Matière** d'une feuille, pour un message : les morceaux (étiquette, longueur, valeur) du
champ dans la sérialisation déterministe, dans l'ordre ; un octet ``01`` par occurrence pour
une colonne de forme. Matière d'un groupe : les octets de la sérialisation déterministe de
ce qu'il prend (pour une part du compte rendu, la suite des morceaux de ses champs).

**Valeur** d'une colonne non vide : les 16 premiers octets de
``sha256(slot ‖ rang ‖ matière)`` pour une transaction, de ``sha256(slot ‖ matière)`` pour
un en-tête (slot et rang sur 8 octets, gros-boutistes), lus comme un entier. Une colonne
vide ne vaut rien. La **somme** d'une colonne, sur une tranche de slots ou sur la plage, est
celle de ses valeurs modulo 2^128 : elle ne dépend pas de l'ordre d'arrivée, et les sommes
de deux tranches s'additionnent.

**Empreinte d'ensemble** d'une transaction : les 16 premiers octets du ``sha256`` des sept
``sha256(slot ‖ rang ‖ matière)`` de ses groupes, mis bout à bout, vides compris.

**Doubles.** Une transaction est connue par son slot et son rang. Revue à l'identique, elle
est comptée comme double et n'est pas sommée ; revue différente, c'est une anomalie.

**Coupe des journaux.** Un nœud ne garde des journaux d'une transaction que ce qui tient
sous une limite d'octets (voir ``couper``) ; une archive peut les avoir entiers. Avec
``--coupe``, les journaux lus passent par la même règle avant d'être rangés. Une
transaction est comptée **coupée** quand une ligne de ses journaux est la marque de coupe.
"""

from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import platform
import struct
import sys
from collections import Counter, OrderedDict, deque
from collections.abc import Iterator, Sequence
from pathlib import Path
from typing import Any, BinaryIO, TextIO

import google.protobuf
from google.protobuf.message import DecodeError

FORMAT = 2
MESSAGE = 1
EVENEMENT = 2
MARQUE = "Log truncated"
TAILLE_MAX = 256 << 20
# L'en-tête du fichier des slots clos (voir ``Empreintes``).
SLOTS = "slot,entete,transactions,empreinte"
# Ce qui, dans un en-tête de bloc, ne se compare pas d'une source à l'autre : les
# récompenses (toutes les sources ne les rendent pas) et le numéro de banque, compteur
# propre au nœud qui émet.
HORS_BLOC = ("b.rewards", "b.bank_id")
GROUPES = (
    "e.identite",
    "e.transaction",
    "e.debut",
    "e.internes",
    "e.journaux",
    "e.jetons",
    "e.fin",
)
EXEMPLES = 5

_ENTETE = struct.Struct(">BI")
_CHAMP_MESSAGE = 11  # le type « message » d'un champ, dans les descripteurs du protocole
_INCONNU = object()
_MODULE = 1 << 128
_FIXES = 4  # blocs, transactions, échecs, journaux coupés : avant les sommes d'une tranche
# Les parts du compte rendu d'exécution avant la dernière, et le plus grand numéro de champ
# que chacune prend ; ``e.fin`` prend le reste.
_PARTS = (("e.debut", 4), ("e.internes", 5), ("e.journaux", 6), ("e.jetons", 8))


class FluxCoupe(Exception):
    """Le flux s'arrête au milieu d'un enregistrement, ou n'en est pas un."""


class FusionRefusee(Exception):
    """Des passages qui ne peuvent pas être réunis en un seul."""


def lire(flux: BinaryIO) -> Iterator[tuple[int, bytes]]:
    """Les enregistrements du flux, dans l'ordre : (genre, contenu)."""
    while True:
        entete = flux.read(_ENTETE.size)
        if not entete:
            return
        if len(entete) < _ENTETE.size:
            raise FluxCoupe("en-tête d'enregistrement coupé")
        genre, longueur = _ENTETE.unpack(entete)
        if genre not in (MESSAGE, EVENEMENT):
            raise FluxCoupe(f"genre {genre} inconnu")
        if longueur > TAILLE_MAX:
            raise FluxCoupe(f"longueur {longueur} démesurée")
        contenu = flux.read(longueur)
        if len(contenu) < longueur:
            raise FluxCoupe("contenu d'enregistrement coupé")
        yield genre, contenu


def couper(lignes: Sequence[str], limite: int) -> list[str]:
    """Les journaux tels qu'un nœud les garde sous une limite d'octets.

    Règle du collecteur de journaux du validateur : chaque ligne ajoute sa taille (en
    octets) à un compteur ; la ligne qui le ferait atteindre la limite est écartée et, la
    première fois, remplacée par ``Log truncated`` ; une ligne écartée n'avance pas le
    compteur, si bien qu'une ligne plus courte peut encore passer ensuite. La règle ne
    s'applique qu'à des journaux entiers : l'appliquer deux fois ne redonne pas le même
    résultat.
    """
    gardees: list[str] = []
    ecrits, avertie = 0, False
    for ligne in lignes:
        apres = ecrits + len(ligne.encode())
        if apres >= limite:
            if not avertie:
                avertie = True
                gardees.append(MARQUE)
        else:
            ecrits = apres
            gardees.append(ligne)
    return gardees


class _Plan:
    """Pour un type de message, ce que devient chaque champ, par numéro.

    Une feuille vaut le rang de sa colonne ; un champ de type message, le couple (rang de
    sa colonne de forme, plan du sous-message) ; un champ laissé de côté, ``None``. Un
    numéro absent du plan est un champ que les définitions ne connaissent pas.
    """

    __slots__ = ("champs",)

    def __init__(self, descripteur: Any, prefixe: str, noms: list[str], hors: Sequence[str]):
        self.champs: dict[int, int | tuple[int, _Plan] | None] = {}
        for champ in sorted(descripteur.fields, key=lambda champ: champ.number):
            chemin = f"{prefixe}.{champ.name}"
            if chemin in hors:
                self.champs[champ.number] = None
            elif champ.type == _CHAMP_MESSAGE:
                noms.append(chemin + "#")
                colonne = len(noms) - 1
                self.champs[champ.number] = (colonne, _Plan(champ.message_type, chemin, noms, hors))
            else:
                noms.append(chemin)
                self.champs[champ.number] = len(noms) - 1


def _ranger(
    donnees: bytes, plan: _Plan, matiere: dict[int, list[bytes]], inconnus: list[bytes]
) -> None:
    """Range les champs d'un message sérialisé dans leurs colonnes, sous-messages compris."""
    champs = plan.champs
    position, fin = 0, len(donnees)
    while position < fin:
        debut = position
        cle = donnees[position]
        position += 1
        if cle >= 0x80:
            cle &= 0x7F
            decalage = 7
            while True:
                octet = donnees[position]
                position += 1
                cle |= (octet & 0x7F) << decalage
                if octet < 0x80:
                    break
                decalage += 7
        nature = cle & 7
        corps = position
        if nature == 2:
            taille = donnees[position]
            position += 1
            if taille >= 0x80:
                taille &= 0x7F
                decalage = 7
                while True:
                    octet = donnees[position]
                    position += 1
                    taille |= (octet & 0x7F) << decalage
                    if octet < 0x80:
                        break
                    decalage += 7
            corps = position
            position += taille
        elif nature == 0:
            while donnees[position] >= 0x80:
                position += 1
            position += 1
        elif nature == 1:
            position += 8
        elif nature == 5:
            position += 4
        else:
            raise ValueError(f"nature de champ {nature} inattendue")
        if position > fin:
            raise ValueError("message coupé")
        connu = champs.get(cle >> 3, _INCONNU)
        if connu is None:
            continue
        if connu is _INCONNU:
            inconnus.append(donnees[debut:position])
            continue
        if connu.__class__ is int:
            colonne, morceau = connu, donnees[debut:position]
        elif nature != 2:
            colonne, morceau = connu[0], donnees[debut:position]
        else:
            colonne, morceau = connu[0], b"\x01"
            _ranger(donnees[corps:position], connu[1], matiere, inconnus)
        rangee = matiere.get(colonne)
        if rangee is None:
            matiere[colonne] = [morceau]
        else:
            rangee.append(morceau)


def _valeur(cle: bytes, morceaux: Sequence[bytes]) -> int:
    matiere = morceaux[0] if len(morceaux) == 1 else b"".join(morceaux)
    return int.from_bytes(hashlib.sha256(cle + matiere).digest()[:16], "big")


def part_du_lot(
    premier: int, dernier: int, tranche: int, lot: tuple[int, int]
) -> tuple[int, int] | None:
    """Les slots, bornes comprises, que le lot ``(k, n)`` prend dans une plage.

    Les tranches que touche la plage sont partagées en ``n`` suites qui se suivent, de même
    longueur à une tranche près : la tranche de rang ``r``, parmi les ``t`` de la plage,
    revient au lot ``r × n ÷ t`` (division entière). ``None`` si le lot n'a aucune tranche.
    """
    rang, lots = lot
    origine = premier // tranche
    tranches = dernier // tranche - origine + 1
    debut = -(-rang * tranches // lots)
    fin = -(-(rang + 1) * tranches // lots) - 1
    if debut > fin:
        return None
    return (
        max(premier, (origine + debut) * tranche),
        min(dernier, (origine + fin + 1) * tranche - 1),
    )


def _bibliotheque() -> str:
    try:
        from google.protobuf.internal import api_implementation

        moteur = api_implementation.Type()
    except Exception:
        moteur = "?"
    return f"protobuf {google.protobuf.__version__} ({moteur})"


class Empreintes:
    """Les sommes d'empreintes d'une plage de slots, message après message.

    ``message`` est la classe ``SubscribeUpdate`` des définitions du flux. ``coupe``, s'il
    est donné, est la limite d'octets sous laquelle les journaux lus sont d'abord coupés.
    Les feuilles d'une transaction ne sont calculées que dans une tranche sur
    ``echantillon`` (1 : dans toutes) ; ses groupes le sont toujours.
    Un slot est clos quand ``retard`` en-têtes sont arrivés après le sien ; ce qu'il
    contenait reste en mémoire pendant ``souvenir`` clôtures, pour reconnaître un double
    tardif. ``slots`` reçoit une ligne par slot clos : slot, en-tête (0 ou 1), nombre de
    transactions, empreinte d'ensemble.

    ``lot``, s'il est donné, vaut ``(k, n)`` : ``n`` passages se partagent alors la plage,
    celui-ci ne prenant que les messages de sa part (voir ``part_du_lot``) ; hors de la
    plage, un message revient au lot du bord le plus proche. Ce qui n'a pas de slot revient
    au lot 0 ; quand les passages ne lisent pas tous les mêmes messages, celui qui les donne
    dit à chacun, par ``orphelins``, s'il prend ceux-là. ``fusionner`` réunit ensuite les
    résultats.
    """

    def __init__(
        self,
        message: Any,
        *,
        premier: int,
        dernier: int,
        coupe: int | None = None,
        tranche: int = 100,
        echantillon: int = 16,
        retard: int = 256,
        souvenir: int = 4096,
        slots: TextIO | None = None,
        lot: tuple[int, int] | None = None,
    ) -> None:
        if premier > dernier or tranche < 1 or retard < 0 or souvenir < 0 or echantillon < 1:
            raise ValueError("plage ou réglages impossibles")
        if lot is not None and not 0 <= lot[0] < lot[1]:
            raise ValueError("plage ou réglages impossibles")
        self._lot = lot
        self._part = None if lot is None else part_du_lot(premier, dernier, tranche, lot)
        self.orphelins = lot is None or lot[0] == 0
        self._message = message
        self._premier, self._dernier = premier, dernier
        self._coupe, self._pas, self._echantillon = coupe, tranche, echantillon
        self._retard, self._souvenir = retard, souvenir
        self._slots = slots
        descripteur = message.DESCRIPTOR
        bloc = descripteur.fields_by_name["block_meta"].message_type
        enveloppe = descripteur.fields_by_name["transaction"].message_type
        transaction = enveloppe.fields_by_name["transaction"].message_type
        noms: list[str] = []
        self._plan_bloc = _Plan(bloc, "b", noms, HORS_BLOC)
        self._bloc_inconnus = len(noms)
        noms.append("b.inconnus")
        self._groupes = _FIXES + len(noms)  # dans une ligne, la place du premier groupe
        noms.extend(GROUPES)
        self._feuilles = len(noms)  # parmi les colonnes, le rang de la première feuille
        self._plan_transaction = _Plan(transaction, "t", noms, ())
        self._inconnus = len(noms)
        noms.append("t.inconnus")
        self.colonnes: tuple[str, ...] = tuple(noms)
        # Les champs de chaque part du compte rendu d'exécution, par leurs numéros.
        compte_rendu = transaction.fields_by_name["meta"].message_type
        champs = sorted(compte_rendu.fields, key=lambda champ: champ.number)
        self._parts: list[tuple[str, ...]] = []
        self._contenu: dict[str, list[str]] = {}
        borne = 0
        for nom, plus_grand in _PARTS:
            pris = tuple(champ.name for champ in champs if borne < champ.number <= plus_grand)
            self._parts.append(pris)
            self._contenu[nom] = list(pris)
            borne = plus_grand
        self._contenu[GROUPES[-1]] = [champ.name for champ in champs if champ.number > borne]
        # La marque de coupe telle qu'elle est sérialisée : une ligne de journaux entière.
        journaux = compte_rendu.fields_by_name["log_messages"].number
        self._marque = bytes((journaux << 3 | 2, len(MARQUE))) + MARQUE.encode()
        # tranche -> [blocs, transactions, échecs, coupées, puis une somme par colonne]
        self._tranches: dict[int, list[int]] = {}
        # slot ouvert -> [empreinte de son en-tête ou None, {rang: empreinte d'ensemble}]
        self._ouverts: dict[int, list[Any]] = {}
        self._attente: deque[int] = deque()
        self._souvenirs: OrderedDict[int, list[Any]] = OrderedDict()
        self._clos = bytearray((dernier - premier) // 8 + 1)
        self._comptes: Counter[str] = Counter()
        self._autres: Counter[str] = Counter()
        self._anomalies: Counter[str] = Counter()
        self._exemples: dict[str, list[str]] = {}
        self._resume: dict[str, Any] | None = None

    # -- ce qui arrive --------------------------------------------------------------------

    def ajouter(self, contenu: bytes) -> None:
        """Prend un message du flux (le contenu d'un enregistrement de genre 1)."""
        if self._resume is not None:
            raise RuntimeError("le calcul est fini")
        self._comptes["messages"] += 1
        try:
            message = self._message.FromString(contenu)
        except DecodeError:
            if self._a_moi(None):
                self._anomalie("illisible", f"message n° {self._comptes['messages']}")
            return
        genre = message.WhichOneof("update_oneof")
        try:
            if genre == "transaction":
                if self._a_moi(message.transaction.slot):
                    self._transaction(message.transaction)
            elif genre == "block_meta":
                if self._a_moi(message.block_meta.slot):
                    self._bloc(message.block_meta)
            elif self._a_moi(None):
                self._autres[genre or "vide"] += 1
        except (ValueError, IndexError):
            self._anomalie("illisible", f"message n° {self._comptes['messages']}")

    def _a_moi(self, slot: int | None) -> bool:
        """Ce message revient-il à ce passage ? Sans lot, toujours."""
        if self._lot is None:
            return True
        if slot is None:
            a_moi = self.orphelins
        else:
            part = self._part
            ramene = min(max(slot, self._premier), self._dernier)
            a_moi = part is not None and part[0] <= ramene <= part[1]
        if not a_moi:
            self._comptes["autres_lots"] += 1
        return a_moi

    def _transaction(self, recue: Any) -> None:
        slot = recue.slot
        if not self._premier <= slot <= self._dernier:
            self._comptes["hors_plage"] += 1
            return
        if not recue.HasField("transaction"):
            raise ValueError("transaction sans contenu")
        info = recue.transaction
        rang = info.index
        cle = slot.to_bytes(8, "big") + rang.to_bytes(8, "big")
        # Un sous-message absent se lit comme un sous-message vide : seule l'identité dit
        # s'il était là.
        presence = bytes((info.HasField("transaction") | info.HasField("meta") << 1,))
        compte_rendu = info.meta
        echec = compte_rendu.HasField("err")
        serie = compte_rendu.SerializeToString(deterministic=True)
        if self._coupe is not None and len(serie) >= self._coupe:
            # Des journaux qui atteignent la limite pèsent au moins autant dans la série :
            # en dessous, la règle ne couperait rien.
            entiers = list(compte_rendu.log_messages)
            gardes = couper(entiers, self._coupe)
            if gardes != entiers:
                del compte_rendu.log_messages[:]
                compte_rendu.log_messages.extend(gardes)
                serie = compte_rendu.SerializeToString(deterministic=True)
        coupee = self._marque in serie and MARQUE in compte_rendu.log_messages
        detail: tuple[dict[int, list[bytes]], list[bytes]] | None = None
        if (slot // self._pas) % self._echantillon == 0:
            detail = ({}, [])
            _ranger(info.SerializeToString(deterministic=True), self._plan_transaction, *detail)
        # Les champs sortent dans l'ordre de leurs numéros, les inconnus à la fin : en
        # effaçant les parts du compte rendu une à une depuis le début, la taille de ce qui
        # reste dit où chacune finissait dans la série.
        bornes = [0]
        for noms in self._parts:
            for nom in noms:
                compte_rendu.ClearField(nom)
            bornes.append(len(serie) - compte_rendu.ByteSize())
        bornes.append(len(serie))
        signee = info.transaction.SerializeToString(deterministic=True)
        info.ClearField("transaction")
        info.ClearField("meta")
        identite = presence + info.SerializeToString(deterministic=True)
        parts = (serie[debut:fin] for debut, fin in itertools.pairwise(bornes))
        matieres = (identite, signee, *parts)
        marques = [hashlib.sha256(cle + matiere).digest() for matiere in matieres]
        ensemble = hashlib.sha256(b"".join(marques)).digest()[:16]

        etat = self._ouverts.get(slot)
        if etat is None:
            if self._est_clos(slot):
                self._apres_cloture(slot, rang, ensemble)
                return
            etat = self._ouverts[slot] = [None, {}]
        connue = etat[1].get(rang)
        if connue is not None:
            if connue == ensemble:
                self._comptes["doubles"] += 1
            else:
                self._anomalie("double_different", f"{slot}/{rang}")
            return
        etat[1][rang] = ensemble

        ligne = self._tranche(slot)
        ligne[1] += 1
        ligne[2] += echec
        ligne[3] += coupee
        place = self._groupes
        for marque, matiere in zip(marques, matieres, strict=True):
            if matiere:
                ligne[place] += int.from_bytes(marque[:16], "big")
            place += 1
        if detail is not None:
            self._comptes["echantillonnees"] += 1
            for colonne, morceaux in detail[0].items():
                ligne[_FIXES + colonne] += _valeur(cle, morceaux)
            if detail[1]:
                ligne[_FIXES + self._inconnus] += _valeur(cle, detail[1])

    def _bloc(self, recu: Any) -> None:
        slot = recu.slot
        if not self._premier <= slot <= self._dernier:
            self._comptes["hors_plage_blocs"] += 1
            return
        matiere: dict[int, list[bytes]] = {}
        inconnus: list[bytes] = []
        _ranger(recu.SerializeToString(deterministic=True), self._plan_bloc, matiere, inconnus)
        cle = slot.to_bytes(8, "big")
        gardee = hashlib.sha256()
        for colonne in sorted(matiere):
            contenu = b"".join(matiere[colonne])
            gardee.update(colonne.to_bytes(2, "big") + len(contenu).to_bytes(4, "big") + contenu)
        gardee.update(b"".join(inconnus))
        empreinte = gardee.digest()

        etat = self._ouverts.get(slot)
        if etat is None:
            if self._est_clos(slot):
                souvenir = self._souvenirs.get(slot)
                if souvenir is None:
                    self._anomalie("bloc_tardif", str(slot))
                elif souvenir[0] == empreinte:
                    self._comptes["blocs_doubles"] += 1
                else:
                    self._anomalie("bloc_different", str(slot))
                return
            etat = self._ouverts[slot] = [None, {}]
        if etat[0] is not None:
            if etat[0] == empreinte:
                self._comptes["blocs_doubles"] += 1
            else:
                self._anomalie("bloc_different", str(slot))
            return
        etat[0] = empreinte

        ligne = self._tranche(slot)
        ligne[0] += 1
        for colonne, morceaux in matiere.items():
            ligne[_FIXES + colonne] += _valeur(cle, morceaux)
        if inconnus:
            ligne[_FIXES + self._bloc_inconnus] += _valeur(cle, inconnus)
        self._attente.append(slot)
        while len(self._attente) > self._retard:
            self._clore(self._attente.popleft())

    # -- la tenue des slots ----------------------------------------------------------------

    def _tranche(self, slot: int) -> list[int]:
        debut = slot // self._pas * self._pas
        ligne = self._tranches.get(debut)
        if ligne is None:
            ligne = self._tranches[debut] = [0] * (_FIXES + len(self.colonnes))
        return ligne

    def _est_clos(self, slot: int) -> bool:
        place = slot - self._premier
        return bool(self._clos[place >> 3] & (1 << (place & 7)))

    def _clore(self, slot: int) -> None:
        etat = self._ouverts.pop(slot)
        place = slot - self._premier
        self._clos[place >> 3] |= 1 << (place & 7)
        self._comptes["slots"] += 1
        if self._slots is not None:
            en_tete, transactions = etat
            ensemble = hashlib.sha256(slot.to_bytes(8, "big"))
            ensemble.update(b"\x00" if en_tete is None else b"\x01" + en_tete)
            for rang in sorted(transactions):
                ensemble.update(rang.to_bytes(8, "big") + transactions[rang])
            self._slots.write(
                f"{slot},{0 if en_tete is None else 1},{len(transactions)},"
                f"{ensemble.digest()[:8].hex()}\n"
            )
        self._souvenirs[slot] = etat
        while len(self._souvenirs) > self._souvenir:
            self._souvenirs.popitem(last=False)

    def _apres_cloture(self, slot: int, rang: int, ensemble: bytes) -> None:
        souvenir = self._souvenirs.get(slot)
        if souvenir is None:
            self._anomalie("tardive", f"{slot}/{rang}")
        elif souvenir[1].get(rang) == ensemble:
            self._comptes["doubles"] += 1
        elif rang in souvenir[1]:
            self._anomalie("double_different", f"{slot}/{rang}")
        else:
            self._anomalie("apres_cloture", f"{slot}/{rang}")

    def _anomalie(self, nom: str, ou: str) -> None:
        self._anomalies[nom] += 1
        exemples = self._exemples.setdefault(nom, [])
        if len(exemples) < EXEMPLES:
            exemples.append(ou)

    # -- ce qui en sort --------------------------------------------------------------------

    def finir(self) -> dict[str, Any]:
        """Clôt tous les slots et rend le résumé : réglages, comptes, anomalies, totaux."""
        if self._resume is not None:
            return self._resume
        while self._attente:
            self._clore(self._attente.popleft())
        for slot in sorted(self._ouverts):
            self._anomalie("sans_en_tete", str(slot))
            self._clore(slot)
        totaux = [0] * (_FIXES + len(self.colonnes))
        for ligne in self._tranches.values():
            for rang, nombre in enumerate(ligne):
                totaux[rang] += nombre
        comptes = self._comptes
        self._resume = {
            "format": FORMAT,
            "plage": [self._premier, self._dernier],
            "tranche": self._pas,
            "echantillon": self._echantillon,
            "coupe": self._coupe,
            "retard": self._retard,
            "souvenir": self._souvenir,
            "lot": None if self._lot is None else list(self._lot),
            "messages": comptes["messages"],
            "autres_lots": comptes["autres_lots"],
            "slots": comptes["slots"],
            "blocs": totaux[0],
            "transactions": totaux[1],
            "echecs": totaux[2],
            "coupees": totaux[3],
            "echantillonnees": comptes["echantillonnees"],
            "doubles": comptes["doubles"],
            "blocs_doubles": comptes["blocs_doubles"],
            "hors_plage": comptes["hors_plage"],
            "hors_plage_blocs": comptes["hors_plage_blocs"],
            "autres": dict(sorted(self._autres.items())),
            "anomalies": dict(sorted(self._anomalies.items())),
            "exemples": dict(sorted(self._exemples.items())),
            "groupes": self._contenu,
            "colonnes": list(self.colonnes),
            "totaux": {
                nom: format(totaux[_FIXES + rang] % _MODULE, "032x")
                for rang, nom in enumerate(self.colonnes)
            },
            "programme": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "bibliotheque": _bibliotheque(),
            "python": platform.python_version(),
        }
        return self._resume

    def lignes(self) -> Iterator[list[Any]]:
        """Après ``finir`` : une ligne par tranche, dans l'ordre des slots.

        ``[tranche, blocs, transactions, échecs, coupées, puis la somme de chaque colonne]`` ;
        hors de l'échantillon, les feuilles d'une tranche n'ont pas été calculées et leurs
        sommes sont laissées vides.
        """
        if self._resume is None:
            raise RuntimeError("le calcul n'est pas fini")
        for debut in sorted(self._tranches):
            ligne = self._tranches[debut]
            sommes = [format(somme % _MODULE, "032x") for somme in ligne[_FIXES:]]
            if (debut // self._pas) % self._echantillon:
                sommes[self._feuilles :] = [""] * (len(sommes) - self._feuilles)
            yield [debut, *ligne[:_FIXES], *sommes]


def ecrire(dossier: Path, empreintes: Empreintes, resume: dict[str, Any]) -> None:
    """Écrit ``tranches.csv`` et ``resume.json`` dans ``dossier``, créé au besoin."""
    dossier.mkdir(parents=True, exist_ok=True)
    with (dossier / "tranches.csv").open("w", encoding="utf-8") as tranches:
        tranches.write(
            "tranche,blocs,transactions,echecs,coupees," + ",".join(empreintes.colonnes) + "\n"
        )
        for ligne in empreintes.lignes():
            tranches.write(",".join(str(valeur) for valeur in ligne) + "\n")
    texte = json.dumps(resume, ensure_ascii=False, indent=1)
    (dossier / "resume.json").write_text(texte + "\n", encoding="utf-8")


_COMMUNS = (
    "format",
    "plage",
    "tranche",
    "echantillon",
    "coupe",
    "retard",
    "souvenir",
    "groupes",
    "colonnes",
    "programme",
    "bibliotheque",
)
_SOMMES = (
    "slots",
    "blocs",
    "transactions",
    "echecs",
    "coupees",
    "echantillonnees",
    "doubles",
    "blocs_doubles",
    "hors_plage",
    "hors_plage_blocs",
)


def fusionner(dossiers: Sequence[Path], sortie: Path) -> dict[str, Any]:
    """Réunit les résultats de passages qui se sont partagé une plage ; rend le résumé.

    Les passages doivent avoir les mêmes réglages et le même programme, et aucune tranche
    en commun. S'ils se partageaient la plage par lots, tous les lots doivent être là.
    Le résultat est écrit dans ``sortie`` : ``tranches.csv``, ``slots.csv``, ``resume.json``.
    """
    if not dossiers:
        raise FusionRefusee("rien à fusionner")
    resumes = [
        json.loads((dossier / "resume.json").read_text(encoding="utf-8")) for dossier in dossiers
    ]
    premier = resumes[0]
    for nom in _COMMUNS:
        if any(resume[nom] != premier[nom] for resume in resumes):
            raise FusionRefusee(f"les passages n'ont pas le même réglage « {nom} »")
    lots = [resume["lot"] for resume in resumes]
    if any(lot is not None for lot in lots):
        attendus = sorted([rang, len(lots)] for rang in range(len(lots)))
        if any(lot is None for lot in lots) or sorted(lots) != attendus:
            raise FusionRefusee(f"les lots ne font pas un tout : {lots}")

    tranches: dict[int, str] = {}
    en_tete = ""
    for dossier in dossiers:
        with (dossier / "tranches.csv").open(encoding="utf-8") as lues:
            en_tete = lues.readline()
            for ligne in lues:
                debut = int(ligne.split(",", 1)[0])
                if debut in tranches:
                    raise FusionRefusee(f"tranche {debut} présente deux fois")
                tranches[debut] = ligne
    colonnes = premier["colonnes"]
    totaux = dict.fromkeys(colonnes, 0)
    for resume in resumes:
        for nom in colonnes:
            totaux[nom] += int(resume["totaux"][nom], 16)
    autres: Counter[str] = Counter()
    anomalies: Counter[str] = Counter()
    exemples: dict[str, list[str]] = {}
    for resume in resumes:
        autres.update(resume["autres"])
        anomalies.update(resume["anomalies"])
        for nom, lieux in resume["exemples"].items():
            exemples[nom] = (exemples.get(nom, []) + lieux)[:EXEMPLES]
    fusion = {nom: premier[nom] for nom in _COMMUNS if nom != "colonnes"}
    fusion.update(
        lot=None,
        lots=len(resumes),
        messages=sum(resume["messages"] - resume["autres_lots"] for resume in resumes),
        autres_lots=0,
    )
    fusion.update({nom: sum(resume[nom] for resume in resumes) for nom in _SOMMES})
    fusion.update(
        autres=dict(sorted(autres.items())),
        anomalies=dict(sorted(anomalies.items())),
        exemples=dict(sorted(exemples.items())),
        colonnes=colonnes,
        totaux={nom: format(totaux[nom] % _MODULE, "032x") for nom in colonnes},
        python=premier["python"],
    )
    if any("flux" in resume for resume in resumes):
        # Des voies qui lisaient chacune un flux : ce que chacune en dit se garde, et une
        # seule voie coupée suffit pour que l'ensemble ne soit pas dit entier.
        evenements: Counter[str] = Counter()
        coupees = []
        for rang, resume in enumerate(resumes):
            evenements.update(resume.get("evenements", {}))
            if resume.get("flux") != "entier":
                coupees.append(f"voie {rang} : {resume.get('flux')}")
        fusion.update(
            flux=" ; ".join(coupees) or "entier",
            evenements=dict(sorted(evenements.items())),
            evenements_illisibles=sum(resume.get("evenements_illisibles", 0) for resume in resumes),
            fins=[resume.get("fin") for resume in resumes],
        )

    sortie.mkdir(parents=True, exist_ok=True)
    with (sortie / "tranches.csv").open("w", encoding="utf-8") as ecrites:
        ecrites.write(en_tete)
        for debut in sorted(tranches):
            ecrites.write(tranches[debut])
    with (sortie / "slots.csv").open("w", encoding="utf-8") as ecrits:
        ecrits.write(SLOTS + "\n")
        for dossier in dossiers:
            with (dossier / "slots.csv").open(encoding="utf-8") as lus:
                lus.readline()
                for ligne in lus:
                    ecrits.write(ligne)
    texte = json.dumps(fusion, ensure_ascii=False, indent=1)
    (sortie / "resume.json").write_text(texte + "\n", encoding="utf-8")
    return fusion


def _plage(texte: str) -> tuple[int, int]:
    bornes = texte.split(":")
    if len(bornes) != 2 or not all(borne.isdigit() for borne in bornes):
        raise argparse.ArgumentTypeError("plage attendue : « premier:dernier », bornes comprises")
    premier, dernier = int(bornes[0]), int(bornes[1])
    if premier > dernier:
        raise argparse.ArgumentTypeError("plage attendue : le premier slot avant le dernier")
    return premier, dernier


def _positif(texte: str) -> int:
    if not texte.isdigit() or int(texte) < 1:
        raise argparse.ArgumentTypeError("nombre attendu, au moins 1")
    return int(texte)


def main(argv: list[str] | None = None) -> int:
    """Lit un flux d'enregistrements sur l'entrée standard et écrit ses empreintes ; ou, avec
    ``--fusionner``, réunit les résultats de plusieurs passages."""
    lecteur = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    lecteur.add_argument(
        "--definitions",
        type=Path,
        help="dossier du module geyser_pb2 produit depuis les définitions du flux",
    )
    lecteur.add_argument("--plage", type=_plage, help="premier:dernier, bornes comprises")
    lecteur.add_argument(
        "--coupe", type=_positif, help="limite d'octets des journaux, s'ils sont entiers"
    )
    lecteur.add_argument("--tranche", type=_positif, default=100, help="slots par tranche")
    lecteur.add_argument(
        "--echantillon",
        type=_positif,
        default=16,
        help="une tranche sur N porte le détail des feuilles",
    )
    lecteur.add_argument(
        "--fusionner", type=Path, nargs="+", help="dossiers de passages à réunir en un seul"
    )
    lecteur.add_argument("--sortie", type=Path, required=True, help="dossier des résultats")
    dit = lecteur.parse_args(argv)

    if dit.fusionner is not None:
        if dit.plage is not None or dit.definitions is not None or dit.coupe is not None:
            lecteur.error("--fusionner ne se combine qu'avec --sortie")
        try:
            resume = fusionner(dit.fusionner, dit.sortie)
        except FusionRefusee as refus:
            print(f"fusion refusée : {refus}", file=sys.stderr)
            return 2
        print(json.dumps(resume, ensure_ascii=False, indent=1))
        etat = resume.get("flux", "entier")
        if etat != "entier":
            print(f"flux coupé : {etat}", file=sys.stderr)
            return 1
        return 0
    if dit.plage is None or dit.definitions is None:
        lecteur.error("il faut --definitions et --plage, ou bien --fusionner")

    sys.path.insert(0, str(dit.definitions))
    import geyser_pb2

    dit.sortie.mkdir(parents=True, exist_ok=True)
    evenements: Counter[str] = Counter()
    illisibles, fin, etat = 0, None, "entier"
    with (dit.sortie / "slots.csv").open("w", encoding="utf-8") as slots:
        slots.write(SLOTS + "\n")
        empreintes = Empreintes(
            geyser_pb2.SubscribeUpdate,
            premier=dit.plage[0],
            dernier=dit.plage[1],
            coupe=dit.coupe,
            tranche=dit.tranche,
            echantillon=dit.echantillon,
            slots=slots,
        )
        try:
            for genre, contenu in lire(sys.stdin.buffer):
                if genre == MESSAGE:
                    empreintes.ajouter(contenu)
                    continue
                try:
                    evenement = json.loads(contenu)
                    nature = evenement["e"]
                    if not isinstance(nature, str):
                        raise TypeError(nature)
                except (ValueError, TypeError, KeyError):
                    illisibles += 1
                    continue
                evenements[nature] += 1
                if nature == "fin":
                    fin = evenement
        except FluxCoupe as coupure:
            etat = f"coupé : {coupure}"
            print(f"flux coupé : {coupure}", file=sys.stderr)
        resume = empreintes.finir()
    resume.update(
        evenements=dict(sorted(evenements.items())),
        evenements_illisibles=illisibles,
        fin=fin,
        flux=etat,
    )
    ecrire(dit.sortie, empreintes, resume)
    print(json.dumps(resume, ensure_ascii=False, indent=1))
    return 0 if etat == "entier" else 1


if __name__ == "__main__":
    sys.exit(main())
