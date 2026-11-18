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
//! - `ESSAI_VOIES` : chemins des voies, séparés par des virgules ;
//! - `ESSAI_TRANCHE` : nombre de slots d'une tranche (vide : 100) ;
//! - `ESSAI_A_PART` : transactions dont l'enregistrement est aussi écrit dans `a_part.flux`,
//!   « slot/rang » séparés par des virgules (peut rester vide) ;
//! - `ESSAI_FILS` : nombre de fils de lecture (vide : au choix de l'outil) ;
//! - `ESSAI_SORTIE` : répertoire des résultats.
//!
//! Résultats : `resume.json`, les comptes du greffon, et `a_part.flux`. Chaque voie commence
//! par un événement `debut` et finit par un événement `fin` qui dit ce qu'elle a reçu ; un slot
//! sans bloc et une reprise de lecture y laissent aussi un événement. Quand la lecture bute et
//! reprend, les transactions du slot en cours sont rendues une seconde fois : à qui lit le flux
//! de les reconnaître. Une voie qui ne s'écrit plus arrête tout, avec le code 3.

use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
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
    part: Mutex<BufWriter<File>>,
    sortie: PathBuf,
    vues: AtomicU64,
    votes: AtomicU64,
    gardees: AtomicU64,
    echouees: AtomicU64,
    blocs: AtomicU64,
    executees: AtomicU64,
    sautes: AtomicU64,
    reprises: AtomicU64,
    ecrites_a_part: AtomicU64,
    /// Le premier et le dernier bloc daté : (slot, heure).
    heures: Mutex<Option<((u64, i64), (u64, i64))>>,
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

    /// Ferme le flux (un événement `fin` par voie) et écrit le résumé.
    fn rendre(
        &self,
        plage: (u64, u64),
        saisies: (&str, &str),
        duree_s: f64,
        sans_erreur: bool,
    ) -> std::io::Result<()> {
        let lu = |compte: &AtomicU64| compte.load(Ordering::Relaxed);
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
                    "{{\"e\":\"fin\",\"voie\":{rang},{comptes},\"lecture_sans_erreur\":{sans_erreur}}}"
                ),
            );
            voie.sortie.lock().expect("voie").flush()?;
            par_voie.push(format!("{{{comptes}}}"));
        }
        self.part.lock().expect("transactions à part").flush()?;

        let borne = |bloc: Option<(u64, i64)>| {
            outils::ou_rien(bloc, |(slot, heure)| {
                format!("{{\"slot\":{slot},\"heure\":{heure}}}")
            })
        };
        let heures = *self.heures.lock().expect("heures des blocs");
        let (programmes, fils) = saisies;
        let champs = [
            format!("\"plage\":[{},{}]", plage.0, plage.1),
            format!("\"programmes\":{}", outils::chaine(programmes)),
            format!("\"fils\":{}", outils::chaine(fils)),
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
            format!("\"reprises\":{}", lu(&self.reprises)),
            format!("\"voies\":{}", outils::liste(par_voie)),
            format!("\"a_part_demandees\":{}", self.a_part.len()),
            format!("\"a_part_ecrites\":{}", lu(&self.ecrites_a_part)),
        ];
        let mut resume = File::create(self.sortie.join("resume.json"))?;
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
            essai.vues.fetch_add(1, Ordering::Relaxed);
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
            if essai.a_part.contains(&(slot, rang)) {
                let mut part = essai.part.lock().expect("transactions à part");
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
            let voie = essai.voie(bloc.slot());
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
                    essai.blocs.fetch_add(1, Ordering::Relaxed);
                    essai
                        .executees
                        .fetch_add(*executed_transaction_count, Ordering::Relaxed);
                    let hash = blockhash.to_string();
                    let hash_parent = parent_blockhash.to_string();
                    let entete = flux::Entete {
                        slot: *slot,
                        hash: &hash,
                        heure: *block_time,
                        hauteur: *block_height,
                        parent: *parent_slot,
                        hash_parent: &hash_parent,
                        executees: *executed_transaction_count,
                        entrees: *entry_count,
                    };
                    {
                        let mut sortie = voie.sortie.lock().expect("voie");
                        if let Err(erreur) = flux::ecrire_entete(&mut *sortie, &entete) {
                            abandonner("voie", &erreur);
                        }
                    }
                    voie.blocs.fetch_add(1, Ordering::Relaxed);
                    if let Some(heure) = block_time {
                        let vu = (*slot, *heure);
                        let mut heures = essai.heures.lock().expect("heures des blocs");
                        *heures = Some(match *heures {
                            None => (vu, vu),
                            Some((premier, dernier)) => (premier.min(vu), dernier.max(vu)),
                        });
                    }
                }
                BlockData::PossibleLeaderSkipped { slot } => {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let saisie = |nom: &str| std::env::var(nom).unwrap_or_default();
    let plage =
        outils::plage(&saisie("ESSAI_PLAGE")).ok_or("ESSAI_PLAGE : attendu « début:fin »")?;
    let programmes = saisie("ESSAI_PROGRAMMES");
    let suivis = outils::adresses(&programmes).ok_or("ESSAI_PROGRAMMES : adresses illisibles")?;
    let fils = saisie("ESSAI_FILS");
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
    let sortie = PathBuf::from(saisie("ESSAI_SORTIE"));
    if sortie.as_os_str().is_empty() {
        return Err("ESSAI_SORTIE : répertoire attendu".into());
    }
    std::fs::create_dir_all(&sortie)?;
    let chemins = saisie("ESSAI_VOIES");
    if chemins.trim().is_empty() {
        return Err("ESSAI_VOIES : au moins une voie attendue".into());
    }
    // Une voie est d'ordinaire un tube : l'ouvrir attend que son lecteur soit là.
    let mut voies = Vec::new();
    for chemin in chemins.split(',') {
        let fichier = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(chemin.trim())?;
        voies.push(Voie {
            sortie: Mutex::new(BufWriter::with_capacity(1 << 20, fichier)),
            transactions: AtomicU64::new(0),
            blocs: AtomicU64::new(0),
            sans_bloc: AtomicU64::new(0),
        });
    }
    let essai = Arc::new(Essai {
        programmes: suivis,
        tranche,
        voies,
        a_part: a_part.into_iter().collect(),
        part: Mutex::new(BufWriter::new(File::create(sortie.join("a_part.flux"))?)),
        sortie,
        vues: AtomicU64::new(0),
        votes: AtomicU64::new(0),
        gardees: AtomicU64::new(0),
        echouees: AtomicU64::new(0),
        blocs: AtomicU64::new(0),
        executees: AtomicU64::new(0),
        sautes: AtomicU64::new(0),
        reprises: AtomicU64::new(0),
        ecrites_a_part: AtomicU64::new(0),
        heures: Mutex::new(None),
    });
    for (rang, voie) in essai.voies.iter().enumerate() {
        essai.evenement(
            voie,
            &format!(
                "{{\"e\":\"debut\",\"plage\":[{},{}],\"voie\":{rang},\"voies\":{},\"tranche\":{tranche}}}",
                plage.0,
                plage.1,
                essai.voies.len()
            ),
        );
    }
    let mut lanceur = JetstreamerRunner::new()
        .with_log_level("info")
        .with_plugin(Box::new(Greffon(essai.clone())))
        .with_slot_range_bounds(plage.0, plage.1 + 1);
    if !fils.trim().is_empty() {
        let nombre: usize = fils
            .trim()
            .parse()
            .map_err(|_| "ESSAI_FILS : nombre attendu")?;
        lanceur = lanceur.with_threads(nombre);
    }
    let depart = Instant::now();
    let issue = lanceur.run();
    essai.rendre(
        plage,
        (programmes.as_str(), fils.as_str()),
        depart.elapsed().as_secs_f64(),
        issue.is_ok(),
    )?;
    issue.map_err(|erreur| -> Box<dyn std::error::Error> { Box::new(erreur) })
}
