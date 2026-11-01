//! sys-vpn, the machine every connection of the PC goes through while a VPN
//! is set (docs/vpn.md): the providers it takes and the WireGuard file it
//! reads, shared by the host's `losos-vpn` and the machine's.

pub mod config;
pub mod provider;

pub use config::Tunnel;
pub use provider::Provider;

/// Where the host keeps the person's choice: `provider` and `tunnel.conf`,
/// root's alone.
pub const STATE_DIR: &str = "/var/lib/losos-vpn";
/// What the host hands sys-vpn, read-only over 9p: the tunnel file again.
pub const SHARE_DIR: &str = "/run/losos-vpn";
pub const TUNNEL_FILE: &str = "tunnel.conf";
pub const PROVIDER_FILE: &str = "provider";
