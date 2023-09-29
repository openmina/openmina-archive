use std::collections::BTreeMap;

use mina_p2p_messages::{rpc::GetStagedLedgerAuxAndPendingCoinbasesAtHashV2Response as Aux, v2};
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
    Database, BaseLedger,
};
use mina_signer::CompressedPubKey;

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

pub fn again(
    accounts: Vec<v2::MinaBaseAccountBinableArgStableV2>,
    aux: Aux,
    mut blocks: impl Iterator<Item = Vec<v2::MinaBlockBlockStableV2>>,
) {
    let root_blocks = blocks.next().unwrap();
    let root_block = root_blocks.first().unwrap();
    let root_block_hash = root_block.hash();

    let last_protocol_state = root_block.header.protocol_state.clone();

    let mut snarked_ledger = Mask::new_root(Database::create(35));
    for account in accounts {
        let account = mina_tree::Account::from(&account);
        let account_id = account.id();
        snarked_ledger
            .get_or_create_account(account_id, account)
            .unwrap();
    }

    let _ = snarked_ledger.merkle_root();

    let info = aux;

    let expected_hash = last_protocol_state
        .body
        .blockchain_state
        .staged_ledger_hash
        .clone();
    let storage = Storage::new(snarked_ledger, info, expected_hash);

    log::info!("obtain staged ledger");

    let mut ancestors = Some((root_block_hash.clone(), (last_protocol_state, storage)))
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    for x in blocks {
        let hashes = x
            .into_iter()
            .map(|b| (b.hash(), b))
            .collect::<BTreeMap<_, _>>();
        let mut new_ancestors = BTreeMap::new();
        for (prev_hash, (prev_protocol_state, storage)) in ancestors {
            for (hash, block) in hashes.clone() {
                if block
                    .header
                    .protocol_state
                    .previous_state_hash
                    .ne(&prev_hash)
                {
                    continue;
                }
                let mut storage = storage.clone();
                let height = block
                    .header
                    .protocol_state
                    .body
                    .consensus_state
                    .blockchain_length
                    .as_u32();
                log::info!("will apply: {} prev: {prev_hash}, this: {hash}", height);
                storage.apply_block(&block, &prev_protocol_state);
                new_ancestors.insert(hash, (block.header.protocol_state.clone(), storage));
            }
        }
        ancestors = new_ancestors;
    }
}

#[derive(Clone)]
pub struct Storage {
    staged_ledger: StagedLedger,
}

impl Storage {
    pub fn new(
        snarked_ledger: Mask,
        info: Aux,
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
