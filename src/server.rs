use std::sync::Arc;

use mina_p2p_messages::v2;

use warp::{
    Filter, Rejection, Reply,
    reply::{WithStatus, Json, self},
    http::StatusCode,
};

use tokio::{signal, sync::mpsc};

use super::db::{Db, DbError, BlockId};

pub fn spawn(db: Arc<Db>, port: u16, tx: mpsc::UnboundedSender<v2::StateHash>) {
    let (addr, server) =
        warp::serve(routes(db, tx)).bind_with_graceful_shutdown(([0; 4], port), async move {
            signal::ctrl_c().await.unwrap_or_default();
        });
    log::info!("running server on {addr}");
    tokio::spawn(server);
}

fn routes(
    db: Arc<Db>,
    tx: mpsc::UnboundedSender<v2::StateHash>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone + Sync + Send + 'static {
    use warp::reply::with;

    let cors_filter = warp::cors()
        .allow_any_origin()
        .allow_methods(["OPTIONS", "GET", "POST", "DELETE", "PUT", "HEAD"])
        .allow_credentials(true)
        .allow_headers([
            "Accept",
            "Authorization",
            "baggage",
            "Cache-Control",
            "Content-Type",
            "DNT",
            "If-Modified-Since",
            "Keep-Alive",
            "Origin",
            "sentry-trace",
            "User-Agent",
            "X-Requested-With",
            "X-Cache-Hash",
        ])
        .build();

    let get_version =
        warp::path!("version")
            .and(warp::get())
            .map(move || -> reply::WithStatus<Json> {
                reply::with_status(reply::json(&env!("GIT_HASH")), StatusCode::OK)
            });

    let get_root = warp::path!("root").and(warp::get()).map({
        let db = db.clone();
        move || -> reply::WithStatus<Json> {
            match db.root() {
                Ok(root) => match db.block(BlockId::Latest).next() {
                    None => reply::with_status(reply::json(&()), StatusCode::OK),
                    Some(Ok((head, _))) => {
                        reply::with_status(reply::json(&(root, head)), StatusCode::OK)
                    }
                    Some(Err(err)) => reply::with_status(
                        reply::json(&err.to_string()),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                },
                Err(err) => reply::with_status(
                    reply::json(&err.to_string()),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            }
        }
    });

    let get_brief = warp::path!("brief" / u32).and(warp::get()).map({
        let db = db.clone();
        move |root: u32| -> reply::WithStatus<Json> {
            let mut v = vec![];
            for x in db.block(BlockId::Forward(root)) {
                match x {
                    Ok((height, hashes)) => v.push((height, hashes)),
                    Err(err) => log::error!("fetch blocks error: {err}"),
                }
            }

            reply::with_status(reply::json(&v), StatusCode::OK)
        }
    });

    let post_append = warp::path!("append" / String).and(warp::get()).map({
        let tx = tx.clone();
        move |hash| -> WithStatus<Json> {
            if let Ok(hash) = serde_json::from_str(&format!("\"{hash}\"")) {
                tx.send(hash).unwrap_or_default();
                reply::with_status(reply::json(&"enqueued"), StatusCode::OK)
            } else {
                reply::with_status(reply::json(&()), StatusCode::BAD_REQUEST)
            }
        }
    });

    let get_root_ledger = warp::path!("ledger").and(warp::get()).map({
        let db = db.clone();
        move || -> reply::WithStatus<Vec<u8>> {
            use std::collections::BTreeSet;
            use mina_p2p_messages::binprot::BinProtWrite;
            use crate::db::BlockHeader;

            fn get(db: &Db) -> Result<impl BinProtWrite, DbError> {
                let root = db.root()?;
                let (actual_root, hashes) = db
                    .block(BlockId::Forward(root))
                    .next()
                    .ok_or(DbError::RootNotFound)??;
                if actual_root != root || hashes.iter().cloned().collect::<BTreeSet<_>>().len() != 1
                {
                    return Err(DbError::RootNotFound);
                }
                let hash = hashes[0].clone();
                let root_block = db.block_full(&hash)?;
                let root_ledger = root_block.snarked_ledger_hash();
                let ledger = db.ledger(&root_ledger)?;
                let aux = db.aux(&hash)?;

                Ok((ledger, aux))
            }

            match get(&db) {
                Ok(v) => {
                    let mut bytes = vec![];
                    v.binprot_write(&mut bytes).unwrap();
                    reply::with_status(bytes, StatusCode::OK)
                }
                Err(err) => {
                    reply::with_status(err.to_string().as_bytes().to_vec(), StatusCode::BAD_REQUEST)
                }
            }
        }
    });

    let get_transitions = warp::path!("transitions" / u32).and(warp::get()).map({
        let db = db.clone();
        move |height: u32| -> reply::WithStatus<Vec<u8>> {
            use mina_p2p_messages::binprot::BinProtWrite;

            fn get(db: &Db, height: u32) -> Result<Option<impl BinProtWrite>, DbError> {
                let Some((_, hashes)) = db.block(BlockId::Forward(height)).next().transpose()? else {
                    return Ok(None);
                };

                let blocks = hashes.iter().filter_map(|h| db.block_full(h).ok()).collect::<Vec<_>>();
                Ok(Some(blocks))
            }

            match get(&db, height) {
                Err(err) => {
                    reply::with_status(err.to_string().as_bytes().to_vec(), StatusCode::BAD_REQUEST)

                }
                Ok(None) => reply::with_status(vec![], StatusCode::NOT_FOUND),
                Ok(Some(v)) => {
                    let mut bytes = vec![];
                    v.binprot_write(&mut bytes).unwrap();
                    reply::with_status(bytes, StatusCode::OK)
                },
            }
        }
    });

    let binary = get_root_ledger
        .or(get_transitions)
        .with(with::header("Content-Type", "application/octet-stream"));

    let json = get_version
        .or(get_root)
        .or(get_brief)
        .or(post_append)
        .with(with::header("Content-Type", "application/json"));

    json.or(binary).with(cors_filter)
}
