use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};

use mina_p2p_messages::{rpc::GetStagedLedgerAuxAndPendingCoinbasesAtHashV2Response, v2};
use mina_tree::{
    mask::Mask,
    staged_ledger::{staged_ledger::StagedLedger, diff::Diff},
    verifier::Verifier,
    scan_state::{
        scan_state::ConstraintConstants,
        currency::{Amount, Fee},
        transaction_logic::{local_state::LocalState, protocol_state},
        self,
    },
};
use mina_signer::CompressedPubKey;

use crate::db::BlockHeader;

use super::{
    snarked_ledger::SnarkedLedger,
    db::{Db, DbError, BlockId},
};

pub const CONSTRAINT_CONSTANTS: ConstraintConstants = ConstraintConstants {
    sub_windows_per_window: 11,
    ledger_depth: 35,
    work_delay: 2,
    block_window_duration_ms: 180000,
    transaction_capacity_log_2: 7,
    pending_coinbase_depth: 5,
    coinbase_amount: Amount::from_u64(720000000000),
    supercharged_coinbase_factor: 2,
    account_creation_fee: Fee::from_u64(1000000000),
    fork: None,
};

pub fn again(db: Arc<Db>) -> Result<(), DbError> {
    let root = match db.root() {
        Some(v) => v?,
        None => {
            log::info!("no root in database, run again later");
            return Ok(());
        }
    };

    let mut blocks = db.block(BlockId::Forward(root));

    let (_, root_block_hash) = blocks.next().unwrap()?;

    let root_block_hash = root_block_hash.first().unwrap();
    let root_block = db.block_full(&root_block_hash)?;

    let last_protocol_state = root_block.header.protocol_state;

    let snarked_ledger_hash = last_protocol_state
        .body
        .blockchain_state
        .ledger_proof_statement
        .target
        .first_pass_ledger
        .clone();
    let snarked_ledger =
        SnarkedLedger::load_accounts(&snarked_ledger_hash, db.ledger(&snarked_ledger_hash)?);

    let info = db.aux(&root_block_hash)?;

    let expected_hash = last_protocol_state
        .body
        .blockchain_state
        .staged_ledger_hash
        .clone();
    let storage = Storage::new(snarked_ledger.inner, info, expected_hash);

    log::info!("obtain staged ledger");

    let mut ancestors = Some((root_block_hash.clone(), (last_protocol_state, storage)))
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for x in blocks {
        let (_, hashes) = x?;
        let hashes = hashes.into_iter().collect::<BTreeSet<_>>();
        let mut new_ancestors = BTreeMap::new();
        for (prev_hash, (prev_protocol_state, storage)) in ancestors {
            for hash in hashes.clone() {
                let Ok(block) = db.block_full(&hash) else {
                    continue;
                };
                if block
                    .header
                    .protocol_state
                    .previous_state_hash
                    .ne(&prev_hash)
                {
                    continue;
                }
                let mut storage = storage.clone();
                log::info!(
                    "will apply: {} prev: {prev_hash}, this: {hash}",
                    block.height()
                );
                storage.apply_block(&block, &prev_protocol_state);
                new_ancestors.insert(hash, (block.header.protocol_state.clone(), storage));
            }
        }
        ancestors = new_ancestors;
    }

    Ok(())
}

#[derive(Clone)]
pub struct Storage {
    staged_ledger: StagedLedger,
}

impl Storage {
    pub fn new(
        snarked_ledger: Mask,
        info: GetStagedLedgerAuxAndPendingCoinbasesAtHashV2Response,
        expected_hash: v2::MinaBaseStagedLedgerHashStableV1,
    ) -> Self {
        // TODO: fix for genesis block
        let (scan_state, expected_ledger_hash, pending_coinbase, states) = info.unwrap();

        let states = states
            .into_iter()
            .map(|state| (state.hash().to_fp().unwrap(), state))
            .collect::<BTreeMap<_, _>>();

        let mut staged_ledger = StagedLedger::of_scan_state_pending_coinbases_and_snarked_ledger(
            (),
            &CONSTRAINT_CONSTANTS,
            Verifier,
            (&scan_state).into(),
            snarked_ledger.clone(),
            LocalState::empty(),
            expected_ledger_hash.clone().into(),
            (&pending_coinbase).into(),
            |key| states.get(&key).cloned().unwrap(),
        )
        .unwrap();

        let expected_hash_str = serde_json::to_string(&expected_hash).unwrap();
        log::info!("expected staged ledger hash: {expected_hash_str}");

        let actual_hash = v2::MinaBaseStagedLedgerHashStableV1::from(&staged_ledger.hash());
        let actual_hash_str = serde_json::to_string(&actual_hash).unwrap();
        log::info!("actual staged ledger hash {actual_hash_str}");

        assert_eq!(expected_hash, actual_hash);

        Storage { staged_ledger }
    }

    pub fn apply_block(
        &mut self,
        block: &v2::MinaBlockBlockStableV2,
        prev_protocol_state: &v2::MinaStateProtocolStateValueStableV2,
    ) {
        let previous_state_hash = block.header.protocol_state.previous_state_hash.clone();
        let _previous_state_hash = v2::StateHash::from(v2::DataHashLibStateHashStableV1(
            prev_protocol_state.hash().inner().0.clone(),
        ));
        assert_eq!(previous_state_hash, _previous_state_hash);

        let global_slot = block
            .header
            .protocol_state
            .body
            .consensus_state
            .global_slot_since_genesis
            .clone();

        let prev_state_view = protocol_state::protocol_state_view(prev_protocol_state);

        let protocol_state = &block.header.protocol_state;
        let consensus_state = &protocol_state.body.consensus_state;
        let coinbase_receiver: CompressedPubKey = (&consensus_state.coinbase_receiver).into();
        let _supercharge_coinbase = consensus_state.supercharge_coinbase;

        // FIXME: Using `supercharge_coinbase` (from block) above does not work
        let supercharge_coinbase = false;

        let diff: Diff = (&block.body.staged_ledger_diff).into();

        let result = self
            .staged_ledger
            .apply(
                None,
                &CONSTRAINT_CONSTANTS,
                (&global_slot).into(),
                diff,
                (),
                &Verifier,
                &prev_state_view,
                scan_state::protocol_state::hashes(prev_protocol_state),
                coinbase_receiver,
                supercharge_coinbase,
            )
            .unwrap();
        let hash = v2::MinaBaseStagedLedgerHashStableV1::from(&result.hash_after_applying);
        let hash_str = serde_json::to_string(&hash).unwrap();
        log::info!("new staged ledger hash {hash_str}");
        let expected_hash_str = serde_json::to_string(
            &block
                .header
                .protocol_state
                .body
                .blockchain_state
                .staged_ledger_hash,
        )
        .unwrap();
        log::info!("expected staged ledger hash {expected_hash_str}");
        assert_eq!(hash_str, expected_hash_str);
    }
}
