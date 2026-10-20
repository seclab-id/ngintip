// Global branding & identifier — the single place to rebrand the frontend.
//
// Change these and the window/app name, the project file extension, the save/open
// dialog filters, localStorage key prefixes and sample bodies all follow. The
// static manifests that can't import this (index.html, package.json,
// tauri.conf.json) and the Rust side (src-tauri/src/brand.rs) keep their own
// copies — keep them in sync when renaming.
export const BRAND = {
  // Display name shown in the UI.
  name: 'Ngintip',
  // Lowercase identifier: localStorage key prefix and default file base names.
  id: 'ngintip',
  // Project file extension, without the dot (e.g. project.ngintip).
  ext: 'ngintip',
  // Extensions offered when opening a project. Legacy ".cegat"/".cegatc" projects
  // are no longer supported, so only the current format is offered.
  openExts: ['ngintip'],
  // User-Agent used in sample requests.
  userAgent: 'Ngintip/0.1',
  // Boundary used in the sample multipart body.
  multipartBoundary: '----NgintipBoundary',
};

// Namespaced localStorage key, e.g. brandKey('theme') -> "ngintip.theme".
export const brandKey = (suffix) => `${BRAND.id}.${suffix}`;
