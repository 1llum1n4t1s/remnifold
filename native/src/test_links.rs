//! fixture 専用。既存パスを置き換えず Rust からリンクを作る。
use anyhow::{Result, ensure};
use std::{
    fs,
    path::{Component, Path},
};

fn checked(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir)),
        "fixture link path must be absolute"
    );
    for prefix in path.ancestors() {
        match fs::symlink_metadata(prefix) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "linked fixture ancestor"
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "reparse fixture ancestor"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn directory_link(target: &Path, link: &Path) -> Result<()> {
    checked(target)?;
    checked(link)?;
    ensure!(
        target.is_dir() && !link.exists(),
        "fixture link requires an existing target and a new link"
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)?;
    #[cfg(windows)]
    junction(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn junction(target: &Path, link: &Path) -> Result<()> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            sharing: u32,
            security: *mut c_void,
            creation: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            handle: *mut c_void,
            code: u32,
            input: *mut c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let target = target.canonicalize()?;
    let target = target.as_os_str().encode_wide().collect::<Vec<_>>();
    ensure!(
        !target.contains(&0) && target.len() <= 4000,
        "fixture target length rejected"
    );
    let prefix = "\\\\?\\".encode_utf16().collect::<Vec<_>>();
    let native = target.strip_prefix(prefix.as_slice()).unwrap_or(&target);
    let substitute = "\\??\\"
        .encode_utf16()
        .chain(native.iter().copied())
        .collect::<Vec<_>>();
    let mut paths = substitute.clone();
    paths.push(0);
    let print_offset = paths.len() * 2;
    paths.extend_from_slice(&target);
    paths.push(0);
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&0xa0000003u32.to_le_bytes());
    buffer.extend_from_slice(&((8 + paths.len() * 2) as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    for value in [
        0u16,
        (substitute.len() * 2) as u16,
        print_offset as u16,
        (target.len() * 2) as u16,
    ] {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    for value in paths {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    let name = link.as_os_str().encode_wide().collect::<Vec<_>>();
    ensure!(!name.contains(&0), "fixture link name rejected");
    let name = name.into_iter().chain(Some(0)).collect::<Vec<_>>();
    fs::create_dir(link)?;
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x40000000,
            7,
            std::ptr::null_mut(),
            3,
            0x02000000 | 0x00200000,
            std::ptr::null_mut(),
        )
    };
    ensure!(
        handle as isize != -1,
        "fixture junction handle: {}",
        std::io::Error::last_os_error()
    );
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            handle,
            0x000900a4,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    let error = std::io::Error::last_os_error();
    unsafe {
        CloseHandle(handle);
    }
    ensure!(result != 0, "fixture junction creation: {error}");
    Ok(())
}
