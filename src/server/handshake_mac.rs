use std::net::SocketAddr;

use siphasher::sip::SipHasher13;

pub struct HandshakeMacHasher {
    key: [u8; 16],
}

impl Default for HandshakeMacHasher {
    fn default() -> Self {
        // This samples the thread-local RNG, which is a CSPRNG (see docs for rand::rngs::StdRng)
        let key = (0..16)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        Self { key }
    }
}

impl HandshakeMacHasher {
    pub fn compute(
        &self,
        sender_addr: &SocketAddr,
        client_nonce: u32,
        server_nonce: u32,
        server_timestamp: u32,
    ) -> u64 {
        use core::hash::Hasher;

        let mut hasher = SipHasher13::new_with_key(&self.key);

        match sender_addr {
            SocketAddr::V4(addr) => {
                hasher.write(&addr.ip().octets());
                hasher.write_u16(addr.port());
            }
            SocketAddr::V6(addr) => {
                hasher.write(&addr.ip().octets());
                hasher.write_u16(addr.port());
            }
        }

        hasher.write_u32(client_nonce);
        hasher.write_u32(server_nonce);
        hasher.write_u32(server_timestamp);

        hasher.finish()
    }
}
