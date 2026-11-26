//! Desktop-owned process protection. This gate prevents accidental destructive
//! shell commands; it is not an OS sandbox for arbitrary native programs.
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
    /// OS creation time, not a model-supplied PID or command-line substring.
    pub started: String,
}

type Snapshot = fn() -> Result<Vec<ProcessIdentity>, String>;
struct Protection {
    host: u32,
    snapshot: Snapshot,
    roots: HashMap<u32, (String, usize)>,
}
static PROTECTION: OnceLock<Mutex<Protection>> = OnceLock::new();

pub fn install(snapshot: Snapshot) {
    PROTECTION.get_or_init(|| {
        Mutex::new(Protection {
            host: std::process::id(),
            snapshot,
            roots: HashMap::new(),
        })
    });
}

/// Hold this lease for the lifetime of an infrastructure child. Creation-time
/// checks prevent a recycled PID from becoming a permanently protected process.
pub struct ProcessLease {
    pid: u32,
    started: String,
}
impl Drop for ProcessLease {
    fn drop(&mut self) {
        if let Some(state) = PROTECTION.get() {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((started, count)) = state.roots.get_mut(&self.pid) {
                if *started == self.started {
                    *count -= 1;
                    if *count == 0 {
                        state.roots.remove(&self.pid);
                    }
                }
            }
        }
    }
}

pub fn protect_tree(pid: u32) -> Result<Option<ProcessLease>, String> {
    let Some(state) = PROTECTION.get() else {
        return Ok(None);
    };
    let snapshot = state.lock().unwrap_or_else(|e| e.into_inner()).snapshot;
    let process = snapshot()?
        .into_iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("无法确认待保护进程 {pid} 的身份"))?;
    if process.started.is_empty() {
        return Err("无法读取进程创建时间".into());
    }
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let entry = state
        .roots
        .entry(pid)
        .or_insert_with(|| (process.started.clone(), 0));
    if entry.0 != process.started {
        *entry = (process.started.clone(), 0);
    }
    entry.1 += 1;
    Ok(Some(ProcessLease {
        pid,
        started: process.started,
    }))
}

fn basename(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_owned()
}

#[derive(Debug)]
struct Word {
    value: String,
    command: bool,
}

/// Remove here-document data before classifying executable words. Only command
/// substitutions in an unquoted here-document are executed by the shell.
fn executable_text(script: &str) -> String {
    let mut lines = script.split_inclusive('\n');
    let mut output = String::new();
    let mut quote = None;
    while let Some(line) = lines.next() {
        output.push_str(line);
        let mut chars = line.chars().peekable();
        let mut documents = Vec::new();
        while let Some(ch) = chars.next() {
            if ch == '\\' && quote != Some('\'') {
                chars.next();
                continue;
            }
            if let Some(active) = quote {
                if ch == active {
                    quote = None;
                }
                continue;
            }
            if matches!(ch, '\'' | '"') {
                quote = Some(ch);
                continue;
            }
            if ch == '#' {
                break;
            }
            if ch != '<' || chars.peek() != Some(&'<') {
                continue;
            }
            chars.next();
            if chars.peek() == Some(&'<') {
                chars.next();
                continue;
            }
            let tabs = chars.peek() == Some(&'-');
            if tabs {
                chars.next();
            }
            while chars.peek().is_some_and(|c| matches!(c, ' ' | '\t')) {
                chars.next();
            }
            let mut delimiter = String::new();
            let mut literal = false;
            while let Some(ch) = chars.peek().copied() {
                if ch.is_whitespace() || matches!(ch, ';' | '&' | '|' | '<' | '>' | '(' | ')') {
                    break;
                }
                chars.next();
                if matches!(ch, '\'' | '"') {
                    literal = true;
                    for value in chars.by_ref() {
                        if value == ch {
                            break;
                        }
                        delimiter.push(value);
                    }
                } else if ch == '\\' {
                    literal = true;
                    if let Some(value) = chars.next() {
                        delimiter.push(value);
                    }
                } else {
                    delimiter.push(ch);
                }
            }
            if !delimiter.is_empty() {
                documents.push((delimiter, tabs, literal));
            }
        }
        for (delimiter, tabs, literal) in documents {
            let mut body = String::new();
            for line in lines.by_ref() {
                let value = line.strip_suffix('\n').unwrap_or(line);
                let value = if tabs {
                    value.trim_start_matches('\t')
                } else {
                    value
                };
                if value == delimiter {
                    break;
                }
                body.push_str(line);
            }
            output.push('\n');
            if !literal {
                for command in command_substitutions(&body) {
                    output.push_str(&command);
                    output.push('\n');
                }
            }
        }
    }
    output
}

fn command_substitutions(text: &str) -> Vec<String> {
    let mut chars = text.chars().peekable();
    let mut commands = Vec::new();
    while let Some(ch) = chars.next() {
        if (cfg!(unix) && ch == '\\') || (cfg!(windows) && ch == '`') {
            chars.next();
            continue;
        }
        if ch == '`' {
            let mut command = String::new();
            for value in chars.by_ref() {
                if value == '`' {
                    break;
                }
                command.push(value);
            }
            commands.push(command);
        } else if ch == '$' && chars.peek() == Some(&'(') {
            chars.next();
            let mut depth = 1;
            let mut command = String::new();
            let mut quote = None;
            while let Some(value) = chars.next() {
                if value == '\\' {
                    command.push(value);
                    if let Some(next) = chars.next() {
                        command.push(next);
                    }
                    continue;
                }
                if matches!(value, '\'' | '"') {
                    if quote == Some(value) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(value);
                    }
                }
                if quote.is_none() {
                    if value == '(' {
                        depth += 1;
                    }
                    if value == ')' {
                        depth -= 1;
                    }
                }
                if depth == 0 {
                    break;
                }
                command.push(value);
            }
            commands.push(command);
        }
    }
    commands
}

/// Retain command positions and unwrap explicit shell -c/-Command arguments.
/// Quoted text passed to echo/rg is data, not another executable command.
fn words(script: &str) -> Vec<Word> {
    let executable = executable_text(script);
    let mut result = Vec::new();
    let mut chars = executable.chars().peekable();
    let mut at_command = true;
    let mut program = String::new();
    let mut wrapper_argument = false;
    let mut redirection_argument = false;
    while let Some(ch) = chars.peek().copied() {
        if ch.is_whitespace() {
            chars.next();
            if ch == '\n' {
                at_command = true;
            }
            continue;
        }
        if matches!(ch, '<' | '>') {
            while chars.peek().is_some_and(|c| matches!(c, '<' | '>' | '&')) {
                chars.next();
            }
            redirection_argument = true;
            continue;
        }
        #[cfg(unix)]
        if ch == '`' {
            chars.next();
            let mut command = String::new();
            for value in chars.by_ref() {
                if value == '`' {
                    break;
                }
                command.push(value);
            }
            result.extend(words(&command));
            redirection_argument = false;
            continue;
        }
        if ch == '#' {
            for ch in chars.by_ref() {
                if ch == '\n' {
                    break;
                }
            }
            at_command = true;
            continue;
        }
        if matches!(ch, ';' | '|' | '&' | '(' | ')' | '{' | '}') {
            chars.next();
            redirection_argument = false;
            at_command = true;
            continue;
        }
        let mut value = String::new();
        let mut quoted = false;
        let mut nested_commands = Vec::new();
        while let Some(ch) = chars.peek().copied() {
            if ch.is_whitespace()
                || matches!(ch, ';' | '|' | '&' | '(' | ')' | '{' | '}' | '<' | '>')
            {
                break;
            }
            chars.next();
            if matches!(ch, '\'' | '"') {
                quoted = true;
                let quote = ch;
                let mut quoted_source = String::new();
                while let Some(ch) = chars.next() {
                    if ch == quote {
                        if chars.peek() == Some(&quote) {
                            chars.next();
                            value.push(quote);
                            continue;
                        }
                        break;
                    }
                    quoted_source.push(ch);
                    if ch == '`'
                        || (ch == '\\'
                            && quote == '"'
                            && chars.peek().is_some_and(|c| matches!(c, '"' | '\\' | '$')))
                    {
                        if let Some(next) = chars.next() {
                            quoted_source.push(next);
                            value.push(next);
                        }
                    } else {
                        value.push(ch);
                    }
                }
                if quote == '"' {
                    nested_commands.extend(command_substitutions(&quoted_source));
                }
            } else if ch == '`' {
                if let Some(next) = chars.next() {
                    value.push(next);
                }
            } else {
                value.push(ch);
            }
        }
        if redirection_argument {
            for command in nested_commands {
                result.extend(words(&command));
            }
            redirection_argument = false;
            continue;
        }
        if !value.is_empty()
            && value.chars().all(|ch| ch.is_ascii_digit())
            && chars.peek().is_some_and(|ch| matches!(ch, '<' | '>'))
        {
            continue;
        }
        let previous = result.last().map(|w: &Word| w.value.to_ascii_lowercase());
        let shell = matches!(
            program.as_str(),
            "powershell" | "pwsh" | "cmd" | "bash" | "sh" | "zsh" | "python" | "python3" | "node"
        );
        let unwrap = shell
            && matches!(
                previous.as_deref(),
                Some("-c" | "-lc" | "-command" | "/c" | "-e")
            )
            || matches!(
                previous.as_deref(),
                Some("iex" | "invoke-expression" | "eval")
            ) && result.last().is_some_and(|word| word.command);
        if unwrap && quoted {
            result.extend(words(&value));
            at_command = false;
            continue;
        }
        if wrapper_argument {
            result.push(Word {
                value,
                command: false,
            });
            wrapper_argument = false;
            continue;
        }
        // Environment assignments and wrapper flags do not consume the real
        // executable position (env FOO=bar sudo -n kill ..., sudo -u user ...).
        if at_command && (value.contains('=') || value.starts_with('-')) {
            wrapper_argument = matches!(program.as_str(), "sudo" | "env")
                && matches!(
                    value.as_str(),
                    "-u" | "-g" | "--user" | "--group" | "--unset" | "-C" | "--chdir"
                );
            result.push(Word {
                value,
                command: false,
            });
            continue;
        }
        let wrapper = at_command
            && matches!(
                basename(&value).as_str(),
                "sudo"
                    | "command"
                    | "exec"
                    | "nohup"
                    | "env"
                    | "builtin"
                    | "time"
                    | "if"
                    | "then"
                    | "elif"
                    | "else"
                    | "while"
                    | "until"
                    | "do"
                    | "!"
            );
        result.push(Word {
            value,
            command: at_command,
        });
        if at_command {
            program = basename(&result.last().unwrap().value);
        }
        for command in nested_commands {
            result.extend(words(&command));
        }
        at_command = wrapper;
    }
    result
}

fn terminates(word: &str) -> bool {
    matches!(
        basename(word).as_str(),
        "stop-process" | "spps" | "kill" | "killall" | "pkill" | "taskkill" | "pskill" | "tskill"
    )
}
fn opaque_termination(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    matches!(
        word.as_str(),
        "os.kill" | "os.killpg" | "process.kill" | "terminateprocess" | "ntterminateprocess"
    ) || word.ends_with(".kill")
        || word.ends_with(".terminate")
        || word.ends_with(".closemainwindow")
}

fn critical(name: &str) -> bool {
    matches!(
        basename(name).as_str(),
        "msedgewebview2"
            | "explorer"
            | "dwm"
            | "winlogon"
            | "lsass"
            | "csrss"
            | "services"
            | "windowserver"
            | "loginwindow"
            | "launchd"
    )
}

fn protected_processes<'a>(
    all: &'a [ProcessIdentity],
    host: u32,
    roots: &HashMap<u32, (String, usize)>,
) -> Vec<&'a ProcessIdentity> {
    let mut trees: HashSet<u32> = roots
        .iter()
        .filter_map(|(pid, (stamp, _))| {
            all.iter()
                .any(|p| p.pid == *pid && p.started == *stamp)
                .then_some(*pid)
        })
        .collect();
    // WebView2 is created by the native runtime rather than our child launcher.
    let mut descendants = HashSet::from([host]);
    for _ in 0..all.len() {
        let before = descendants.len();
        for p in all {
            if descendants.contains(&p.parent) {
                descendants.insert(p.pid);
            }
        }
        if descendants.len() == before {
            break;
        }
    }
    for p in all {
        if descendants.contains(&p.pid) && critical(&p.name) {
            trees.insert(p.pid);
        }
    }
    for _ in 0..all.len() {
        let before = trees.len();
        for p in all {
            if trees.contains(&p.parent) {
                trees.insert(p.pid);
            }
        }
        if trees.len() == before {
            break;
        }
    }
    all.iter()
        .filter(|p| p.pid == host || trees.contains(&p.pid) || critical(&p.name))
        .collect()
}

/// Parse targets separately from signal numbers and option values. An empty
/// target list represents a non-destructive signal-zero probe or signal list.
fn termination_targets(invocation: &str, args: &[Word]) -> Result<(Vec<u32>, bool), String> {
    let ambiguous = || "进程保护：无法确认终止操作的完整目标，请使用明确的正数 PID。".to_string();
    let parse_ids = |raw: &str| -> Result<Vec<u32>, String> {
        raw.split(',')
            .map(|value| {
                value
                    .parse::<u32>()
                    .ok()
                    .filter(|pid| *pid > 0)
                    .ok_or_else(ambiguous)
            })
            .collect()
    };
    let mut ids = Vec::new();
    let mut tree = false;
    let mut probe = false;
    let mut index = 0;
    match invocation {
        "kill" => {
            let mut signal = "TERM";
            if let Some(first) = args.first().map(|arg| arg.value.as_str()) {
                match first {
                    "-l" | "-L" | "--list" | "--table" => return Ok((ids, false)),
                    "-s" | "--signal" | "-n" => {
                        signal = args
                            .get(1)
                            .map(|arg| arg.value.as_str())
                            .ok_or_else(ambiguous)?;
                        index = 2;
                    }
                    "--" => index = 1,
                    value if value.starts_with("--signal=") => {
                        signal = &value[9..];
                        index = 1;
                    }
                    value if value.starts_with('-') => {
                        signal = &value[1..];
                        index = 1;
                    }
                    _ => {}
                }
            }
            if signal.is_empty() || !signal.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(ambiguous());
            }
            probe = signal == "0";
            if args.get(index).is_some_and(|arg| arg.value == "--") {
                index += 1;
            }
            for arg in &args[index..] {
                ids.extend(parse_ids(&arg.value)?);
            }
        }
        "stop-process" | "spps" => {
            let mut taking_ids = false;
            while let Some(arg) = args.get(index) {
                let value = arg.value.to_ascii_lowercase();
                match value.as_str() {
                    "-id" => taking_ids = true,
                    "-force" | "-passthru" | "-confirm:$false" | "-confirm:$true" => {
                        taking_ids = false
                    }
                    "-whatif" => {
                        probe = true;
                        taking_ids = false;
                    }
                    value if value.starts_with("-id:") => {
                        ids.extend(parse_ids(&value[4..])?);
                        taking_ids = false;
                    }
                    value if taking_ids && !value.starts_with('-') => {
                        // PowerShell permits spaces after commas in an array.
                        let raw = value.strip_suffix(',').unwrap_or(value);
                        ids.extend(parse_ids(raw)?);
                    }
                    _ => return Err(ambiguous()),
                }
                index += 1;
            }
        }
        "taskkill" => {
            while let Some(arg) = args.get(index) {
                match arg.value.to_ascii_lowercase().as_str() {
                    "/f" => {}
                    "/t" => tree = true,
                    "/pid" => {
                        index += 1;
                        ids.extend(parse_ids(&args.get(index).ok_or_else(ambiguous)?.value)?);
                    }
                    _ => return Err(ambiguous()),
                }
                index += 1;
            }
        }
        "pskill" | "tskill" => {
            for arg in args {
                if arg.value.eq_ignore_ascii_case("-t") {
                    tree = true;
                } else {
                    ids.extend(parse_ids(&arg.value)?);
                }
            }
        }
        _ => {
            return Err(
                "进程保护：不允许按名称批量终止进程，请先查询归属，再使用明确的 PID。".into(),
            )
        }
    }
    if ids.is_empty() {
        return Err(ambiguous());
    }
    if probe {
        ids.clear();
    }
    Ok((ids, tree))
}

fn evaluate(
    script: &str,
    all: &[ProcessIdentity],
    host: u32,
    roots: &HashMap<u32, (String, usize)>,
) -> Result<(), String> {
    let tokens = words(script);
    let protected = protected_processes(all, host, roots);
    for (index, word) in tokens.iter().enumerate() {
        if word.command && opaque_termination(&word.value) {
            return Err("进程保护：内联系统 API 终止操作无法核实目标，已阻止执行。请先核实 PID，再使用可检查的进程操作命令。".into());
        }
        if !word.command || !terminates(&word.value) {
            continue;
        }
        let end = tokens[index + 1..]
            .iter()
            .position(|w| w.command)
            .map_or(tokens.len(), |n| index + 1 + n);
        let args = &tokens[index + 1..end];
        let (ids, tree) = termination_targets(&basename(&word.value), args)?;
        for pid in ids {
            if protected.iter().any(|process| process.pid == pid) {
                return Err(format!("进程保护：PID {pid} 属于 EchoAgent 或系统关键进程，已阻止终止操作。请使用应用内的停止、重新连接或恢复功能。"));
            }
            if tree {
                for process in &protected {
                    let mut current = process.pid;
                    let mut visited = HashSet::new();
                    while visited.insert(current) {
                        if current == pid {
                            return Err(
                                "进程保护：该进程树包含客户端或系统关键进程，已阻止终止操作。"
                                    .into(),
                            );
                        }
                        let Some(parent) = all
                            .iter()
                            .find(|process| process.pid == current)
                            .map(|process| process.parent)
                        else {
                            break;
                        };
                        current = parent;
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn check_command(script: &str) -> Result<(), String> {
    check_command_in(script, &std::env::current_dir().unwrap_or_default())
}

pub fn check_command_in(script: &str, cwd: &std::path::Path) -> Result<(), String> {
    let Some(state) = PROTECTION.get() else {
        return Ok(());
    };
    let scripts = inspect_scripts(script, cwd)?;
    if !scripts.iter().any(|script| {
        words(script)
            .iter()
            .any(|w| w.command && (terminates(&w.value) || opaque_termination(&w.value)))
    }) {
        return Ok(());
    }
    let (host, snapshot, roots) = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        (state.host, state.snapshot, state.roots.clone())
    };
    let all = snapshot()
        .map_err(|error| format!("进程保护：无法核实进程身份，终止操作未执行：{error}"))?;
    for script in scripts {
        evaluate(&script, &all, host, &roots)?;
    }
    Ok(())
}

fn inspect_scripts(script: &str, cwd: &std::path::Path) -> Result<Vec<String>, String> {
    let mut scripts = vec![script.to_owned()];
    let mut visited = HashSet::new();
    // Inspect directly invoked script files too. Limit work/recursion and fail
    // closed for opaque encoded commands; ordinary read/search arguments remain
    // data. This intentionally does not claim to sandbox arbitrary executables.
    for index in 0..8 {
        let Some(script) = scripts.get(index) else {
            break;
        };
        let tokens = words(script);
        let encoded_shell = tokens
            .iter()
            .any(|w| w.command && matches!(basename(&w.value).as_str(), "powershell" | "pwsh"))
            && tokens.iter().any(|w| {
                matches!(
                    w.value.to_ascii_lowercase().as_str(),
                    "-encodedcommand" | "-enc" | "-ec" | "/encodedcommand"
                )
            });
        let filtered_kill = tokens
            .iter()
            .any(|w| w.command && basename(&w.value) == "taskkill")
            && tokens.iter().any(|w| w.value.eq_ignore_ascii_case("/fi"));
        if encoded_shell || filtered_kill {
            return Err(
                "进程保护：编码命令或动态进程筛选无法核实执行范围，请改用明文命令和明确的 PID。"
                    .into(),
            );
        }
        let mut host = false;
        let mut discovered = Vec::new();
        for token in &tokens {
            if token.command {
                host = matches!(
                    basename(&token.value).as_str(),
                    "powershell"
                        | "pwsh"
                        | "bash"
                        | "sh"
                        | "zsh"
                        | "cmd"
                        | "python"
                        | "python3"
                        | "node"
                );
            }
            let path = std::path::Path::new(&token.value);
            let script_file = path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                matches!(
                    x.to_ascii_lowercase().as_str(),
                    "ps1" | "bat" | "cmd" | "sh" | "py" | "js" | "mjs"
                )
            });
            if script_file && (token.command || host) {
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    cwd.join(path)
                };
                if let Ok(path) = path.canonicalize() {
                    if visited.insert(path.clone()) {
                        use std::io::Read;
                        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                        let mut content = String::new();
                        file.take(256 * 1024 + 1)
                            .read_to_string(&mut content)
                            .map_err(|_| {
                                "进程保护：脚本内容无法检查，请使用明文命令".to_string()
                            })?;
                        if content.len() > 256 * 1024 || scripts.len() + discovered.len() >= 8 {
                            return Err(
                                "进程保护：脚本超过检查范围，请拆分为可检查的明文命令".into()
                            );
                        }
                        discovered.push(content);
                    }
                }
            }
        }
        scripts.extend(discovered);
    }
    Ok(scripts)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn processes() -> Vec<ProcessIdentity> {
        [
            (10, 1, "terminal", "a"),
            (20, 10, "echoagent.exe", "b"),
            (30, 20, "msedgewebview2.exe", "c"),
            (31, 30, "msedgewebview2.exe", "d"),
            (40, 20, "node", "e"),
            (50, 20, "python", "f"),
            (60, 1, "notepad.exe", "g"),
        ]
        .into_iter()
        .map(|(pid, parent, name, started)| ProcessIdentity {
            pid,
            parent,
            name: name.into(),
            started: started.into(),
        })
        .collect()
    }
    fn check(script: &str) -> Result<(), String> {
        evaluate(
            script,
            &processes(),
            20,
            &HashMap::from([(40, ("e".into(), 1))]),
        )
    }
    #[test]
    fn shell_control_flow_and_substitutions_cannot_hide_termination() {
        #[cfg(unix)]
        for command in ["echo `kill 20`", "cat <<EOF\n`kill 20`\nEOF"] {
            assert!(check(command).is_err(), "allowed {command}");
        }
        #[cfg(windows)]
        assert!(check("echo \"\\$(Stop-Process -Id 20)\"").is_err());
        for command in [
            "if true; then kill -9 20; fi",
            "> /dev/null kill 20",
            "2>/dev/null kill 20",
            "cat <(kill 20)",
            "for n in 1; do kill -9 20; done",
            "while true; do builtin kill 20; done",
            "until false; do command kill 20; done",
            "if kill -0 20; then kill -TERM 20; fi",
            "echo \"$(kill -9 20)\"",
            "cat <<EOF\n$(kill 20)\nEOF",
            "cat <<'EOF'\ntext\nEOF\nkill 20",
            "cat <<'A' <<'B'\ntext\nA\ntext\nB\nkill 20",
        ] {
            assert!(check(command).is_err(), "allowed {command}");
        }
    }

    #[test]
    fn here_document_data_and_non_destructive_process_queries_remain_usable() {
        #[cfg(unix)]
        assert!(check("echo \"\\$(kill 20)\"").is_ok());
        #[cfg(windows)]
        assert!(check("echo \"`$(Stop-Process -Id 20)\"").is_ok());
        for command in [
            "cat > cleanup.sh <<'EOF'\npkill node\nEOF",
            "cat <<EOF\nkill 20 is documentation\nEOF",
            "cat <<-'EOF'\n\tStop-Process -Id 20\n\tEOF",
            "cat <<'A' <<'B'\nkill 20\nA\npkill node\nB",
            "cat <<'EOF'\n$(kill 20)\nEOF",
            "kill -0 20",
            "kill -0 20 2>/dev/null",
            "printf '%s\n' 'text\n<<EOF\n' ; kill -0 20",
            "kill -s 0 30",
            "kill --signal=0 40",
            "kill -l",
            "kill -l TERM",
            "Stop-Process -Id:60",
            "Stop-Process -Id:20 -WhatIf",
            "Stop-Process -Id 50, 60 -Force",
            "kill -9 60",
        ] {
            assert!(check(command).is_ok(), "blocked {command}");
        }
        let mut all = processes();
        all.push(ProcessIdentity {
            pid: 9,
            parent: 1,
            name: "lsass.exe".into(),
            started: "a".into(),
        });
        assert!(evaluate("kill -9 60", &all, 20, &HashMap::new()).is_ok());
        for command in [
            "Stop-Process -Id:20",
            "Stop-Process -Id 50, 20",
            "kill -s TERM 20",
            "taskkill /PID 10 /T",
            "pskill -t 10",
        ] {
            assert!(check(command).is_err(), "allowed {command}");
        }
    }

    #[test]
    fn prevents_reported_windows_bulk_kill_and_aliases() {
        for command in [
            "Get-Process msedgewebview2; Stop-Process -Name msedgewebview2 -Force",
            "taskkill /F /IM msedgewebview2.exe",
            "spps -Name explorer -Force",
            "Microsoft.PowerShell.Management\\Stop-Process -Id 20",
            "& 'C:\\Windows\\System32\\taskkill.exe' /PID 31 /F",
            "& \"C:\\Windows\\System32\\taskkill.exe\" /PID 31 /F",
            "powershell -Command 'Stop-Process -Name msedgewebview2 -Force'",
            "Get-Process msedgewebview2 | Stop-Process -Force",
            "Stop-Process -Id:20",
            "Stop-Process -Name:msedgewebview2",
            "python -c 'import os; os.kill(20, 9)'",
            "node -e 'process.kill(20)'",
            "Get-Process msedgewebview2 | ForEach-Object { $_.Kill() }",
            "[System.Diagnostics.Process]::GetProcessById(20).Kill()",
            "python -c 'p.terminate()'",
        ] {
            assert!(check(command).is_err(), "allowed {command}");
        }
    }
    #[test]
    fn protects_owned_infrastructure_but_allows_task_children() {
        for command in [
            "kill 20",
            "kill -9 30",
            "taskkill /PID 40 /F",
            "taskkill /PID 10 /T",
            "pkill node",
            "kill -1",
            "env FOO=bar sudo -n kill 20",
            "sudo -u root kill 20",
            "A=b kill 20",
            "kill -9 -50 60",
            "Stop-Process -Name notepad",
        ] {
            assert!(check(command).is_err(), "allowed {command}");
        }
        for command in [
            "kill 50",
            "taskkill /PID 60 /F",
            "Stop-Process -Id 60",
            "echo 'Stop-Process -Name msedgewebview2'",
            "echo -c 'Stop-Process -Name msedgewebview2'",
            "rg 'taskkill /PID 20' README.md",
            "Get-Process msedgewebview2",
        ] {
            assert!(check(command).is_ok(), "blocked {command}");
        }
    }
    #[test]
    fn inspects_invoked_scripts_and_rejects_opaque_windows_selectors() {
        let directory = std::env::temp_dir().join(format!(
            "echo-process-safety-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("danger.ps1"),
            "Stop-Process -Name msedgewebview2",
        )
        .unwrap();
        let scripts = inspect_scripts("powershell -File danger.ps1", &directory).unwrap();
        assert!(scripts.iter().any(|script| check(script).is_err()));
        for script in ["pwsh -EncodedCommand AA==", "taskkill /FI 'PID gt 0' /F"] {
            assert!(
                inspect_scripts(script, &directory).is_err(),
                "allowed {script}"
            );
        }
        for script in [
            "tasklist /FI 'PID gt 0'",
            "cat danger.ps1",
            "rg 'taskkill' danger.ps1",
        ] {
            assert_eq!(inspect_scripts(script, &directory).unwrap().len(), 1);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn rejects_ambiguous_targets_and_does_not_protect_reused_pid() {
        for command in [
            "Stop-Process -Id $pid",
            "pkill -f webview.*",
            "kill 0",
            "Stop-Process",
        ] {
            assert!(check(command).is_err());
        }
        assert!(evaluate(
            "kill 40",
            &processes(),
            20,
            &HashMap::from([(40, ("old".into(), 1))])
        )
        .is_ok());
    }
}
