//! 更新領域だけを保護し、祖先の差し替えを更新完了まで防ぐ。
use super::path_safety::no_links;
#[cfg(target_os = "macos")]
use super::process as acl_process;
use anyhow::{Result, bail, ensure};
use std::{fs, path::Path};
#[cfg(test)]
#[path = "../test_links.rs"]
mod test_links;
#[cfg(all(test, windows))]
fn absolute(value: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(value);
    ensure!(path.is_absolute(), "INVALID_WORK_ROOT");
    Ok(path)
}

#[cfg(not(windows))]
pub(crate) struct PrivateWorkGuard;
#[cfg(not(windows))]
impl PrivateWorkGuard {
    pub(super) fn open_delivery(_work: &Path, _profile: &Path) -> Result<Self> {
        bail!("WINDOWS_BOOTSTRAP_REQUIRED")
    }
    pub(crate) fn open(work: &Path) -> Result<Self> {
        ensure!(work.is_absolute(), "INVALID_WORK_ROOT");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            unsafe extern "C" {
                fn geteuid() -> u32;
            }
            let uid = unsafe { geteuid() };
            let paths: Vec<_> = work.ancestors().collect();
            for path in paths.into_iter().rev() {
                match fs::symlink_metadata(path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        fs::DirBuilder::new().mode(0o700).create(path)?;
                    }
                    Err(error) => return Err(error.into()),
                }
                let info = fs::symlink_metadata(path)?;
                ensure!(
                    !info.file_type().is_symlink() && info.is_dir(),
                    "LINK_PATH_REJECTED"
                );
                ensure!(
                    unix_metadata_safe(info.uid(), info.mode(), uid, path == work),
                    "PRIVATE_WORK_PERMISSIONS_UNSAFE"
                );
                #[cfg(target_os = "macos")]
                validate_macos_acl(path)?;
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(work)?;
        no_links(work, false)?;
        Ok(Self)
    }
}

#[cfg(target_os = "macos")]
fn validate_macos_acl(path: &Path) -> Result<()> {
    let mut command = std::process::Command::new("/bin/ls");
    command.arg("-lde").arg(path);
    let output = acl_process::run(command, std::time::Duration::from_secs(120), 1024 * 1024)
        .map_err(|_| anyhow::anyhow!("PRIVATE_WORK_ACL_UNSAFE"))?;
    ensure!(macos_acl_safe(&output), "PRIVATE_WORK_ACL_UNSAFE");
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn validate_receipt(path: &Path) -> Result<()> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        info.is_file() && !info.file_type().is_symlink(),
        "NATIVE_RECEIPT_UNSAFE"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        let uid = unsafe { geteuid() };
        ensure!(
            (info.uid() == uid || info.uid() == 0) && info.mode() & 0o022 == 0 && info.nlink() == 1,
            "NATIVE_RECEIPT_UNSAFE"
        );
        let opened = fs::File::open(path)?.metadata()?;
        ensure!(
            opened.dev() == info.dev() && opened.ino() == info.ino(),
            "NATIVE_RECEIPT_UNSAFE"
        );
        #[cfg(target_os = "macos")]
        validate_macos_acl(path)?;
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn unix_metadata_safe(owner: u32, mode: u32, current: u32, root: bool) -> bool {
    if root {
        owner == current && mode & 0o022 == 0
    } else {
        (owner == current || owner == 0) && (mode & 0o022 == 0 || mode & 0o1000 != 0)
    }
}
#[cfg(any(target_os = "macos", test))]
fn macos_acl_safe(output: &[u8]) -> bool {
    let Ok(output) = std::str::from_utf8(output) else {
        return false;
    };
    for line in output.lines() {
        let Some((index, entry)) = line.trim().split_once(':') else {
            continue;
        };
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let words: Vec<_> = entry.split_ascii_whitespace().collect();
        let Some(allow) = words
            .iter()
            .rposition(|word| *word == "allow" || *word == "deny")
        else {
            return false;
        };
        if words[allow] == "deny" {
            continue;
        }
        if allow + 1 == words.len() {
            return false;
        }
        for permission in words[allow + 1..].iter().flat_map(|word| word.split(',')) {
            if ![
                "read",
                "readattr",
                "readextattr",
                "readsecurity",
                "list",
                "search",
                "execute",
                "synchronize",
                "inherited",
                "file_inherit",
                "directory_inherit",
                "limit_inherit",
                "only_inherit",
            ]
            .contains(&permission)
            {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    #[test]
    fn unix_existing_roots_and_sticky_ancestors_keep_their_permissions() {
        assert!(unix_metadata_safe(42, 0o700, 42, true));
        assert!(!unix_metadata_safe(42, 0o777, 42, true));
        assert!(!unix_metadata_safe(0, 0o700, 42, true));
        assert!(unix_metadata_safe(0, 0o1777, 42, false));
        assert!(!unix_metadata_safe(0, 0o777, 42, false));
        assert!(!unix_metadata_safe(17, 0o1777, 42, false));
    }
    #[test]
    fn macos_deny_only_acl_is_safe_but_mutating_allow_acl_is_not() {
        assert!(macos_acl_safe(
            b"drwx------+ 3 user staff\n 0: group:everyone deny delete\n"
        ));
        assert!(macos_acl_safe(
            b" 0: group:everyone allow list,search,readattr,readsecurity\n"
        ));
        assert!(!macos_acl_safe(
            b" 0: group:everyone allow list,add_file,writeattr\n"
        ));
        assert!(!macos_acl_safe(b" 0: group:everyone allow writesecurity\n"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_real_existing_root_and_unsafe_ancestor_are_checked_without_chmod() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("remnifold-unix-private-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let safe = root.join("safe");
        fs::DirBuilder::new().mode(0o700).create(&safe).unwrap();
        fs::write(safe.join("receipt"), b"preserved").unwrap();
        let mode = fs::metadata(&safe).unwrap().permissions().mode();
        PrivateWorkGuard::open(&safe).unwrap();
        assert_eq!(fs::metadata(&safe).unwrap().permissions().mode(), mode);
        assert_eq!(fs::read(safe.join("receipt")).unwrap(), b"preserved");
        let receipt = safe.join("receipt");
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
        validate_receipt(&receipt).unwrap();
        for mode in [0o664, 0o666] {
            fs::set_permissions(&receipt, fs::Permissions::from_mode(mode)).unwrap();
            assert!(validate_receipt(&receipt).is_err());
            assert_eq!(
                fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert_eq!(fs::read(&receipt).unwrap(), b"preserved");
        }
        let linked = safe.join("linked-receipt");
        fs::write(&linked, b"legacy").unwrap();
        fs::set_permissions(&linked, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&linked, safe.join("second-link")).unwrap();
        assert!(validate_receipt(&linked).is_err());
        let unsafe_root = root.join("shared");
        fs::create_dir(&unsafe_root).unwrap();
        fs::set_permissions(&unsafe_root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(PrivateWorkGuard::open(&unsafe_root).is_err());
        let missing = unsafe_root.join("not-created");
        assert!(PrivateWorkGuard::open(&missing).is_err());
        assert!(!missing.exists());
        assert_eq!(
            fs::metadata(&unsafe_root).unwrap().permissions().mode() & 0o777,
            0o777
        );
        let private = root.join("new/private");
        PrivateWorkGuard::open(&private).unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        eprintln!("retained Unix private-work fixture: {}", root.display());
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        os::windows::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GENERIC_EXECUTE, GENERIC_READ, LocalFree},
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct LocalMemory(*mut c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }
    struct Identity {
        token: Vec<usize>,
        descriptor: LocalMemory,
    }
    impl Identity {
        fn current() -> Result<Self> {
            unsafe {
                let mut handle = std::ptr::null_mut();
                ensure!(
                    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) != 0,
                    "PRIVATE_WORK_IDENTITY_FAILED"
                );
                let handle = OwnedHandle::from_raw_handle(handle);
                let mut size = 0;
                GetTokenInformation(
                    handle.as_raw_handle(),
                    TokenUser,
                    std::ptr::null_mut(),
                    0,
                    &mut size,
                );
                ensure!(size > 0 && size < 65536, "PRIVATE_WORK_IDENTITY_FAILED");
                let mut token =
                    vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
                ensure!(
                    GetTokenInformation(
                        handle.as_raw_handle(),
                        TokenUser,
                        token.as_mut_ptr().cast(),
                        size,
                        &mut size
                    ) != 0,
                    "PRIVATE_WORK_IDENTITY_FAILED"
                );
                let user = &*token.as_ptr().cast::<TOKEN_USER>();
                let mut text = std::ptr::null_mut();
                ensure!(
                    ConvertSidToStringSidW(user.User.Sid, &mut text) != 0,
                    "PRIVATE_WORK_IDENTITY_FAILED"
                );
                let _text = LocalMemory(text.cast());
                let mut length = 0;
                while *text.add(length) != 0 {
                    length += 1;
                }
                let sid = String::from_utf16(std::slice::from_raw_parts(text, length))?;
                let sddl: Vec<u16> =
                    format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                let mut descriptor = std::ptr::null_mut();
                ensure!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        std::ptr::null_mut()
                    ) != 0,
                    "PRIVATE_WORK_SECURITY_FAILED"
                );
                Ok(Self {
                    token,
                    descriptor: LocalMemory(descriptor),
                })
            }
        }
        fn sid(&self) -> PSID {
            unsafe { (*self.token.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
        #[cfg(test)]
        fn trusted_sid(&self, sid: PSID) -> Result<bool> {
            self.trusted_sid_policy(sid, false)
        }
        fn trusted_sid_policy(&self, sid: PSID, ancestor: bool) -> Result<bool> {
            if unsafe { EqualSid(sid, self.sid()) } != 0 {
                return Ok(true);
            }
            let text: Vec<u16> = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut installer = std::ptr::null_mut();
            ensure!(
                unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut installer) } != 0,
                "PRIVATE_WORK_IDENTITY_FAILED"
            );
            let _installer = LocalMemory(installer);
            if ancestor && unsafe { EqualSid(sid, installer) } != 0 {
                return Ok(true);
            }
            for kind in [WinLocalSystemSid, WinBuiltinAdministratorsSid] {
                let mut buffer = vec![0usize; 16];
                let mut size = (buffer.len() * std::mem::size_of::<usize>()) as u32;
                ensure!(
                    unsafe {
                        CreateWellKnownSid(
                            kind,
                            std::ptr::null_mut(),
                            buffer.as_mut_ptr().cast(),
                            &mut size,
                        )
                    } != 0,
                    "PRIVATE_WORK_IDENTITY_FAILED"
                );
                if unsafe { EqualSid(sid, buffer.as_mut_ptr().cast()) } != 0 {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        fn validate(&self, root: &fs::File) -> Result<()> {
            self.validate_policy(root, false)
        }
        fn validate_policy(&self, root: &fs::File, ancestor: bool) -> Result<()> {
            unsafe {
                let mut owner = std::ptr::null_mut();
                let mut descriptor = std::ptr::null_mut();
                let result = GetSecurityInfo(
                    root.as_raw_handle(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut owner,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut descriptor,
                );
                let _descriptor = LocalMemory(descriptor);
                ensure!(
                    result == 0 && !owner.is_null() && self.trusted_sid_policy(owner, ancestor)?,
                    "PRIVATE_WORK_OWNER_MISMATCH"
                );
                let mut present = 0;
                let mut defaulted = 0;
                let mut dacl = std::ptr::null_mut();
                ensure!(
                    GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                        != 0
                        && present != 0
                        && !dacl.is_null(),
                    "PRIVATE_WORK_SECURITY_FAILED"
                );
                for index in 0..(*dacl).AceCount as u32 {
                    let mut ace = std::ptr::null_mut();
                    ensure!(
                        GetAce(dacl, index, &mut ace) != 0,
                        "PRIVATE_WORK_SECURITY_FAILED"
                    );
                    let header = &*ace.cast::<ACE_HEADER>();
                    // 通常の allow/deny 以外は意味を推測せず拒否する。
                    ensure!(
                        header.AceType == 0 || header.AceType == 1,
                        "PRIVATE_WORK_ACL_UNSAFE"
                    );
                    if header.AceType == 1 {
                        continue;
                    }
                    ensure!(
                        header.AceSize as usize >= std::mem::size_of::<ACCESS_ALLOWED_ACE>(),
                        "PRIVATE_WORK_ACL_UNSAFE"
                    );
                    let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    let read_only = FILE_READ_DATA
                        | FILE_READ_EA
                        | FILE_EXECUTE
                        | FILE_READ_ATTRIBUTES
                        | READ_CONTROL
                        | SYNCHRONIZE
                        | GENERIC_READ
                        | GENERIC_EXECUTE;
                    if header.AceFlags & 8 != 0 {
                        continue;
                    }
                    let unsafe_mask = if ancestor {
                        allowed.Mask & 0x500D0040
                    } else {
                        allowed.Mask & !read_only
                    };
                    if unsafe_mask != 0 {
                        let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
                        ensure!(
                            self.trusted_sid_policy(sid, ancestor)?,
                            "PRIVATE_WORK_ACL_UNSAFE"
                        );
                    }
                }
                Ok(())
            }
        }
        fn create(&self, path: &Path) -> Result<()> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor.0,
                bInheritHandle: 0,
            };
            if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
                let error = std::io::Error::last_os_error();
                ensure!(
                    error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32),
                    "PRIVATE_WORK_CREATE_FAILED"
                );
            }
            Ok(())
        }
    }

    pub(crate) struct PrivateWorkGuard {
        _locks: Vec<fs::File>,
    }
    pub(crate) fn validate_receipt(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(info) => ensure!(
                info.is_file() && !info.file_type().is_symlink(),
                "NATIVE_RECEIPT_UNSAFE"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let file = fs::OpenOptions::new()
            .access_mode(FILE_READ_DATA | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(|_| anyhow::anyhow!("NATIVE_RECEIPT_UNSAFE"))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "NATIVE_RECEIPT_UNSAFE"
        );
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0
                && info.nNumberOfLinks == 1,
            "NATIVE_RECEIPT_UNSAFE"
        );
        Identity::current()?
            .validate(&file)
            .map_err(|_| anyhow::anyhow!("NATIVE_RECEIPT_UNSAFE"))
    }
    impl PrivateWorkGuard {
        pub(crate) fn open(work: &Path) -> Result<Self> {
            Self::open_policy(work, None)
        }
        pub(crate) fn open_delivery(work: &Path, profile: &Path) -> Result<Self> {
            ensure!(work.starts_with(profile), "INVALID_DELIVERY_ROOT");
            Self::open_policy(work, Some(profile))
        }
        fn open_policy(work: &Path, delivery: Option<&Path>) -> Result<Self> {
            ensure!(work.is_absolute(), "INVALID_WORK_ROOT");
            let identity = Identity::current()?;
            let mut locks = Vec::new();
            let ancestors: Vec<_> = work.ancestors().collect();
            for path in ancestors.into_iter().rev() {
                match fs::symlink_metadata(path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        identity.create(path)?
                    }
                    Err(_) => bail!("PRIVATE_WORK_OPEN_FAILED"),
                }
                // LIST_DIRECTORY がある handle を DELETE 共有なしで保持する。
                let access = FILE_LIST_DIRECTORY
                    | if path == work || delivery.is_some() {
                        READ_CONTROL
                    } else {
                        0
                    };
                let handle = fs::OpenOptions::new()
                    .access_mode(access)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(path)
                    .map_err(|_| anyhow::anyhow!("PRIVATE_WORK_OPEN_FAILED"))?;
                let info = handle.metadata()?;
                ensure!(
                    info.is_dir() && info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                    "LINK_PATH_REJECTED"
                );
                no_links(path, false)?;
                if path == work || delivery.is_some_and(|profile| path.starts_with(profile)) {
                    identity.validate(&handle)?;
                } else if delivery.is_some() {
                    identity.validate_policy(&handle, true)?;
                }
                locks.push(handle);
            }
            Ok(Self { _locks: locks })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn allow_authenticated_writes(path: &Path) {
            unsafe {
                let text: Vec<u16> = "D:P(A;;FA;;;AU)(A;;FA;;;SY)(A;;FA;;;BA)"
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let mut descriptor = std::ptr::null_mut();
                assert_ne!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        text.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        std::ptr::null_mut()
                    ),
                    0
                );
                let _descriptor = LocalMemory(descriptor);
                let mut dacl = std::ptr::null_mut();
                let mut present = 0;
                let mut defaulted = 0;
                assert_ne!(
                    GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted),
                    0
                );
                let file = fs::OpenOptions::new()
                    .access_mode(WRITE_DAC | READ_CONTROL | FILE_READ_DATA)
                    .open(path)
                    .unwrap();
                assert_eq!(
                    SetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null()
                    ),
                    0
                );
            }
        }

        #[test]
        fn receipt_acl_and_link_count_are_checked_without_repair() {
            let root = absolute(&std::env::temp_dir().to_string_lossy())
                .unwrap()
                .join(format!("remnifold-receipt-{}", uuid::Uuid::new_v4()));
            let _guard = PrivateWorkGuard::open(&root).unwrap();
            let root_acl = dacl_text(&root);
            for (name, bytes) in [
                ("schema1.json", b"legacy".as_slice()),
                ("schema2.json", b"r2".as_slice()),
            ] {
                let path = root.join(name);
                fs::write(&path, bytes).unwrap();
                let acl = dacl_text(&path);
                validate_receipt(&path).unwrap();
                assert_eq!(dacl_text(&path), acl);
                assert_eq!(fs::read(&path).unwrap(), bytes);
            }
            let unsafe_path = root.join("unsafe.json");
            fs::write(&unsafe_path, b"unchanged").unwrap();
            allow_authenticated_writes(&unsafe_path);
            let acl = dacl_text(&unsafe_path);
            assert!(validate_receipt(&unsafe_path).is_err());
            assert_eq!(dacl_text(&unsafe_path), acl);
            assert_eq!(fs::read(&unsafe_path).unwrap(), b"unchanged");
            assert_eq!(dacl_text(&root), root_acl);
            let linked = root.join("linked.json");
            fs::write(&linked, b"linked").unwrap();
            fs::hard_link(&linked, root.join("other-link.json")).unwrap();
            assert!(validate_receipt(&linked).is_err());
            eprintln!("retained receipt fixture: {}", root.display());
        }

        fn dacl_text(path: &Path) -> Vec<u16> {
            let file = fs::OpenOptions::new()
                .access_mode(READ_CONTROL | FILE_LIST_DIRECTORY)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
                .unwrap();
            unsafe {
                let mut descriptor = std::ptr::null_mut();
                assert_eq!(
                    GetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut descriptor
                    ),
                    0
                );
                let _descriptor = LocalMemory(descriptor);
                let mut text = std::ptr::null_mut();
                let mut length = 0;
                assert_ne!(
                    ConvertSecurityDescriptorToStringSecurityDescriptorW(
                        descriptor,
                        SDDL_REVISION_1,
                        DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                        &mut text,
                        &mut length
                    ),
                    0
                );
                let _text = LocalMemory(text.cast());
                std::slice::from_raw_parts(text, length as usize).to_vec()
            }
        }

        #[test]
        fn private_root_keeps_receipts_and_blocks_ancestor_replacement() {
            let parent = absolute(&std::env::temp_dir().to_string_lossy()).unwrap();
            no_links(&parent, false).unwrap();
            let root = parent.join(format!("remnifold-private-work-{}", uuid::Uuid::new_v4()));
            let identity = Identity::current().unwrap();
            identity.create(&root).unwrap();
            let work = root.join("existing-work");
            identity.create(&work).unwrap();
            fs::write(work.join("verified-receipt.json"), b"preserved").unwrap();
            let source = root.join("source");
            identity.create(&source).unwrap();
            fs::write(source.join("unchanged"), b"source").unwrap();
            let source_before = fs::read(source.join("unchanged")).unwrap();
            let source_acl = dacl_text(&source);
            // 明示的に広い既存 ACL は、内容・子 ACL ごと保持して拒否する。
            unsafe {
                let text: Vec<u16> = "D:P(A;OICI;FA;;;AU)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                let mut descriptor = std::ptr::null_mut();
                assert_ne!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        text.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        std::ptr::null_mut()
                    ),
                    0
                );
                let _descriptor = LocalMemory(descriptor);
                let mut dacl = std::ptr::null_mut();
                let mut present = 0;
                let mut defaulted = 0;
                assert_ne!(
                    GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted),
                    0
                );
                let file = fs::OpenOptions::new()
                    .access_mode(WRITE_DAC | READ_CONTROL | FILE_LIST_DIRECTORY)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(&work)
                    .unwrap();
                assert_eq!(
                    SetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null()
                    ),
                    0
                );
            }
            let unsafe_acl = dacl_text(&work);
            let unsafe_child_acl = dacl_text(&work.join("verified-receipt.json"));
            assert!(
                matches!(PrivateWorkGuard::open(&work),Err(error) if error.to_string() == "PRIVATE_WORK_ACL_UNSAFE")
            );
            assert_eq!(dacl_text(&work), unsafe_acl);
            assert_eq!(
                dacl_text(&work.join("verified-receipt.json")),
                unsafe_child_acl
            );
            assert_eq!(
                fs::read(work.join("verified-receipt.json")).unwrap(),
                b"preserved"
            );
            let work = root.join("safe-existing");
            // 通常の既定 owner（管理者 CI なら BA もあり得る）を維持する。
            fs::create_dir(&work).unwrap();
            fs::write(work.join("verified-receipt.json"), b"preserved").unwrap();
            let safe_acl = dacl_text(&work);
            let safe_child_acl = dacl_text(&work.join("verified-receipt.json"));
            let guard = PrivateWorkGuard::open(&work).unwrap();
            assert_eq!(dacl_text(&work), safe_acl);
            assert_eq!(
                dacl_text(&work.join("verified-receipt.json")),
                safe_child_acl
            );
            assert_eq!(
                fs::read(work.join("verified-receipt.json")).unwrap(),
                b"preserved"
            );
            assert_eq!(fs::read(source.join("unchanged")).unwrap(), source_before);
            assert_eq!(dacl_text(&source), source_acl);
            for path in [&root, &work] {
                let error = fs::OpenOptions::new()
                    .access_mode(DELETE)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(path)
                    .unwrap_err();
                assert_eq!(error.raw_os_error(), Some(32));
            }
            unsafe {
                let mut descriptor = std::ptr::null_mut();
                let mut dacl = std::ptr::null_mut();
                assert_eq!(
                    GetSecurityInfo(
                        guard._locks.last().unwrap().as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut dacl,
                        std::ptr::null_mut(),
                        &mut descriptor
                    ),
                    0
                );
                let _descriptor = LocalMemory(descriptor);
                assert!(!dacl.is_null());
                assert_eq!((*dacl).AceCount, 3);
                let mut control = 0;
                let mut revision = 0;
                assert_ne!(
                    GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
                    0
                );
                assert_eq!(dacl_text(&work), safe_acl);
                let well_known = |kind| {
                    let mut sid = vec![0usize; 16];
                    let mut size = (sid.len() * std::mem::size_of::<usize>()) as u32;
                    assert_ne!(
                        CreateWellKnownSid(
                            kind,
                            std::ptr::null_mut(),
                            sid.as_mut_ptr().cast(),
                            &mut size
                        ),
                        0
                    );
                    sid
                };
                let mut system = well_known(WinLocalSystemSid);
                let mut admin = well_known(WinBuiltinAdministratorsSid);
                assert!(identity.trusted_sid(system.as_mut_ptr().cast()).unwrap());
                assert!(identity.trusted_sid(admin.as_mut_ptr().cast()).unwrap());
                let mut authenticated = well_known(WinAuthenticatedUserSid);
                assert!(
                    !identity
                        .trusted_sid(authenticated.as_mut_ptr().cast())
                        .unwrap()
                );
                for index in 0..3 {
                    let mut ace = std::ptr::null_mut();
                    assert_ne!(GetAce(dacl, index, &mut ace), 0);
                    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                    assert_eq!((*ace).Header.AceType, 0);
                    let sid = std::ptr::addr_of_mut!((*ace).SidStart).cast();
                    assert!(
                        EqualSid(sid, identity.sid()) != 0
                            || EqualSid(sid, system.as_mut_ptr().cast()) != 0
                            || EqualSid(sid, admin.as_mut_ptr().cast()) != 0
                    );
                }
            }
            drop(guard);
            let new_work = root.join("new/private-work");
            let _guard = PrivateWorkGuard::open(&new_work).unwrap();
            assert!(new_work.is_dir());
            let alias = root.join("reparse-work");
            super::super::test_links::directory_link(&source, &alias).unwrap();
            assert!(
                matches!(PrivateWorkGuard::open(&alias),Err(error) if error.to_string() == "LINK_PATH_REJECTED")
            );
            assert_eq!(dacl_text(&source), source_acl);
            eprintln!("retained private-work fixture: {}", root.display());
        }
    }
}
#[cfg(windows)]
pub(crate) use windows::PrivateWorkGuard;
#[cfg(windows)]
pub(crate) use windows::validate_receipt;
