//! Tiny package manager.
//!
//! No real archives — packages are described by a plain-text manifest at
//! `/var/lib/pkg/<name>.pkg` plus inline file contents.  This is enough to
//! demonstrate package install/list/remove against the VFS.
//!
//! Manifest grammar (one entry per line, blank lines and `#` comments
//! ignored):
//!
//!     NAME <name>
//!     VERSION <version>
//!     DESCRIPTION <text>
//!     FILE <vfs-path> <length>
//!     <length bytes of file content>
//!     ... repeated FILE blocks ...
//!
//! `pkg install <pkgfile>` parses the manifest, writes each FILE into the
//! VFS, then records the package name in /var/lib/pkg/<name>.installed
//! along with the list of installed paths.  `pkg remove <name>` reads the
//! .installed manifest and unlinks the listed paths.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub files: Vec<(String, Vec<u8>)>,
}

impl PackageManifest {
    pub fn parse(data: &[u8]) -> Result<Self, &'static str> {
        let mut i = 0usize;
        let mut name = String::new();
        let mut version = String::new();
        let mut description = String::new();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();

        while i < data.len() {
            // Skip blank lines and comments at line start.
            let line_end = match data[i..].iter().position(|&b| b == b'\n') {
                Some(p) => i + p,
                None => data.len(),
            };
            let line = &data[i..line_end];
            i = line_end + 1; // step past newline

            let line_str = match core::str::from_utf8(line) {
                Ok(s) => s.trim(),
                Err(_) => continue,
            };
            if line_str.is_empty() || line_str.starts_with('#') { continue; }

            if let Some(rest) = line_str.strip_prefix("NAME ") {
                name = rest.trim().to_string();
            } else if let Some(rest) = line_str.strip_prefix("VERSION ") {
                version = rest.trim().to_string();
            } else if let Some(rest) = line_str.strip_prefix("DESCRIPTION ") {
                description = rest.trim().to_string();
            } else if let Some(rest) = line_str.strip_prefix("FILE ") {
                let mut parts = rest.trim().rsplitn(2, char::is_whitespace);
                let len: usize = match parts.next().and_then(|s| s.parse().ok()) {
                    Some(n) => n,
                    None => return Err("FILE: expected length"),
                };
                let path = match parts.next() {
                    Some(p) => p.to_string(),
                    None => return Err("FILE: expected path"),
                };
                if i + len > data.len() {
                    return Err("FILE: truncated content");
                }
                let content = data[i..i + len].to_vec();
                files.push((path, content));
                i += len;
                // Skip a single trailing newline if present.
                if i < data.len() && data[i] == b'\n' { i += 1; }
            }
        }

        if name.is_empty() { return Err("missing NAME"); }
        Ok(PackageManifest { name, version, description, files })
    }
}

/// Install a package read from `pkg_file` (a path in the VFS).
pub fn install(pkg_file: &str) -> Result<PackageManifest, &'static str> {
    use crate::fs::vfs::VfsContext;
    let bytes = VfsContext::read(pkg_file).map_err(|_| "cannot read package file")?;
    let manifest = PackageManifest::parse(&bytes)?;
    // Make sure /var/lib/pkg exists.
    let _ = VfsContext::mkdir("/var");
    let _ = VfsContext::mkdir("/var/lib");
    let _ = VfsContext::mkdir("/var/lib/pkg");

    // Write each file.
    for (path, content) in &manifest.files {
        let _ = VfsContext::write(path, content.clone());
    }

    // Record the install: a small text manifest with one path per line.
    let mut record = alloc::format!("NAME {}\nVERSION {}\nDESCRIPTION {}\n",
        manifest.name, manifest.version, manifest.description);
    for (path, _) in &manifest.files {
        record.push_str(&alloc::format!("PATH {}\n", path));
    }
    let installed_path = alloc::format!("/var/lib/pkg/{}.installed", manifest.name);
    let _ = VfsContext::write(&installed_path, record.into_bytes());

    Ok(manifest)
}

/// Remove a previously-installed package by name.
pub fn remove(name: &str) -> Result<usize, &'static str> {
    use crate::fs::vfs::VfsContext;
    let installed_path = alloc::format!("/var/lib/pkg/{}.installed", name);
    let bytes = VfsContext::read(&installed_path).map_err(|_| "package not installed")?;
    let text = core::str::from_utf8(&bytes).map_err(|_| "invalid manifest")?;
    let mut removed = 0usize;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("PATH ") {
            if VfsContext::delete(p.trim()).is_ok() {
                removed += 1;
            }
        }
    }
    let _ = VfsContext::delete(&installed_path);
    Ok(removed)
}

/// List installed packages by walking /var/lib/pkg.
pub fn list_installed() -> Vec<String> {
    use crate::fs::vfs::VfsContext;
    let entries = match VfsContext::list_dir("/var/lib/pkg") {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for e in entries {
        if let Some(stem) = e.name.strip_suffix(".installed") {
            out.push(stem.to_string());
        }
    }
    out
}

/// Read the manifest fields (name/version/description) of an installed
/// package, if present.
pub fn info(name: &str) -> Option<(String, String, String)> {
    use crate::fs::vfs::VfsContext;
    let installed_path = alloc::format!("/var/lib/pkg/{}.installed", name);
    let bytes = VfsContext::read(&installed_path).ok()?;
    let text = core::str::from_utf8(&bytes).ok()?;
    let mut name_v = String::new();
    let mut ver_v = String::new();
    let mut desc_v = String::new();
    for line in text.lines() {
        if let Some(r) = line.strip_prefix("NAME ") { name_v = r.trim().to_string(); }
        if let Some(r) = line.strip_prefix("VERSION ") { ver_v = r.trim().to_string(); }
        if let Some(r) = line.strip_prefix("DESCRIPTION ") { desc_v = r.trim().to_string(); }
    }
    Some((name_v, ver_v, desc_v))
}
