use std::{io, collections::BTreeSet};

use mina_p2p_messages::{v2, binprot::BinProtRead};

pub fn run(blocks: impl Iterator<Item = impl io::Read>) {
    let blocks = blocks
        .map(|mut reader| Vec::<v2::MinaBlockBlockStableV2>::binprot_read(&mut reader).unwrap());

    let mut prev = BTreeSet::new();
    for level in blocks {
        if !prev.is_empty() {
            let mut has = false;
            let mut height = 0;
            for block in &level {
                height = block
                    .header
                    .protocol_state
                    .body
                    .consensus_state
                    .blockchain_length
                    .as_u32();
                has |= prev.contains(&block.header.protocol_state.previous_state_hash);
            }
            assert!(has, "breaks at {height}");
            log::info!("ok {height}");
        }
        prev = level.iter().map(|b| b.hash()).collect();
    }
}
