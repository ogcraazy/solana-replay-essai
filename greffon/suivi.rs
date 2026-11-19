//! Le suivi des blocs en cours, sans autre dépendance que la bibliothèque standard.
//!
//! L'outil passe au greffon les transactions d'un bloc, puis son en-tête, qui dit combien le
//! bloc en a exécuté. Ce module retient, pour chaque bloc en cours, les rangs des transactions
//! rendues — toutes, votes compris, gardées ou non — et dit à l'en-tête si le bloc est
//! complet : autant de rangs distincts que l'en-tête en annonce, tous plus petits que ce
//! nombre. Une transaction rendue deux fois ne compte qu'une fois : quand la lecture bute,
//! l'outil reprend le slot inachevé à son début.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Les blocs en cours, et le compte de ceux que leur en-tête a trouvés incomplets.
pub struct Suivi {
    /// Par slot, les rangs des transactions rendues.
    en_cours: Mutex<HashMap<u64, HashSet<u64>>>,
    incomplets: AtomicU64,
}

impl Suivi {
    pub fn new() -> Self {
        Suivi {
            en_cours: Mutex::new(HashMap::new()),
            incomplets: AtomicU64::new(0),
        }
    }

    /// Une transaction de ce slot, à ce rang, a été rendue.
    pub fn vue(&self, slot: u64, rang: u64) {
        self.en_cours
            .lock()
            .expect("blocs en cours")
            .entry(slot)
            .or_default()
            .insert(rang);
    }

    /// L'en-tête du slot est là, et dit combien de transactions le bloc a exécutées : rend vrai
    /// si elles ont toutes été rendues. Le slot n'est plus suivi.
    pub fn bloc(&self, slot: u64, annoncees: u64) -> bool {
        let rangs = self
            .en_cours
            .lock()
            .expect("blocs en cours")
            .remove(&slot)
            .unwrap_or_default();
        let complet =
            rangs.len() as u64 == annoncees && rangs.iter().all(|rang| *rang < annoncees);
        if !complet {
            self.incomplets.fetch_add(1, Ordering::Relaxed);
        }
        complet
    }

    /// Les blocs trouvés incomplets à leur en-tête, et ceux dont l'en-tête n'est pas venu.
    pub fn incomplets(&self) -> u64 {
        let sans_en_tete = self.en_cours.lock().expect("blocs en cours").len() as u64;
        self.incomplets.load(Ordering::Relaxed) + sans_en_tete
    }
}

#[cfg(test)]
mod essais {
    use super::*;

    /// Un suivi qui a vu ces rangs pour ce slot.
    fn apres(slot: u64, rangs: &[u64]) -> Suivi {
        let suivi = Suivi::new();
        for rang in rangs {
            suivi.vue(slot, *rang);
        }
        suivi
    }

    #[test]
    fn un_bloc_dont_tous_les_rangs_sont_vus_est_complet() {
        let suivi = apres(7, &[0, 1, 2]);
        assert!(suivi.bloc(7, 3));
        assert_eq!(suivi.incomplets(), 0);
        // L'ordre d'arrivée des rangs ne compte pas.
        assert!(apres(7, &[2, 0, 1]).bloc(7, 3));
    }

    #[test]
    fn un_rang_manquant_rend_le_bloc_incomplet() {
        for rangs in [&[0, 2][..], &[0, 1][..], &[1, 2][..]] {
            let suivi = apres(7, rangs);
            assert!(!suivi.bloc(7, 3), "{rangs:?}");
            assert_eq!(suivi.incomplets(), 1, "{rangs:?}");
        }
    }

    #[test]
    fn un_rang_vu_deux_fois_ne_compte_qu_une_fois() {
        // Trois transactions annoncées, trois reçues, dont une en double : il en manque une.
        assert!(!apres(7, &[0, 1, 1]).bloc(7, 3));
        // Le début du bloc rendu deux fois autour d'une reprise, puis la suite : complet.
        assert!(apres(7, &[0, 1, 0, 1, 2]).bloc(7, 3));
    }

    #[test]
    fn un_rang_au_dela_de_l_annonce_rend_le_bloc_incomplet() {
        // Autant de rangs distincts que d'annoncées, mais le rang 3 n'est pas d'un bloc de trois.
        assert!(!apres(7, &[0, 1, 3]).bloc(7, 3));
        assert!(!apres(7, &[1, 2, 3]).bloc(7, 3));
    }

    #[test]
    fn un_bloc_sans_transaction_est_complet_s_il_n_en_annonce_aucune() {
        let suivi = Suivi::new();
        assert!(suivi.bloc(7, 0));
        assert!(!suivi.bloc(8, 2));
        assert_eq!(suivi.incomplets(), 1);
    }

    #[test]
    fn deux_blocs_entremeles_se_suivent_a_part() {
        let suivi = Suivi::new();
        suivi.vue(7, 0);
        suivi.vue(9, 0);
        suivi.vue(9, 1);
        suivi.vue(7, 1);
        assert!(suivi.bloc(9, 2));
        assert!(!suivi.bloc(7, 3));
        assert_eq!(suivi.incomplets(), 1);
    }

    #[test]
    fn apres_son_en_tete_un_slot_n_est_plus_suivi() {
        let suivi = apres(7, &[0, 1]);
        assert!(suivi.bloc(7, 2));
        // Un second en-tête du même slot ne retrouve rien de ce qui a été vu avant le premier.
        assert!(!suivi.bloc(7, 2));
        assert!(suivi.bloc(7, 0));
        assert_eq!(suivi.incomplets(), 1);
    }

    #[test]
    fn un_bloc_reste_sans_en_tete_compte_parmi_les_incomplets() {
        let suivi = apres(7, &[0, 1]);
        suivi.vue(9, 0);
        assert_eq!(suivi.incomplets(), 2);
        assert!(suivi.bloc(7, 2));
        assert_eq!(suivi.incomplets(), 1);
        assert!(!suivi.bloc(9, 5));
        // Trouvé incomplet à son en-tête, il n'est compté qu'une fois.
        assert_eq!(suivi.incomplets(), 1);
    }
}
