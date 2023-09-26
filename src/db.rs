use std::sync::Mutex;
use std::{path::Path, time::Duration};

use binprot::{BinProtWrite, BinProtRead};
use rocksdb::{DBWithThreadMode, SingleThreaded, ColumnFamilyDescriptor};
use thiserror::Error;
use mina_p2p_messages::v2;
use mina_p2p_messages::rpc::GetStagedLedgerAuxAndPendingCoinbasesAtHashV2Response as Aux;

pub struct Db {
    inner: DBWithThreadMode<SingleThreaded>,
    cache: Mutex<DbCache>,
}

#[derive(Default)]
struct DbCache {
    hashes_at_height: Option<(u32, Vec<v2::StateHash>)>,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("db inner {_0}")]
    Inner(#[from] rocksdb::Error),
    #[error("db binprot {_0}")]
    Binprot(#[from] binprot::Error),
    #[error("bad index")]
    BadIndex,
    #[error("ledger not found {_0}")]
    LedgerNotFound(v2::LedgerHash),
    #[error("block not found {_0}")]
    BlockNotFound(v2::StateHash),
    #[error("staged ledger aux info not found {_0}")]
    AuxNotFound(v2::StateHash),
}

pub enum BlockId {
    Earliest,
    Latest,
    PositionForward(u32),
    PositionBackward(u32),
}

pub trait BlockHeader {
    fn height(&self) -> u32;

    fn snarked_ledger_hash(&self) -> v2::LedgerHash;
}

impl BlockHeader for v2::MinaBlockBlockStableV2 {
    fn height(&self) -> u32 {
        self.header
            .protocol_state
            .body
            .consensus_state
            .blockchain_length
            .0
            .as_u32()
    }

    fn snarked_ledger_hash(&self) -> v2::LedgerHash {
        self.header
            .protocol_state
            .body
            .blockchain_state
            .ledger_proof_statement
            .target
            .first_pass_ledger
            .clone()
    }
}

impl Db {
    const TTL: Duration = Duration::from_secs(0);

    pub fn open<P>(path: P) -> Result<Db, DbError>
    where
        P: AsRef<Path>,
    {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cfs = [
            // v2::LedgerHash -> Vec<v2::MinaBaseAccountBinableArgStableV2>
            ColumnFamilyDescriptor::new("ledger", Default::default()),
            // v2::StateHash -> mina_p2p_messages::rpc::GetStagedLedgerAuxAndPendingCoinbasesAtHashV2Response
            ColumnFamilyDescriptor::new("aux", Default::default()),
            // v2::StateHash -> v2::MinaBlockBlockStableV2
            ColumnFamilyDescriptor::new("block", Default::default()),
            // u32 -> Vec<v2::StateHash>
            ColumnFamilyDescriptor::new("block_hash_by_height", Default::default()),
            // v2::StateHash -> u32
            ColumnFamilyDescriptor::new("block_height_by_hash", Default::default()),
            // u32
            ColumnFamilyDescriptor::new("root", Default::default()),
        ];

        let inner = rocksdb::DB::open_cf_descriptors_with_ttl(&opts, path, cfs, Self::TTL)?;

        Ok(Db {
            inner,
            cache: Mutex::new(DbCache::default()),
        })
    }

    pub fn root(&self) -> Option<Result<u32, DbError>> {
        let cf = self.inner.cf_handle("ledger").expect("must exist");

        self.inner
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .next()
            .map(|r| {
                r.map_err(Into::into).and_then(|(key, _)| {
                    key.as_ref()
                        .try_into()
                        .map_err(|_| DbError::BadIndex)
                        .map(u32::from_be_bytes)
                        .map_err(Into::into)
                })
            })
    }

    pub fn block(
        &self,
        id: BlockId,
    ) -> impl Iterator<Item = Result<(u32, Vec<v2::StateHash>), DbError>> + '_ {
        let cf_handle: &rocksdb::ColumnFamily = self
            .inner
            .cf_handle("block_hash_by_height")
            .expect("must exist");
        let pos_bytes;
        let mode = match id {
            BlockId::Earliest => rocksdb::IteratorMode::Start,
            BlockId::Latest => rocksdb::IteratorMode::End,
            BlockId::PositionForward(pos) => {
                pos_bytes = pos.to_be_bytes();
                rocksdb::IteratorMode::From(&pos_bytes, rocksdb::Direction::Forward)
            }
            BlockId::PositionBackward(pos) => {
                pos_bytes = pos.to_be_bytes();
                rocksdb::IteratorMode::From(&pos_bytes, rocksdb::Direction::Reverse)
            }
        };
        self.inner.iterator_cf(cf_handle, mode).map(|x| {
            let (k, v) = x?;
            let mut v = v.as_ref();
            let height = u32::from_be_bytes(k.as_ref().try_into().map_err(|_| DbError::BadIndex)?);
            let hash = Vec::<v2::StateHash>::binprot_read(&mut v)?;

            Ok((height, hash))
        })
    }

    pub fn block_full(&self, hash: &v2::StateHash) -> Result<v2::MinaBlockBlockStableV2, DbError> {
        let mut key = vec![];
        hash.binprot_write(&mut key).unwrap();
        let cf = self.inner.cf_handle("block").expect("must exist");
        let value = self
            .inner
            .get_cf(cf, key)?
            .ok_or_else(|| DbError::BlockNotFound(hash.clone()))?;
        let mut slice = value.as_slice();
        let block = BinProtRead::binprot_read(&mut slice)?;

        Ok(block)
    }

    pub fn ledger(
        &self,
        hash: &v2::LedgerHash,
    ) -> Result<Vec<v2::MinaBaseAccountBinableArgStableV2>, DbError> {
        let mut key = vec![];
        hash.binprot_write(&mut key).unwrap();
        let cf = self.inner.cf_handle("ledger").expect("must exist");
        let value = self
            .inner
            .get_cf(cf, key)?
            .ok_or_else(|| DbError::LedgerNotFound(hash.clone()))?;
        let mut slice = value.as_slice();
        let ledger = BinProtRead::binprot_read(&mut slice)?;

        Ok(ledger)
    }

    pub fn aux(&self, hash: &v2::StateHash) -> Result<Aux, DbError> {
        let mut key = vec![];
        hash.binprot_write(&mut key).unwrap();
        let cf = self.inner.cf_handle("aux").expect("must exist");
        let value = self
            .inner
            .get_cf(cf, key)?
            .ok_or_else(|| DbError::AuxNotFound(hash.clone()))?;
        let mut slice = value.as_slice();
        let aux = BinProtRead::binprot_read(&mut slice)?;

        Ok(aux)
    }

    pub fn put_root(&self, height: u32) -> Result<(), DbError> {
        let cf = self.inner.cf_handle("ledger").expect("must exist");
        self.inner.put_cf(cf, height.to_be_bytes(), [])?;

        Ok(())
    }

    pub fn put_ledger(
        &self,
        hash: v2::LedgerHash,
        ledger: Vec<v2::MinaBaseAccountBinableArgStableV2>,
    ) -> Result<(), DbError> {
        let mut key = vec![];
        hash.binprot_write(&mut key).unwrap();
        let mut value = vec![];
        ledger.binprot_write(&mut value).unwrap();

        let cf = self.inner.cf_handle("ledger").expect("must exist");
        self.inner.put_cf(cf, key, value).map_err(Into::into)
    }

    pub fn put_aux(&self, hash: v2::StateHash, aux: Aux) -> Result<(), DbError> {
        let mut key = vec![];
        hash.binprot_write(&mut key).unwrap();
        let mut value = vec![];
        aux.binprot_write(&mut value).unwrap();

        let cf = self.inner.cf_handle("aux").expect("must exist");
        self.inner.put_cf(cf, key, value).map_err(Into::into)
    }

    pub fn put_block(
        &self,
        hash: v2::StateHash,
        block: v2::MinaBlockBlockStableV2,
    ) -> Result<(), DbError> {
        let height = block.height();
        let mut cache = self.cache.lock().expect("mutex");
        let hashes = match &mut cache.hashes_at_height {
            Some((h, hashes)) if *h == height => {
                hashes.push(hash.clone());
                hashes.clone()
            }
            _ => {
                cache.hashes_at_height = None;
                let mut hashes = self
                    .block(BlockId::PositionForward(height))
                    .next()
                    .transpose()?
                    .map(|(_, hashes)| hashes)
                    .unwrap_or_default();
                hashes.push(hash.clone());
                hashes
            }
        };
        drop(cache);

        let mut key = vec![];
        hashes.binprot_write(&mut key).unwrap();
        let mut value = vec![];
        block.binprot_write(&mut value).unwrap();

        let cf = self
            .inner
            .cf_handle("block_hash_by_height")
            .expect("must exist");
        self.inner.put_cf(cf, height.to_be_bytes(), key.clone())?;
        let cf = self
            .inner
            .cf_handle("block_height_by_hash")
            .expect("must exist");
        self.inner.put_cf(cf, hash.as_ref(), height.to_be_bytes())?;

        let cf = self.inner.cf_handle("block").expect("must exist");
        self.inner.put_cf(cf, key, value).map_err(Into::into)
    }
}