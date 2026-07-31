//! Zip creation + hashing.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use zip::write::FileOptions;
use zip::CompressionMethod;

/// Pack the *contents* of `root` into `out` (zip top-level == project root,
/// matching the existing `zip-folder.sh` semantics).
pub fn zip_dir(root: &Path, out: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", root.display()))?;
    let file = File::create(out).with_context(|| format!("create {}", out.display()))?;
    let mut zw = zip::ZipWriter::new(file);

    for entry in WalkDir::new(&root).follow_links(false).into_iter().flatten() {
        let rel = match entry.path().strip_prefix(&root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if entry.file_type().is_dir() {
            let opts = base_opts();
            zw.add_directory(format!("{rel_str}/"), opts)
                .with_context(|| format!("zip add dir {}", rel_str))?;
        } else if entry.file_type().is_file() {
            #[cfg(unix)]
            let opts = {
                use std::os::unix::fs::PermissionsExt;
                let mut opts = base_opts();
                if let Ok(m) = std::fs::metadata(entry.path()) {
                    opts = opts.unix_permissions(m.permissions().mode() & 0o777);
                }
                opts
            };
            #[cfg(not(unix))]
            let opts = base_opts();
            zw.start_file(&rel_str, opts)
                .with_context(|| format!("zip start {}", rel_str))?;
            let mut f = File::open(entry.path())?;
            std::io::copy(&mut f, &mut zw)?;
        }
    }

    zw.finish()?;
    Ok(())
}

fn base_opts() -> FileOptions {
    FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(9))
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = File::open(p)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
