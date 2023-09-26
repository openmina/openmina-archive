use std::{sync::Arc, borrow::Cow, ops::DerefMut, thread};

use binprot::BinProtRead;
use libp2p::{
    swarm::{NetworkBehaviour, SwarmEvent, THandlerErr},
    Swarm, gossipsub,
    identity::Keypair,
    futures::{Stream, StreamExt},
};
use mina_tree::BaseLedger;
use tokio::{sync::mpsc, signal};
use vru_cancel::{Canceler, cancelable};

use libp2p_rpc_behaviour::BehaviourBuilder;
use mina_p2p_messages::v2;

use super::{
    client::Client,
    db::{Db, DbError, BlockHeader, BlockId},
    snarked_ledger::SnarkedLedger,
    bootstrap,
};

#[derive(NetworkBehaviour)]
pub struct B {
    pub rpc: libp2p_rpc_behaviour::Behaviour,
    gossip: libp2p::gossipsub::Behaviour,
}

impl B {
    pub fn new(local_key: Keypair) -> Self {
        use mina_p2p_messages::rpc::*;

        let gossip = {
            let message_authenticity = gossipsub::MessageAuthenticity::Signed(local_key.clone());
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .max_transmit_size(1024 * 1024 * 32)
                .build()
                .expect("the config must be a valid constant");
            let mut behaviour = gossipsub::Behaviour::<
                gossipsub::IdentityTransform,
                gossipsub::subscription_filter::AllowAllSubscriptionFilter,
            >::new(message_authenticity, gossipsub_config)
            .expect("strict validation mode must be compatible with this `message_authenticity`");
            let topic = gossipsub::IdentTopic::new("coda/consensus-messages/0.0.1");
            behaviour.subscribe(&topic).unwrap();
            behaviour
        };

        let rpc = BehaviourBuilder::default()
            .register_method::<GetBestTipV2>()
            .register_method::<GetAncestryV2>()
            .register_method::<GetStagedLedgerAuxAndPendingCoinbasesAtHashV2>()
            .register_method::<AnswerSyncLedgerQueryV2>()
            .register_method::<GetTransitionChainV2>()
            .register_method::<GetTransitionChainProofV1ForV2>()
            .build();

        B { rpc, gossip }
    }
}

pub async fn bootstrap(
    swarm: impl Unpin
        + Send
        + Stream<Item = SwarmEvent<BEvent, THandlerErr<B>>>
        + DerefMut<Target = Swarm<B>>,
    db: Arc<Db>,
    tx: mpsc::UnboundedSender<SwarmEvent<BEvent, THandlerErr<B>>>,
    head: Option<String>,
) -> Result<(), DbError> {
    use mina_p2p_messages::rpc;

    let mut client = Client::new(swarm, tx);

    if db.root().is_none() {
        let best_tip = client.rpc::<rpc::GetBestTipV2>(()).await.unwrap().unwrap();

        log::info!("best tip {}", best_tip.data.height());

        let hash = best_tip.proof.1.hash();
        let root = best_tip.proof.1.height();

        db.put_block(hash.clone(), best_tip.proof.1.clone())?;

        let ledger_hash = best_tip.proof.1.snarked_ledger_hash();
        log::info!("syncing {ledger_hash}...");

        let mut ledger = SnarkedLedger::empty();
        ledger.sync_new(&mut client, &ledger_hash).await;

        log::info!("sync done {ledger_hash}");

        let mut accounts = vec![];
        ledger.inner.iter(|account| accounts.push(account.into()));
        db.put_ledger(ledger_hash, accounts)?;

        let aux = client
            .rpc::<rpc::GetStagedLedgerAuxAndPendingCoinbasesAtHashV2>(hash.clone().into_inner().0)
            .await
            .unwrap();
        db.put_aux(hash.clone(), aux)?;

        log::info!("aux done {hash}");

        let mut block = best_tip.data;
        let mut head = block.hash();
        while head != hash {
            let prev = block.header.protocol_state.previous_state_hash;
            block = client
                .rpc::<rpc::GetTransitionChainV2>(vec![prev.clone().into_inner().0])
                .await
                .unwrap()
                .unwrap()
                .first()
                .unwrap()
                .clone();
            head = prev;
            db.put_block(head.clone(), block.clone())?;
        }

        db.put_root(root)?;
    }

    // TODO:
    thread::spawn({
        let db = db.clone();
        move || {
            log::info!("test...");
            bootstrap::again(db).unwrap();
        }
    });

    let (_, head_hashes) = db.block(BlockId::Latest).next().unwrap()?;

    if let Some(head) = head {
        let true_head = serde_json::from_str::<v2::StateHash>(&format!("\"{head}\"")).unwrap();
        let mut prev = true_head;
        while !head_hashes.contains(&prev) {
            let blocks = client
                .rpc::<rpc::GetTransitionChainV2>(vec![prev.inner().0.clone()])
                .await
                .unwrap()
                .unwrap();
            let block = blocks[0].clone();
            log::info!("catchup {}", block.height());
            let new = block.header.protocol_state.previous_state_hash.clone();
            db.put_block(prev, block)?;
            prev = new;
        }
    }

    while let Some(event) = client.swarm.next().await {
        client.tx.send(event).unwrap_or_default();
    }

    Ok(())
}

pub async fn run(swarm: Swarm<B>, db: Arc<Db>, head: Option<String>) -> Result<(), DbError> {
    let (tx, mut rx) = mpsc::unbounded_channel();

    let trigger = Canceler::spawn({
        let db = db.clone();
        move |canceler| {
            tokio::spawn(async move {
                cancelable!(swarm, canceler);
                bootstrap(swarm, db.clone(), tx, head).await
            })
        }
    });

    let ctrlc = tokio::spawn(async move {
        signal::ctrl_c().await.expect("failed to wait ctrlc");
        println!(" ... terminating");

        trigger().await.unwrap()
    });

    while let Some(event) = rx.recv().await {
        match event {
            SwarmEvent::Behaviour(BEvent::Gossip(gossipsub::Event::Message {
                message: gossipsub::Message { source, data, .. },
                ..
            })) => {
                if data.len() > 8 && data[8] == 0 {
                    let source = source
                        .as_ref()
                        .map(ToString::to_string)
                        .map(Cow::Owned)
                        .unwrap_or_else(|| "unknown".into());
                    let mut slice = &data[9..];
                    let block = match v2::MinaBlockBlockStableV2::binprot_read(&mut slice) {
                        Ok(v) => v,
                        Err(err) => {
                            log::warn!("recv bad block: {err}");
                            continue;
                        }
                    };
                    let height = block.height();
                    let hash = block.hash();
                    log::info!("block {height} {hash} from {source}");
                    db.put_block(hash, block)?;
                }
            }
            _ => {}
        }
    }

    ctrlc.await.unwrap()
}
