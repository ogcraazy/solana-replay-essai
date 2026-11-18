//! Le flux que rend le greffon, sans autre dépendance que la bibliothèque standard.
//!
//! Une suite d'enregistrements :
//!
//! ```text
//! genre (1 octet) | longueur (4 octets, gros-boutiste) | contenu
//! ```
//!
//! - genre 1 : un message `SubscribeUpdate` du protocole Yellowstone gRPC (une transaction, ou
//!   l'en-tête d'un bloc), écrit ici champ par champ d'après `geyser.proto` ;
//! - genre 2 : un événement de celui qui écrit, objet JSON dont la clé `e` dit la nature.
//!
//! La transaction signée et son compte rendu d'exécution sont donnés déjà sérialisés (les
//! messages `Transaction` et `TransactionStatusMeta` de `solana-storage.proto`) : ce module
//! n'écrit que ce qui les enveloppe.

use std::io::{self, Write};

/// Genre d'un enregistrement : un message du flux.
pub const MESSAGE: u8 = 1;
/// Genre d'un enregistrement : un événement, en JSON.
pub const EVENEMENT: u8 = 2;

/// Un entier au format de longueur variable de protobuf.
fn variable(sortie: &mut Vec<u8>, mut valeur: u64) {
    while valeur >= 0x80 {
        sortie.push((valeur & 0x7f) as u8 | 0x80);
        valeur >>= 7;
    }
    sortie.push(valeur as u8);
}

/// Le nombre d'octets de cette écriture.
fn taille(mut valeur: u64) -> usize {
    let mut octets = 1;
    while valeur >= 0x80 {
        octets += 1;
        valeur >>= 7;
    }
    octets
}

/// Un champ entier ; comme le veut proto3, un zéro ne s'écrit pas.
fn entier(sortie: &mut Vec<u8>, numero: u64, valeur: u64) {
    if valeur != 0 {
        variable(sortie, numero << 3);
        variable(sortie, valeur);
    }
}

fn taille_entier(numero: u64, valeur: u64) -> usize {
    if valeur == 0 {
        0
    } else {
        taille(numero << 3) + taille(valeur)
    }
}

/// L'étiquette et la longueur d'un champ d'octets, de texte ou de sous-message ; le contenu
/// suit. Un tel champ s'écrit même vide : pour un sous-message, c'est dire qu'il est présent.
fn ouvrir(sortie: &mut Vec<u8>, numero: u64, longueur: usize) {
    variable(sortie, numero << 3 | 2);
    variable(sortie, longueur as u64);
}

fn taille_ouverture(numero: u64, longueur: usize) -> usize {
    taille(numero << 3 | 2) + taille(longueur as u64)
}

/// Le genre et la longueur d'un enregistrement.
fn tete(genre: u8, longueur: usize) -> io::Result<[u8; 5]> {
    let longueur = u32::try_from(longueur)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "enregistrement démesuré"))?;
    let octets = longueur.to_be_bytes();
    Ok([genre, octets[0], octets[1], octets[2], octets[3]])
}

/// Écrit un enregistrement.
pub fn ecrire(sortie: &mut impl Write, genre: u8, contenu: &[u8]) -> io::Result<()> {
    sortie.write_all(&tete(genre, contenu.len())?)?;
    sortie.write_all(contenu)
}

/// Écrit l'enregistrement d'une transaction : `SubscribeUpdate.transaction` (champ 4), qui
/// porte le slot (2) et `SubscribeUpdateTransactionInfo` (1) : signature (1), vote (2),
/// transaction signée (3), compte rendu d'exécution (4), rang dans le bloc (5).
///
/// `signee` et `compte_rendu` sont écrits tels quels, sans être copiés ailleurs.
pub fn ecrire_transaction(
    sortie: &mut impl Write,
    slot: u64,
    rang: u64,
    signature: &[u8],
    vote: bool,
    signee: &[u8],
    compte_rendu: &[u8],
) -> io::Result<()> {
    let vote = u64::from(vote);
    let info = taille_ouverture(1, signature.len())
        + signature.len()
        + taille_entier(2, vote)
        + taille_ouverture(3, signee.len())
        + signee.len()
        + taille_ouverture(4, compte_rendu.len())
        + compte_rendu.len()
        + taille_entier(5, rang);
    let recue = taille_ouverture(1, info) + info + taille_entier(2, slot);
    let message = taille_ouverture(4, recue) + recue;

    let mut morceau = Vec::with_capacity(128);
    morceau.extend_from_slice(&tete(MESSAGE, message)?);
    ouvrir(&mut morceau, 4, recue);
    ouvrir(&mut morceau, 1, info);
    ouvrir(&mut morceau, 1, signature.len());
    morceau.extend_from_slice(signature);
    entier(&mut morceau, 2, vote);
    ouvrir(&mut morceau, 3, signee.len());
    sortie.write_all(&morceau)?;
    sortie.write_all(signee)?;
    morceau.clear();
    ouvrir(&mut morceau, 4, compte_rendu.len());
    sortie.write_all(&morceau)?;
    sortie.write_all(compte_rendu)?;
    morceau.clear();
    entier(&mut morceau, 5, rang);
    entier(&mut morceau, 2, slot);
    sortie.write_all(&morceau)
}

/// L'en-tête d'un bloc, tel que la chaîne le donne.
pub struct Entete<'a> {
    pub slot: u64,
    pub hash: &'a str,
    pub heure: Option<i64>,
    pub hauteur: Option<u64>,
    pub parent: u64,
    pub hash_parent: &'a str,
    pub executees: u64,
    pub entrees: u64,
}

/// Écrit l'enregistrement d'un en-tête de bloc : `SubscribeUpdate.block_meta` (champ 7), soit
/// `SubscribeUpdateBlockMeta` : slot (1), hash (2), heure (4, un message dont le champ 1 est
/// l'heure), hauteur (5, de même), slot parent (6), hash du parent (7), nombre de transactions
/// exécutées (8), nombre d'entrées (9). Ni les récompenses (3) ni le numéro de banque (10), que
/// seul le nœud qui émet connaît, ne sont écrits.
pub fn ecrire_entete(sortie: &mut impl Write, entete: &Entete) -> io::Result<()> {
    let mut bloc = Vec::with_capacity(192);
    entier(&mut bloc, 1, entete.slot);
    ouvrir(&mut bloc, 2, entete.hash.len());
    bloc.extend_from_slice(entete.hash.as_bytes());
    let mut dedans = Vec::with_capacity(11);
    if let Some(heure) = entete.heure {
        // Un entier signé s'écrit comme l'entier sans signe de mêmes bits.
        entier(&mut dedans, 1, heure as u64);
        ouvrir(&mut bloc, 4, dedans.len());
        bloc.extend_from_slice(&dedans);
    }
    if let Some(hauteur) = entete.hauteur {
        dedans.clear();
        entier(&mut dedans, 1, hauteur);
        ouvrir(&mut bloc, 5, dedans.len());
        bloc.extend_from_slice(&dedans);
    }
    entier(&mut bloc, 6, entete.parent);
    ouvrir(&mut bloc, 7, entete.hash_parent.len());
    bloc.extend_from_slice(entete.hash_parent.as_bytes());
    entier(&mut bloc, 8, entete.executees);
    entier(&mut bloc, 9, entete.entrees);

    let mut message = Vec::with_capacity(bloc.len() + 8);
    ouvrir(&mut message, 7, bloc.len());
    message.extend_from_slice(&bloc);
    ecrire(sortie, MESSAGE, &message)
}

/// Écrit l'enregistrement d'un événement.
pub fn ecrire_evenement(sortie: &mut impl Write, json: &str) -> io::Result<()> {
    ecrire(sortie, EVENEMENT, json.as_bytes())
}

/// La voie d'un slot parmi `voies` : le rang de sa tranche de slots, modulo le nombre de voies.
/// Une tranche entière va ainsi à une seule voie.
pub fn voie(slot: u64, tranche: u64, voies: usize) -> usize {
    ((slot / tranche) % voies as u64) as usize
}

#[cfg(test)]
mod essais {
    use super::*;

    // Les octets attendus ont été calculés à part, par la bibliothèque protobuf, depuis les
    // définitions officielles (`geyser.proto`, `solana-storage.proto`) : chaque message y est
    // construit champ par champ puis sérialisé.
    const A_SIGNEE: &str = concat!(
        "0a40000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425",
        "262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f127a0a0408011801122001010101",
        "01010101010101010101010101010101010101010101010101010101122002020202020202020202",
        "020202020202020202020202020202020202020202021a2003030303030303030303030303030303",
        "03030303030303030303030303030303220a0801120200011a0209082801",
    );
    const A_COMPTE_RENDU: &str = "1088271a020a1422020915320161320262638001d2098801ae2c";
    const A_MESSAGE: &str = concat!(
        "22aa020aa1020a40000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f1abe010a40000102",
        "030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a",
        "2b2c2d2e2f303132333435363738393a3b3c3d3e3f127a0a04080118011220010101010101010101",
        "01010101010101010101010101010101010101010101011220020202020202020202020202020202",
        "02020202020202020202020202020202021a20030303030303030303030303030303030303030303",
        "0303030303030303030303220a0801120200011a0209082801221a1088271a020a14220209153201",
        "61320262638001d2098801ae2c28031082dda2e001",
    );
    const B_MESSAGE: &str = concat!(
        "224c0a480a40ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff10011a0022001005",
    );
    const C_MESSAGE_TETE: &str = concat!(
        "22f99c010aef9c010a40070707070707070707070707070707070707070707070707070707070707",
        "070707070707070707070707070707070707070707070707070707070707070707071a0022a49c01",
    );
    const C_MESSAGE_QUEUE: &str = "28ac021082dda2e001";
    const D_MESSAGE: &str = concat!(
        "3a7b0882dda2e001122b34764a394a5531624a4a4539364657534a4b7648736d6d46414443673467",
        "705a516666345033626b4c4b6922060880eeeec8062a0608c0e48cd6013081dda2e0013a2b387162",
        "48627732426262544842573173626571616b5958564b52514d384e6537704c4b376d364356666552",
        "40dc0b4840",
    );
    const E_MESSAGE: &str = "3a0808011201613a0162";
    const F_MESSAGE: &str = "3a190802120161220b08ffffffffffffffffff012a0030013a0162";

    fn hexa(texte: &str) -> Vec<u8> {
        assert_eq!(texte.len() % 2, 0);
        (0..texte.len())
            .step_by(2)
            .map(|rang| u8::from_str_radix(&texte[rang..rang + 2], 16).expect("hexadécimal"))
            .collect()
    }

    /// Un enregistrement de genre 1 autour de ce contenu.
    fn enregistre(contenu: &[u8]) -> Vec<u8> {
        let mut attendu = vec![MESSAGE];
        attendu.extend_from_slice(&(contenu.len() as u32).to_be_bytes());
        attendu.extend_from_slice(contenu);
        attendu
    }

    #[test]
    fn un_entier_s_ecrit_sur_ce_qu_il_lui_faut() {
        let cas: [(u64, &str); 9] = [
            (0, "00"),
            (1, "01"),
            (127, "7f"),
            (128, "8001"),
            (300, "ac02"),
            (16383, "ff7f"),
            (16384, "808001"),
            (4294967296, "8080808010"),
            (18446744073709551615, "ffffffffffffffffff01"),
        ];
        for (valeur, attendu) in cas {
            let mut ecrit = Vec::new();
            variable(&mut ecrit, valeur);
            assert_eq!(ecrit, hexa(attendu), "{valeur}");
            assert_eq!(taille(valeur), ecrit.len(), "{valeur}");
        }
    }

    #[test]
    fn une_transaction_s_ecrit_comme_le_protocole_la_serialise() {
        let signature: Vec<u8> = (0..64).collect();
        let mut ecrit = Vec::new();
        ecrire_transaction(
            &mut ecrit,
            470_331_010,
            3,
            &signature,
            false,
            &hexa(A_SIGNEE),
            &hexa(A_COMPTE_RENDU),
        )
        .expect("écriture");
        assert_eq!(ecrit, enregistre(&hexa(A_MESSAGE)));
    }

    #[test]
    fn un_vote_de_rang_nul_sans_contenu_garde_ses_sous_messages_presents() {
        let mut ecrit = Vec::new();
        ecrire_transaction(&mut ecrit, 5, 0, &[0xff; 64], true, &[], &[]).expect("écriture");
        assert_eq!(ecrit, enregistre(&hexa(B_MESSAGE)));
    }

    #[test]
    fn un_long_compte_rendu_a_ses_longueurs_sur_plusieurs_octets() {
        // Un compte rendu dont le seul champ est une ligne de journal de 20 000 lettres.
        let mut compte_rendu = hexa("32a09c01");
        compte_rendu.extend(std::iter::repeat(b'x').take(20_000));
        let mut ecrit = Vec::new();
        ecrire_transaction(&mut ecrit, 470_331_010, 300, &[7; 64], false, &[], &compte_rendu)
            .expect("écriture");
        let mut contenu = hexa(C_MESSAGE_TETE);
        contenu.extend_from_slice(&compte_rendu);
        contenu.extend_from_slice(&hexa(C_MESSAGE_QUEUE));
        assert_eq!(contenu.len(), 20_093);
        assert_eq!(ecrit, enregistre(&contenu));
    }

    #[test]
    fn un_en_tete_s_ecrit_comme_le_protocole_le_serialise() {
        let mut ecrit = Vec::new();
        let complet = Entete {
            slot: 470_331_010,
            hash: "4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi",
            heure: Some(1_763_424_000),
            hauteur: Some(449_000_000),
            parent: 470_331_009,
            hash_parent: "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR",
            executees: 1_500,
            entrees: 64,
        };
        ecrire_entete(&mut ecrit, &complet).expect("écriture");
        assert_eq!(ecrit, enregistre(&hexa(D_MESSAGE)));
    }

    #[test]
    fn un_en_tete_sans_heure_ni_hauteur_ne_les_ecrit_pas() {
        let mut ecrit = Vec::new();
        let nu = Entete {
            slot: 1,
            hash: "a",
            heure: None,
            hauteur: None,
            parent: 0,
            hash_parent: "b",
            executees: 0,
            entrees: 0,
        };
        ecrire_entete(&mut ecrit, &nu).expect("écriture");
        assert_eq!(ecrit, enregistre(&hexa(E_MESSAGE)));
    }

    #[test]
    fn une_heure_negative_et_une_hauteur_nulle_restent_presentes() {
        let mut ecrit = Vec::new();
        let etrange = Entete {
            slot: 2,
            hash: "a",
            heure: Some(-1),
            hauteur: Some(0),
            parent: 1,
            hash_parent: "b",
            executees: 0,
            entrees: 0,
        };
        ecrire_entete(&mut ecrit, &etrange).expect("écriture");
        assert_eq!(ecrit, enregistre(&hexa(F_MESSAGE)));
    }

    #[test]
    fn un_evenement_est_un_enregistrement_de_genre_2() {
        let mut ecrit = Vec::new();
        ecrire_evenement(&mut ecrit, "{\"e\":\"fin\"}").expect("écriture");
        assert_eq!(ecrit, b"\x02\x00\x00\x00\x0b{\"e\":\"fin\"}");
        let mut vide = Vec::new();
        ecrire(&mut vide, MESSAGE, &[]).expect("écriture");
        assert_eq!(vide, [1, 0, 0, 0, 0]);
    }

    #[test]
    fn une_tranche_entiere_va_a_une_seule_voie() {
        assert_eq!(voie(470_331_000, 100, 3), voie(470_331_099, 100, 3));
        assert_eq!(voie(0, 100, 3), 0);
        assert_eq!(voie(99, 100, 3), 0);
        assert_eq!(voie(100, 100, 3), 1);
        assert_eq!(voie(250, 100, 3), 2);
        assert_eq!(voie(300, 100, 3), 0);
        assert_eq!(voie(12_345, 100, 1), 0);
        // 470 331 010 / 100 = 4 703 310, qui laisse 0 divisé par 3 et 2 divisé par 4.
        assert_eq!(voie(470_331_010, 100, 3), 0);
        assert_eq!(voie(470_331_010, 100, 4), 2);
    }
}
