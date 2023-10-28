pub mod serial;

#[derive(Debug, Eq, PartialEq)]
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
