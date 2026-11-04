use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn path_equal(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        // GetFinalPathNameByHandle がUnicodeケース差や8.3名を実在パスへ正規化する。
        if let (Ok(left), Ok(right)) = (fs::canonicalize(a), fs::canonicalize(b)) {
            if left == right {
                return true;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(left), Ok(right)) = (fs::metadata(a), fs::metadata(b)) {
            if left.dev() == right.dev() && left.ino() == right.ino() {
                return true;
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let left: Vec<u16> = a.as_os_str().encode_wide().collect();
        let right: Vec<u16> = b.as_os_str().encode_wide().collect();
        if left.len() > i32::MAX as usize || right.len() > i32::MAX as usize {
            return false;
        }
        unsafe {
            windows_sys::Win32::Globalization::CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            ) == windows_sys::Win32::Globalization::CSTR_EQUAL
        }
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}
pub(crate) fn within(parent: &Path, child: &Path) -> bool {
    child
        .ancestors()
        .any(|ancestor| path_equal(parent, ancestor))
}
// 未作成の末尾だけを保存し、存在する最長 prefix の実体を解決する。
pub(crate) fn physical_path(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(&existing)?;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("INVALID_WORK_ROOT"))?
                        .to_os_string(),
                );
                ensure!(existing.pop(), "INVALID_WORK_ROOT");
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub(crate) fn isolated_physical_roots(source: &Path, work: &Path, codex_home: &Path) -> Result<()> {
    let source = physical_path(source)?;
    let work = physical_path(work)?;
    let codex = physical_path(codex_home)?;
    for (left, right) in [(&source, &work), (&source, &codex), (&work, &codex)] {
        ensure!(
            !within(left, right) && !within(right, left),
            "INVALID_WORK_ROOT"
        );
    }
    Ok(())
}
pub(crate) fn no_links(path: &Path, missing: bool) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            // `\\?\C:` 単独は照会可能なディレクトリではない。RootDir 結合後に検査する。
            ensure!(
                path.is_absolute()
                    && matches!(
                        path.components().nth(1),
                        Some(std::path::Component::RootDir)
                    ),
                "INVALID_PATH"
            );
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(info) => {
                ensure!(!info.file_type().is_symlink(), "LINK_PATH_REJECTED");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(info.file_attributes() & 0x400 == 0, "LINK_PATH_REJECTED");
                }
            }
            Err(error) if missing && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
