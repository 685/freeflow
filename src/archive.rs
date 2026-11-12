use crate::files::{self, Problem, Result};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};
use tokio_util::sync::CancellationToken;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub fn zip_folder(
    root: &Path,
    output: &Path,
    source: bool,
    cancel: &CancellationToken,
) -> Result<()> {
    let include: fn(&str) -> bool = if source {
        files::visible_name
    } else {
        |_| true
    };
    let tree = files::walk(root, include, false, 20_000)?;
    if tree.truncated {
        return Err(Problem::new(
            413,
            "The export contains too many files for a single ZIP.",
        ));
    }
    let folder = output.parent().expect("export directory");
    let mut temp = tempfile::NamedTempFile::new_in(folder)?;
    let mut zip = ZipWriter::new(temp.as_file_mut());
    let mut total = 0_u64;
    let mut chunk = vec![0; 128 * 1024];
    for item in tree.entries {
        let path = root.join(&item.path);
        let meta = fs::symlink_metadata(&path)?;
        if files::is_link(&meta) || !meta.is_file() {
            continue;
        }
        total = total.saturating_add(meta.len() + 96 + 2 * item.path.len() as u64);
        if total >= u32::MAX as u64 || item.path.len() > u16::MAX as usize {
            return Err(Problem::new(
                413,
                "ZIP exports are limited to 4 GiB and standard ZIP paths.",
            ));
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .unix_permissions(mode);
        zip.start_file(item.path, options)
            .map_err(Problem::internal)?;
        let mut file = fs::File::open(path)?;
        loop {
            if cancel.is_cancelled() {
                return Err(Problem::new(409, "Command stopped."));
            }
            let size = file.read(&mut chunk)?;
            if size == 0 {
                break;
            }
            zip.write_all(&chunk[..size])?;
        }
    }
    zip.finish().map_err(Problem::internal)?.sync_all()?;
    temp.persist_noclobber(output)
        .map_err(|error| Problem::from(error.error))?;
    Ok(())
}

pub fn copy_artifact(source: &Path, output: &Path) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(output.parent().expect("export directory"))?;
    io::copy(&mut fs::File::open(source)?, &mut temp)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)
        .map_err(|error| Problem::from(error.error))?;
    Ok(())
}
