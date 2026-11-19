//! La demande adressée au lecteur : une ligne de texte, et rien d'autre.
//!
//! `v1 <premier>:<dernier> <avant> <programmes>` — quatre champs séparés par une espace : la
//! version, la plage de slots à rendre (bornes comprises), le slot d'où la lecture part (le
//! dernier slot à bloc d'avant la plage), et les programmes suivis, une à huit adresses
//! séparées par des virgules.
//!
//! La ligne vient du réseau : ce sont des octets, pas encore du texte. Elle est lue telle
//! qu'elle est écrite, sans rien arranger, et refusée au premier écart, avec son motif. Un
//! motif ne reprend jamais ce qui a été reçu : il ne porte que des mots d'ici et des nombres
//! déjà lus.

// Éprouvé seul (`rustc --test demande.rs`), ce module prend lui-même le fichier des outils ;
// dans le greffon, ce sont ceux de la racine.
#[cfg(not(test))]
use crate::outils;
#[cfg(test)]
#[path = "outils.rs"]
mod outils;

/// Le plus grand nombre de slots d'une plage, et le plus grand écart entre le départ et la
/// plage : une époque.
pub const SLOTS_MAX: u64 = 432_000;
/// La plus longue ligne lue, en octets, saut de ligne non compris.
pub const LONGUEUR_MAX: usize = 1_024;
/// Le plus grand nombre de programmes suivis.
const PROGRAMMES_MAX: usize = 8;
/// Le plus grand nombre de chiffres d'un slot.
const CHIFFRES_MAX: usize = 12;

/// Ce qu'une ligne juste demande.
#[derive(Debug, PartialEq, Eq)]
pub struct Demande {
    /// Premier et dernier slot à rendre, bornes comprises.
    pub plage: (u64, u64),
    /// Le slot d'où la lecture part : avant la plage, à une époque au plus.
    pub avant: u64,
    /// Les programmes suivis, tels que la ligne les écrit.
    pub programmes: String,
}

/// Un slot écrit en chiffres, et rien d'autre : ni signe, ni espace.
fn slot(texte: &str) -> Option<u64> {
    // Un texte vide ne se lit pas comme un nombre : il est refusé à la lecture.
    let en_chiffres = texte.bytes().all(|octet| octet.is_ascii_digit());
    if en_chiffres && texte.len() <= CHIFFRES_MAX {
        texte.parse().ok()
    } else {
        None
    }
}

/// Ce que la ligne demande, ou le motif de son refus. La ligne est donnée telle qu'elle est
/// arrivée, en octets, sans son saut de ligne.
pub fn comprendre(ligne: &[u8]) -> Result<Demande, String> {
    if ligne.len() > LONGUEUR_MAX {
        return Err(format!("ligne de plus de {LONGUEUR_MAX} octets"));
    }
    let imprimable = |octet: &u8| (0x20..=0x7e).contains(octet);
    if let Some(position) = ligne.iter().position(|octet| !imprimable(octet)) {
        return Err(format!(
            "caractère hors de l'ASCII imprimable (octet n° {})",
            position + 1
        ));
    }
    // Rien que de l'ASCII : chaque octet est un caractère.
    let ligne: String = ligne.iter().map(|&octet| char::from(octet)).collect();
    let champs: Vec<&str> = ligne.split(' ').collect();
    if champs[0] != "v1" {
        return Err("version inconnue : ce lecteur ne sert que « v1 »".to_string());
    }
    if champs.iter().any(|champ| champ.is_empty()) {
        return Err("champ vide : une seule espace entre deux champs, aucune au bout".to_string());
    }
    let [_, plage, avant, programmes] = champs[..] else {
        return Err(format!("nombre de champs : {} au lieu de 4", champs.len()));
    };

    let (premier, dernier) = plage
        .split_once(':')
        .ok_or("plage : « premier:dernier » attendu")?;
    let (Some(premier), Some(dernier)) = (slot(premier), slot(dernier)) else {
        return Err(format!("plage : une borne s'écrit en 1 à {CHIFFRES_MAX} chiffres"));
    };
    if dernier < premier {
        return Err(format!(
            "plage : le dernier slot ({dernier}) est avant le premier ({premier})"
        ));
    }
    let slots = dernier - premier + 1;
    if slots > SLOTS_MAX {
        return Err(format!("plage : {slots} slots, au plus {SLOTS_MAX}"));
    }

    let Some(avant) = slot(avant) else {
        return Err(format!("départ : un slot s'écrit en 1 à {CHIFFRES_MAX} chiffres"));
    };
    if avant >= premier {
        return Err(format!(
            "départ : le slot {avant} n'est pas avant la plage, qui commence au slot {premier}"
        ));
    }
    let ecart = premier - avant;
    if ecart > SLOTS_MAX {
        return Err(format!("départ : {ecart} slots avant la plage, au plus {SLOTS_MAX}"));
    }

    let adresses: Vec<&str> = programmes.split(',').collect();
    if adresses.len() > PROGRAMMES_MAX {
        return Err(format!(
            "programmes : {} adresses, au plus {PROGRAMMES_MAX}",
            adresses.len()
        ));
    }
    if let Some(position) = adresses.iter().position(|texte| outils::adresse(texte).is_none()) {
        return Err(format!("programmes : l'adresse n° {} n'en est pas une", position + 1));
    }
    Ok(Demande {
        plage: (premier, dernier),
        avant,
        programmes: programmes.to_string(),
    })
}

#[cfg(test)]
mod essais {
    use super::*;

    /// Programme système, puis programme des jetons SPL : deux adresses justes.
    const A: &str = "11111111111111111111111111111111";
    const B: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

    const VERSION: &str = "version inconnue : ce lecteur ne sert que « v1 »";
    const CHAMP_VIDE: &str = "champ vide : une seule espace entre deux champs, aucune au bout";
    const PLAGE: &str = "plage : « premier:dernier » attendu";
    const BORNE: &str = "plage : une borne s'écrit en 1 à 12 chiffres";
    const DEPART: &str = "départ : un slot s'écrit en 1 à 12 chiffres";

    fn ligne(plage: &str, avant: &str, programmes: &str) -> String {
        format!("v1 {plage} {avant} {programmes}")
    }

    /// Ce que demande une ligne écrite en texte.
    fn lue(ligne: &str) -> Result<Demande, String> {
        comprendre(ligne.as_bytes())
    }

    fn refus(ligne: &str) -> String {
        lue(ligne).expect_err("cette ligne devait être refusée")
    }

    fn hors_ascii(octet: usize) -> String {
        format!("caractère hors de l'ASCII imprimable (octet n° {octet})")
    }

    #[test]
    fn une_ligne_juste_dit_la_plage_le_depart_et_les_programmes() {
        assert_eq!(
            lue(&format!("v1 470800000:470800019 470799999 {A},{B}")),
            Ok(Demande {
                plage: (470_800_000, 470_800_019),
                avant: 470_799_999,
                programmes: format!("{A},{B}"),
            })
        );
    }

    #[test]
    fn une_a_huit_adresses_sont_admises() {
        assert_eq!(
            lue(&ligne("5:9", "4", A)).map(|demande| demande.programmes),
            Ok(A.to_string())
        );
        let huit = [A, B, A, B, A, B, A, B].join(",");
        assert_eq!(
            lue(&ligne("5:9", "4", &huit)).map(|demande| demande.programmes),
            Ok(huit)
        );
    }

    #[test]
    fn neuf_adresses_sont_refusees() {
        let neuf = [A, B, A, B, A, B, A, B, A].join(",");
        assert_eq!(
            refus(&ligne("5:9", "4", &neuf)),
            "programmes : 9 adresses, au plus 8"
        );
    }

    #[test]
    fn une_adresse_qui_n_en_est_pas_une_est_refusee() {
        let seconde = "programmes : l'adresse n° 2 n'en est pas une";
        assert_eq!(refus(&ligne("5:9", "4", &format!("{A},abc"))), seconde);
        assert_eq!(refus(&ligne("5:9", "4", &format!("{A},,{B}"))), seconde);
        assert_eq!(refus(&ligne("5:9", "4", &format!("{A},"))), seconde);
        assert_eq!(refus(&ligne("5:9", "4", &format!("{A},{B}z"))), seconde);
        assert_eq!(
            refus(&ligne("5:9", "4", &format!("{A},{B},0{B}"))),
            "programmes : l'adresse n° 3 n'en est pas une"
        );
    }

    #[test]
    fn sans_adresse_la_ligne_est_refusee() {
        assert_eq!(refus("v1 5:9 4 "), CHAMP_VIDE);
        assert_eq!(refus("v1 5:9 4"), "nombre de champs : 3 au lieu de 4");
        assert_eq!(
            refus(&ligne("5:9", "4", ",")),
            "programmes : l'adresse n° 1 n'en est pas une"
        );
    }

    #[test]
    fn la_plage_va_d_un_slot_a_432_000() {
        assert_eq!(
            lue(&ligne("5:5", "4", A)).map(|demande| demande.plage),
            Ok((5, 5))
        );
        // De 1 000 à 432 999 : 432 000 slots, bornes comprises.
        assert_eq!(
            lue(&ligne("1000:432999", "999", A)).map(|demande| demande.plage),
            Ok((1_000, 432_999))
        );
        assert_eq!(
            refus(&ligne("1000:433000", "999", A)),
            "plage : 432001 slots, au plus 432000"
        );
    }

    #[test]
    fn une_plage_a_l_envers_est_refusee() {
        assert_eq!(
            refus(&ligne("9:5", "4", A)),
            "plage : le dernier slot (5) est avant le premier (9)"
        );
    }

    #[test]
    fn une_plage_s_ecrit_premier_deux_points_dernier() {
        assert_eq!(refus(&ligne("59", "4", A)), PLAGE);
        assert_eq!(refus(&ligne("5-9", "4", A)), PLAGE);
    }

    #[test]
    fn une_borne_s_ecrit_en_un_a_douze_chiffres() {
        for plage in [
            ":9",
            "5:",
            ":",
            "+5:9",
            "5:+9",
            "-5:9",
            "5:9:10",
            "0x5:9",
            "5:9a",
            "5.0:9",
            "1234567890123:1234567890124",
            "5:1234567890123",
        ] {
            assert_eq!(refus(&ligne(plage, "4", A)), BORNE, "plage « {plage} »");
        }
        // Douze chiffres, la plus grande borne qui s'écrive.
        assert_eq!(
            lue(&ligne("999999999990:999999999999", "999999999989", A)),
            Ok(Demande {
                plage: (999_999_999_990, 999_999_999_999),
                avant: 999_999_999_989,
                programmes: A.to_string(),
            })
        );
        // Des zéros de tête ne changent pas le nombre.
        assert_eq!(
            lue(&ligne("005:009", "04", A)).map(|demande| (demande.plage, demande.avant)),
            Ok(((5, 9), 4))
        );
    }

    #[test]
    fn le_depart_s_ecrit_en_un_a_douze_chiffres() {
        for avant in ["+4", "-1", "4.0", "x", "4:", "1234567890123"] {
            assert_eq!(refus(&ligne("5:9", avant, A)), DEPART, "départ « {avant} »");
        }
    }

    #[test]
    fn le_depart_precede_la_plage_d_au_plus_432_000_slots() {
        // 500 000 − 432 000 = 68 000 : le départ le plus lointain qui soit admis.
        assert_eq!(
            lue(&ligne("500000:500009", "68000", A)).map(|demande| demande.avant),
            Ok(68_000)
        );
        assert_eq!(
            refus(&ligne("500000:500009", "67999", A)),
            "départ : 432001 slots avant la plage, au plus 432000"
        );
        assert_eq!(
            lue(&ligne("500000:500009", "499999", A)).map(|demande| demande.avant),
            Ok(499_999)
        );
        assert_eq!(
            refus(&ligne("500000:500009", "500000", A)),
            "départ : le slot 500000 n'est pas avant la plage, qui commence au slot 500000"
        );
        assert_eq!(
            refus(&ligne("500000:500009", "500001", A)),
            "départ : le slot 500001 n'est pas avant la plage, qui commence au slot 500000"
        );
    }

    #[test]
    fn une_autre_version_est_refusee() {
        for texte in [
            format!("v2 5:9 4 {A}"),
            format!("V1 5:9 4 {A}"),
            format!("v10 5:9 4 {A}"),
            format!("v 5:9 4 {A}"),
            format!("5:9 4 {A}"),
            format!(" v1 5:9 4 {A}"),
            // Une version qui viendrait avec d'autres champs se dit inconnue, pas mal formée.
            format!("v2 5:9 4 {A} suite"),
            String::new(),
        ] {
            assert_eq!(refus(&texte), VERSION, "ligne « {texte} »");
        }
    }

    #[test]
    fn trois_ou_cinq_champs_sont_refuses() {
        assert_eq!(refus("v1"), "nombre de champs : 1 au lieu de 4");
        assert_eq!(refus("v1 5:9 4"), "nombre de champs : 3 au lieu de 4");
        assert_eq!(
            refus(&format!("v1 5:9 4 {A} {B}")),
            "nombre de champs : 5 au lieu de 4"
        );
    }

    #[test]
    fn deux_espaces_de_suite_sont_refusees() {
        for texte in [
            format!("v1  5:9 4 {A}"),
            format!("v1 5:9  4 {A}"),
            format!("v1 5:9 4  {A}"),
            format!("v1 5:9 4 {A} "),
            "v1 ".to_string(),
        ] {
            assert_eq!(refus(&texte), CHAMP_VIDE, "ligne « {texte} »");
        }
    }

    #[test]
    fn un_caractere_hors_de_l_ascii_imprimable_est_refuse() {
        let juste = ligne("5:9", "4", A);
        assert!(lue(&juste).is_ok());
        // Une fin de ligne à la manière de Windows, ou gardée par mégarde : l'octet d'après.
        for fin in ["\r", "\n", "\0", "\u{1f}", "\u{7f}"] {
            assert_eq!(refus(&format!("{juste}{fin}")), hors_ascii(juste.len() + 1));
        }
        // Une tabulation à la place de la première espace : le troisième octet.
        assert_eq!(refus(&format!("v1\t5:9 4 {A}")), hors_ascii(3));
        // Une lettre accentuée en tête des programmes : son premier octet suit « v1 5:9 4 ».
        assert_eq!(refus(&format!("v1 5:9 4 é{A}")), hors_ascii("v1 5:9 4 ".len() + 1));
        // Des octets qui ne sont pas du texte : le premier d'entre eux est dit, à sa place.
        assert_eq!(
            comprendre(b"v1 \xff\xfe 5"),
            Err(hors_ascii(4))
        );
        // Un chiffre qui n'est pas de l'ASCII (chiffre arabe-indien) n'est pas un chiffre ici.
        assert_eq!(refus(&format!("v1 \u{665}:9 4 {A}")), hors_ascii(4));
        // Le tilde, dernier caractère imprimable, passe ce contrôle : il n'est d'aucune adresse.
        assert_eq!(
            refus("v1 5:9 4 ~"),
            "programmes : l'adresse n° 1 n'en est pas une"
        );
    }

    #[test]
    fn une_ligne_de_plus_de_1024_octets_est_refusee() {
        assert_eq!(LONGUEUR_MAX, 1_024);
        assert_eq!(SLOTS_MAX, 432_000);
        // 1 024 octets : la ligne est lue, et refusée pour ce qu'elle dit.
        let pleine = format!("v1 {}", "x".repeat(LONGUEUR_MAX - 3));
        assert_eq!(pleine.len(), 1_024);
        assert_eq!(refus(&pleine), "nombre de champs : 2 au lieu de 4");
        // Un octet de plus : elle n'est pas lue.
        assert_eq!(
            refus(&format!("{pleine}x")),
            "ligne de plus de 1024 octets"
        );
        // La longueur passe avant tout le reste, même devant un caractère interdit.
        assert_eq!(
            refus(&format!("{}\r", "x".repeat(LONGUEUR_MAX))),
            "ligne de plus de 1024 octets"
        );
        // Elle se compte en octets reçus, quels qu'ils soient.
        assert_eq!(comprendre(&[0xff; 1_024]), Err(hors_ascii(1)));
        assert_eq!(
            comprendre(&[0xff; 1_025]),
            Err("ligne de plus de 1024 octets".to_string())
        );
    }
}
