use std::sync::Arc;

use mina_p2p_messages::v2;

use warp::{
    Filter, Rejection, Reply,
    reply::{WithStatus, Json, self},
    http::StatusCode,
};

use tokio::{signal, sync::mpsc};

use super::db::{Db, BlockId};

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

    let version =
        warp::path!("version")
            .and(warp::get())
            .map(move || -> reply::WithStatus<Json> {
                reply::with_status(reply::json(&env!("GIT_HASH")), StatusCode::OK)
            });

    let root = warp::path!("root").and(warp::get()).map({
        let db = db.clone();
        move || -> reply::WithStatus<Json> {
            match db.root() {
                None => reply::with_status(reply::json(&()), StatusCode::OK),
                Some(Ok(root)) => match db.block(BlockId::Latest).next() {
                    None => reply::with_status(reply::json(&()), StatusCode::OK),
                    Some(Ok((head, _))) => {
                        reply::with_status(reply::json(&(root, head)), StatusCode::OK)
                    }
                    Some(Err(err)) => reply::with_status(
                        reply::json(&err.to_string()),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                },
                Some(Err(err)) => reply::with_status(
                    reply::json(&err.to_string()),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            }
        }
    });

    let blocks = warp::path!("blocks" / u32).and(warp::get()).map({
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

    let test = warp::path!("test" / u32)
        .and(warp::post())
        .map(move |limit| -> WithStatus<Json> {
            // TODO: run the test
            let _ = limit;
            reply::with_status(reply::json(&()), StatusCode::OK)
        });

    let append = warp::path!("append" / String).and(warp::get()).map({
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

    version
        .or(root)
        .or(blocks)
        .or(test)
        .or(append)
        .with(with::header("Content-Type", "application/json"))
        .with(cors_filter)
}
