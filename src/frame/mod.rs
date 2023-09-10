pub mod serial;

#[derive(Debug)]
pub enum FrameType {
    HandshakeSyn,
    HandshakeSynAck,
    HandshakeAck,
    Close,
    CloseAck,
    StreamData,
    StreamAck,
    StreamDataAck,
    StreamSync,
}

#[derive(Debug)]
pub struct ConnectionParams {
    pub protocol_id: u8,
    pub receive_rate_max: u32,
    pub packet_size_in_max: u32,
    pub packet_size_out_max: u32,
}

#[derive(Debug)]
pub struct Datagram<'a> {
    pub id: u32,
    pub unrel: bool,
    pub first: bool,
    pub last: bool,
    pub data: &'a [u8],
}

#[derive(Debug)]
pub struct HandshakeSynFrame {
    pub client_params: ConnectionParams,

    pub client_mode: u32,
    pub client_nonce: u32,
}

#[derive(Debug)]
pub struct HandshakeSynAckFrame {
    pub server_params: ConnectionParams,

    pub client_mode: u32,
    pub client_nonce: u32,
    pub server_nonce: u32,
    pub mac: u64,
}

#[derive(Debug)]
pub struct HandshakeAckFrame {
    pub client_mode: u32,
    pub client_nonce: u32,
    pub server_nonce: u32,
    pub mac: u64,
}

#[derive(Debug)]
pub struct CloseFrame {
    pub mode: u32,

    pub remote_nonce: u32,
}

#[derive(Debug)]
pub struct CloseAckFrame {
    pub remote_nonce: u32,
}

#[derive(Debug)]
pub struct StreamDataHeader {
    pub id: u32,
}

#[derive(Debug)]
pub struct StreamAck {
    pub data_id: u32,
    pub unrel_id: Option<u32>,
    pub rel_id: Option<u32>,
}

#[derive(Debug)]
pub struct StreamSync {
    pub data_id: u32,
    pub unrel_id: u32,
    pub rel_id: u32,
}

/// Sent back and forth to exchange data
pub struct StreamFrameHeader {
    /// This frame's ID. Only frames which have datagrams are assigned an ID.
    pub frame_id: Option<u32>,

    /// Next expected frame ID.
    pub ack_frame_id: u32,

    /// Set to instruct the receiver to reset its frame rx window such that this frame ID is
    /// expected next. Set in response to an RTO.
    pub sync_frame_id: Option<u32>,

    /// Next expected unreliable fragment ID.
    pub ack_unrel_id: u32,

    /// Set to instruct the receiver to reset its unreliable rx fragment window such that this frame
    /// ID is expected next. Set in response to an RTO.
    pub sync_unrel_id: Option<u32>,

    /// Next expected reliable fragment ID. Set in response to a stream frame containing any
    /// reliable datagrams.
    pub ack_rel_id: Option<u32>,
}
