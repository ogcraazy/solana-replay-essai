//! Greffon de l'atelier pour Jetstreamer.
//!
//! Il relit une plage de slots de l'archive publique Old Faithful et rend, au format du flux
//! Yellowstone gRPC (voir `flux.rs`), l'en-tête de chaque bloc et les transactions qui citent
//! l'un des programmes donnés — parmi les comptes du message ou parmi ceux que la transaction
//! charge par une table d'adresses —, votes exclus, échecs compris. Rien n'est gardé sur le
//! disque : le flux est écrit dans des voies (des tubes, que d'autres programmes lisent au fil
//! de l'eau), une tranche de slots entière allant à une seule voie.
//!
//! Saisies, par l'environnement :
//! - `ESSAI_PLAGE` : « début:fin », bornes comprises ;
//! - `ESSAI_PROGRAMMES` : adresses en base 58, séparées par des virgules ;
//! - `ESSAI_VOIES` : chemins des voies, séparés par des virgules ; « fd:N » (N de 3 à 9)
//!   désigne un descripteur que le lanceur a déjà ouvert pour le flux ;
//! - `ESSAI_TRANCHE` : nombre de slots d'une tranche (vide : 100) ;
//! - `ESSAI_A_PART` : transactions dont l'enregistrement est aussi écrit dans `a_part.flux`,
//!   « slot/rang » séparés par des virgules (peut rester vide) ;
//! - `ESSAI_FILS` : nombre de fils de lecture (vide : au choix de l'outil) ;
//! - `ESSAI_ORDRE` : `parallele` (ou vide), plusieurs fils lisent chacun sa part de la plage ;
//!   `sequentiel`, un seul fil rend la plage dans l'ordre de la chaîne, l'outil ne gardant
//!   plusieurs connexions que pour télécharger ;
//! - `ESSAI_AVANT` : le slot d'où partir, avant la plage (vide : soixante-quatre slots avant
//!   elle) — ce qui est lu avant la plage n'est pas rendu, et dit le hash du parent du
//!   premier bloc ;
//! - `ESSAI_SORTIE` : répertoire des résultats ; vide, rien n'est écrit hors des voies (et
//!   `ESSAI_A_PART` doit alors rester vide).
//!
//! Deux valeurs sont lues à la compilation et redites par l'événement `debut` : `ESSAI_ATELIER`,
//! le commit de l'atelier dont le programme est construit, et `ESSAI_OUTIL`, celui de l'outil.
//!
//! Résultats : `resume.json`, les comptes du greffon, et `a_part.flux`. Chaque voie commence
//! par un événement `debut` et finit par un événement `fin` qui dit ce qu'elle a reçu ; un slot
//! sans bloc et une reprise de lecture y laissent aussi un événement. Quand la lecture bute et
//! reprend, les transactions du slot en cours sont rendues une seconde fois : à qui lit le flux
//! de les reconnaître. Une voie qui ne s'écrit plus arrête tout, avec le code 3.
//!
//! Le hash du bloc parent. L'outil ne le lit pas dans l'archive : il rend celui du bloc qu'il a
//! lu juste avant sur la même connexion, et un hash nul pour le premier bloc de chaque connexion.
//! Or le hash du parent d'un bloc est, par construction de la chaîne, le hash du bloc de son
//! slot parent. Le greffon retient donc le hash de chaque bloc lu et écrit, pour chaque en-tête,
//! celui du bloc de son slot parent ; un en-tête dont le parent n'est pas encore lu (un autre fil
//! le lira) attend la fin de la lecture, et quelques slots sont lus avant la plage, sans rien en
//! rendre, pour connaître le parent du premier bloc. En ordre séquentiel aucun autre fil ne
//! le lira : l'en-tête est écrit aussitôt, précédé de l'événement `parent_inconnu`, pour que
//! le flux reste dans l'ordre de la chaîne. Le résumé compte les en-têtes qui ont
//! attendu, ceux dont le parent est resté inconnu (écrits alors avec ce que l'outil donnait) et
//! les désaccords entre l'outil, quand il dit un hash, et le bloc parent.
//!
//! L'ordre du flux. Pour un bloc, l'outil passe ses transactions puis son en-tête. Le résumé
//! compte ce qui s'écarte de l'ordre de la chaîne : les en-têtes écrits après celui d'un slot
//! égal ou plus haut, et les transactions gardées arrivées après l'en-tête de leur slot ou
//! d'un slot plus haut. Avec plusieurs fils, ces comptes sont grands, et c'est attendu ; en
//! ordre séquentiel ils devraient être nuls, et le résumé dit s'ils le sont.
//!
//! Les blocs incomplets. Le greffon retient, pour chaque bloc en cours, les rangs des
//! transactions que l'outil lui passe — toutes, avant tout tri — et les confronte, à l'en-tête,
//! au nombre que le bloc dit avoir exécuté (voir `suivi.rs`). Le résumé et l'événement `fin` de
//! chaque voie disent combien de blocs de toute la lecture n'ont pas reçu toutes leurs
//! transactions, ou n'ont pas reçu leur en-tête, et combien de fois la lecture a repris.

use std::{
    collections::{BTreeSet, HashMap},
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    os::fd::FromRawFd,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use clickhouse::Client;
use jetstreamer::{
    JetstreamerRunner,
    firehose::{BlockData, TransactionData},
    plugin::{FirehoseErrorContext, Plugin, PluginFuture},
};

mod flux;
mod outils;
mod suivi;

/// Slots lus avant la plage sans rien en rendre, quand le départ n'est pas donné : leurs blocs
/// disent le hash du parent du premier bloc de la plage.
const MARGE: u64 = 64;

/// Le commit de l'atelier dont ce programme est construit, et celui de l'outil : donnés à la
/// compilation, vides sinon.
const ATELIER: &str = match option_env!("ESSAI_ATELIER") {
    Some(commit) => commit,
    None => "",
};
const OUTIL: &str = match option_env!("ESSAI_OUTIL") {
    Some(commit) => commit,
    None => "",
};

/// L'en-tête d'un bloc de la plage, en attendant le hash de son parent.
struct Bloc {
    slot: u64,
    hash: String,
    heure: Option<i64>,
    hauteur: Option<u64>,
    parent: u64,
    executees: u64,
    entrees: u64,
    /// Ce que l'outil donne pour hash du parent.
    donne: String,
}

/// Une voie du flux, et ce qui y a été écrit.
struct Voie {
    sortie: Mutex<BufWriter<File>>,
    transactions: AtomicU64,
    blocs: AtomicU64,
    sans_bloc: AtomicU64,
}

/// Ce que l'atelier écrit et compte, partagé entre les fils de lecture.
struct Essai {
    programmes: Vec<[u8; 32]>,
    tranche: u64,
    voies: Vec<Voie>,
    a_part: BTreeSet<(u64, u64)>,
    /// Le fichier des transactions à part et le répertoire des résultats, s'il y en a un.
    part: Option<Mutex<BufWriter<File>>>,
    sortie: Option<PathBuf>,
    /// Le premier slot de la plage : ce qui est lu avant lui n'est pas rendu.
    debut: u64,
    /// Le slot d'où part la lecture.
    avant: u64,
    /// Un seul fil rend la plage, dans l'ordre de la chaîne.
    sequentiel: bool,
    /// Le hash de chaque bloc lu, par slot.
    hashs: Mutex<HashMap<u64, String>>,
    /// Les en-têtes dont le bloc parent n'était pas encore lu.
    en_attente: Mutex<Vec<Bloc>>,
    lus_avant: AtomicU64,
    differes: AtomicU64,
    inconnus: AtomicU64,
    desaccords: AtomicU64,
    vues: AtomicU64,
    votes: AtomicU64,
    gardees: AtomicU64,
    echouees: AtomicU64,
    blocs: AtomicU64,
    executees: AtomicU64,
    sautes: AtomicU64,
    reprises: AtomicU64,
    ecrites_a_part: AtomicU64,
    /// Le plus haut slot dont l'en-tête est écrit.
    plus_haut: AtomicU64,
    reculs: AtomicU64,
    retards: AtomicU64,
    /// Le premier et le dernier bloc daté : (slot, heure).
    heures: Mutex<Option<((u64, i64), (u64, i64))>>,
    /// Les blocs en cours : ce que l'outil a rendu de chacun.
    suivi: suivi::Suivi,
}

/// Le greffon lui-même : l'outil le garde, l'essai reste lisible après la lecture.
struct Greffon(Arc<Essai>);

/// Une écriture a échoué (le lecteur d'une voie est parti, le disque est plein) : rien d'utile
/// ne peut suivre, et lire le reste de la plage ne servirait à rien.
fn abandonner(quoi: &str, erreur: &std::io::Error) -> ! {
    eprintln!("essai : écriture impossible ({quoi}) : {erreur}");
    std::process::exit(3)
}

impl Essai {
    fn cite(&self, cle: [u8; 32]) -> bool {
        self.programmes.iter().any(|programme| *programme == cle)
    }

    fn garde(&self, transaction: &TransactionData) -> bool {
        let charges = &transaction.transaction_status_meta.loaded_addresses;
        transaction
            .transaction
            .message
            .static_account_keys()
            .iter()
            .chain(charges.writable.iter())
            .chain(charges.readonly.iter())
            .any(|cle| self.cite(cle.to_bytes()))
    }

    fn voie(&self, slot: u64) -> &Voie {
        &self.voies[flux::voie(slot, self.tranche, self.voies.len())]
    }

    /// Écrit un événement dans une voie.
    fn evenement(&self, voie: &Voie, json: &str) {
        let mut sortie = voie.sortie.lock().expect("voie");
        if let Err(erreur) = flux::ecrire_evenement(&mut *sortie, json) {
            abandonner("voie", &erreur);
        }
    }

    /// Le hash du bloc lu à ce slot, s'il l'a été.
    fn hash_du_bloc(&self, slot: u64) -> Option<String> {
        self.hashs.lock().expect("hashs des blocs").get(&slot).cloned()
    }

    /// Écrit l'en-tête d'un bloc dans sa voie, avec ce hash pour parent.
    fn ecrire_entete(&self, bloc: &Bloc, hash_parent: &str) {
        // Quand l'outil dit un hash (il rend un hash nul au premier bloc d'une connexion), ce
        // doit être celui du bloc parent.
        if outils::adresse(&bloc.donne) != Some([0u8; 32]) && bloc.donne != hash_parent {
            self.desaccords.fetch_add(1, Ordering::Relaxed);
        }
        if self.plus_haut.fetch_max(bloc.slot, Ordering::Relaxed) >= bloc.slot {
            self.reculs.fetch_add(1, Ordering::Relaxed);
        }
        let voie = self.voie(bloc.slot);
        let entete = flux::Entete {
            slot: bloc.slot,
            hash: &bloc.hash,
            heure: bloc.heure,
            hauteur: bloc.hauteur,
            parent: bloc.parent,
            hash_parent,
            executees: bloc.executees,
            entrees: bloc.entrees,
        };
        {
            let mut sortie = voie.sortie.lock().expect("voie");
            if let Err(erreur) = flux::ecrire_entete(&mut *sortie, &entete) {
                abandonner("voie", &erreur);
            }
        }
        voie.blocs.fetch_add(1, Ordering::Relaxed);
    }

    /// Écrit l'en-tête d'un bloc dont le parent n'a pas été lu, avec ce que l'outil donnait
    /// pour hash du parent, et le dit par un événement.
    fn ecrire_sans_parent(&self, bloc: &Bloc) {
        self.inconnus.fetch_add(1, Ordering::Relaxed);
        self.evenement(
            self.voie(bloc.slot),
            &format!("{{\"e\":\"parent_inconnu\",\"slot\":{}}}", bloc.slot),
        );
        self.ecrire_entete(bloc, &bloc.donne);
    }

    /// Écrit les en-têtes qui attendaient le hash de leur parent, ferme le flux (un événement
    /// `fin` par voie) et écrit le résumé.
    fn rendre(
        &self,
        plage: (u64, u64),
        saisies: (&str, &str, bool),
        duree_s: f64,
        sans_erreur: bool,
    ) -> std::io::Result<()> {
        let attendaient = std::mem::take(&mut *self.en_attente.lock().expect("en-têtes en attente"));
        for bloc in &attendaient {
            match self.hash_du_bloc(bloc.parent) {
                Some(hash_parent) => self.ecrire_entete(bloc, &hash_parent),
                None => self.ecrire_sans_parent(bloc),
            }
        }
        let lu = |compte: &AtomicU64| compte.load(Ordering::Relaxed);
        let reprises = lu(&self.reprises);
        let incomplets = self.suivi.incomplets();
        let mut par_voie = Vec::with_capacity(self.voies.len());
        for (rang, voie) in self.voies.iter().enumerate() {
            let comptes = format!(
                "\"transactions\":{},\"blocs\":{},\"sans_bloc\":{}",
                lu(&voie.transactions),
                lu(&voie.blocs),
                lu(&voie.sans_bloc)
            );
            self.evenement(
                voie,
                &format!(
                    "{{\"e\":\"fin\",\"voie\":{rang},{comptes},\"reprises\":{reprises},\"blocs_incomplets\":{incomplets},\"lecture_sans_erreur\":{sans_erreur}}}"
                ),
            );
            voie.sortie.lock().expect("voie").flush()?;
            par_voie.push(format!("{{{comptes}}}"));
        }
        if let Some(part) = &self.part {
            part.lock().expect("transactions à part").flush()?;
        }
        let Some(sortie) = &self.sortie else {
            return Ok(());
        };

        let borne = |bloc: Option<(u64, i64)>| {
            outils::ou_rien(bloc, |(slot, heure)| {
                format!("{{\"slot\":{slot},\"heure\":{heure}}}")
            })
        };
        let heures = *self.heures.lock().expect("heures des blocs");
        let (programmes, fils, sequentiel) = saisies;
        let ordre = if sequentiel { "sequentiel" } else { "parallele" };
        let champs = [
            format!("\"plage\":[{},{}]", plage.0, plage.1),
            format!("\"programmes\":{}", outils::chaine(programmes)),
            format!("\"fils\":{}", outils::chaine(fils)),
            format!("\"ordre\":{}", outils::chaine(ordre)),
            format!("\"tranche\":{}", self.tranche),
            format!("\"slots_de_la_plage\":{}", plage.1 - plage.0 + 1),
            format!("\"lecture_sans_erreur\":{sans_erreur}"),
            format!("\"duree_s\":{duree_s:.1}"),
            format!("\"transactions_vues\":{}", lu(&self.vues)),
            format!("\"dont_votes\":{}", lu(&self.votes)),
            format!("\"transactions_gardees\":{}", lu(&self.gardees)),
            format!("\"dont_echouees\":{}", lu(&self.echouees)),
            format!("\"blocs\":{}", lu(&self.blocs)),
            format!(
                "\"transactions_annoncees_par_les_blocs\":{}",
                lu(&self.executees)
            ),
            format!("\"slots_sans_bloc\":{}", lu(&self.sautes)),
            format!(
                "\"premier_bloc\":{}",
                borne(heures.map(|(premier, _)| premier))
            ),
            format!(
                "\"dernier_bloc\":{}",
                borne(heures.map(|(_, dernier)| dernier))
            ),
            format!("\"reprises\":{reprises}"),
            format!("\"blocs_incomplets\":{incomplets}"),
            format!("\"marge_de_lecture\":{}", plage.0 - self.avant),
            format!("\"avant_la_plage\":{}", lu(&self.lus_avant)),
            format!("\"parents_differes\":{}", lu(&self.differes)),
            format!("\"parents_inconnus\":{}", lu(&self.inconnus)),
            format!("\"parents_en_desaccord\":{}", lu(&self.desaccords)),
            format!("\"en_tetes_en_recul\":{}", lu(&self.reculs)),
            format!("\"transactions_en_retard\":{}", lu(&self.retards)),
            format!("\"voies\":{}", outils::liste(par_voie)),
            format!("\"a_part_demandees\":{}", self.a_part.len()),
            format!("\"a_part_ecrites\":{}", lu(&self.ecrites_a_part)),
        ];
        let mut resume = File::create(sortie.join("resume.json"))?;
        writeln!(resume, "{{\n  {}\n}}", champs.join(",\n  "))
    }
}

impl Plugin for Greffon {
    fn name(&self) -> &'static str {
        "essai"
    }

    fn on_transaction<'a>(
        &'a self,
        _fil: usize,
        _base: Option<Arc<Client>>,
        transaction: &'a TransactionData,
    ) -> PluginFuture<'a> {
        Box::pin(async move {
            let essai = &self.0;
            if transaction.slot < essai.debut {
                essai.lus_avant.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            essai.vues.fetch_add(1, Ordering::Relaxed);
            essai
                .suivi
                .vue(transaction.slot, transaction.transaction_slot_index as u64);
            if transaction.is_vote {
                essai.votes.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            if !essai.garde(transaction) {
                return Ok(());
            }
            essai.gardees.fetch_add(1, Ordering::Relaxed);
            if transaction.transaction_status_meta.status.is_err() {
                essai.echouees.fetch_add(1, Ordering::Relaxed);
            }
            let slot = transaction.slot;
            if slot <= essai.plus_haut.load(Ordering::Relaxed) {
                essai.retards.fetch_add(1, Ordering::Relaxed);
            }
            let rang = transaction.transaction_slot_index as u64;
            let signature: &[u8] = transaction.signature.as_ref();
            let signee = transaction.transaction_proto_bytes();
            let compte_rendu = transaction.transaction_status_meta_bytes.as_slice();
            let voie = essai.voie(slot);
            {
                let mut sortie = voie.sortie.lock().expect("voie");
                if let Err(erreur) = flux::ecrire_transaction(
                    &mut *sortie,
                    slot,
                    rang,
                    signature,
                    false,
                    &signee,
                    compte_rendu,
                ) {
                    abandonner("voie", &erreur);
                }
            }
            voie.transactions.fetch_add(1, Ordering::Relaxed);
            if let Some(part) = &essai.part
                && essai.a_part.contains(&(slot, rang))
            {
                let mut part = part.lock().expect("transactions à part");
                if let Err(erreur) = flux::ecrire_transaction(
                    &mut *part,
                    slot,
                    rang,
                    signature,
                    false,
                    &signee,
                    compte_rendu,
                ) {
                    abandonner("transactions à part", &erreur);
                }
                essai.ecrites_a_part.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    }

    fn on_block<'a>(
        &'a self,
        _fil: usize,
        _base: Option<Arc<Client>>,
        bloc: &'a BlockData,
    ) -> PluginFuture<'a> {
        Box::pin(async move {
            let essai = &self.0;
            match bloc {
                BlockData::Block {
                    parent_slot,
                    parent_blockhash,
                    slot,
                    blockhash,
                    block_time,
                    block_height,
                    executed_transaction_count,
                    entry_count,
                    ..
                } => {
                    let hash = blockhash.to_string();
                    essai
                        .hashs
                        .lock()
                        .expect("hashs des blocs")
                        .insert(*slot, hash.clone());
                    if *slot < essai.debut {
                        essai.lus_avant.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                    essai.blocs.fetch_add(1, Ordering::Relaxed);
                    essai
                        .executees
                        .fetch_add(*executed_transaction_count, Ordering::Relaxed);
                    essai.suivi.bloc(*slot, *executed_transaction_count);
                    if let Some(heure) = block_time {
                        let vu = (*slot, *heure);
                        let mut heures = essai.heures.lock().expect("heures des blocs");
                        *heures = Some(match *heures {
                            None => (vu, vu),
                            Some((premier, dernier)) => (premier.min(vu), dernier.max(vu)),
                        });
                    }
                    let lu = Bloc {
                        slot: *slot,
                        hash,
                        heure: *block_time,
                        hauteur: *block_height,
                        parent: *parent_slot,
                        executees: *executed_transaction_count,
                        entrees: *entry_count,
                        donne: parent_blockhash.to_string(),
                    };
                    match essai.hash_du_bloc(*parent_slot) {
                        Some(hash_parent) => essai.ecrire_entete(&lu, &hash_parent),
                        // En ordre séquentiel, aucun autre fil ne lira ce parent plus tard.
                        None if essai.sequentiel => essai.ecrire_sans_parent(&lu),
                        None => {
                            essai.differes.fetch_add(1, Ordering::Relaxed);
                            essai
                                .en_attente
                                .lock()
                                .expect("en-têtes en attente")
                                .push(lu);
                        }
                    }
                }
                BlockData::PossibleLeaderSkipped { slot } => {
                    if *slot < essai.debut {
                        return Ok(());
                    }
                    let voie = essai.voie(*slot);
                    essai.sautes.fetch_add(1, Ordering::Relaxed);
                    essai.evenement(voie, &format!("{{\"e\":\"sans_bloc\",\"slot\":{slot}}}"));
                    voie.sans_bloc.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        })
    }

    fn on_error<'a>(
        &'a self,
        _fil: usize,
        _base: Option<Arc<Client>>,
        erreur: &'a FirehoseErrorContext,
    ) -> PluginFuture<'a> {
        Box::pin(async move {
            let essai = &self.0;
            essai.reprises.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "essai : la lecture a buté au slot {} (fil {}) : {}",
                erreur.slot, erreur.thread_id, erreur.error_message
            );
            let voie = essai.voie(erreur.slot);
            essai.evenement(
                voie,
                &format!(
                    "{{\"e\":\"reprise\",\"slot\":{},\"fil\":{}}}",
                    erreur.slot, erreur.thread_id
                ),
            );
            Ok(())
        })
    }
}

/// Ouvre une voie : un chemin (d'ordinaire un tube, dont l'ouverture attend le lecteur), ou
/// « fd:N », un descripteur que le lanceur a ouvert pour le flux.
fn ouvrir_voie(texte: &str) -> Result<File, Box<dyn std::error::Error>> {
    let Some(numero) = texte.strip_prefix("fd:") else {
        let fichier = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(texte)?;
        return Ok(fichier);
    };
    let numero: i32 = numero
        .parse()
        .ok()
        .filter(|numero| (3..=9).contains(numero))
        .ok_or("ESSAI_VOIES : « fd:N » attendu, N de 3 à 9")?;
    // SAFETY : le descripteur est donné à ce programme par celui qui le lance, pour le flux
    // et pour lui seul ; rien d'autre ici ne l'ouvre ni ne le ferme.
    let fichier = unsafe { File::from_raw_fd(numero) };
    // Un descripteur qui n'est pas ouvert se dit tout de suite, pas à la première écriture ;
    // et il n'y a alors rien à fermer.
    if let Err(erreur) = fichier.metadata() {
        std::mem::forget(fichier);
        return Err(format!("ESSAI_VOIES : descripteur {numero} : {erreur}").into());
    }
    Ok(fichier)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let saisie = |nom: &str| std::env::var(nom).unwrap_or_default();
    let plage =
        outils::plage(&saisie("ESSAI_PLAGE")).ok_or("ESSAI_PLAGE : attendu « début:fin »")?;
    let programmes = saisie("ESSAI_PROGRAMMES");
    let suivis = outils::adresses(&programmes).ok_or("ESSAI_PROGRAMMES : adresses illisibles")?;
    let fils = saisie("ESSAI_FILS");
    let sequentiel = match saisie("ESSAI_ORDRE").trim() {
        "" | "parallele" => false,
        "sequentiel" => true,
        _ => return Err("ESSAI_ORDRE : « parallele » ou « sequentiel » attendu".into()),
    };
    let tranche = match saisie("ESSAI_TRANCHE").trim() {
        "" => 100,
        texte => texte
            .parse::<u64>()
            .ok()
            .filter(|tranche| *tranche >= 1)
            .ok_or("ESSAI_TRANCHE : nombre de slots attendu, au moins 1")?,
    };
    let a_part =
        outils::couples(&saisie("ESSAI_A_PART")).ok_or("ESSAI_A_PART : « slot/rang » illisibles")?;
    let sortie = match saisie("ESSAI_SORTIE") {
        vide if vide.is_empty() => None,
        chemin => Some(PathBuf::from(chemin)),
    };
    if sortie.is_none() && !a_part.is_empty() {
        return Err("ESSAI_A_PART : il faut alors ESSAI_SORTIE, le répertoire des résultats".into());
    }
    let avant = match saisie("ESSAI_AVANT").trim() {
        "" => plage.0.saturating_sub(MARGE),
        texte => texte
            .parse::<u64>()
            .ok()
            .filter(|avant| *avant < plage.0)
            .ok_or("ESSAI_AVANT : slot attendu, avant la plage")?,
    };
    let chemins = saisie("ESSAI_VOIES");
    if chemins.trim().is_empty() {
        return Err("ESSAI_VOIES : au moins une voie attendue".into());
    }
    let mut voies = Vec::new();
    for chemin in chemins.split(',') {
        let fichier = ouvrir_voie(chemin.trim())?;
        voies.push(Voie {
            sortie: Mutex::new(BufWriter::with_capacity(1 << 20, fichier)),
            transactions: AtomicU64::new(0),
            blocs: AtomicU64::new(0),
            sans_bloc: AtomicU64::new(0),
        });
    }
    let part = match &sortie {
        Some(sortie) => {
            std::fs::create_dir_all(sortie)?;
            let fichier = File::create(sortie.join("a_part.flux"))?;
            Some(Mutex::new(BufWriter::new(fichier)))
        }
        None => None,
    };
    let essai = Arc::new(Essai {
        programmes: suivis,
        tranche,
        voies,
        a_part: a_part.into_iter().collect(),
        part,
        sortie,
        debut: plage.0,
        avant,
        sequentiel,
        hashs: Mutex::new(HashMap::new()),
        en_attente: Mutex::new(Vec::new()),
        lus_avant: AtomicU64::new(0),
        differes: AtomicU64::new(0),
        inconnus: AtomicU64::new(0),
        desaccords: AtomicU64::new(0),
        vues: AtomicU64::new(0),
        votes: AtomicU64::new(0),
        gardees: AtomicU64::new(0),
        echouees: AtomicU64::new(0),
        blocs: AtomicU64::new(0),
        executees: AtomicU64::new(0),
        sautes: AtomicU64::new(0),
        reprises: AtomicU64::new(0),
        ecrites_a_part: AtomicU64::new(0),
        plus_haut: AtomicU64::new(0),
        reculs: AtomicU64::new(0),
        retards: AtomicU64::new(0),
        heures: Mutex::new(None),
        suivi: suivi::Suivi::new(),
    });
    for (rang, voie) in essai.voies.iter().enumerate() {
        essai.evenement(
            voie,
            &format!(
                "{{\"e\":\"debut\",\"plage\":[{},{}],\"voie\":{rang},\"voies\":{},\"tranche\":{tranche},\"ordre\":{},\"avant\":{avant},\"atelier\":{},\"outil\":{}}}",
                plage.0,
                plage.1,
                essai.voies.len(),
                outils::chaine(if sequentiel { "sequentiel" } else { "parallele" }),
                outils::chaine(ATELIER),
                outils::chaine(OUTIL)
            ),
        );
    }
    let mut lanceur = JetstreamerRunner::new()
        .with_log_level("info")
        .with_plugin(Box::new(Greffon(essai.clone())))
        .with_slot_range_bounds(avant, plage.1 + 1);
    if !fils.trim().is_empty() {
        let nombre: usize = fils
            .trim()
            .parse()
            .map_err(|_| "ESSAI_FILS : nombre attendu")?;
        lanceur = lanceur.with_threads(nombre);
    }
    if sequentiel {
        lanceur = lanceur.with_sequential(true);
    }
    let depart = Instant::now();
    let issue = lanceur.run();
    essai.rendre(
        plage,
        (programmes.as_str(), fils.as_str(), sequentiel),
        depart.elapsed().as_secs_f64(),
        issue.is_ok(),
    )?;
    issue.map_err(|erreur| -> Box<dyn std::error::Error> { Box::new(erreur) })
}
