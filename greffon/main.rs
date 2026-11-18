//! Greffon d'essai pour Jetstreamer.
//!
//! Il relit une plage de slots de l'archive publique Old Faithful et ne garde que les
//! transactions qui citent l'un des programmes donnés, parmi les comptes du message ou parmi
//! ceux que la transaction charge par une table d'adresses. Il compte, slot par slot, celles
//! qu'il garde et celles qui ont échoué ; pour les slots nommés, il écrit chaque transaction
//! gardée en entier, une ligne JSON par transaction.
//!
//! Saisies, par l'environnement :
//! - `ESSAI_PLAGE` : « début:fin », bornes comprises ;
//! - `ESSAI_PROGRAMMES` : adresses en base 58, séparées par des virgules ;
//! - `ESSAI_SLOTS_ENTIERS` : slots dont les transactions gardées sont écrites en entier
//!   (peut rester vide) ;
//! - `ESSAI_FILS` : nombre de fils de lecture (vide : au choix de l'outil) ;
//! - `ESSAI_SORTIE` : répertoire des résultats.
//!
//! Résultats : `resume.json`, `comptes.csv` (slot, gardées, échouées) et
//! `transactions.jsonl`. Si la lecture a buté et repris (`reprises` au résumé), des
//! transactions ont pu être comptées deux fois : le résumé le dit, il ne le corrige pas. Une
//! lecture arrêtée avant la fin rend les comptes de ce qu'elle a lu : `blocs` et
//! `slots_sans_bloc` disent jusqu'où, face à `slots_de_la_plage`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use clickhouse::Client;
use jetstreamer::{
    JetstreamerRunner,
    firehose::{BlockData, TransactionData},
    plugin::{FirehoseErrorContext, Plugin, PluginFuture},
};

mod outils;

/// Ce que l'essai compte et écrit, partagé entre les fils de lecture.
struct Essai {
    programmes: Vec<[u8; 32]>,
    entiers: BTreeSet<u64>,
    sortie: PathBuf,
    vues: AtomicU64,
    votes: AtomicU64,
    gardees: AtomicU64,
    echouees: AtomicU64,
    blocs: AtomicU64,
    executees: AtomicU64,
    sautes: AtomicU64,
    reprises: AtomicU64,
    ecrites: AtomicU64,
    ecriture_ratee: AtomicBool,
    /// Par slot : transactions gardées, dont échouées.
    par_slot: Mutex<BTreeMap<u64, (u32, u32)>>,
    /// Le premier et le dernier bloc daté : (slot, heure).
    heures: Mutex<Option<((u64, i64), (u64, i64))>>,
    entieres: Mutex<BufWriter<File>>,
}

/// Le greffon lui-même : l'outil le garde, l'essai reste lisible après la lecture.
struct Greffon(Arc<Essai>);

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

    /// Écrit le résumé et les comptes par slot.
    fn rendre(
        &self,
        plage: (u64, u64),
        saisies: (&str, &str),
        duree_s: f64,
        sans_erreur: bool,
    ) -> std::io::Result<()> {
        let mut comptes = BufWriter::new(File::create(self.sortie.join("comptes.csv"))?);
        writeln!(comptes, "slot,gardees,echouees")?;
        let par_slot = self.par_slot.lock().expect("comptes par slot");
        for (slot, (gardees, echouees)) in par_slot.iter() {
            writeln!(comptes, "{slot},{gardees},{echouees}")?;
        }
        comptes.flush()?;
        self.entieres
            .lock()
            .expect("transactions entières")
            .flush()?;

        let borne = |bloc: Option<(u64, i64)>| {
            outils::ou_rien(bloc, |(slot, heure)| {
                format!("{{\"slot\":{slot},\"heure\":{heure}}}")
            })
        };
        let heures = *self.heures.lock().expect("heures des blocs");
        let lu = |compte: &AtomicU64| compte.load(Ordering::Relaxed);
        let (programmes, fils) = saisies;
        let champs = [
            format!("\"plage\":[{},{}]", plage.0, plage.1),
            format!("\"programmes\":{}", outils::chaine(programmes)),
            format!("\"fils\":{}", outils::chaine(fils)),
            format!("\"slots_de_la_plage\":{}", plage.1 - plage.0 + 1),
            format!("\"lecture_sans_erreur\":{sans_erreur}"),
            format!("\"duree_s\":{duree_s:.1}"),
            format!("\"transactions_vues\":{}", lu(&self.vues)),
            format!("\"dont_votes\":{}", lu(&self.votes)),
            format!("\"transactions_gardees\":{}", lu(&self.gardees)),
            format!("\"dont_echouees\":{}", lu(&self.echouees)),
            format!("\"slots_avec_transactions_gardees\":{}", par_slot.len()),
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
            format!("\"slots_entiers_demandes\":{}", self.entiers.len()),
            format!("\"transactions_entieres_ecrites\":{}", lu(&self.ecrites)),
            format!(
                "\"ecriture_ratee\":{}",
                self.ecriture_ratee.load(Ordering::Relaxed)
            ),
        ];
        let mut resume = File::create(self.sortie.join("resume.json"))?;
        writeln!(resume, "{{\n  {}\n}}", champs.join(",\n  "))
    }
}

/// Une instruction : le rang de son programme parmi les comptes, ses comptes et ses données
/// en hexadécimal, puis ce que l'appelant ajoute (la hauteur d'appel d'une instruction interne).
fn instruction(programme: u8, comptes: &[u8], donnees: &[u8], suite: &str) -> String {
    format!(
        "{{\"programme\":{programme},\"comptes\":\"{}\",\"donnees\":\"{}\"{suite}}}",
        outils::hexa(comptes),
        outils::hexa(donnees)
    )
}

/// Une transaction gardée, en entier, sur une ligne JSON.
fn ligne(transaction: &TransactionData) -> String {
    let compte_rendu = &transaction.transaction_status_meta;
    let message = &transaction.transaction.message;
    let entete = message.header();
    let adresses = |cles: &mut dyn Iterator<Item = String>| {
        outils::liste(cles.map(|cle| outils::chaine(&cle)))
    };
    let soldes = |soldes: &[u64]| outils::liste(soldes.iter().map(|solde| solde.to_string()));
    let instructions = outils::liste(
        message
            .instructions()
            .iter()
            .map(|appel| instruction(appel.program_id_index, &appel.accounts, &appel.data, "")),
    );
    let internes = outils::ou_rien(compte_rendu.inner_instructions.as_ref(), |groupes| {
        outils::liste(groupes.iter().map(|groupe| {
            let appels = outils::liste(groupe.instructions.iter().map(|interne| {
                let hauteur = outils::ou_rien(interne.stack_height, |hauteur| hauteur.to_string());
                instruction(
                    interne.instruction.program_id_index,
                    &interne.instruction.accounts,
                    &interne.instruction.data,
                    &format!(",\"hauteur\":{hauteur}"),
                )
            }));
            format!("{{\"rang\":{},\"instructions\":{appels}}}", groupe.index)
        }))
    });
    let tables = outils::ou_rien(message.address_table_lookups(), |tables| {
        outils::liste(tables.iter().map(|table| {
            format!(
                "{{\"table\":{},\"ecriture\":\"{}\",\"lecture\":\"{}\"}}",
                outils::chaine(&table.account_key.to_string()),
                outils::hexa(&table.writable_indexes),
                outils::hexa(&table.readonly_indexes)
            )
        }))
    });
    let journaux = outils::ou_rien(compte_rendu.log_messages.as_ref(), |journaux| {
        outils::liste(journaux.iter().map(|journal| outils::chaine(journal)))
    });
    // Les soldes des comptes de jetons, tels que le nœud les rapporte, avant ou après.
    macro_rules! jetons {
        ($soldes:expr) => {
            outils::ou_rien($soldes.as_ref(), |soldes| {
                outils::liste(soldes.iter().map(|solde| {
                    format!(
                        "{{\"compte\":{},\"jeton\":{},\"montant\":{},\"decimales\":{},\"proprietaire\":{},\"programme\":{}}}",
                        solde.account_index,
                        outils::chaine(&solde.mint),
                        outils::chaine(&solde.ui_token_amount.amount),
                        solde.ui_token_amount.decimals,
                        outils::chaine(&solde.owner),
                        outils::chaine(&solde.program_id)
                    )
                }))
            })
        };
    }
    let echec = match &compte_rendu.status {
        Ok(()) => "null".to_string(),
        Err(erreur) => outils::chaine(&format!("{erreur:?}")),
    };
    let champs = [
        format!("\"slot\":{}", transaction.slot),
        format!("\"rang\":{}", transaction.transaction_slot_index),
        format!(
            "\"signature\":{}",
            outils::chaine(&transaction.signature.to_string())
        ),
        format!("\"echec\":{echec}"),
        format!("\"frais\":{}", compte_rendu.fee),
        format!(
            "\"unites_de_calcul\":{}",
            outils::ou_rien(compte_rendu.compute_units_consumed, |unites| unites
                .to_string())
        ),
        format!(
            "\"entete\":[{},{},{}]",
            entete.num_required_signatures,
            entete.num_readonly_signed_accounts,
            entete.num_readonly_unsigned_accounts
        ),
        format!(
            "\"bloc_recent\":{}",
            outils::chaine(&message.recent_blockhash().to_string())
        ),
        format!(
            "\"comptes\":{}",
            adresses(
                &mut message
                    .static_account_keys()
                    .iter()
                    .map(|cle| cle.to_string())
            )
        ),
        format!("\"tables\":{tables}"),
        format!(
            "\"charges_en_ecriture\":{}",
            adresses(
                &mut compte_rendu
                    .loaded_addresses
                    .writable
                    .iter()
                    .map(|cle| cle.to_string())
            )
        ),
        format!(
            "\"charges_en_lecture\":{}",
            adresses(
                &mut compte_rendu
                    .loaded_addresses
                    .readonly
                    .iter()
                    .map(|cle| cle.to_string())
            )
        ),
        format!("\"instructions\":{instructions}"),
        format!("\"internes\":{internes}"),
        format!("\"journaux\":{journaux}"),
        format!("\"soldes_avant\":{}", soldes(&compte_rendu.pre_balances)),
        format!("\"soldes_apres\":{}", soldes(&compte_rendu.post_balances)),
        format!(
            "\"jetons_avant\":{}",
            jetons!(compte_rendu.pre_token_balances)
        ),
        format!(
            "\"jetons_apres\":{}",
            jetons!(compte_rendu.post_token_balances)
        ),
    ];
    format!("{{{}}}", champs.join(","))
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
            }
            if !essai.garde(transaction) {
                return Ok(());
            }
            let echouee = transaction.transaction_status_meta.status.is_err();
            essai.gardees.fetch_add(1, Ordering::Relaxed);
            if echouee {
                essai.echouees.fetch_add(1, Ordering::Relaxed);
            }
            {
                let mut par_slot = essai.par_slot.lock().expect("comptes par slot");
                let compte = par_slot.entry(transaction.slot).or_insert((0, 0));
                compte.0 += 1;
                compte.1 += u32::from(echouee);
            }
            if essai.entiers.contains(&transaction.slot) {
                let entiere = ligne(transaction);
                let mut entieres = essai.entieres.lock().expect("transactions entières");
                if writeln!(entieres, "{entiere}").is_ok() {
                    essai.ecrites.fetch_add(1, Ordering::Relaxed);
                } else {
                    essai.ecriture_ratee.store(true, Ordering::Relaxed);
                }
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
            if let BlockData::Block {
                executed_transaction_count,
                ..
            } = bloc
            {
                essai.blocs.fetch_add(1, Ordering::Relaxed);
                essai
                    .executees
                    .fetch_add(*executed_transaction_count, Ordering::Relaxed);
            } else {
                essai.sautes.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(heure) = bloc.block_time() {
                let vu = (bloc.slot(), heure);
                let mut heures = essai.heures.lock().expect("heures des blocs");
                *heures = Some(match *heures {
                    None => (vu, vu),
                    Some((premier, dernier)) => (premier.min(vu), dernier.max(vu)),
                });
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
            self.0.reprises.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "essai : la lecture a buté au slot {} (fil {}) : {}",
                erreur.slot, erreur.thread_id, erreur.error_message
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
    let fils = saisie("ESSAI_FILS");
    let sortie = PathBuf::from(saisie("ESSAI_SORTIE"));
    if sortie.as_os_str().is_empty() {
        return Err("ESSAI_SORTIE : répertoire attendu".into());
    }
    std::fs::create_dir_all(&sortie)?;
    let essai = Arc::new(Essai {
        programmes: outils::adresses(&programmes)
            .ok_or("ESSAI_PROGRAMMES : adresses illisibles")?,
        entiers: outils::slots(&saisie("ESSAI_SLOTS_ENTIERS"))
            .ok_or("ESSAI_SLOTS_ENTIERS : slots illisibles")?
            .into_iter()
            .collect(),
        entieres: Mutex::new(BufWriter::new(File::create(
            sortie.join("transactions.jsonl"),
        )?)),
        sortie,
        vues: AtomicU64::new(0),
        votes: AtomicU64::new(0),
        gardees: AtomicU64::new(0),
        echouees: AtomicU64::new(0),
        blocs: AtomicU64::new(0),
        executees: AtomicU64::new(0),
        sautes: AtomicU64::new(0),
        reprises: AtomicU64::new(0),
        ecrites: AtomicU64::new(0),
        ecriture_ratee: AtomicBool::new(false),
        par_slot: Mutex::new(BTreeMap::new()),
        heures: Mutex::new(None),
    });
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
