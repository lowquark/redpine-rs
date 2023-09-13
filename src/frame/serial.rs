use super::*;

const FRAME_HEADER_SIZE: usize = 1;
const FRAME_CRC_SIZE: usize = 4;

const HANDSHAKE_SYN_SIZE: usize = 21;
const HANDSHAKE_SYN_ACK_SIZE: usize = 33;
const HANDSHAKE_ACK_SIZE: usize = 20;
const CLOSE_SIZE: usize = 8;
const CLOSE_ACK_SIZE: usize = 4;
const STREAM_DATA_HEADER_SIZE: usize = 4;
const STREAM_ACK_SIZE: usize = 13;

const DATAGRAM_HEADER_SIZE: usize = 1 + 4 + 2;
const DATAGRAM_DATA_SIZE_MAX: usize = u16::max_value() as usize;
const DATAGRAM_UNREL_BIT: u8 = 0x01;
const DATAGRAM_FIRST_BIT: u8 = 0x02;
const DATAGRAM_LAST_BIT: u8 = 0x04;

const FRAME_TYPE_MASK: u8 = 0x0F;
const FRAME_TYPE_HANDSHAKE_SYN: u8 = 0x00;
const FRAME_TYPE_HANDSHAKE_SYN_ACK: u8 = 0x01;
const FRAME_TYPE_HANDSHAKE_ACK: u8 = 0x02;
const FRAME_TYPE_CLOSE: u8 = 0x03;
const FRAME_TYPE_CLOSE_ACK: u8 = 0x04;
const FRAME_TYPE_STREAM_DATA: u8 = 0x05;
const FRAME_TYPE_STREAM_ACK: u8 = 0x06;
const FRAME_TYPE_STREAM_DATA_ACK: u8 = 0x07;
const FRAME_TYPE_STREAM_SYNC: u8 = 0x08;

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
    max_size: usize,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Writer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            ptr: buffer.as_mut_ptr(),
            bytes_written: 0,
            max_size: buffer.len(),
            _lifetime: Default::default(),
        }
    }

    pub fn bytes_remaining(&self) -> usize {
        self.max_size - self.bytes_written
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
        self.ptr = self.ptr.offset(bytes.len() as isize);
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

        let ref mut rd = Reader::new(buffer);

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

        return Some(wr.bytes_written());
    }
}

impl BlockSerial for HandshakeSynFrame {
    const SIZE: usize = HANDSHAKE_SYN_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let protocol_id = rd.read_u8();
        let receive_rate_max = rd.read_u32();
        let packet_size_in_max = rd.read_u32();
        let packet_size_out_max = rd.read_u32();
        let client_mode = rd.read_u32();
        let client_nonce = rd.read_u32();

        Self {
            client_params: ConnectionParams {
                protocol_id,
                receive_rate_max,
                packet_size_in_max,
                packet_size_out_max,
            },
            client_mode,
            client_nonce,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u8(obj.client_params.protocol_id);
        wr.write_u32(obj.client_params.receive_rate_max);
        wr.write_u32(obj.client_params.packet_size_in_max);
        wr.write_u32(obj.client_params.packet_size_out_max);
        wr.write_u32(obj.client_mode);
        wr.write_u32(obj.client_nonce);
    }
}

impl BlockSerial for HandshakeSynAckFrame {
    const SIZE: usize = HANDSHAKE_SYN_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let protocol_id = rd.read_u8();
        let receive_rate_max = rd.read_u32();
        let packet_size_in_max = rd.read_u32();
        let packet_size_out_max = rd.read_u32();
        let client_mode = rd.read_u32();
        let client_nonce = rd.read_u32();
        let server_nonce = rd.read_u32();
        let mac = rd.read_u64();

        Self {
            server_params: ConnectionParams {
                protocol_id,
                receive_rate_max,
                packet_size_in_max,
                packet_size_out_max,
            },
            client_mode,
            client_nonce,
            server_nonce,
            mac,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u8(obj.server_params.protocol_id);
        wr.write_u32(obj.server_params.receive_rate_max);
        wr.write_u32(obj.server_params.packet_size_in_max);
        wr.write_u32(obj.server_params.packet_size_out_max);
        wr.write_u32(obj.client_mode);
        wr.write_u32(obj.client_nonce);
        wr.write_u32(obj.server_nonce);
        wr.write_u64(obj.mac);
    }
}

impl BlockSerial for HandshakeAckFrame {
    const SIZE: usize = HANDSHAKE_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let client_mode = rd.read_u32();
        let client_nonce = rd.read_u32();
        let server_nonce = rd.read_u32();
        let mac = rd.read_u64();

        Self {
            client_mode,
            client_nonce,
            server_nonce,
            mac,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.client_mode);
        wr.write_u32(obj.client_nonce);
        wr.write_u32(obj.server_nonce);
        wr.write_u64(obj.mac);
    }
}

impl BlockSerial for CloseFrame {
    const SIZE: usize = CLOSE_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let mode = rd.read_u32();
        let remote_nonce = rd.read_u32();

        Self { mode, remote_nonce }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.mode);
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

impl BlockSerial for StreamDataHeader {
    const SIZE: usize = STREAM_DATA_HEADER_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let id = rd.read_u32();

        Self { id }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        wr.write_u32(obj.id);
    }
}

impl BlockSerial for StreamAck {
    const SIZE: usize = STREAM_ACK_SIZE;

    unsafe fn read(rd: &mut Reader) -> Self {
        let info = rd.read_u8();

        let data_id = rd.read_u32();

        let unrel_id = if info & 0x01 != 0 {
            Some(rd.read_u32())
        } else {
            None
        };

        let rel_id = if info & 0x02 != 0 {
            Some(rd.read_u32())
        } else {
            None
        };

        Self {
            data_id,
            unrel_id,
            rel_id,
        }
    }

    unsafe fn write(wr: &mut Writer, obj: &Self) {
        let mut info: u8 = 0;
        if obj.unrel_id.is_some() {
            info |= 0x01;
        }
        if obj.rel_id.is_some() {
            info |= 0x02;
        }

        wr.write_u8(info);
        wr.write_u32(obj.data_id);
        wr.write_u32(obj.unrel_id.unwrap_or(0));
        wr.write_u32(obj.rel_id.unwrap_or(0));
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

        return Some((
            Datagram {
                id,
                unrel: info_bits & DATAGRAM_UNREL_BIT != 0,
                first: info_bits & DATAGRAM_FIRST_BIT != 0,
                last: info_bits & DATAGRAM_LAST_BIT != 0,
                data: &src[DATAGRAM_HEADER_SIZE..DATAGRAM_HEADER_SIZE + data_len],
            },
            DATAGRAM_HEADER_SIZE + data_len,
        ));
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

        assert!(obj.data.len() <= u16::max_value() as usize);
        let data_len_u16 = obj.data.len() as u16;

        let mut wr = Writer::new(dst);

        unsafe {
            wr.write_u8(info_bits);
            wr.write_u32(obj.id);
            wr.write_u16(data_len_u16);

            debug_assert_eq!(wr.bytes_written(), DATAGRAM_HEADER_SIZE);

            wr.write_slice(obj.data);
        }

        return Some(DATAGRAM_HEADER_SIZE + obj.data.len());
    }
}

pub struct FrameWriter<'a> {
    buffer: &'a mut [u8],
    write_idx: usize,
}

impl<'a> FrameWriter<'a> {
    pub fn new(buffer: &'a mut [u8], frame_type: FrameType) -> Option<Self> {
        if buffer.len() < FRAME_HEADER_SIZE + FRAME_CRC_SIZE {
            return None;
        }

        let type_bits = match frame_type {
            FrameType::HandshakeSyn => FRAME_TYPE_HANDSHAKE_SYN,
            FrameType::HandshakeSynAck => FRAME_TYPE_HANDSHAKE_SYN_ACK,
            FrameType::HandshakeAck => FRAME_TYPE_HANDSHAKE_ACK,
            FrameType::Close => FRAME_TYPE_CLOSE,
            FrameType::CloseAck => FRAME_TYPE_CLOSE_ACK,
            FrameType::StreamData => FRAME_TYPE_STREAM_DATA,
            FrameType::StreamAck => FRAME_TYPE_STREAM_ACK,
            FrameType::StreamDataAck => FRAME_TYPE_STREAM_DATA_ACK,
            FrameType::StreamSync => FRAME_TYPE_STREAM_SYNC,
        };

        let mut wr = Writer::new(buffer);

        unsafe {
            wr.write_u8(type_bits);
        }

        debug_assert_eq!(wr.bytes_written(), FRAME_HEADER_SIZE);

        Some(Self {
            buffer,
            write_idx: FRAME_HEADER_SIZE,
        })
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

        return false;
    }

    pub fn finalize(self) -> &'a [u8] {
        // TODO: Compute CRC

        let mut wr = Writer::new(&mut self.buffer[self.write_idx..]);

        unsafe {
            wr.write_u32(0x00000000);
        }

        let frame_size = self.write_idx + FRAME_CRC_SIZE;

        &self.buffer[..frame_size]
    }
}

pub struct FrameReader<'a> {
    buffer: &'a [u8],
    read_idx: usize,
}

impl<'a> FrameReader<'a> {
    pub fn new(buffer: &'a [u8]) -> Option<(Self, FrameType)> {
        if buffer.len() < FRAME_HEADER_SIZE + FRAME_CRC_SIZE {
            return None;
        }

        let mut rd = Reader::new(buffer);

        let type_bits = unsafe { rd.read_u8() } & FRAME_TYPE_MASK;

        let frame_type = match type_bits {
            FRAME_TYPE_HANDSHAKE_SYN => FrameType::HandshakeSyn,
            FRAME_TYPE_HANDSHAKE_SYN_ACK => FrameType::HandshakeSynAck,
            FRAME_TYPE_HANDSHAKE_ACK => FrameType::HandshakeAck,
            FRAME_TYPE_CLOSE => FrameType::Close,
            FRAME_TYPE_CLOSE_ACK => FrameType::CloseAck,
            FRAME_TYPE_STREAM_DATA => FrameType::StreamData,
            FRAME_TYPE_STREAM_ACK => FrameType::StreamAck,
            FRAME_TYPE_STREAM_DATA_ACK => FrameType::StreamDataAck,
            FRAME_TYPE_STREAM_SYNC => FrameType::StreamSync,
            _ => return None,
        };

        debug_assert_eq!(rd.bytes_read(), FRAME_HEADER_SIZE);

        // TODO: Validate CRC

        let bytes_rem = buffer.len() - FRAME_HEADER_SIZE - FRAME_CRC_SIZE;

        Some((
            Self {
                buffer,
                read_idx: FRAME_HEADER_SIZE,
            },
            frame_type,
        ))
    }

    pub fn read<A>(&mut self) -> Option<A>
    where
        A: Serial<'a>,
    {
        let begin_idx = self.read_idx;
        let end_idx = self.buffer.len() - FRAME_CRC_SIZE;

        if let Some((obj, size)) = A::read(&self.buffer[begin_idx..end_idx]) {
            self.read_idx += size;
            return Some(obj);
        }

        return None;
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
            let mut writer = serial::FrameWriter::new(&mut buf, FrameType::StreamData).unwrap();

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

            let (mut reader, frame_type) = serial::FrameReader::new(frame).unwrap();

            for i in 0..datagram_count {
                let dg = reader.read::<Datagram>().unwrap();

                assert_eq!(dg.id, datagrams[i].id);
                assert_eq!(dg.unrel, datagrams[i].unrel);
                assert_eq!(dg.first, datagrams[i].first);
                assert_eq!(dg.last, datagrams[i].last);
                // assert_eq!(dg.data, datagrams[i].data.as_ref());
            }

            assert!(reader.read::<Datagram>().is_none());
        }
    }
}