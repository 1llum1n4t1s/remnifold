//! Windows の shell frontend に依存しない、固定 Git for Windows 起動文字列。
#[cfg(windows)]
pub(crate) const WINDOWS_SH: &str = r"C:\Program Files\Git\usr\bin\sh.exe";

pub(crate) fn windows_script(event: &str) -> Option<String> {
    if !["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"].contains(&event) {
        return None;
    }
    // パスはここへ埋めず、sh が環境変数を引用したデータとして読む。
    let script = format!("exec \"$PLUGIN_ROOT/bin/remnifold.exe\" hook {event}");
    let octal = script
        .bytes()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<String>();
    Some(format!(". <(printf '{octal}')"))
}

pub(crate) fn windows_hook_command(event: &str) -> Option<String> {
    // .exe を省略し、PowerShell の PATHEXT 不備は待たない起動ではなく失敗にする。
    Some(format!(
        r#"C:\"Program Files"\Git\usr\bin\sh -p -c "{}""#,
        windows_script(event)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_contains_only_fixed_octal_and_never_expands_plugin_root_in_the_outer_shell() {
        for event in ["PreToolUse", "SessionStart", "Stop", "UserPromptSubmit"] {
            let script = windows_script(event).unwrap();
            let octal = script
                .strip_prefix(". <(printf '")
                .unwrap()
                .strip_suffix("')")
                .unwrap();
            let decoded = octal
                .as_bytes()
                .chunks_exact(4)
                .map(|chunk| {
                    assert_eq!(chunk[0], b'\\');
                    u8::from_str_radix(std::str::from_utf8(&chunk[1..]).unwrap(), 8).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                String::from_utf8(decoded).unwrap(),
                format!("exec \"$PLUGIN_ROOT/bin/remnifold.exe\" hook {event}")
            );
            let command = windows_hook_command(event).unwrap();
            assert!(!command.contains('$') && !command.contains('%') && !command.contains('!'));
            assert!(command.starts_with(r#"C:\"Program Files"\Git\usr\bin\sh -p -c ""#));
        }
        assert!(windows_hook_command("Stop; arbitrary-command").is_none());
    }
}
