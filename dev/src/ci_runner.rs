//! CI専用のパス検証・レポート列挙。初期化前にrustcで直接コンパイルできる標準ライブラリのみの補助。
use std::{
    env,
    error::Error,
    fs,
    path::{Component, Path, PathBuf},
    process::ExitCode,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn main() -> ExitCode {
    let result = (|| -> Result<()> {
        let args = env::args_os().skip(1).collect::<Vec<_>>();
        if args.len() != 2 {
            return Err("Expected ensure-directory|reports <absolute path>".into());
        }
        let path = Path::new(&args[1]);
        if args[0] == "ensure-directory" {
            safe_directory(path)?;
            fs::create_dir_all(path)?;
        } else if args[0] == "reports" {
            for report in validation_reports(path)? {
                println!("{}", report.display());
            }
        } else {
            return Err("Unknown CI path operation".into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn safe_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("CI directory must be absolute and normalized".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if linked(&metadata) => return Err("CI root has a linked ancestor".into()),
            Ok(metadata) if !metadata.is_dir() => {
                return Err("CI ancestor is not a directory".into());
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn validation_reports(root: &Path) -> Result<Vec<PathBuf>> {
    safe_directory(root)?;
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut pending = vec![root.to_path_buf()];
    let mut result = Vec::new();
    let mut visited = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            visited += 1;
            if visited > 100_000 {
                return Err("Validation report inventory exceeds bound".into());
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if linked(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with("validation.json")
            {
                result.push(entry.path());
            }
        }
    }
    result.sort();
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let root = env::temp_dir().canonicalize().unwrap().join(format!(
            "remnifold-ci-contract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn directory_boundaries() {
        let root = root();
        assert!(safe_directory(Path::new("relative")).is_err());
        // Windows verbatim PathBuf::join normalizes '..'; construct the literal input instead.
        let unnormalized = PathBuf::from(format!(
            "{}{}..{}escape",
            root.display(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        ));
        assert!(safe_directory(&unnormalized).is_err());
        let file = root.join("ordinary-file");
        fs::write(&file, "fixture").unwrap();
        assert!(safe_directory(&file.join("child")).is_err());
    }
    #[test]
    fn report_inventory_is_recursive_and_exact() {
        let root = root();
        let reports = root.join("reports");
        fs::create_dir_all(reports.join("nested")).unwrap();
        fs::write(reports.join("nested/one-validation.json"), "{}").unwrap();
        fs::write(reports.join("ignore.json"), "{}").unwrap();
        assert_eq!(
            validation_reports(&reports).unwrap(),
            [reports.join("nested/one-validation.json")]
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_junction_ancestors_rejected_and_report_target_not_followed() {
        // Win32 junction fixture uses the ordinary DOS path supplied by the invocation's TEMP.
        let root = env::temp_dir().join(format!(
            "remnifold-ci-junction-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        safe_directory(&root).unwrap();
        fs::create_dir(&root).unwrap();
        let reports = root.join("reports");
        let target = root.join("junction-target");
        fs::create_dir(&reports).unwrap();
        fs::create_dir(&target).unwrap();
        let local_report = reports.join("local-validation.json");
        fs::write(&local_report, "local-report").unwrap();
        fs::write(
            target.join("external-validation.json"),
            "junction-target-report",
        )
        .unwrap();
        let junction = reports.join("alias");
        crate::updater_e2e::directory_link(&target, &junction).unwrap();
        assert!(
            linked(&fs::symlink_metadata(&junction).unwrap()),
            "fixture must be a real Windows reparse point"
        );
        // A readable target and a successful ordinary-path control rule out access-denied failures.
        assert_eq!(
            fs::read(junction.join("external-validation.json")).unwrap(),
            b"junction-target-report"
        );
        safe_directory(&target).unwrap();
        safe_directory(&reports).unwrap();
        for path in [&junction, &junction.join("uncreated/child")] {
            assert_eq!(
                safe_directory(path).unwrap_err().to_string(),
                "CI root has a linked ancestor"
            );
        }
        assert_eq!(
            validation_reports(&junction).unwrap_err().to_string(),
            "CI root has a linked ancestor"
        );
        assert_eq!(validation_reports(&reports).unwrap(), [local_report]);
        assert!(!target.join("uncreated").exists());
        eprintln!("Windows CI junction fixture retained: {}", root.display());
    }
    #[cfg(unix)]
    #[test]
    fn linked_ancestors_rejected_and_report_links_not_followed() {
        let root = root();
        let reports = root.join("reports");
        let outside = root.join("outside");
        fs::create_dir_all(&reports).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("external-validation.json"), "{}").unwrap();
        let link = reports.join("alias");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(safe_directory(&link.join("child")).is_err());
        assert!(validation_reports(&reports).unwrap().is_empty());
    }
}
