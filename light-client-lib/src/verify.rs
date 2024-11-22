use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::Arc,
};

use ckb_chain_spec::consensus::Consensus;
use ckb_error::Error;
use ckb_script::TxVerifyEnv;
use ckb_traits::{CellDataProvider, ExtensionProvider, HeaderFieldsProvider, HeaderProvider};
use ckb_types::{
    core::{
        cell::{CellMeta, CellProvider, CellStatus, ResolvedTransaction},
        error::OutPointError,
        Cycle, DepType, HeaderView, TransactionView,
    },
    packed::{OutPoint, OutPointVec},
    prelude::Entity,
};
use ckb_verification::{
    CapacityVerifier, NonContextualTransactionVerifier, ScriptVerifier,
    TimeRelativeTransactionVerifier,
};

#[cfg(target_arch = "wasm32")]
use crate::storage::{StorageWithChainData, TmpDB};

/// Light client can only verify non-cellbase transaction,
/// can not reuse the `ContextualTransactionVerifier` in ckb_verification crate which is used to verify cellbase also.
pub struct ContextualTransactionVerifier<T>
where
    T: CellDataProvider + HeaderProvider + ExtensionProvider + Send + Sync + Clone + 'static,
{
    pub(crate) time_relative: TimeRelativeTransactionVerifier<T>,
    pub(crate) capacity: CapacityVerifier,
    pub(crate) script: ScriptVerifier<T>,
}

impl<T> ContextualTransactionVerifier<T>
where
    T: CellDataProvider
        + HeaderFieldsProvider
        + CellProvider
        + HeaderProvider
        + ExtensionProvider
        + Send
        + Sync
        + Clone
        + 'static,
{
    /// Creates a new ContextualTransactionVerifier
    pub fn new(
        rtx: Arc<ResolvedTransaction>,
        consensus: Arc<Consensus>,
        swc: &T,
        tx_env: Arc<TxVerifyEnv>,
    ) -> Self {
        ContextualTransactionVerifier {
            time_relative: TimeRelativeTransactionVerifier::new(
                Arc::clone(&rtx),
                Arc::clone(&consensus),
                swc.clone(),
                Arc::clone(&tx_env),
            ),
            script: ScriptVerifier::new(
                Arc::clone(&rtx),
                swc.clone(),
                Arc::clone(&consensus),
                Arc::clone(&tx_env),
            ),
            capacity: CapacityVerifier::new(Arc::clone(&rtx), consensus.dao_type_hash()),
        }
    }

    pub fn verify(&self, max_cycles: Cycle) -> Result<Cycle, Error> {
        self.time_relative.verify()?;
        self.capacity.verify()?;
        self.script.verify(max_cycles)
    }
}

pub fn verify_tx<T>(
    transaction: TransactionView,
    swc: &T,
    consensus: Arc<Consensus>,
    tip_header: &HeaderView,
) -> Result<Cycle, Error>
where
    T: CellDataProvider
        + HeaderFieldsProvider
        + HeaderProvider
        + ExtensionProvider
        + CellProvider
        + Send
        + Sync
        + Clone
        + 'static,
{
    NonContextualTransactionVerifier::new(&transaction, &consensus).verify()?;

    let rtx = resolve_tx(swc, transaction)?;
    let tx_env = TxVerifyEnv::new_submit(&tip_header);
    ContextualTransactionVerifier::new(Arc::new(rtx), Arc::clone(&consensus), swc, Arc::new(tx_env))
        .verify(consensus.max_block_cycles())
}

fn resolve_tx<T>(
    swc: &T,
    transaction: TransactionView,
) -> Result<ResolvedTransaction, OutPointError>
where
    T: CellDataProvider
        + HeaderFieldsProvider
        + HeaderProvider
        + ExtensionProvider
        + Send
        + Sync
        + Clone
        + CellProvider
        + 'static,
{
    let (mut resolved_inputs, mut resolved_cell_deps, mut resolved_dep_groups) = (
        Vec::with_capacity(transaction.inputs().len()),
        Vec::with_capacity(transaction.cell_deps().len()),
        Vec::new(),
    );
    let mut current_inputs = HashSet::new();

    let mut resolved_cells: HashMap<(OutPoint, bool), CellMeta> = HashMap::new();
    let mut resolve_cell =
        |out_point: &OutPoint, eager_load: bool| -> Result<CellMeta, OutPointError> {
            match resolved_cells.entry((out_point.clone(), eager_load)) {
                Entry::Occupied(entry) => Ok(entry.get().clone()),
                Entry::Vacant(entry) => {
                    let cell_status = swc.cell(out_point, eager_load);
                    match cell_status {
                        CellStatus::Dead => Err(OutPointError::Dead(out_point.clone())),
                        CellStatus::Unknown => Err(OutPointError::Unknown(out_point.clone())),
                        CellStatus::Live(cell_meta) => {
                            entry.insert(cell_meta.clone());
                            Ok(cell_meta)
                        }
                    }
                }
            }
        };

    for out_point in transaction.input_pts_iter() {
        if !current_inputs.insert(out_point.to_owned()) {
            return Err(OutPointError::Dead(out_point));
        }
        resolved_inputs.push(resolve_cell(&out_point, false)?);
    }

    for cell_dep in transaction.cell_deps_iter() {
        if cell_dep.dep_type() == DepType::DepGroup.into() {
            let outpoint = cell_dep.out_point();
            let dep_group = resolve_cell(&outpoint, true)?;
            let data = dep_group
                .mem_cell_data
                .as_ref()
                .expect("Load cell meta must with data");
            let sub_out_points =
                parse_dep_group_data(data).map_err(|_| OutPointError::InvalidDepGroup(outpoint))?;

            for sub_out_point in sub_out_points.into_iter() {
                resolved_cell_deps.push(resolve_cell(&sub_out_point, false)?);
            }
            resolved_dep_groups.push(dep_group);
        } else {
            resolved_cell_deps.push(resolve_cell(&cell_dep.out_point(), false)?);
        }
    }

    Ok(ResolvedTransaction {
        transaction,
        resolved_inputs,
        resolved_cell_deps,
        resolved_dep_groups,
    })
}

#[cfg(target_arch = "wasm32")]
pub async fn generate_temporary_db(
    swc: &StorageWithChainData,
    transaction: TransactionView,
) -> Result<TmpDB, OutPointError> {
    let mut cells = HashMap::new();
    let mut headers = HashMap::new();
    let mut outpoints = HashSet::new();

    for hash in transaction.header_deps_iter() {
        if let Some(header) = swc
            .storage
            .get_header(&hash)
            .await
            .or_else(|| swc.peers.find_header_in_proved_state(&hash))
        {
            headers.insert(hash, header);
        }
    }
    for out_point in transaction.input_pts_iter() {
        if !outpoints.insert(out_point.clone()) {
            continue;
        }
        let meta = swc.storage().cell(&out_point, true).await;

        cells.insert(out_point, meta);
    }

    for cell_dep in transaction.cell_deps_iter() {
        if cell_dep.dep_type() == DepType::DepGroup.into() {
            let outpoint = cell_dep.out_point();
            if let CellStatus::Live(dep_group) = swc.storage().cell(&outpoint, true).await {
                let data = dep_group
                    .mem_cell_data
                    .as_ref()
                    .expect("Load cell meta must with data");
                let sub_out_points = parse_dep_group_data(data)
                    .map_err(|_| OutPointError::InvalidDepGroup(outpoint.clone()))?;

                for sub_out_point in sub_out_points.into_iter() {
                    let meta = swc.storage().cell(&sub_out_point, true).await;
                    cells.insert(sub_out_point, meta);
                }
                cells.insert(outpoint, CellStatus::Live(dep_group));
            }
        } else {
            cells.insert(
                cell_dep.out_point(),
                swc.storage().cell(&cell_dep.out_point(), false).await,
            );
        }
    }
    Ok(TmpDB { cells, headers })
}

fn parse_dep_group_data(slice: &[u8]) -> Result<OutPointVec, String> {
    if slice.is_empty() {
        Err("data is empty".to_owned())
    } else {
        match OutPointVec::from_slice(slice) {
            Ok(v) => {
                if v.is_empty() {
                    Err("dep group is empty".to_owned())
                } else {
                    Ok(v)
                }
            }
            Err(err) => Err(err.to_string()),
        }
    }
}