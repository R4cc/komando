//! Host certificates: a server proves it is the host it claims to be with a certificate signed
//! by a Certificate Authority the user trusts, instead of a key compared on first connect.
//!
//! Trusted authorities are the `@cert-authority` lines of a known_hosts file, as OpenSSH reads
//! them: `@cert-authority *.example.com ssh-ed25519 AAAA…`. Komando reads its own known_hosts
//! and the user's `~/.ssh/known_hosts`. Certificates are asked for only from hosts an authority
//! covers, so every other host connects exactly as before.

use std::path::Path;

use russh::keys::ssh_key::certificate::CertType;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Certificate, HashAlg, PublicKey};

/// One `@cert-authority` line: the hosts it vouches for, and its key.
pub struct Authority {
    patterns: String,
    pub key: PublicKey,
}

/// The authorities in the known_hosts file at `path`; none when it can't be read.
pub fn read(path: &Path) -> Vec<Authority> {
    std::fs::read_to_string(path).map(|text| parse(&text)).unwrap_or_default()
}

/// The `@cert-authority` lines of a known_hosts file. Other lines, and lines that don't parse,
/// are skipped.
pub fn parse(text: &str) -> Vec<Authority> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "@cert-authority" {
                return None;
            }
            let patterns = fields.next()?.to_string();
            let _algorithm = fields.next()?;
            let key = russh::keys::parse_public_key_base64(fields.next()?).ok()?;
            Some(Authority { patterns, key })
        })
        .collect()
}

/// The keys of the `@revoked` lines of a known_hosts file (`@revoked * ssh-ed25519 AAAA…`),
/// as OpenSSH reads them: such a key is never accepted. Komando refuses it for every host,
/// whatever hosts the line names, as a host key, as a host certificate's key and as the
/// authority that signed one. Lines that don't parse are skipped.
pub fn parse_revoked(text: &str) -> Vec<PublicKey> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next()? != "@revoked" {
                return None;
            }
            let _patterns = fields.next()?;
            let _algorithm = fields.next()?;
            russh::keys::parse_public_key_base64(fields.next()?).ok()
        })
        .collect()
}

/// The `@revoked` keys in the known_hosts file at `path`; none when it can't be read.
pub fn read_revoked(path: &Path) -> Vec<PublicKey> {
    std::fs::read_to_string(path).map(|text| parse_revoked(&text)).unwrap_or_default()
}

/// Whether `key` is among `revoked` (the key itself, whatever comment either has).
pub fn is_revoked(revoked: &[PublicKey], key: &KeyData) -> bool {
    revoked.iter().any(|r| r.key_data() == key)
}

/// The authorities among `all` that vouch for `host` on `port`.
pub fn for_host<'a>(all: &'a [Authority], host: &str, port: u16) -> Vec<&'a PublicKey> {
    let name = if port == 22 { host.to_string() } else { format!("[{host}]:{port}") };
    all.iter().filter(|authority| host_matches(&name, &authority.patterns)).map(|authority| &authority.key).collect()
}

/// OpenSSH's host pattern list: comma-separated, with `*` and `?`, case-insensitive. A `!`
/// pattern that matches rules the host out whatever else matches. Hashed names (`|1|…`) are
/// for single hosts and never appear on authority lines in practice; they don't match.
fn host_matches(name: &str, patterns: &str) -> bool {
    let name = name.to_lowercase();
    let mut matched = false;
    for pattern in patterns.split(',') {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        if crate::text::wildcard(&pattern.to_lowercase(), &name) {
            if negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Whether `cert` proves the server is `host`: a host certificate signed by one of
/// `authorities`, valid at `now` (Unix seconds), naming `host` among its principals (or naming
/// none, which OpenSSH takes as any host). `Ok` holds the authority's fingerprint, `Err` why
/// the certificate isn't accepted, in a sentence's middle ("it expired on …").
pub fn check(cert: &Certificate, host: &str, authorities: &[&PublicKey], now: u64) -> Result<String, String> {
    if cert.cert_type() != CertType::Host {
        return Err("it is a user certificate, not a host certificate".into());
    }
    let signer = cert.signature_key().fingerprint(HashAlg::Sha256);
    let trusted: Vec<_> = authorities.iter().map(|key| key.fingerprint(HashAlg::Sha256)).collect();
    if !trusted.contains(&signer) {
        return Err(format!("it is signed by {signer}, which isn't trusted for this host"));
    }
    if now < cert.valid_after() {
        return Err(format!("it isn't valid until {}", date(cert.valid_after())));
    }
    if now >= cert.valid_before() {
        return Err(format!("it expired on {}", date(cert.valid_before())));
    }
    // The signature, now that the authority and dates are known to be right.
    cert.validate_at(now, &trusted).map_err(|_| "its signature doesn't verify".to_string())?;
    let principals = cert.valid_principals();
    if !principals.is_empty() && !principals.iter().any(|principal| principal.eq_ignore_ascii_case(host)) {
        return Err(format!("it is for {}, not {host}", principals.join(", ")));
    }
    // OpenSSH defines none for host certificates and refuses any: they could restrict use
    // in ways Komando wouldn't honour.
    if !cert.critical_options().is_empty() {
        return Err("it has critical options, which host certificates don't take".into());
    }
    Ok(signer.to_string())
}

fn date(unix: u64) -> String {
    crate::text::local_date_time(unix.min(i64::MAX as u64) as i64).unwrap_or_else(|| "a date past the year 262143".into())
}

#[cfg(test)]
mod tests {
    use super::{check, for_host, host_matches, is_revoked, parse, parse_revoked};
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use russh::keys::{Certificate, PrivateKey};

    fn key(seed: u8) -> PrivateKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
    }

    fn certificate(ca: &PrivateKey, host: &PrivateKey, kind: CertType, principals: &[&str], valid: (u64, u64)) -> Certificate {
        let mut builder = Builder::new([7u8; 16], host.public_key().key_data().clone(), valid.0, valid.1).unwrap();
        builder.cert_type(kind).unwrap();
        if principals.is_empty() {
            builder.all_principals_valid().unwrap();
        }
        for principal in principals {
            builder.valid_principal(*principal).unwrap();
        }
        builder.sign(ca).unwrap()
    }

    #[test]
    fn authority_lines_are_read_and_matched_as_openssh_does() {
        let ca = key(1).public_key().to_openssh().unwrap();
        let text = format!(
            "# comment\nexample.com {ca}\n@cert-authority *.example.com,!bad.example.com {ca}\n\
             @cert-authority [*.lab]:2222 {ca}\n@revoked * {ca}\n@cert-authority broken ssh-ed25519 !!!\n"
        );
        let authorities = parse(&text);
        assert_eq!(authorities.len(), 2);
        assert_eq!(for_host(&authorities, "db.example.com", 22).len(), 1);
        assert_eq!(for_host(&authorities, "DB.Example.COM", 22).len(), 1);
        assert!(for_host(&authorities, "bad.example.com", 22).is_empty());
        assert!(for_host(&authorities, "example.com", 22).is_empty());
        // A port other than 22 is matched as [host]:port.
        assert!(for_host(&authorities, "db.example.com", 2222).is_empty());
        assert_eq!(for_host(&authorities, "pi.lab", 2222).len(), 1);
        assert!(for_host(&authorities, "pi.lab", 22).is_empty());
        assert!(host_matches("a.b", "?.b"));
        assert!(!host_matches("ab.b", "?.b"));
    }

    #[test]
    fn revoked_lines_are_read_whatever_hosts_they_name() {
        let gone = key(1).public_key().to_openssh().unwrap();
        let other = key(2).public_key().to_openssh().unwrap();
        let text = format!("example.com {other}\n@revoked * {gone}\n@cert-authority * {other}\n@revoked broken ssh-ed25519 !!!\n");
        let revoked = parse_revoked(&text);
        assert_eq!(revoked.len(), 1);
        assert!(is_revoked(&revoked, key(1).public_key().key_data()));
        assert!(!is_revoked(&revoked, key(2).public_key().key_data()));
    }

    #[test]
    fn a_host_certificate_is_accepted_only_when_everything_holds() {
        let (ca, other_ca, host) = (key(1), key(2), key(3));
        let trusted = [ca.public_key()];
        let now = 1_800_000_000;
        let window = (now - 60, now + 3600);
        let good = certificate(&ca, &host, CertType::Host, &["db.example.com"], window);
        let fingerprint = ca.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        assert_eq!(check(&good, "db.example.com", &trusted, now), Ok(fingerprint));
        assert!(check(&good, "DB.example.com", &trusted, now).is_ok());

        let refused = |cert: &Certificate, host: &str, at: u64| check(cert, host, &trusted, at).unwrap_err();
        assert!(refused(&good, "web.example.com", now).contains("is for db.example.com, not web.example.com"));
        assert!(refused(&good, "db.example.com", now + 7200).starts_with("it expired on"));
        assert!(refused(&good, "db.example.com", now - 3600).starts_with("it isn't valid until"));
        let stranger = certificate(&other_ca, &host, CertType::Host, &["db.example.com"], window);
        assert!(refused(&stranger, "db.example.com", now).contains("isn't trusted for this host"));
        let user = certificate(&ca, &host, CertType::User, &["db.example.com"], window);
        assert!(refused(&user, "db.example.com", now).contains("user certificate"));
        // No principals: any host, as in OpenSSH.
        let any = certificate(&ca, &host, CertType::Host, &[], window);
        assert!(check(&any, "whatever.example.com", &trusted, now).is_ok());
    }

    #[test]
    fn a_certificate_with_a_forged_signature_is_refused() {
        let (ca, host) = (key(1), key(3));
        let now = 1_800_000_000;
        let cert = certificate(&ca, &host, CertType::Host, &["db"], (now - 60, now + 60));
        // Same fields, but claimed by a different subject key: the CA never signed this.
        let mut bytes = cert.to_bytes().unwrap();
        let subject = host.public_key().key_data().ed25519().unwrap().0;
        let at = bytes.windows(32).position(|window| window == subject).unwrap();
        bytes[at] ^= 1;
        let forged = Certificate::from_bytes(&bytes).unwrap();
        assert_eq!(check(&forged, "db", &[ca.public_key()], now), Err("its signature doesn't verify".into()));
    }
}
