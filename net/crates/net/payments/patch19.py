import pathlib

p = pathlib.Path('src/http_policy.rs')
s = p.read_text(encoding='utf-8')

# `AllowPrivate` should also cover deprecated site-local, since it is the
# LAN-ish policy — otherwise a self-hosted fec0:: node becomes unreachable.
old = '''/// Private-use / LAN ranges an operator might legitimately self-host on.
fn is_private_use(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        // Unique local addresses, fc00::/7.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}'''
new = '''/// Private-use / LAN ranges an operator might legitimately self-host on.
fn is_private_use(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            // fc00::/7 unique local, plus deprecated fec0::/10 site local
            // — both are "an operator's own network", which is what
            // `AllowPrivate` is for. They are refused by the stricter
            // policies via `is_public`.
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfec0
        }
    }
}'''
assert old in s
s = s.replace(old, new, 1)

# Test updates: localhost is no longer a scheme exception.
old = '''    #[test]
    fn https_is_required_except_for_loopback() {
        for ok in [
            "https://facilitator.example.com",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://localhost:8080/base",
            "http://LOCALHOST:8080",
        ] {
            assert!(require_secure_endpoint(ok).is_ok(), "should accept {ok}");
        }
        for bad in [
            "http://facilitator.example.com",
            "http://10.0.0.5:8080",
            "ftp://facilitator.example.com",
            "not-a-url",
        ] {
            assert!(require_secure_endpoint(bad).is_err(), "should reject {bad}");
        }
    }'''
new = '''    #[test]
    fn https_is_required_except_for_loopback_literals() {
        for ok in [
            "https://facilitator.example.com",
            "https://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://[::ffff:127.0.0.1]:8080",
        ] {
            assert!(require_secure_endpoint(ok).is_ok(), "should accept {ok}");
        }
        for bad in [
            "http://facilitator.example.com",
            "http://10.0.0.5:8080",
            "ftp://facilitator.example.com",
            "not-a-url",
        ] {
            assert!(require_secure_endpoint(bad).is_err(), "should reject {bad}");
        }
    }

    /// The cleartext exception is address-level, so the NAME `localhost`
    /// does not get it.
    ///
    /// A name is whatever DNS says it is: a hosts file or a resolver can
    /// point `localhost` at a public address, and then "http to
    /// localhost" is a cleartext request to a remote host — precisely
    /// what the scheme rule exists to prevent. Only a literal loopback
    /// address is self-evidently local.
    #[test]
    fn cleartext_to_the_name_localhost_is_refused() {
        for name in [
            "http://localhost:8080",
            "http://localhost:8080/base",
            "http://LOCALHOST:8080",
            "http://localhost.localdomain:8080",
        ] {
            assert!(
                require_secure_endpoint(name).is_err(),
                "`{name}` is a name, not a loopback literal — it must not get the \\
                 cleartext exception"
            );
        }
        // https to the same name is fine: the guard is about cleartext.
        assert!(require_secure_endpoint("https://localhost:8080").is_ok());
    }'''
assert old in s
s = s.replace(old, new, 1)

# Add fec0:: to the refused set and assert AllowPrivate still admits it.
old = '''            "fc00::1",      // unique local
            "fe80::1",      // link local'''
new = '''            "fc00::1",      // unique local
            "fe80::1",      // link local
            "fec0::1",      // site local (deprecated but still routed)'''
assert old in s
s = s.replace(old, new, 1)

old = '''        assert!(!DestinationPolicy::PublicOrLoopback.admits(private));
        assert!(DestinationPolicy::AllowPrivate.admits(private));'''
new = '''        assert!(!DestinationPolicy::PublicOrLoopback.admits(private));
        assert!(DestinationPolicy::AllowPrivate.admits(private));

        // Deprecated IPv6 site-local is an operator's own network: never
        // public, but reachable under `AllowPrivate` so a self-hosted node
        // there is not collateral damage.
        let site_local: IpAddr = "fec0::1".parse().unwrap();
        assert!(!DestinationPolicy::PublicOnly.admits(site_local));
        assert!(!DestinationPolicy::PublicOrLoopback.admits(site_local));
        assert!(DestinationPolicy::AllowPrivate.admits(site_local));'''
assert old in s
s = s.replace(old, new, 1)

p.write_text(s, encoding='utf-8')
print('http_policy patched')
