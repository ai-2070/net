import pathlib

# --- C1: remaining non-global IPv6 ranges ---------------------------------
p = pathlib.Path('src/http_policy.rs')
s = p.read_text(encoding='utf-8')
old = '''        || (first == 0x2001 && (s[1] & 0xff00) == 0x0d00) // 2001:db8::/32 doc
        || first == 0x0100) // 100::/64  discard-only'''
new = '''        || (first == 0x2001 && (s[1] & 0xff00) == 0x0d00) // 2001:db8::/32 doc
        || (first == 0x2001 && s[1] == 0x0002)            // 2001:2::/48 benchmarking
        // 64:ff9b::/96 well-known NAT64 and 64:ff9b:1::/48 local-use
        // translation. Both carry an embedded IPv4 destination, so a
        // translating host turns them into a v4 reach — including into
        // the private ranges the v4 rules refuse. Left admitted, they are
        // a way round every guard above.
        || (first == 0x0064 && (s[1] == 0xff9b))
        || first == 0x0100) // 100::/64  discard-only'''
assert old in s, 'v6 ranges'
s = s.replace(old, new, 1)

# --- C2: the outbound door defaults to PublicOnly -------------------------
old = '''        Self::with_destination_policy(
            caller,
            spend,
            registry,
            clock,
            // Default: public unicast plus loopback. This is the one
            // money-path client whose URL may be chosen by a model rather
            // than an operator, so the SSRF-shaped ranges — link-local
            // (including the cloud metadata address), private/LAN,
            // carrier-NAT, reserved — are refused by default. Loopback
            // stays admitted because the local-testing path is documented
            // and `is_payment_safe_url` already allows http to it.
            //
            // A host that passes agent-supplied URLs straight through
            // should tighten this to `PublicOnly`.
            crate::http_policy::DestinationPolicy::PublicOrLoopback,
        )'''
new = '''        Self::with_destination_policy(
            caller,
            spend,
            registry,
            clock,
            // Default: public unicast only.
            //
            // This is the one money-path client whose URL may be chosen
            // by a model rather than an operator, so it gets the
            // strictest policy and local testing opts *in* rather than
            // out. Loopback was admitted here at first, on the reasoning
            // that `is_payment_safe_url` already allows http to it — but
            // that rule is about not putting a bearer authorization on
            // the wire in the clear, which is a different question from
            // what an agent-supplied URL should be able to reach.
            // Loopback is where admin surfaces live.
            //
            // A local or self-hosted x402 server is reached by asking for
            // it: `with_destination_policy(PublicOrLoopback)` or
            // `AllowPrivate`.
            crate::http_policy::DestinationPolicy::PublicOnly,
        )'''
assert old in s.replace('\r\n', '\n') or old in s, 'default policy'
p2 = pathlib.Path('src/flow/http402.rs')
s2 = p2.read_text(encoding='utf-8')
assert old in s2, 'default policy in http402'
s2 = s2.replace(old, new, 1)

# --- C3: a policy refusal on the PAID send is terminal, and releases ------
old = '''            Err(e) => {
                // Transport ambiguity after sending a payment: the
                // reservation stands (fail-closed accounting).
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: e.is_timeout() || e.is_connect(),
                };
            }'''
new = '''            Err(e) => {
                // A destination-policy refusal is the one send failure
                // that provably happened BEFORE anything left: it is
                // raised inside the resolver, so no connection was made
                // and no authorization was transmitted. Releasing the
                // reservation is therefore correct rather than optimistic
                // — and not releasing it would let a policy denial (which
                // is permanent, and which a caller may hit repeatedly on
                // the same URL) eat the day's budget a fetch at a time.
                //
                // Any other send failure stays ambiguous: the payment may
                // have landed, so the reservation stands (fail-closed
                // accounting).
                if crate::http_policy::is_policy_refusal(&e) {
                    self.release(&quote, now_ns).await;
                    return X402HttpOutcome::Denied {
                        policy_reason: format!(
                            "destination policy refused the paid retry ({}): {e}",
                            self.destinations.describe()
                        ),
                    };
                }
                return X402HttpOutcome::Failed {
                    message: e.to_string(),
                    retryable: e.is_timeout() || e.is_connect(),
                };
            }'''
assert old in s2, 'paid send error'
s2 = s2.replace(old, new, 1)
p2.write_text(s2, encoding='utf-8')
p.write_text(s, encoding='utf-8')
print('C1/C2/C3 applied')

# --- C4: tests opt into loopback ------------------------------------------
p = pathlib.Path('tests/http402_outbound.rs')
s = p.read_text(encoding='utf-8')
s = s.replace('''    X402HttpFlow::new(
        caller,
        SpendPolicyEngine::new(dir.path().join("spend.json"), profile),
        registry,
        Arc::new(TestClock),
    )
    .expect("flow")''',
'''    // These servers bind loopback, which the default policy now refuses:
    // an agent-supplied URL should not reach local admin surfaces, so
    // local testing opts in rather than out.
    X402HttpFlow::with_destination_policy(
        caller,
        SpendPolicyEngine::new(dir.path().join("spend.json"), profile),
        registry,
        Arc::new(TestClock),
        net_payments::http_policy::DestinationPolicy::PublicOrLoopback,
    )
    .expect("flow")''', 1)
p.write_text(s, encoding='utf-8')
print('C4: test helper opts into loopback')
