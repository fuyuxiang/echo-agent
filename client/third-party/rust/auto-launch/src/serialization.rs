//! Pure platform encoders. No registry, login item or filesystem mutation.

#[cfg(any(target_os = "windows", test))]
fn windows_argument(value: &str) -> String {
    // Windows argv rules: escape quotes, double backslashes before quotes and
    // before the closing delimiter. Always quote to preserve empty arguments.
    let mut result = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let count = if character == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        result.extend(std::iter::repeat('\\').take(count));
        result.push(character);
        backslashes = 0;
    }
    result.extend(std::iter::repeat('\\').take(backslashes * 2));
    result.push('"');
    result
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_command_line(executable: &str, args: &[String]) -> String {
    std::iter::once(executable)
        .chain(args.iter().map(String::as_str))
        .map(windows_argument)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(target_os = "macos", test))]
fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn launch_agent_contents(name: &str, executable: &str, args: &[String]) -> String {
    let arguments = std::iter::once(executable)
        .chain(args.iter().map(String::as_str))
        .map(|value| format!("<string>{}</string>", escape_xml(value)))
        .collect::<String>();
    format!(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\"><dict><key>Label</key><string>{}</string>",
        "<key>ProgramArguments</key><array>{}</array><key>RunAtLoad</key><true/></dict></plist>"
    ), escape_xml(name), arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_quotes_executable_and_chinese_data_home_as_distinct_arguments() {
        let args = [
            "--background".into(),
            "--data-home".into(),
            r"C:\中文 目录\EchoAgent".into(),
        ];
        assert_eq!(
            windows_command_line(r"C:\Program Files\EchoAgent\echoagent.exe", &args),
            r#""C:\Program Files\EchoAgent\echoagent.exe" "--background" "--data-home" "C:\中文 目录\EchoAgent""#
        );
    }

    #[test]
    fn windows_preserves_empty_quotes_and_trailing_backslashes() {
        assert_eq!(windows_argument(""), "\"\"");
        assert_eq!(windows_argument(r#"a"b"#), r#""a\"b""#);
        assert_eq!(windows_argument("C:\\中文 空格\\"), "\"C:\\中文 空格\\\\\"");
        assert_eq!(
            windows_argument("prefix\\\"suffix"),
            "\"prefix\\\\\\\"suffix\""
        );
    }

    #[test]
    fn launch_agent_escapes_xml_without_changing_argument_boundaries() {
        let data = launch_agent_contents(
            "Echo&Agent",
            "/Applications/Echo & Agent.app/Contents/MacOS/echoagent",
            &[
                "--background".into(),
                "--data-home".into(),
                "/tmp/中文 空格 & <资料> \"引用\" '单引号'".into(),
            ],
        );
        assert!(data.contains("<key>Label</key><string>Echo&amp;Agent</string>"));
        assert!(data.contains(
            "<string>/Applications/Echo &amp; Agent.app/Contents/MacOS/echoagent</string>"
        ));
        assert!(data.contains("<string>/tmp/中文 空格 &amp; &lt;资料&gt; &quot;引用&quot; &apos;单引号&apos;</string>"));
        assert_eq!(data.matches("<string>").count(), 5);
        assert!(data.contains("<string>--background</string><string>--data-home</string>"));
    }
}
