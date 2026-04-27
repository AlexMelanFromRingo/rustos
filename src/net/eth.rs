//! Ethernet II frame helpers.
//!
//! No NIC driver yet, so this only exercises the on-wire format: build a
//! frame around a payload, parse a frame back into its parts.  Used by the
//! ARP layer and (eventually) by any L2 NIC driver.

use alloc::vec::Vec;

pub const ETH_HEADER_LEN: usize = 14;

pub mod ethertype {
    pub const IPV4: u16 = 0x0800;
    pub const ARP:  u16 = 0x0806;
    pub const IPV6: u16 = 0x86DD;
}

#[derive(Debug, Clone, Copy)]
pub struct EthHeader {
    pub dst: [u8; 6],
    pub src: [u8; 6],
    pub ethertype: u16,
}

impl EthHeader {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < ETH_HEADER_LEN { return None; }
        let mut dst = [0u8; 6]; dst.copy_from_slice(&buf[0..6]);
        let mut src = [0u8; 6]; src.copy_from_slice(&buf[6..12]);
        let et = u16::from_be_bytes([buf[12], buf[13]]);
        Some(EthHeader { dst, src, ethertype: et })
    }

    pub fn build(&self) -> [u8; ETH_HEADER_LEN] {
        let mut out = [0u8; ETH_HEADER_LEN];
        out[0..6].copy_from_slice(&self.dst);
        out[6..12].copy_from_slice(&self.src);
        out[12..14].copy_from_slice(&self.ethertype.to_be_bytes());
        out
    }
}

/// Wrap a payload in an Ethernet II frame.
pub fn build_frame(dst: [u8; 6], src: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ETH_HEADER_LEN + payload.len());
    let h = EthHeader { dst, src, ethertype };
    out.extend_from_slice(&h.build());
    out.extend_from_slice(payload);
    out
}

/// Format a MAC address as `aa:bb:cc:dd:ee:ff`.
pub fn fmt_mac(mac: &[u8; 6]) -> alloc::string::String {
    alloc::format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
}

pub const BROADCAST_MAC: [u8; 6] = [0xFF; 6];
