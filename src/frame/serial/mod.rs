mod crc;

use super::*;

pub const PROTOCOL_ID: u16 = 0;

pub const FRAME_HEADER_SIZE: usize = 1;
pub const FRAME_CRC_SIZE: usize = 4;
pub const FRAME_OVERHEAD_SIZE: usize = FRAME_HEADER_SIZE + FRAME_CRC_SIZE;

pub const HANDSHAKE_ALPHA_SIZE: usize = 6;
pub const HANDSHAKE_ALPHA_ACK_SIZE: usize = 28;
pub const HANDSHAKE_BETA_SIZE: usize = 28;
pub const HANDSHAKE_BETA_ACK_SIZE: usize = 5;
pub const CLOSE_SIZE: usize = 4;
pub const CLOSE_ACK_SIZE: usize = 4;
pub const STREAM_SEGMENT_HEADER_SIZE: usize = 5;
pub const STREAM_ACK_SIZE: usize = 9;
pub const STREAM_SYNC_SIZE: usize = 4;

const DATAGRAM_HEADER_SIZE: usize = 1 + 4 + 2;
const DATAGRAM_DATA_SIZE_MAX: usize = u16::max_value() as usize;
const DATAGRAM_UNREL_BIT: u8 = 0x01;
const DATAGRAM_FIRST_BIT: u8 = 0x02;
const DATAGRAM_LAST_BIT: u8 = 0x04;

const FRAME_TYPE_MASK: u8 = 0x0F;
const FRAME_TYPE_HANDSHAKE_ALPHA: u8 = 0x00;
const FRAME_TYPE_HANDSHAKE_ALPHA_ACK: u8 = 0x01;
const FRAME_TYPE_HANDSHAKE_BETA: u8 = 0x02;
const FRAME_TYPE_HANDSHAKE_BETA_ACK: u8 = 0x03;
const FRAME_TYPE_CLOSE: u8 = 0x04;
const FRAME_TYPE_CLOSE_ACK: u8 = 0x05;
const FRAME_TYPE_STREAM_SEGMENT: u8 = 0x06;
const FRAME_TYPE_STREAM_ACK: u8 = 0x07;
const FRAME_TYPE_STREAM_SEGMENT_ACK: u8 = 0x08;
const FRAME_TYPE_STREAM_SYNC: u8 = 0x09;

const HANDSHAKE_ALPHA_FRAME_SIZE_MAX: usize = 1472;

pub struct Reader<'a> {
    ptr: *const u8,
    bytes_read: usize,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Reader<'a> {
    pub fn new(buffer: &'a [u8]) -> Self {
        Self {
            ptr: buffer.as_ptr(),
            bytes_read: 0,
            _lifetime: Default::default(),
        }
    }

    pub fn bytes_read(&self) -> usize {
        self.bytes_read
    }

    pub unsafe fn read_u8(&mut self) -> u8 {
        let value = *self.ptr;
        self.ptr = self.ptr.offset(1);
        self.bytes_read += 1;
        value
    }

    pub unsafe fn read_u16(&mut self) -> u16 {
        let mut value = 0;
        value |= (*self.ptr.offset(0) as u16) << 8;
        value |= *self.ptr.offset(1) as u16;
        self.ptr = self.ptr.offset(2);
        self.bytes_read += 2;
        value
    }

    pub unsafe fn read_u32(&mut self) -> u32 {
        let mut value = 0;
        value |= (*self.ptr.offset(0) as u32) << 24;
        value |= (*self.ptr.offset(1) as u32) << 16;
        value |= (*self.ptr.offset(2) as u32) << 8;
        value |= *self.ptr.offset(3) as u32;
        self.ptr = self.ptr.offset(4);
        self.bytes_read += 4;
        value
    }

    pub unsafe fn read_u64(&mut self) -> u64 {
        let mut value = 0;
        value |= (*self.ptr.offset(0) as u64) << 56;
        value |= (*self.ptr.offset(1) as u64) << 48;
        value |= (*self.ptr.offset(2) as u64) << 40;
        value |= (*self.ptr.offset(3) as u64) << 32;
        value |= (*self.ptr.offset(4) as u64) << 24;
        value |= (*self.ptr.offset(5) as u64) << 16;
        value |= (*self.ptr.offset(6) as u64) << 8;
        value |= *self.ptr.offset(7) as u64;
        self.ptr = self.ptr.offset(8);
        self.bytes_read += 8;
        value
    }
}

pub struct Writer<'a> {
    ptr: *mut u8,
    bytes_written: usize,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Writer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            ptr: buffer.as_mut_ptr(),
            bytes_written: 0,
            _lifetime: Default::default(),
        }
    }

    pub fn bytes_written(&self) -> usize {
        self.bytes_written
    }

    pub unsafe fn write_u8(&mut self, value: u8) {
        *self.ptr = value;
        self.ptr = self.ptr.offset(1);
        self.bytes_written += 1;
    }

    pub unsafe fn write_u16(&mut self, value: u16) {
        *self.ptr.offset(0) = (value >> 8) as u8;
        *self.ptr.offset(1) = (value) as u8;
        self.ptr = self.ptr.offset(2);
        self.bytes_written += 2;
    }

    pub unsafe fn write_u32(&mut self, value: u32) {
        *self.ptr.offset(0) = (value >> 24) as u8;
        *self.ptr.offset(1) = (value >> 16) as u8;
        *self.ptr.offset(2) = (value >> 8) as u8;
        *self.ptr.offset(3) = (value) as u8;
        self.ptr = self.ptr.offset(4);
        self.bytes_written += 4;
    }

    pub unsafe fn write_u64(&mut self, value: u64) {
        *self.ptr.offset(0) = (value >> 56) as u8;
        *self.ptr.offset(1) = (value >> 48) as u8;
        *self.ptr.offset(2) = (value >> 40) as u8;
        *self.ptr.offset(3) = (value >> 32) as u8;
        *self.ptr.offset(4) = (value >> 24) as u8;
        *self.ptr.offset(5) = (value >> 16) as u8;
        *self.ptr.offset(6) = (value >> 8) as u8;
        *self.ptr.offset(7) = (value) as u8;
        self.ptr = self.ptr.offset(8);
        self.bytes_written += 8;
    }

    pub unsafe fn write_slice(&mut self, bytes: &[u8]) {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr, bytes.len());
        self.ptr = self.ptr.add(bytes.len());
        self.bytes_written += bytes.len();
    }
}

pub trait BlockSerial {
    const SIZE: usize;

    unsafe fn read(rd: &mut Reader) -> Self
    where
        Self: Sized;

    unsafe fn write(wr: &mut Writer, obj: &Self);
}

pub trait Serial<'a> {
    fn read(buffer: &'a [u8]) -> Option<(Self, usize)>
    where
        Self: Sized;

    fn write(buffer: &mut [u8], obj: &Self) -> Option<usize>;
}

impl<'a, T> Serial<'a> for T
where
    T: BlockSerial,
{
    fn read(buffer: &'a [u8]) -> Option<(Self, usize)> {
        if buffer.len() < T::SIZE {
            return None;
        }

        let rd = &mut Reader::new(buffer);

        let obj = unsafe { T::read(rd) };

        debug_assert_eq!(rd.bytes_read(), T::SIZE);

        Some((obj, rd.bytes_read()))
    }

    fn write(buffer: &mut [u8], obj: &Self) -> Option<usize> {
        if buffer.len() < T::SIZE {
            return None;
        }

        let mut wr = Writer::new(buffer);

        unsafe {
            T::write(&mut wr, obj);
        }

        debug_assert_eq!(wr.bytes_written(), T::SIZE);

        Some(wr.bytes_written())
    }
}

impl BlockSerial for HandshakeAlphaFrame {
    const SIZE: usize = HANDSHAKE_ALPHA_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let protocol_id = rd.read_u16();
        let client_nonce = rd.read_u32();

        Self {
            protocol_id,
            client_nonce,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u16(obj.protocol_id);
        wr.write_u32(obj.client_nonce);
    }
}

impl BlockSerial for HandshakeAlphaAckFrame {
    const SIZE: usize = HANDSHAKE_ALPHA_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let packet_size_in_max = rd.read_u32();
        let packet_size_out_max = rd.read_u32();
        let client_nonce = rd.read_u32();
        let server_nonce = rd.read_u32();
        let server_timestamp = rd.read_u32();
        let server_mac = rd.read_u64();

        Self {
            server_params: ConnectionParams {
                packet_size_in_max,
                packet_size_out_max,
            },
            client_nonce,
            server_nonce,
            server_timestamp,
            server_mac,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.server_params.packet_size_in_max);
        wr.write_u32(obj.server_params.packet_size_out_max);
        wr.write_u32(obj.client_nonce);
        wr.write_u32(obj.server_nonce);
        wr.write_u32(obj.server_timestamp);
        wr.write_u64(obj.server_mac);
    }
}

impl BlockSerial for HandshakeBetaFrame {
    const SIZE: usize = HANDSHAKE_BETA_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let packet_size_in_max = rd.read_u32();
        let packet_size_out_max = rd.read_u32();
        let client_nonce = rd.read_u32();
        let server_nonce = rd.read_u32();
        let server_timestamp = rd.read_u32();
        let server_mac = rd.read_u64();

        Self {
            client_params: ConnectionParams {
                packet_size_in_max,
                packet_size_out_max,
            },
            client_nonce,
            server_nonce,
            server_timestamp,
            server_mac,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.client_params.packet_size_in_max);
        wr.write_u32(obj.client_params.packet_size_out_max);
        wr.write_u32(obj.client_nonce);
        wr.write_u32(obj.server_nonce);
        wr.write_u32(obj.server_timestamp);
        wr.write_u64(obj.server_mac);
    }
}

impl BlockSerial for HandshakeBetaAckFrame {
    const SIZE: usize = HANDSHAKE_BETA_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let status = rd.read_u8();
        let client_nonce = rd.read_u32();

        let error = match status {
            0x00 => None,
            0x01 => Some(HandshakeErrorKind::Capacity),
            0x02 | _ => Some(HandshakeErrorKind::Parameter),
        };

        Self {
            error,
            client_nonce,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        let status = match obj.error {
            None => 0x00,
            Some(HandshakeErrorKind::Capacity) => 0x01,
            Some(HandshakeErrorKind::Parameter) => 0x02,
        };

        wr.write_u8(status);
        wr.write_u32(obj.client_nonce);
    }
}

impl BlockSerial for CloseFrame {
    const SIZE: usize = CLOSE_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let remote_nonce = rd.read_u32();

        Self { remote_nonce }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.remote_nonce);
    }
}

impl BlockSerial for CloseAckFrame {
    const SIZE: usize = CLOSE_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let remote_nonce = rd.read_u32();

        Self { remote_nonce }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.remote_nonce);
    }
}

impl BlockSerial for StreamSegmentHeader {
    const SIZE: usize = STREAM_SEGMENT_HEADER_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let info = rd.read_u8();
        let id = rd.read_u32();

        Self {
            id,
            nonce: info & 0x01 != 0,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        let mut info: u8 = 0;
        if obj.nonce {
            info |= 0x01;
        }

        wr.write_u8(info);
        wr.write_u32(obj.id);
    }
}

impl BlockSerial for StreamAck {
    const SIZE: usize = STREAM_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let info = rd.read_u8();
        let segment_id = rd.read_u32();
        let rel_fragment_id = rd.read_u32();

        Self {
            segment_id,
            segment_history: info & 0x1F,
            segment_checksum: info & 0x20 != 0,
            rel_fragment_id: if info & 0x80 != 0 {
                Some(rel_fragment_id)
            } else {
                None
            },
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        let mut info: u8 = 0;
        if obj.rel_fragment_id.is_some() {
            info |= 0x80;
        }
        if obj.segment_checksum {
            info |= 0x20;
        }
        info |= obj.segment_history & 0x1F;

        wr.write_u8(info);
        wr.write_u32(obj.segment_id);
        wr.write_u32(obj.rel_fragment_id.unwrap_or(0));
    }
}

impl BlockSerial for StreamSync {
    const SIZE: usize = STREAM_SYNC_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let next_segment_id = rd.read_u32();

        Self { next_segment_id }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.next_segment_id);
    }
}

impl<'a> Serial<'a> for Datagram<'a> {
    fn read(src: &'a [u8]) -> Option<(Self, usize)> {
        if src.len() < DATAGRAM_HEADER_SIZE {
            return None;
        }

        let mut rd = Reader::new(src);

        let info_bits = unsafe { rd.read_u8() };
        let id = unsafe { rd.read_u32() };
        let data_len = unsafe { rd.read_u16() } as usize;

        debug_assert_eq!(rd.bytes_read(), DATAGRAM_HEADER_SIZE);

        if src.len() < DATAGRAM_HEADER_SIZE + data_len {
            return None;
        }

        Some((
            Datagram {
                id,
                unrel: info_bits & DATAGRAM_UNREL_BIT != 0,
                first: info_bits & DATAGRAM_FIRST_BIT != 0,
                last: info_bits & DATAGRAM_LAST_BIT != 0,
                data: &src[DATAGRAM_HEADER_SIZE..DATAGRAM_HEADER_SIZE + data_len],
            },
            DATAGRAM_HEADER_SIZE + data_len,
        ))
    }

    fn write(dst: &mut [u8], obj: &Self) -> Option<usize> {
        if dst.len() < DATAGRAM_HEADER_SIZE + obj.data.len() {
            return None;
        }

        let mut info_bits = 0;
        if obj.unrel {
            info_bits |= DATAGRAM_UNREL_BIT;
        }
        if obj.first {
            info_bits |= DATAGRAM_FIRST_BIT;
        }
        if obj.last {
            info_bits |= DATAGRAM_LAST_BIT;
        }

        assert!(obj.data.len() <= DATAGRAM_DATA_SIZE_MAX);
        let data_len_u16 = obj.data.len() as u16;

        let mut wr = Writer::new(dst);

        unsafe {
            wr.write_u8(info_bits);
            wr.write_u32(obj.id);
            wr.write_u16(data_len_u16);

            debug_assert_eq!(wr.bytes_written(), DATAGRAM_HEADER_SIZE);

            wr.write_slice(obj.data);
        }

        Some(DATAGRAM_HEADER_SIZE + obj.data.len())
    }
}

pub struct FrameWriter<'a> {
    buffer: &'a mut [u8],
    write_idx: usize,
}

impl<'a> FrameWriter<'a> {
    pub fn new(buffer: &'a mut [u8], frame_type: FrameType) -> Self {
        debug_assert!(buffer.len() >= FRAME_OVERHEAD_SIZE);

        let type_bits = match frame_type {
            FrameType::HandshakeAlpha => FRAME_TYPE_HANDSHAKE_ALPHA,
            FrameType::HandshakeAlphaAck => FRAME_TYPE_HANDSHAKE_ALPHA_ACK,
            FrameType::HandshakeBeta => FRAME_TYPE_HANDSHAKE_BETA,
            FrameType::HandshakeBetaAck => FRAME_TYPE_HANDSHAKE_BETA_ACK,
            FrameType::Close => FRAME_TYPE_CLOSE,
            FrameType::CloseAck => FRAME_TYPE_CLOSE_ACK,
            FrameType::StreamSegment => FRAME_TYPE_STREAM_SEGMENT,
            FrameType::StreamAck => FRAME_TYPE_STREAM_ACK,
            FrameType::StreamSegmentAck => FRAME_TYPE_STREAM_SEGMENT_ACK,
            FrameType::StreamSync => FRAME_TYPE_STREAM_SYNC,
        };

        let mut wr = Writer::new(buffer);

        unsafe {
            wr.write_u8(type_bits);
        }

        debug_assert_eq!(wr.bytes_written(), FRAME_HEADER_SIZE);

        Self {
            buffer,
            write_idx: FRAME_HEADER_SIZE,
        }
    }

    pub fn write<'b, A>(&mut self, obj: &A) -> bool
    where
        A: Serial<'b>,
    {
        let begin_idx = self.write_idx;
        let end_idx = self.buffer.len() - FRAME_CRC_SIZE;

        if let Some(size) = A::write(&mut self.buffer[begin_idx..end_idx], obj) {
            self.write_idx += size;

            return true;
        }

        false
    }

    pub fn finalize(self) -> &'a [u8] {
        let payload = &self.buffer[..self.write_idx];

        let frame_crc = crc::compute(payload);
        let frame_crc_bytes = &frame_crc.to_le_bytes();

        let frame_size = self.write_idx + crc::SIZE;

        self.buffer[self.write_idx..frame_size].copy_from_slice(frame_crc_bytes);

        &self.buffer[..frame_size]
    }
}

pub struct PayloadReader<'a> {
    buffer: &'a [u8],
    read_idx: usize,
}

impl<'a> PayloadReader<'a> {
    pub fn new(buffer: &'a [u8]) -> Self {
        Self {
            buffer,
            read_idx: 0,
        }
    }

    pub fn read<A>(&mut self) -> Option<A>
    where
        A: Serial<'a>,
    {
        let begin_idx = self.read_idx;
        let end_idx = self.buffer.len();

        if let Some((obj, size)) = A::read(&self.buffer[begin_idx..end_idx]) {
            self.read_idx += size;
            return Some(obj);
        }

        None
    }

    pub fn remaining_bytes(&self) -> &[u8] {
        let begin_idx = self.read_idx;
        let end_idx = self.buffer.len();

        &self.buffer[begin_idx..end_idx]
    }
}

pub fn verify_minimum_size(frame_bytes: &[u8]) -> bool {
    frame_bytes.len() >= FRAME_OVERHEAD_SIZE
}

pub fn verify_handshake_alpha_size_and_crc(frame_bytes: &[u8], frame_size_max: usize) -> bool {
    if frame_bytes.len() == frame_size_max.min(HANDSHAKE_ALPHA_FRAME_SIZE_MAX) {
        return verify_crc(&frame_bytes[..HANDSHAKE_ALPHA_SIZE + FRAME_OVERHEAD_SIZE]);
    }

    false
}

pub fn verify_handshake_beta_size_and_crc(frame_bytes: &[u8]) -> bool {
    if frame_bytes.len() == HANDSHAKE_BETA_SIZE + FRAME_OVERHEAD_SIZE {
        return verify_crc(frame_bytes);
    }

    false
}

pub fn read_type(frame_bytes: &[u8]) -> Option<FrameType> {
    debug_assert!(verify_minimum_size(frame_bytes));

    let frame_type = match frame_bytes[0] & FRAME_TYPE_MASK {
        FRAME_TYPE_HANDSHAKE_ALPHA => FrameType::HandshakeAlpha,
        FRAME_TYPE_HANDSHAKE_ALPHA_ACK => FrameType::HandshakeAlphaAck,
        FRAME_TYPE_HANDSHAKE_BETA => FrameType::HandshakeBeta,
        FRAME_TYPE_HANDSHAKE_BETA_ACK => FrameType::HandshakeBetaAck,
        FRAME_TYPE_CLOSE => FrameType::Close,
        FRAME_TYPE_CLOSE_ACK => FrameType::CloseAck,
        FRAME_TYPE_STREAM_SEGMENT => FrameType::StreamSegment,
        FRAME_TYPE_STREAM_ACK => FrameType::StreamAck,
        FRAME_TYPE_STREAM_SEGMENT_ACK => FrameType::StreamSegmentAck,
        FRAME_TYPE_STREAM_SYNC => FrameType::StreamSync,
        _ => return None,
    };

    Some(frame_type)
}

pub fn verify_crc(buffer: &[u8]) -> bool {
    debug_assert!(buffer.len() >= crc::SIZE);

    let data = &buffer[..buffer.len() - crc::SIZE];

    let crc_bytes = buffer[buffer.len() - crc::SIZE..].try_into().unwrap();
    let crc = u32::from_le_bytes(crc_bytes);

    crc::compute(data) == crc
}

pub fn payload(frame_bytes: &[u8]) -> &[u8] {
    debug_assert!(verify_minimum_size(frame_bytes));

    let payload_begin = FRAME_HEADER_SIZE;
    let payload_end = frame_bytes.len() - FRAME_CRC_SIZE;

    &frame_bytes[payload_begin..payload_end]
}

pub trait SimpleFrame {
    const FRAME_TYPE: FrameType;
    const FRAME_SIZE: usize;
    const PAYLOAD_SIZE: usize;
}

impl SimpleFrame for HandshakeAlphaFrame {
    const FRAME_TYPE: FrameType = FrameType::HandshakeAlpha;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_ALPHA_SIZE;
}

impl SimpleFrame for HandshakeAlphaAckFrame {
    const FRAME_TYPE: FrameType = FrameType::HandshakeAlphaAck;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_ALPHA_ACK_SIZE;
}

impl SimpleFrame for HandshakeBetaFrame {
    const FRAME_TYPE: FrameType = FrameType::HandshakeBeta;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_BETA_SIZE;
}

impl SimpleFrame for HandshakeBetaAckFrame {
    const FRAME_TYPE: FrameType = FrameType::HandshakeBetaAck;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_BETA_ACK_SIZE;
}

impl SimpleFrame for CloseFrame {
    const FRAME_TYPE: FrameType = FrameType::Close;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_BETA_SIZE;
}

impl SimpleFrame for CloseAckFrame {
    const FRAME_TYPE: FrameType = FrameType::CloseAck;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = HANDSHAKE_BETA_ACK_SIZE;
}

impl SimpleFrame for StreamSync {
    const FRAME_TYPE: FrameType = FrameType::StreamSync;
    const FRAME_SIZE: usize = Self::PAYLOAD_SIZE + FRAME_OVERHEAD_SIZE;
    const PAYLOAD_SIZE: usize = STREAM_SYNC_SIZE;
}

pub trait SimpleFrameWrite {
    fn write<'a>(&self, dst: &'a mut [u8]) -> &'a [u8];
    fn write_boxed(&self) -> Box<[u8]>;
}

impl<T> SimpleFrameWrite for T
where
    T: SimpleFrame + BlockSerial,
{
    fn write<'a>(&self, dst: &'a mut [u8]) -> &'a [u8] {
        debug_assert!(dst.len() >= Self::FRAME_SIZE);
        let mut wr = FrameWriter::new(dst, Self::FRAME_TYPE);
        wr.write(self);
        wr.finalize()
    }

    fn write_boxed(&self) -> Box<[u8]> {
        let mut buffer = vec![0; Self::FRAME_SIZE].into_boxed_slice();
        let mut wr = FrameWriter::new(&mut buffer, Self::FRAME_TYPE);
        wr.write(self);
        wr.finalize();
        buffer
    }
}

pub fn write_handshake_alpha(obj: &HandshakeAlphaFrame, frame_size_max: usize) -> Box<[u8]> {
    let frame_size = frame_size_max.min(HANDSHAKE_ALPHA_FRAME_SIZE_MAX);
    let mut buffer = vec![0; frame_size].into_boxed_slice();
    let mut wr = FrameWriter::new(&mut buffer, FrameType::HandshakeAlpha);
    wr.write(obj);
    wr.finalize();
    buffer
}

pub trait SimplePayloadRead {
    fn read(src: &[u8]) -> Option<Self>
    where
        Self: Sized;
}

impl<T> SimplePayloadRead for T
where
    T: SimpleFrame + BlockSerial,
{
    fn read(src: &[u8]) -> Option<Self> {
        PayloadReader::new(src).read::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_bytes(size: usize) -> Box<[u8]> {
        (0..size)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>()
            .into_boxed_slice()
    }

    #[test]
    fn stream_data() {
        for datagram_count in 0..8 {
            struct DatagramEntry {
                id: u32,
                unrel: bool,
                first: bool,
                last: bool,
                data: Box<[u8]>,
            }

            let mut datagrams = Vec::new();

            let mut buf = [0; 32 * 1024];
            let mut writer = serial::FrameWriter::new(&mut buf, FrameType::StreamSegment);

            for i in 0..datagram_count {
                let size = if i == 0 {
                    0
                } else {
                    rand::random::<usize>() % 2048
                };

                let dg_entry = DatagramEntry {
                    id: rand::random::<u32>(),
                    unrel: rand::random::<bool>(),
                    first: rand::random::<bool>(),
                    last: rand::random::<bool>(),
                    data: random_bytes(size),
                };

                writer.write(&Datagram {
                    id: dg_entry.id,
                    unrel: dg_entry.unrel,
                    first: dg_entry.first,
                    last: dg_entry.last,
                    data: &dg_entry.data,
                });

                datagrams.push(dg_entry);
            }

            let frame = writer.finalize();

            assert!(verify_minimum_size(frame));
            assert!(verify_crc(frame));
            assert_eq!(read_type(frame), Some(FrameType::StreamSegment));

            let mut reader = serial::PayloadReader::new(payload(frame));

            for i in 0..datagram_count {
                let dg = reader.read::<Datagram>().unwrap();

                assert_eq!(dg.id, datagrams[i].id);
                assert_eq!(dg.unrel, datagrams[i].unrel);
                assert_eq!(dg.first, datagrams[i].first);
                assert_eq!(dg.last, datagrams[i].last);
                assert_eq!(dg.data, datagrams[i].data.as_ref());
            }

            assert!(reader.read::<Datagram>().is_none());
        }
    }
}
