// Global branding & identifier - the single place to rebrand the frontend.
//
// Change these and the window/app name, the project file extension, the save/open
// dialog filters, localStorage key prefixes and sample bodies all follow. The
// static manifests that can't import this (index.html, package.json) and the Rust
// side (src-tauri/src/brand.rs) keep their own copies - keep them in sync when renaming.
import { version } from '../../package.json';

export const BRAND = {
  // Display name shown in the UI.
  name: 'Ngintip',
  // Lowercase identifier: localStorage key prefix and default file base names.
  id: 'ngintip',
  // Project file extension, without the dot (e.g. project.ngintip).
  ext: 'ngintip',
  // Current app version, taken straight from package.json so the UI can never drift
  // from the build. Bump the version in ONE place: package.json (and src-tauri/Cargo.toml
  // for the Rust/installer side; tauri.conf.json inherits Cargo.toml). Shown in
  // Settings > Project and compared against the latest GitHub release.
  version,
  // GitHub repo "owner/name" for the update check (latest published release).
  repo: 'seclab-id/ngintip',
  // Extensions offered in the file open dialog (only the current project format).
  openExts: ['ngintip'],
  // User-Agent used in sample requests.
  userAgent: 'Ngintip/0.1',
  // Boundary used in the sample multipart body.
  multipartBoundary: '----NgintipBoundary',
};

// Namespaced localStorage key, e.g. brandKey('theme') -> "ngintip.theme".
export const brandKey = (suffix) => `${BRAND.id}.${suffix}`;
