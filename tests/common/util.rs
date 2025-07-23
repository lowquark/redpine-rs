#[derive(Debug)]
pub struct DataPacket {
    pub id: u16,
    pub unrel: bool,
    pub data: Box<[u8]>,
}

#[derive(Debug)]
pub struct SummaryPacket {
    pub rel_md5: [u8; 16],
    pub unrel_md5: [u8; 16],
    pub rel_log: Box<[bool]>,
    pub unrel_log: Box<[bool]>,
}

#[derive(Debug)]
pub enum Packet {
    Data(DataPacket),
    SummaryReq,
    Summary(SummaryPacket),
}

impl Packet {
    pub fn to_bytes(&self) -> Box<[u8]> {
        let mut v = Vec::new();

        match self {
            Self::Data(data_packet) => {
                v.push(if data_packet.unrel { 0x01 } else { 0x00 });
                v.extend_from_slice(&data_packet.id.to_le_bytes());
                v.extend_from_slice(&data_packet.data);
            }
            Self::SummaryReq => {
                v.push(0x02);
            }
            Self::Summary(summary_packet) => {
                v.push(0x03);

                v.extend_from_slice(&summary_packet.rel_md5);
                v.extend_from_slice(&summary_packet.unrel_md5);

                let len_u16: u16 = summary_packet.rel_log.len().try_into().unwrap();
                v.extend_from_slice(&len_u16.to_le_bytes());
                for &x in summary_packet.rel_log.iter() {
                    v.push(x as u8);
                }

                let len_u16: u16 = summary_packet.unrel_log.len().try_into().unwrap();
                v.extend_from_slice(&len_u16.to_le_bytes());
                for &x in summary_packet.unrel_log.iter() {
                    v.push(x as u8);
                }
            }
        }

        return v.into_boxed_slice();
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut read_idx = 0;

        if bytes[0] == 0x00 || bytes[0] == 0x01 {
            let unrel = bytes[0] == 0x01;
            read_idx += 1;

            let id = u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap());
            read_idx += 2;

            let data = bytes[read_idx..].into();

            Self::Data(DataPacket { unrel, id, data })
        } else if bytes[0] == 0x02 {
            Self::SummaryReq
        } else if bytes[0] == 0x03 {
            read_idx += 1;

            let rel_md5: [u8; 16] = bytes[read_idx..read_idx + 16].try_into().unwrap();
            read_idx += 16;

            let unrel_md5: [u8; 16] = bytes[read_idx..read_idx + 16].try_into().unwrap();
            read_idx += 16;

            let log_len =
                u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap()) as usize;
            read_idx += 2;

            let mut rel_log = Vec::new();
            for &x in bytes[read_idx..read_idx + log_len].iter() {
                rel_log.push(x != 0);
                read_idx += 1;
            }

            let log_len =
                u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap()) as usize;
            read_idx += 2;

            let mut unrel_log = Vec::new();
            for &x in bytes[read_idx..read_idx + log_len].iter() {
                unrel_log.push(x != 0);
                read_idx += 1;
            }

            Self::Summary(SummaryPacket {
                rel_md5,
                unrel_md5,
                rel_log: rel_log.into_boxed_slice(),
                unrel_log: unrel_log.into_boxed_slice(),
            })
        } else {
            panic!("invalid packet")
        }
    }
}
