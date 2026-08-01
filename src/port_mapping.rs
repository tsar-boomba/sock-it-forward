use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

/// A single forwarding rule: `<source_port>:<dest_addr>[:no_pp]`
///
/// The destination is a full socket address, so IPv6 must be bracketed:
///   `443:10.0.0.5:8443`          IPv4 dest, PP forwarded
///   `443:10.0.0.5:8443:no_pp`    IPv4 dest, PP stripped
///   `443:[2001:db8::5]:8443`     IPv6 dest
///   `443:[::1]:8443:no_pp`       IPv6 dest, PP stripped
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortMapping {
    pub source_port: u16,
    pub dest_addr: SocketAddr,
    /// Whether to forward the PROXY protocol header to the backend.
    /// `false` when the `no_pp` suffix is present.
    pub proxy_protocol: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMappingParseError(String);

impl fmt::Display for PortMappingParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (expected <source_port>:<dest_addr>[:no_pp], e.g. 443:10.0.0.5:8443 or 443:[::1]:8443:no_pp)",
            self.0
        )
    }
}

impl std::error::Error for PortMappingParseError {}

impl FromStr for PortMapping {
    type Err = PortMappingParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 1. Split off the source port at the first ':'. Everything after may
        //    itself contain ':' (IPv6), so we don't split the rest.
        let (src, rest) = s
            .split_once(':')
            .ok_or_else(|| PortMappingParseError("missing destination address".into()))?;

        let src = src.trim();
        if src.is_empty() {
            return Err(PortMappingParseError("missing source port".into()));
        }
        let source_port = src.parse::<u16>().map_err(|_| {
            PortMappingParseError(format!("invalid source port `{src}` (must be 1-65535)"))
        })?;
        if source_port == 0 {
            return Err(PortMappingParseError("source port must not be 0".into()));
        }

        // 2. Peel a trailing `:no_pp` off the remainder, if present. Checking
        //    the suffix (rather than splitting on ':') keeps IPv6 intact.
        let (dest_str, proxy_protocol) = match rest.rsplit_once(':') {
            Some((head, tail)) if tail.eq_ignore_ascii_case("no_pp") => (head, false),
            _ => (rest, true),
        };

        // 3. The rest must be a complete socket address. SocketAddr's own
        //    parser handles `1.2.3.4:port` and `[v6]:port`, including
        //    rejecting port 0? (No — 0 parses fine, so check it ourselves.)
        let dest_addr = dest_str.trim().parse::<SocketAddr>().map_err(|_| {
            PortMappingParseError(format!(
                "invalid destination address `{dest_str}` (IPv6 must be bracketed, e.g. [::1]:8443)"
            ))
        })?;
        if dest_addr.port() == 0 {
            return Err(PortMappingParseError(
                "destination port must not be 0".into(),
            ));
        }

        Ok(PortMapping {
            source_port,
            dest_addr,
            proxy_protocol,
        })
    }
}

impl fmt::Display for PortMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SocketAddr's Display already brackets IPv6, so this round-trips.
        write!(f, "{}:{}", self.source_port, self.dest_addr)?;
        if !self.proxy_protocol {
            write!(f, ":no_pp")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn parses_ipv4_dest() {
        let m: PortMapping = "443:10.0.0.5:8443".parse().unwrap();
        assert_eq!(m.source_port, 443);
        assert_eq!(m.dest_addr, SocketAddr::from((Ipv4Addr::new(10, 0, 0, 5), 8443)));
        assert!(m.proxy_protocol);
    }

    #[test]
    fn parses_ipv4_dest_no_pp() {
        let m: PortMapping = "443:10.0.0.5:8443:no_pp".parse().unwrap();
        assert!(!m.proxy_protocol);
        let m: PortMapping = "443:10.0.0.5:8443:NO_PP".parse().unwrap();
        assert!(!m.proxy_protocol);
    }

    #[test]
    fn parses_ipv6_dest() {
        let m: PortMapping = "443:[2001:db8::5]:8443".parse().unwrap();
        assert_eq!(
            m.dest_addr,
            SocketAddr::from((Ipv6Addr::from_str("2001:db8::5").unwrap(), 8443))
        );
        assert!(m.proxy_protocol);

        let m: PortMapping = "443:[::1]:8443:no_pp".parse().unwrap();
        assert_eq!(m.dest_addr, SocketAddr::from((Ipv6Addr::LOCALHOST, 8443)));
        assert!(!m.proxy_protocol);
    }

    #[test]
    fn rejects_bad_input() {
        assert!("443".parse::<PortMapping>().is_err()); // no dest
        assert!(":10.0.0.5:8443".parse::<PortMapping>().is_err()); // empty source
        assert!("0:10.0.0.5:8443".parse::<PortMapping>().is_err()); // source port 0
        assert!("443:10.0.0.5:0".parse::<PortMapping>().is_err()); // dest port 0
        assert!("70000:10.0.0.5:80".parse::<PortMapping>().is_err()); // src out of range
        assert!("443:10.0.0.5".parse::<PortMapping>().is_err()); // dest missing port
        assert!("443:2001:db8::5:8443".parse::<PortMapping>().is_err()); // unbracketed v6
        assert!("443:example.com:8443".parse::<PortMapping>().is_err()); // hostname
        assert!("443:10.0.0.5:8443:nope".parse::<PortMapping>().is_err()); // unknown flag
    }

    #[test]
    fn no_pp_not_confused_with_dest() {
        // A dest that merely *ends* in text should fail as an address, not be
        // silently treated as a flag.
        assert!("443:10.0.0.5:no_pp".parse::<PortMapping>().is_err());
    }

    #[test]
    fn display_test() {
        for s in [
            "443:10.0.0.5:8443",
            "80:10.0.0.5:8080:no_pp",
            "443:[2001:db8::5]:8443",
            "80:[::1]:8080:no_pp",
        ] {
            let m: PortMapping = s.parse().unwrap();
            assert_eq!(m.to_string(), s);
        }
    }
}
