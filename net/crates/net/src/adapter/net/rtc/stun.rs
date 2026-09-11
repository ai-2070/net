//! Minimal RFC 5389 STUN: binding request in, binding response out.
//!
//! Enough to answer `stunclient` and a browser's ICE gathering from
//! the RTC socket when `RtcConfig::serve_stun` is set (§6: STUN is
//! served on the dedicated RTC socket, never on the Net socket, so
//! there is no shared-socket demultiplexer to get wrong).
//!
//! Deliberately not a STUN library: no authentication, no
//! FINGERPRINT, no CHANGE-REQUEST, no TURN. Requests that are not
//! plain binding requests are ignored, which for an ICE peer is
//! indistinguishable from a host that does not serve STUN.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// RFC 5389 magic cookie. Its presence at bytes 4..8 is what
/// distinguishes a STUN message from anything else on the socket.
pub const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

/// Binding request message type.
const BINDING_REQUEST: u16 = 0x0001;
/// Binding success response message type.
const BINDING_RESPONSE: u16 = 0x0101;
/// XOR-MAPPED-ADDRESS attribute.
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

const HEADER_LEN: usize = 20;
const TXID_LEN: usize = 12;

/// Is this datagram a STUN binding request?
///
/// Four independent checks, because the RTC socket also carries DTLS,
/// SRTP and ICE traffic: the two leading zero bits, the message type,
/// the magic cookie, and a length that matches the datagram.
pub fn is_binding_request(datagram: &[u8]) -> bool {
    if datagram.len() < HEADER_LEN {
        return false;
    }
    let msg_type = u16::from_be_bytes([datagram[0], datagram[1]]);
    if msg_type != BINDING_REQUEST {
        return false;
    }
    let length = u16::from_be_bytes([datagram[2], datagram[3]]) as usize;
    if HEADER_LEN + length != datagram.len() {
        return false;
    }
    u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]) == STUN_MAGIC_COOKIE
}

/// Build the binding success response for `request`, reporting
/// `source` as the reflexive address.
///
/// Returns `None` if `request` is not a binding request, so the
/// caller cannot accidentally answer arbitrary traffic.
pub fn binding_response(request: &[u8], source: SocketAddr) -> Option<Vec<u8>> {
    if !is_binding_request(request) {
        return None;
    }
    let txid = &request[8..8 + TXID_LEN];

    let attr = xor_mapped_address(source, txid);
    let mut out = Vec::with_capacity(HEADER_LEN + attr.len());
    out.extend_from_slice(&BINDING_RESPONSE.to_be_bytes());
    out.extend_from_slice(&(attr.len() as u16).to_be_bytes());
    out.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    out.extend_from_slice(txid);
    out.extend_from_slice(&attr);
    Some(out)
}

/// Encode one XOR-MAPPED-ADDRESS attribute (type, length, value).
fn xor_mapped_address(addr: SocketAddr, txid: &[u8]) -> Vec<u8> {
    let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
    let xport = addr.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);

    let mut value = Vec::with_capacity(20);
    value.push(0); // reserved
    match addr.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01); // family: IPv4
            value.extend_from_slice(&xport.to_be_bytes());
            let octets = ip.octets();
            for (i, b) in octets.iter().enumerate() {
                value.push(b ^ cookie[i]);
            }
        }
        IpAddr::V6(ip) => {
            value.push(0x02); // family: IPv6
            value.extend_from_slice(&xport.to_be_bytes());
            // XOR against cookie || transaction id, per RFC 5389 §15.2.
            let mut mask = [0u8; 16];
            mask[..4].copy_from_slice(&cookie);
            mask[4..].copy_from_slice(txid);
            for (i, b) in ip.octets().iter().enumerate() {
                value.push(b ^ mask[i]);
            }
        }
    }

    let mut attr = Vec::with_capacity(4 + value.len());
    attr.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    attr.extend_from_slice(&(value.len() as u16).to_be_bytes());
    attr.extend_from_slice(&value);
    attr
}

/// Read the XOR-MAPPED-ADDRESS out of a binding response.
///
/// The decoder half of [`binding_response`]; it is what lets a test
/// assert the bytes rather than the intent, and what a future client
/// path would use.
pub fn parse_xor_mapped_address(response: &[u8]) -> Option<SocketAddr> {
    if response.len() < HEADER_LEN {
        return None;
    }
    if u16::from_be_bytes([response[0], response[1]]) != BINDING_RESPONSE {
        return None;
    }
    if u32::from_be_bytes([response[4], response[5], response[6], response[7]])
        != STUN_MAGIC_COOKIE
    {
        return None;
    }
    let txid = &response[8..8 + TXID_LEN];
    let cookie = STUN_MAGIC_COOKIE.to_be_bytes();

    let mut cursor = HEADER_LEN;
    while cursor + 4 <= response.len() {
        let attr_type = u16::from_be_bytes([response[cursor], response[cursor + 1]]);
        let attr_len = u16::from_be_bytes([response[cursor + 2], response[cursor + 3]]) as usize;
        let value_start = cursor + 4;
        let value_end = value_start.checked_add(attr_len)?;
        if value_end > response.len() {
            return None;
        }
        if attr_type == ATTR_XOR_MAPPED_ADDRESS {
            let value = &response[value_start..value_end];
            if value.len() < 4 {
                return None;
            }
            let port = u16::from_be_bytes([value[2], value[3]]) ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
            return match value[1] {
                0x01 if value.len() >= 8 => {
                    let mut octets = [0u8; 4];
                    for i in 0..4 {
                        octets[i] = value[4 + i] ^ cookie[i];
                    }
                    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(octets)), port))
                }
                0x02 if value.len() >= 20 => {
                    let mut mask = [0u8; 16];
                    mask[..4].copy_from_slice(&cookie);
                    mask[4..].copy_from_slice(txid);
                    let mut octets = [0u8; 16];
                    for i in 0..16 {
                        octets[i] = value[4 + i] ^ mask[i];
                    }
                    Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), port))
                }
                _ => None,
            };
        }
        // Attributes are padded to a 4-byte boundary.
        cursor = value_end + ((4 - (attr_len % 4)) % 4);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built request, byte by byte, so the test does not
    /// depend on this module's own encoder for its input.
    fn hand_built_request(txid: [u8; TXID_LEN]) -> Vec<u8> {
        let mut req = Vec::with_capacity(HEADER_LEN);
        req.extend_from_slice(&[0x00, 0x01]); // binding request
        req.extend_from_slice(&[0x00, 0x00]); // length 0
        req.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        req.extend_from_slice(&txid);
        req
    }

    #[test]
    fn a_hand_built_binding_request_is_recognized_and_answered() {
        let txid = [0xA1u8; TXID_LEN];
        let req = hand_built_request(txid);
        assert!(is_binding_request(&req));

        let source: SocketAddr = "203.0.113.9:51234".parse().expect("addr");
        let resp = binding_response(&req, source).expect("a binding request gets a response");

        assert_eq!(&resp[0..2], &[0x01, 0x01], "binding success response");
        assert_eq!(
            &resp[8..20],
            &txid,
            "the transaction id must be echoed or the client discards the reply"
        );
        assert_eq!(
            parse_xor_mapped_address(&resp),
            Some(source),
            "XOR-MAPPED-ADDRESS must decode back to the observed source"
        );
    }

    /// The XOR is not decoration: the raw address must NOT appear in
    /// the datagram, which is the whole reason RFC 5389 replaced
    /// MAPPED-ADDRESS.
    #[test]
    fn the_address_is_xored_not_copied() {
        let req = hand_built_request([0x7Eu8; TXID_LEN]);
        let source: SocketAddr = "203.0.113.9:51234".parse().expect("addr");
        let resp = binding_response(&req, source).expect("response");
        let raw_ip = [203u8, 0, 113, 9];
        assert!(
            !resp.windows(4).any(|w| w == raw_ip),
            "the plain IPv4 octets must not appear in a XOR-MAPPED-ADDRESS response"
        );
    }

    #[test]
    fn ipv6_round_trips_through_the_transaction_id_mask() {
        let txid = [0x33u8; TXID_LEN];
        let req = hand_built_request(txid);
        let source: SocketAddr = "[2001:db8::1]:9000".parse().expect("addr");
        let resp = binding_response(&req, source).expect("response");
        assert_eq!(parse_xor_mapped_address(&resp), Some(source));
    }

    /// Everything else on the RTC socket — DTLS, SRTP, a truncated
    /// header, a lying length, the wrong cookie — must be ignored.
    #[test]
    fn non_binding_traffic_is_never_answered() {
        let txid = [0x01u8; TXID_LEN];
        let good = hand_built_request(txid);

        let mut wrong_cookie = good.clone();
        wrong_cookie[4] ^= 0xFF;
        let mut wrong_type = good.clone();
        wrong_type[1] = 0x02;
        let mut lying_length = good.clone();
        lying_length[3] = 0x08; // claims 8 bytes of attributes, carries none
        let dtls_client_hello = vec![0x16u8, 0xFE, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00];

        for (label, datagram) in [
            ("wrong cookie", wrong_cookie),
            ("wrong message type", wrong_type),
            ("length disagrees with the datagram", lying_length),
            ("truncated", good[..8].to_vec()),
            ("DTLS", dtls_client_hello),
            ("empty", Vec::new()),
        ] {
            assert!(
                !is_binding_request(&datagram),
                "{label}: must not be classified as a binding request"
            );
            assert!(
                binding_response(&datagram, "127.0.0.1:1".parse().expect("addr")).is_none(),
                "{label}: must not be answered"
            );
        }
    }
}
