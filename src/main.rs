// TODO:
// * cleanup unwraps
// * handle requests
// * add http from bootstrap from root
// * implement `get_ancestry`

mod db;
mod main_loop;
mod client;
mod snarked_ledger;

use std::{path::PathBuf, env, sync::Arc};

use libp2p::{
    Multiaddr,
    identity::{
        ed25519::{SecretKey, Keypair as EdKeypair},
        Keypair,
    },
};

use structopt::StructOpt;

#[derive(StructOpt)]
struct Args {
    #[structopt(long)]
    path: PathBuf,
    #[structopt(long)]
    chain_id: String,
    #[structopt(long)]
    listen: Vec<Multiaddr>,
    #[structopt(long)]
    peer: Vec<Multiaddr>,
}

#[tokio::main]
async fn main() {
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

    let local_key = Keypair::from(EdKeypair::from(sk));
    log::info!("{}", local_key.public().to_peer_id());

    let swarm = mina_transport::swarm(
        local_key.clone(),
        chain_id.as_bytes(),
        listen,
        peer,
        main_loop::B::new(local_key),
    );

    let db = Arc::new(db::Db::open(path).unwrap());
    if let Err(err) = main_loop::run(swarm, db).await {
        log::error!("fatal: {err}");
    }
}
