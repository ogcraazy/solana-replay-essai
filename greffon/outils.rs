//! Outils du greffon d'essai, sans autre dépendance que la bibliothèque standard :
//! lecture des saisies (adresses, plage, slots) et écriture de texte JSON.

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Les 32 octets d'une adresse écrite en base 58. Rien si le texte n'est pas l'écriture
/// exacte d'une adresse : lettre inconnue, nombre trop grand, zéros de tête mal écrits.
pub fn adresse(texte: &str) -> Option<[u8; 32]> {
    let mut octets = [0u8; 32];
    for lettre in texte.bytes() {
        let mut retenue = ALPHABET.iter().position(|&connue| connue == lettre)? as u32;
        for octet in octets.iter_mut().rev() {
            retenue += u32::from(*octet) * 58;
            *octet = (retenue & 0xff) as u8;
            retenue >>= 8;
        }
        if retenue != 0 {
            return None;
        }
    }
    // En base 58, chaque octet nul de tête s'écrit « 1 », et rien d'autre ne commence par « 1 » :
    // un texte vide ou trop court ne passe pas ici, un texte trop long a déjà débordé.
    let nuls = octets.iter().take_while(|&&octet| octet == 0).count();
    let uns = texte.bytes().take_while(|&lettre| lettre == b'1').count();
    if nuls == uns { Some(octets) } else { None }
}

/// Les adresses d'une liste séparée par des virgules. Rien si l'une des adresses est
/// illisible — une liste vide en est une.
pub fn adresses(liste: &str) -> Option<Vec<[u8; 32]>> {
    liste
        .split(',')
        .map(|texte| adresse(texte.trim()))
        .collect()
}

/// Une plage de slots « début:fin », bornes comprises et dans l'ordre.
pub fn plage(texte: &str) -> Option<(u64, u64)> {
    let (debut, fin) = texte.trim().split_once(':')?;
    let (debut, fin) = (debut.parse::<u64>().ok()?, fin.parse::<u64>().ok()?);
    if debut <= fin && fin < u64::MAX {
        Some((debut, fin))
    } else {
        None
    }
}

/// Des slots séparés par des virgules ; une liste vide n'en nomme aucun. Rien si l'un d'eux
/// n'est pas un nombre.
pub fn slots(liste: &str) -> Option<Vec<u64>> {
    if liste.trim().is_empty() {
        return Some(Vec::new());
    }
    liste
        .split(',')
        .map(|texte| texte.trim().parse::<u64>().ok())
        .collect()
}

/// Des octets en chiffres hexadécimaux, deux par octet.
pub fn hexa(octets: &[u8]) -> String {
    const CHIFFRES: &[u8; 16] = b"0123456789abcdef";
    let mut texte = String::with_capacity(octets.len() * 2);
    for &octet in octets {
        texte.push(CHIFFRES[usize::from(octet >> 4)] as char);
        texte.push(CHIFFRES[usize::from(octet & 0x0f)] as char);
    }
    texte
}

/// Un texte en chaîne JSON, guillemets compris.
pub fn chaine(texte: &str) -> String {
    let mut ecrite = String::with_capacity(texte.len() + 2);
    ecrite.push('"');
    for lettre in texte.chars() {
        match lettre {
            '"' => ecrite.push_str("\\\""),
            '\\' => ecrite.push_str("\\\\"),
            '\n' => ecrite.push_str("\\n"),
            '\r' => ecrite.push_str("\\r"),
            '\t' => ecrite.push_str("\\t"),
            autre if u32::from(autre) < 0x20 => {
                ecrite.push_str(&format!("\\u{:04x}", u32::from(autre)));
            }
            autre => ecrite.push(autre),
        }
    }
    ecrite.push('"');
    ecrite
}

/// Une liste JSON de textes déjà écrits en JSON.
pub fn liste(elements: impl IntoIterator<Item = String>) -> String {
    let ecrits: Vec<String> = elements.into_iter().collect();
    format!("[{}]", ecrits.join(","))
}

/// « null », ou la valeur écrite par `ecrire`.
pub fn ou_rien<T>(valeur: Option<T>, ecrire: impl FnOnce(T) -> String) -> String {
    valeur.map_or_else(|| "null".to_string(), ecrire)
}

#[cfg(test)]
mod essais {
    use super::*;

    #[test]
    fn une_adresse_se_lit_en_ses_octets() {
        // Programme système : trente-deux octets nuls.
        assert_eq!(adresse("11111111111111111111111111111111"), Some([0u8; 32]));
        // Programme des jetons SPL.
        assert_eq!(
            adresse("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA").map(|octets| hexa(&octets)),
            Some("06ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a9".to_string())
        );
        // Programme des votes : un octet de tête proche de zéro.
        assert_eq!(
            adresse("Vote111111111111111111111111111111111111111").map(|octets| hexa(&octets)),
            Some("0761481d357474bb7c4d7624ebd3bdb3d8355e73d11043fc0da3538000000000".to_string())
        );
    }

    #[test]
    fn un_texte_qui_n_est_pas_une_adresse_est_refuse() {
        assert_eq!(adresse(""), None);
        assert_eq!(adresse("abc"), None); // trop court : des zéros de tête sans leurs « 1 »
        assert_eq!(adresse("0okenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), None); // « 0 » inconnu
        assert_eq!(
            adresse("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DAz"),
            None
        ); // trop grand
        assert_eq!(
            adresse("1TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
            None
        ); // un « 1 » de trop
        assert_eq!(adresse(&"z".repeat(45)), None);
    }

    #[test]
    fn une_liste_d_adresses_se_lit_entiere_ou_pas_du_tout() {
        let deux = "11111111111111111111111111111111, TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        assert_eq!(adresses(deux).map(|lues| lues.len()), Some(2));
        assert_eq!(adresses(""), None);
        assert_eq!(adresses("11111111111111111111111111111111,abc"), None);
    }

    #[test]
    fn une_plage_a_deux_bornes_dans_l_ordre() {
        assert_eq!(
            plage("358560000:367631999"),
            Some((358_560_000, 367_631_999))
        );
        assert_eq!(plage(" 5:5 "), Some((5, 5)));
        assert_eq!(plage("6:5"), None);
        assert_eq!(plage("5"), None);
        assert_eq!(plage("5:"), None);
        assert_eq!(plage("a:b"), None);
        assert_eq!(plage("0:18446744073709551615"), None); // la borne exclue ne se dirait plus
    }

    #[test]
    fn des_slots_se_lisent_ou_la_liste_est_refusee() {
        assert_eq!(slots(""), Some(vec![]));
        assert_eq!(slots(" 3, 1,2 "), Some(vec![3, 1, 2]));
        assert_eq!(slots("1,,2"), None);
        assert_eq!(slots("1,x"), None);
    }

    #[test]
    fn le_json_s_ecrit_sans_ambiguite() {
        assert_eq!(hexa(&[]), "");
        assert_eq!(hexa(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(chaine("abc"), "\"abc\"");
        assert_eq!(chaine("a\"b\\c\nd\te\rf"), "\"a\\\"b\\\\c\\nd\\te\\rf\"");
        assert_eq!(chaine("\u{1}é\u{1f}"), "\"\\u0001é\\u001f\"");
        assert_eq!(
            liste(vec!["1".to_string(), "\"a\"".to_string()]),
            "[1,\"a\"]"
        );
        assert_eq!(liste(Vec::<String>::new()), "[]");
        assert_eq!(ou_rien(Some(7u64), |valeur| valeur.to_string()), "7");
        assert_eq!(ou_rien(None::<u64>, |valeur| valeur.to_string()), "null");
    }
}
