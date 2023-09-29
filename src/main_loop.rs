use std::{sync::Arc, ops::DerefMut};

use libp2p::{
    swarm::{NetworkBehaviour, SwarmEvent, THandlerErr},
    Swarm, gossipsub,
    identity::Keypair,
    futures::{Stream, StreamExt},
};
use tokio::{sync::mpsc, signal};
use vru_cancel::{Canceler, cancelable};

use libp2p_rpc_behaviour::BehaviourBuilder;
use mina_p2p_messages::v2;
use mina_tree::BaseLedger;

use super::{
    client::Client,
    db::{Db, DbError, BlockHeader},
    snarked_ledger::SnarkedLedger,
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
    mut crx: mpsc::UnboundedReceiver<v2::StateHash>,
) -> Result<(), DbError> {
    use mina_p2p_messages::rpc;

    let mut client = Client::new(swarm, db.clone());

    if db.root().is_err() {
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

    loop {
        tokio::select! {
            event = client.swarm.next() => {
                if let Some(event) = event {
                    client.process(event);
                } else {
                    break;
                }
            }
            command = crx.recv() => {
                if let Some(hash) = command {
                    log::info!("fetching {hash}");
                    let block = client.rpc::<rpc::GetTransitionChainV2>(vec![hash.clone().into_inner().0])
                        .await
                        .unwrap()
                        .unwrap()
                        .first()
                        .unwrap()
                        .clone();
                    log::info!("adding {hash}");
                    db.put_block(hash, block).unwrap();
                }
            }
        }
    }

    Ok(())
}

pub async fn run(
    swarm: Swarm<B>,
    db: Arc<Db>,
    crx: mpsc::UnboundedReceiver<v2::StateHash>,
) -> Result<(), DbError> {
    let trigger = Canceler::spawn({
        let db = db.clone();
        move |canceler| {
            tokio::spawn(async move {
                cancelable!(swarm, canceler);
                bootstrap(swarm, db.clone(), crx).await
            })
        }
    });

    signal::ctrl_c().await.expect("failed to wait ctrlc");
    println!(" ... terminating");

    trigger().await.unwrap()
}
