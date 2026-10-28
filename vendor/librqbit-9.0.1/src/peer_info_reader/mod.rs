use std::{net::SocketAddr, sync::Arc};

use bencode::from_bytes;
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use librqbit_core::{
    constants::CHUNK_SIZE,
    hash_id::Id20,
    lengths::{ChunkInfo, last_element_size},
    torrent_metainfo::TorrentMetaV1Info,
};
use parking_lot::{Mutex, RwLock};
use peer_binary_protocol::{
    Handshake, Message,
    extended::{
        ExtendedMessage,
        handshake::ExtendedHandshake,
        ut_metadata::{UtMetadata, UtMetadataData},
    },
};
use sha1w::{ISha1, Sha1};
use tokio::sync::mpsc::UnboundedSender;
use tracing::trace;

use crate::{
    peer_connection::{
        PeerConnection, PeerConnectionHandler, PeerConnectionOptions, WriterRequest,
    },
    spawn_utils::BlockingSpawner,
    stream_connect::{ConnectionKind, StreamConnector},
};

pub(crate) async fn read_metainfo_from_peer(
    addr: SocketAddr,
    peer_id: Id20,
    info_hash: Id20,
    peer_connection_options: Option<PeerConnectionOptions>,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
    client_name_and_version: String,
) -> anyhow::Result<TorrentAndInfoBytes> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<
        Result<(TorrentMetaV1Info<ByteBufOwned>, ByteBufOwned), bencode::DeserializeError>,
    >();
    let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel::<WriterRequest>();
    let handler = Handler {
        addr,
        info_hash,
        writer_tx,
        result_tx: Mutex::new(Some(result_tx)),
        locked: RwLock::new(None),
        client_name_and_version,
    };
    let connection = PeerConnection::new(
        addr,
        info_hash,
        peer_id,
        handler,
        peer_connection_options,
        spawner,
        connector,
    );

    let result_reader = result_rx;
    let (_, brx) = tokio::sync::broadcast::channel(1);
    let connection_runner = async move { connection.manage_peer_outgoing(writer_rx, brx).await };

    tokio::select! {
        result = result_reader => Ok(result??),
        whatever = connection_runner => match whatever {
            Ok(_) => anyhow::bail!("connection runner completed first"),
            Err(e) => Err(e.into())
        }
    }
}

#[derive(Default)]
struct HandlerLocked {
    metadata_size: u32,
    total_pieces: usize,
    buffer: Vec<u8>,
    received_pieces: Vec<bool>,
    next_request: usize,
    reject_retries: Vec<u8>,
}

impl HandlerLocked {
    fn new(metadata_size: u32) -> anyhow::Result<Self> {
        if metadata_size > 32 * 1024 * 1024 {
            anyhow::bail!("metadata size {} is too big", metadata_size);
        }
        let buffer = vec![0u8; metadata_size as usize];
        let total_pieces: usize = (metadata_size as u64)
            .div_ceil(CHUNK_SIZE as u64)
            .try_into()?;
        let received_pieces = vec![false; total_pieces];
        Ok(Self {
            metadata_size,
            received_pieces,
            buffer,
            total_pieces,
            next_request: 0,
            reject_retries: vec![0; total_pieces],
        })
    }
    fn piece_size(&self, index: u32) -> usize {
        if index as usize == self.total_pieces - 1 {
            last_element_size(self.metadata_size as u64, CHUNK_SIZE as u64)
                .try_into()
                .unwrap()
        } else {
            CHUNK_SIZE as usize
        }
    }
    fn record_piece(
        &mut self,
        d: &UtMetadataData<ByteBuf>,
        info_hash: &Id20,
    ) -> anyhow::Result<bool> {
        let piece = d.piece();
        if piece as usize >= self.total_pieces {
            anyhow::bail!("wrong index");
        }
        let offset = (piece * CHUNK_SIZE) as usize;
        let size = self.piece_size(piece);
        if d.len() != size {
            anyhow::bail!(
                "expected length of piece {} to be {}, but got {}",
                piece,
                size,
                d.len()
            );
        }
        if self.received_pieces[piece as usize] {
            anyhow::bail!("already received piece {}", piece);
        }
        d.copy_to_slice(&mut self.buffer[offset..offset + d.len()]);
        self.received_pieces[piece as usize] = true;

        if self.received_pieces.iter().all(|p| *p) {
            // check metadata
            let mut hash = Sha1::new();
            hash.update(&self.buffer);
            if hash.finish() != info_hash.0 {
                anyhow::bail!("info checksum invalid");
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub type TorrentAndInfoBytes = (TorrentMetaV1Info<ByteBufOwned>, ByteBufOwned);

struct Handler {
    addr: SocketAddr,
    info_hash: Id20,
    writer_tx: UnboundedSender<WriterRequest>,
    result_tx: Mutex<
        Option<
            tokio::sync::oneshot::Sender<Result<TorrentAndInfoBytes, bencode::DeserializeError>>,
        >,
    >,
    locked: RwLock<Option<HandlerLocked>>,
    client_name_and_version: String,
}

impl Handler {
    fn request_next_metadata_piece(&self) -> anyhow::Result<()> {
        let next = {
            let mut locked = self.locked.write();
            let inner = locked.as_mut().ok_or_else(|| anyhow::anyhow!("metadata state missing"))?;
            if inner.next_request >= inner.total_pieces { return Ok(()); }
            let next = inner.next_request;
            inner.next_request += 1;
            next
        };
        self.writer_tx.send(WriterRequest::Message(Message::Extended(
            ExtendedMessage::UtMetadata(UtMetadata::Request(next.try_into()?)),
        )))?;
        Ok(())
    }
}

impl PeerConnectionHandler for Handler {
    fn should_send_bitfield(&self) -> bool {
        false
    }

    fn serialize_bitfield_message_to_buf(&self, _buf: &mut [u8]) -> anyhow::Result<usize> {
        Ok(0)
    }

    fn on_handshake(&self, handshake: Handshake, _kind: ConnectionKind) -> anyhow::Result<()> {
        if !handshake.supports_extended() {
            anyhow::bail!(
                "this peer does not support extended handshaking, which is a prerequisite to download metadata"
            )
        }
        Ok(())
    }

    async fn on_received_message(&self, msg: Message<'_>) -> anyhow::Result<()> {
        if let Message::Extended(ExtendedMessage::UtMetadata(ref metadata)) = msg {
            match metadata {
                UtMetadata::Data(data) => tracing::debug!(
                    addr = %self.addr,
                    info_hash = %self.info_hash.as_string(),
                    piece = data.piece(),
                    bytes = data.len(),
                    "NANANEAS_META_BLOCK"
                ),
                other => tracing::debug!(
                    addr = %self.addr,
                    info_hash = %self.info_hash.as_string(),
                    response = ?other,
                    "NANANEAS_META_RESPONSE"
                ),
            }
        }

        if let Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Reject(piece))) = &msg {
            let attempt = {
                let mut locked = self.locked.write();
                let inner = locked.as_mut().ok_or_else(|| anyhow::anyhow!("metadata state missing"))?;
                let index = *piece as usize;
                if index >= inner.next_request || index >= inner.total_pieces {
                    anyhow::bail!("reject for an unrequested metadata piece: {}", piece);
                }
                if inner.received_pieces[index] { return Ok(()); }
                if inner.reject_retries[index] >= 3 {
                    anyhow::bail!("metadata piece {} rejected after 3 retries", piece);
                }
                inner.reject_retries[index] += 1;
                inner.reject_retries[index]
            };
            tracing::debug!(addr = %self.addr, piece = *piece, attempt,
                "NANANEAS_META_RETRY");
            tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(attempt))).await;
            self.writer_tx.send(WriterRequest::Message(Message::Extended(
                ExtendedMessage::UtMetadata(UtMetadata::Request(*piece)),
            )))?;
            return Ok(());
        }
        if let Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(utdata))) = msg {
            let piece_ready = self
                .locked
                .write()
                .as_mut()
                .unwrap()
                .record_piece(&utdata, &self.info_hash)?;
            if !piece_ready { self.request_next_metadata_piece()?; }
            if piece_ready {
                tracing::debug!(
                    addr = %self.addr,
                    info_hash = %self.info_hash.as_string(),
                    "NANANEAS_META_COMPLETE"
                );
                let buf = Bytes::from(self.locked.write().take().unwrap().buffer);
                let info = from_bytes::<TorrentMetaV1Info<ByteBuf>>(&buf)
                    .map(|i| {
                        use clone_to_owned::CloneToOwned;
                        i.clone_to_owned(Some(&buf))
                    })
                    .map_err(|e| {
                        trace!("error deserializing TorrentMetaV1Info: {e:#}");
                        e.into_kind()
                    })
                    .map(|i| (i, ByteBufOwned(buf)));

                self.result_tx
                    .lock()
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("oneshot is consumed"))?
                    .send(info)
                    .map_err(|_| {
                        anyhow::anyhow!("torrent info deserialized, but consumer closed")
                    })?;
            }
        }
        Ok(())
    }

    fn on_uploaded_bytes(&self, _bytes: u32) {}

    fn read_chunk(&self, _chunk: &ChunkInfo, _buf: &mut [u8]) -> anyhow::Result<()> {
        anyhow::bail!("the peer is not supposed to be requesting chunks")
    }

    fn on_extended_handshake(
        &self,
        extended_handshake: &ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        let metadata_size = match extended_handshake.metadata_size {
            Some(metadata_size) => metadata_size,
            None => anyhow::bail!("peer does not have metadata_size"),
        };

        if extended_handshake.m.ut_metadata.is_none() {
            anyhow::bail!("peer does not support ut_metadata");
        }

        self.writer_tx
            .send(WriterRequest::Message(Message::Unchoke))?;
        self.writer_tx
            .send(WriterRequest::Message(Message::Interested))?;

        tracing::debug!(
            addr = %self.addr,
            info_hash = %self.info_hash.as_string(),
            bytes = metadata_size,
            "NANANEAS_META_HANDSHAKE"
        );
        let inner = HandlerLocked::new(metadata_size)?;
        let total_pieces = inner.total_pieces;

        self.locked.write().replace(inner);

        for _ in 0..total_pieces.min(8) {
            self.request_next_metadata_piece()?;
        }
        Ok(())
    }

    fn should_transmit_have(&self, _id: librqbit_core::lengths::ValidPieceIndex) -> bool {
        false
    }

    fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }
}
