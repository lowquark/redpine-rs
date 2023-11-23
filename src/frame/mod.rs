pub mod serial;

#[derive(Debug, Eq, PartialEq)]
pub enum FrameType {
    HandshakeAlpha,
    HandshakeAlphaAck,
    HandshakeBeta,
    HandshakeBetaAck,
    Close,
    CloseAck,
    StreamSegment,
    StreamAck,
    StreamSegmentAck,
    StreamSync,
}

#[derive(Debug)]
pub struct ConnectionParams {
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
pub struct HandshakeAlphaFrame {
    pub protocol_id: u16,

    pub client_nonce: u32,
}

#[derive(Debug)]
pub struct HandshakeAlphaAckFrame {
    pub server_params: ConnectionParams,

    pub client_nonce: u32,
    pub server_nonce: u32,
    pub server_timestamp: u32,
    pub server_mac: u64,
}

#[derive(Debug)]
pub struct HandshakeBetaFrame {
    pub client_params: ConnectionParams,

    pub client_nonce: u32,
    pub server_nonce: u32,
    pub server_timestamp: u32,
    pub server_mac: u64,
}

#[derive(Debug)]
pub struct HandshakeBetaAckFrame {
    pub client_nonce: u32,
}

#[derive(Debug)]
pub struct CloseFrame {
    pub remote_nonce: u32,
}

#[derive(Debug)]
pub struct CloseAckFrame {
    pub remote_nonce: u32,
}

#[derive(Debug)]
pub struct StreamSegmentHeader {
    pub id: u32,
    pub nonce: bool,
}

#[derive(Debug)]
pub struct StreamAck {
    pub segment_id: u32,
    pub segment_history: u8,
    pub segment_checksum: bool,

    pub unrel_id: Option<u32>,
    pub rel_id: Option<u32>,
}

#[derive(Debug)]
pub struct StreamSync {
    pub segment_id: u32,
    pub unrel_id: u32,
    pub rel_id: u32,
}
