//! Root CA + on-the-fly leaf certificates for TLS interception.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::{
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig,
};
use time::{Duration, OffsetDateTime};

/// The signing material, swapped as a unit when the CA is regenerated.
struct Root {
    cert: Certificate,
    key: KeyPair,
    /// PEM exactly as stored on disk (what the user installs in their trust store).
    pem: String,
    /// The same certificate in DER, which phones and Windows prefer.
    der: Vec<u8>,
}

/// Upper bound on forged leaf certs kept in memory. A long browsing session touches
/// hundreds to thousands of distinct hosts (every CDN/analytics/third-party domain), and
/// without a cap the cache grows for the whole session. 512 covers any realistic working
/// set; a re-visited evicted host just re-forges (cheap), so eviction is behaviour-safe.
const CERT_CACHE_MAX: usize = 512;

/// Host -> TLS config cache with a simple size bound (FIFO eviction of the oldest host).
#[derive(Default)]
struct CertCache {
    map: HashMap<String, Arc<ServerConfig>>,
    order: VecDeque<String>,
}

impl CertCache {
    fn get(&self, host: &str) -> Option<Arc<ServerConfig>> {
        self.map.get(host).cloned()
    }

    fn insert(&mut self, host: String, cfg: Arc<ServerConfig>) {
        // Only track order for genuinely new hosts; a concurrent re-forge of the same
        // host just overwrites the entry without adding a duplicate order record.
        if self.map.insert(host.clone(), cfg).is_none() {
            self.order.push_back(host);
            while self.order.len() > CERT_CACHE_MAX {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

pub struct CertAuthority {
    root: RwLock<Root>,
    /// Path to ca.pem (shown in Settings).
    pub path: PathBuf,
    key_path: PathBuf,
    cache: Mutex<CertCache>,
}

impl CertAuthority {
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("ca.pem");
        let key_path = dir.join("ca.key");

        let (cert, key, pem) = if cert_path.exists() && key_path.exists() {
            let pem = fs::read_to_string(&cert_path)?;
            let key = KeyPair::from_pem(&fs::read_to_string(&key_path)?)
                .context("failed to parse ca.key")?;
            // Rebuild a signing handle for the stored CA (same key + subject).
            let cert = CertificateParams::from_ca_cert_pem(&pem)
                .context("failed to parse ca.pem")?
                .self_signed(&key)?;
            (cert, key, pem)
        } else {
            write_root(&cert_path, &key_path)?
        };

        Ok(Self {
            root: RwLock::new(Root {
                der: pem_to_der(&pem).context("ca.pem is not a PEM certificate")?,
                cert,
                key,
                pem,
            }),
            path: cert_path,
            key_path,
            cache: Mutex::new(CertCache::default()),
        })
    }

    /// PEM of the current root CA.
    pub fn pem(&self) -> String {
        self.root.read().unwrap().pem.clone()
    }

    /// DER of the current root CA.
    pub fn der(&self) -> Vec<u8> {
        self.root.read().unwrap().der.clone()
    }

    /// Throw away the current CA, generate a fresh one, persist it, and drop
    /// every cached leaf so new connections use the new root. Browsers must
    /// re-trust the new certificate.
    pub fn regenerate(&self) -> Result<()> {
        let (cert, key, pem) = write_root(&self.path, &self.key_path)?;
        let der = pem_to_der(&pem).context("generated ca.pem is not a PEM certificate")?;
        *self.root.write().unwrap() = Root { cert, key, pem, der };
        self.cache.lock().unwrap().clear();
        Ok(())
    }

    /// TLS server config presenting a leaf cert for `host`, signed by this CA.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>> {
        if let Some(cfg) = self.cache.lock().unwrap().get(host) {
            return Ok(cfg);
        }

        let mut params = CertificateParams::new(vec![host.to_string()])?;
        params.distinguished_name.push(DnType::CommonName, host);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        // Keep validity short: Apple platforms reject leaf certs valid > 825 days.
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(365);

        let key = KeyPair::generate()?;
        let leaf = {
            let root = self.root.read().unwrap();
            params.signed_by(&key, &root.cert, &root.key)?
        };
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));

        let mut cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![leaf.der().clone()], key_der)?;
        // Browsers get HTTP/2 when they offer it; see proxy::mitm.
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let cfg = Arc::new(cfg);
        self.cache.lock().unwrap().insert(host.to_string(), cfg.clone());
        Ok(cfg)
    }
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let body: String = pem
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END"))
        .collect();
    Ok(STANDARD.decode(body.trim())?)
}

/// Generate a fresh root CA and write it to disk (key at 0600 on Unix).
fn write_root(cert_path: &Path, key_path: &Path) -> Result<(Certificate, KeyPair, String)> {
    let (cert, key) = generate_root()?;
    let pem = cert.pem();
    fs::write(cert_path, &pem)?;
    fs::write(key_path, key.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok((cert, key, pem))
}

fn generate_root() -> Result<(Certificate, KeyPair)> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, format!("{} Proxy CA", crate::brand::NAME));
    params
        .distinguished_name
        .push(DnType::OrganizationName, crate::brand::NAME);
    // Developer/author identifier, so a trusted root is traceable to its origin.
    params
        .distinguished_name
        .push(DnType::OrganizationalUnitName, crate::brand::DEVELOPER);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(365 * 10);

    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok((cert, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloaded_ca_keeps_the_certificate_on_disk() {
        let dir = std::env::temp_dir().join(format!("intip-ca-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first = CertAuthority::load_or_create(&dir).unwrap();
        let again = CertAuthority::load_or_create(&dir).unwrap();
        assert_eq!(first.pem(), again.pem());
        assert_eq!(first.der(), again.der());
        assert_eq!(first.der()[0], 0x30, "DER starts with a SEQUENCE");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_ca_embeds_the_developer_identifier() {
        let dir = std::env::temp_dir().join(format!("intip-ca-dev-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // The subject OU carries the developer id, which appears verbatim in the DER.
        let der = CertAuthority::load_or_create(&dir).unwrap().der().to_vec();
        let needle = crate::brand::DEVELOPER.as_bytes();
        assert!(der.windows(needle.len()).any(|w| w == needle), "CA should carry the developer identifier {}", crate::brand::DEVELOPER);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn regenerate_replaces_and_persists_the_ca() {
        let dir = std::env::temp_dir().join(format!("intip-ca-regen-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let ca = CertAuthority::load_or_create(&dir).unwrap();
        let old = ca.pem();
        // Warm the leaf cache so we can prove it is cleared.
        ca.server_config("example.com").unwrap();
        ca.regenerate().unwrap();
        let new = ca.pem();
        assert_ne!(old, new, "a fresh CA was generated");
        // Persisted: reloading from disk gives the new CA, not the old one.
        assert_eq!(CertAuthority::load_or_create(&dir).unwrap().pem(), new);
        // Still able to sign after regeneration.
        ca.server_config("example.com").unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
