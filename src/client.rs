use std::{ops::DerefMut, borrow::Cow, sync::Arc};

use libp2p::{
    Swarm,
    futures::StreamExt,
    swarm::{SwarmEvent, THandlerErr},
    PeerId,
    futures::Stream,
    gossipsub::Message,
};
use libp2p_rpc_behaviour::{Event, StreamId, Received};

use binprot::BinProtRead;
use mina_p2p_messages::{
    rpc_kernel::{self, RpcMethod, ResponseHeader, ResponsePayload, QueryHeader},
    rpc::GetBestTipV2,
    v2,
};

use thiserror::Error;

use crate::db::{BlockHeader, Db};

use super::main_loop::{B, BEvent};

pub type TSwarm = Swarm<B>;
pub type TSwarmEvent = SwarmEvent<BEvent, THandlerErr<B>>;

pub struct Client<S> {
    pub swarm: S,
    peer: Option<PeerId>,
    stream: Option<StreamId>,
    id: i64,
    db: Arc<Db>,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{0}")]
    Binprot(#[from] binprot::Error),
    #[error("{0:?}")]
    InternalError(rpc_kernel::Error),
    #[error("libp2p stop working")]
    Libp2p,
}

impl<S> Client<S>
where
    S: Unpin + Stream<Item = TSwarmEvent> + DerefMut<Target = TSwarm>,
{
    pub fn new(swarm: S, db: Arc<Db>) -> Self {
        Client {
            swarm,
            peer: None,
            stream: None,
            id: 1,
            db,
        }
    }

    pub async fn rpc<M>(&mut self, query: M::Query) -> Result<M::Response, ClientError>
    where
        M: RpcMethod,
    {
        let mut query = Some(query);
        if let (Some(peer_id), Some(stream_id)) = (self.peer, self.stream) {
            if let Some(query) = query.take() {
                self.swarm
                    .behaviour_mut()
                    .rpc
                    .query::<M>(peer_id, stream_id, self.id, query)?;
                self.id += 1;
            }
        }

        loop {
            match self.swarm.next().await.ok_or(ClientError::Libp2p)? {
                SwarmEvent::Behaviour(BEvent::Rpc((peer_id, Event::ConnectionEstablished))) => {
                    log::info!("new connection {peer_id}");

                    self.peer = Some(peer_id);
                    self.swarm.behaviour_mut().rpc.open(peer_id, 0);
                }
                SwarmEvent::Behaviour(BEvent::Rpc((peer_id, Event::ConnectionClosed))) => {
                    log::info!("connection closed {peer_id}");
                    if self.peer == Some(peer_id) {
                        self.peer = None;
                        // TODO: resend
                    }
                }
                SwarmEvent::Behaviour(BEvent::Rpc((
                    peer_id,
                    Event::Stream {
                        stream_id,
                        received,
                    },
                ))) => match received {
                    Received::HandshakeDone => {
                        log::info!("new stream {peer_id} {stream_id:?}");
                        if self.stream.is_none() {
                            self.stream = Some(stream_id);
                        }

                        if let (Some(peer_id), Some(stream_id)) = (self.peer, self.stream) {
                            if let Some(query) = query.take() {
                                self.swarm
                                    .behaviour_mut()
                                    .rpc
                                    .query::<M>(peer_id, stream_id, self.id, query)?;
                                self.id += 1;
                            }
                        }
                    }
                    Received::Menu(menu) => {
                        log::info!("menu: {menu:?}");
                    }
                    Received::Query {
                        header: QueryHeader { tag, version, id },
                        bytes,
                    } => {
                        if tag.to_string_lossy() == "get_best_tip" && version == 2 {
                            let _ = bytes;
                            self.swarm
                                .behaviour_mut()
                                .rpc
                                .respond::<GetBestTipV2>(peer_id, stream_id, id, Ok(None))
                                .unwrap();
                        } else {
                            log::warn!("unhandled query: {tag} {version}");
                        }
                    }
                    Received::Response {
                        header: ResponseHeader { id },
                        bytes,
                    } => {
                        if id + 1 == self.id {
                            let mut bytes = bytes.as_slice();
                            let response =
                                ResponsePayload::<M::Response>::binprot_read(&mut bytes)?
                                    .0
                                    .map_err(ClientError::InternalError)?
                                    .0;
                            return Ok(response);
                        }
                    }
                },
                event => {
                    self.process(event);
                }
            }
        }
    }

    pub fn process(&mut self, event: TSwarmEvent) {
        if let SwarmEvent::Behaviour(BEvent::Gossip(libp2p::gossipsub::Event::Message {
            propagation_source,
            message: Message { source, data, .. },
            ..
        })) = &event
        {
            self.peer = Some(propagation_source.clone());
            self.stream = Some(StreamId::Outgoing(1));

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
                        return;
                    }
                };
                let height = block.height();
                let hash = block.hash();
                log::info!("block {height} {hash} from {source}");
                self.db.put_block(hash, block).unwrap();
            }
        }
    }
}
