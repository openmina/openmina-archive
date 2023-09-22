use std::{path::PathBuf, env};

use libp2p::{
    Multiaddr,
    identity::ed25519::{SecretKey, Keypair},
};
use structopt::StructOpt;

#[derive(StructOpt)]
struct Args {
    #[structopt(long, default_value = "target/default")]
    path: PathBuf,
    #[structopt(
        long,
        default_value = "3c41383994b87449625df91769dff7b507825c064287d30fada9286f3f1cb15e"
    )]
    chain_id: String,
    #[structopt(long)]
    listen: Vec<Multiaddr>,
    #[structopt(long)]
    peer: Vec<Multiaddr>,
}

fn main() {
    env_logger::init();

    let Args {
        path,
        chain_id,
        listen,
        peer,
    } = Args::from_args();

    let sk = env::var("OPENMINA_P2P_SEC_KEY")
        .map(|key| {
            let mut bytes = bs58::decode(key).with_check(Some(0x80)).into_vec().unwrap();
            SecretKey::from_bytes(&mut bytes[1..]).unwrap()
        })
        .unwrap_or_else(|_| {
            let mut bytes = rand::random::<[u8; 32]>();
            let sk_str = bs58::encode(&bytes).with_check_version(0x80).into_string();
            log::info!("{sk_str}");
            let sk = SecretKey::from_bytes(&mut bytes).unwrap();
            sk
        });

    let local_key: libp2p::identity::Keypair = Keypair::from(sk).into();
    log::info!("{}", local_key.public().to_peer_id());

    let swarm = {
        use mina_p2p_messages::rpc::{
            GetBestTipV2, GetAncestryV2, GetStagedLedgerAuxAndPendingCoinbasesAtHashV2,
            AnswerSyncLedgerQueryV2, GetTransitionChainV2, GetTransitionChainProofV1ForV2,
        };
        use libp2p_rpc_behaviour::BehaviourBuilder;

        let behaviour = BehaviourBuilder::default()
            .register_method::<GetBestTipV2>()
            .register_method::<GetAncestryV2>()
            .register_method::<GetStagedLedgerAuxAndPendingCoinbasesAtHashV2>()
            .register_method::<AnswerSyncLedgerQueryV2>()
            .register_method::<GetTransitionChainV2>()
            .register_method::<GetTransitionChainProofV1ForV2>()
            .build();
        mina_transport::swarm(local_key, chain_id.as_bytes(), listen, peer, behaviour)
    };

    // TODO: communicate peers, keep rocksdb updated
    let _ = path;
    let _ = swarm;
}
