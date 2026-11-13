//! Global branding & identifier — the single place to rebrand the app.
//!
//! Change the constants here and the project file extension, the compressed-file
//! magic, the CA subject, diagnostic messages, the injected User-Agent and the
//! exporters all follow. Static manifests that can't read Rust (tauri.conf.json,
//! Cargo.toml, package.json, index.html) and the frontend (src/lib/brand.js)
//! carry their own copies — keep them in sync when renaming.

/// Display name shown to users.
pub const NAME: &str = "Ngintip";

/// Lowercase identifier used for file base names and temp files.
pub const ID: &str = "ngintip";

/// Project file extension, without the leading dot (e.g. `project.ngintip`).
pub const PROJECT_EXT: &str = "ngintip";

/// Magic prefix written to new compressed project files. Kept at 6 bytes so the
/// on-disk header layout (magic + 1 version byte) is unchanged.
pub const MAGIC: &[u8] = b"NGNTPC";

/// Older magic prefixes still accepted when reading. A previous compressed format is
/// no longer supported, so only the current [`MAGIC`] opens; keep the list here (same
/// length as [`MAGIC`]) if back-compat is ever wanted again.
pub const LEGACY_MAGICS: &[&[u8]] = &[];

/// Organisation behind the app, embedded in generated CA certificates as the subject
/// OU, so a trusted root is traceable to its origin. A bare domain, not a URL: a DN
/// names an entity, it is not a link.
///
/// Keep the CA subject to the organisation. Whoever receives a `.cer` to install can
/// read every attribute in it, on every device it is installed on, for the ten years
/// the root is valid, so no personal name or contact address belongs here.
pub const ORG_DOMAIN: &str = "seclab.id";
