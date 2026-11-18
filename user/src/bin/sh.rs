//! Banna kabuğu: satır düzenleme ve geçmiş, boru hatları, yönlendirmeler,
//! `;` `&&` `||` `&`, arka plan işleri. `/bin`'de olmayan tanı komutları
//! (`ps`, `net`, `ping`...) çekirdekten çalıştırılır.

#![no_std]
#![no_main]

use banna::fs::{self, File};
use banna::io;
use banna::process::{self, SIGINT, SIGQUIT};
use banna::{String, Vec, format, print, println, tty, vec};

banna::entry!(main);

const HISTORY_LIMIT: usize = 100;
const BUILTINS: &[(&str, &str)] = &[
    ("cd", "change directory: cd [dir]"),
    ("pwd", "print the working directory"),
    ("echo", "print the arguments"),
    ("clear", "clear the screen"),
    ("kill", "send a signal: kill [-SIGNAL] <pid|%job>"),
    ("jobs", "list background jobs"),
    ("history", "list previous commands"),
    ("help", "this list"),
    ("exit", "leave the shell: exit [code]"),
];

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Pipe,
    And,
    Or,
    Semi,
    Background,
    Out,
    Append,
    In,
}

#[derive(Default, Debug)]
struct Command {
    args: Vec<String>,
    input: Option<String>,
    output: Option<(String, bool)>,
}

#[derive(Debug)]
struct Pipeline {
    commands: Vec<Command>,
    background: bool,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Link {
    Always,
    And,
    Or,
}

struct Job {
    number: usize,
    pgid: u64,
    pids: Vec<u64>,
    text: String,
}

struct Shell {
    history: Vec<String>,
    jobs: Vec<Job>,
    next_job: usize,
    status: i64,
    interactive: bool,
    /// Önceki satır teslim edilirken aynı okumada gelmiş, henüz işlenmemiş baytlar.
    typeahead: Vec<u8>,
    home: String,
}

fn tokenize(line: &str) -> Result<Vec<Token>, &'static str> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    let mut word = String::new();
    let mut in_word = false;
    let flush = |word: &mut String, in_word: &mut bool, tokens: &mut Vec<Token>| {
        if *in_word {
            tokens.push(Token::Word(core::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => flush(&mut word, &mut in_word, &mut tokens),
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unterminated quote"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(c) = chars.next() {
                                word.push(c);
                            }
                        }
                        Some(c) => word.push(c),
                        None => return Err("unterminated quote"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    word.push(c);
                }
            }
            '|' | '&' | ';' | '>' | '<' => {
                flush(&mut word, &mut in_word, &mut tokens);
                let token = match (c, chars.peek()) {
                    ('|', Some('|')) => {
                        chars.next();
                        Token::Or
                    }
                    ('&', Some('&')) => {
                        chars.next();
                        Token::And
                    }
                    ('>', Some('>')) => {
                        chars.next();
                        Token::Append
                    }
                    ('|', _) => Token::Pipe,
                    ('&', _) => Token::Background,
                    (';', _) => Token::Semi,
                    ('>', _) => Token::Out,
                    _ => Token::In,
                };
                tokens.push(token);
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut word, &mut in_word, &mut tokens);
    Ok(tokens)
}

/// Satırı `;`/`&&`/`||` ile bağlı boru hatlarına böler.
fn parse(tokens: Vec<Token>, line: &str) -> Result<Vec<(Link, Pipeline)>, &'static str> {
    let mut out = Vec::new();
    let mut link = Link::Always;
    let mut commands = Vec::new();
    let mut command = Command::default();
    let mut tokens = tokens.into_iter().peekable();
    let finish = |command: &mut Command, commands: &mut Vec<Command>| -> Result<(), &'static str> {
        if command.args.is_empty() {
            if command.input.is_some() || command.output.is_some() || !commands.is_empty() {
                return Err("syntax error: missing command");
            }
            return Ok(());
        }
        commands.push(core::mem::take(command));
        Ok(())
    };
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(w) => command.args.push(w),
            Token::Out | Token::Append | Token::In => {
                let Some(Token::Word(target)) = tokens.next() else {
                    return Err("syntax error: missing file name");
                };
                match token {
                    Token::In => command.input = Some(target),
                    _ => command.output = Some((target, token == Token::Append)),
                }
            }
            Token::Pipe => {
                if command.args.is_empty() {
                    return Err("syntax error near '|'");
                }
                finish(&mut command, &mut commands)?;
            }
            Token::Semi | Token::And | Token::Or | Token::Background => {
                finish(&mut command, &mut commands)?;
                if commands.is_empty() {
                    if token == Token::Semi && out.is_empty() {
                        continue;
                    }
                    return Err("syntax error: empty command");
                }
                out.push((
                    link,
                    Pipeline {
                        commands: core::mem::take(&mut commands),
                        background: token == Token::Background,
                        text: String::from(line.trim()),
                    },
                ));
                link = match token {
                    Token::And => Link::And,
                    Token::Or => Link::Or,
                    _ => Link::Always,
                };
            }
        }
    }
    finish(&mut command, &mut commands)?;
    if !commands.is_empty() {
        out.push((
            link,
            Pipeline {
                commands,
                background: false,
                text: String::from(line.trim()),
            },
        ));
    } else if link != Link::Always {
        return Err("syntax error: missing command");
    }
    Ok(out)
}

fn resolve(name: &str) -> Option<String> {
    let candidates = if name.contains('/') {
        vec![fs::normalize(&process::cwd(), name)]
    } else {
        // Komutlar /bin'de, masaüstü uygulamaları kendi klasörlerinde.
        vec![format!("/bin/{}", name), format!("/apps/{}/{}", name, name)]
    };
    candidates
        .into_iter()
        .find(|path| fs::stat(path).is_ok_and(|s| s.kind == fs::KIND_FILE))
}

fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(n, _)| *n == name)
}

fn signal_number(name: &str) -> Option<u32> {
    match name {
        "INT" | "SIGINT" => Some(SIGINT),
        "QUIT" | "SIGQUIT" => Some(SIGQUIT),
        "KILL" | "SIGKILL" => Some(process::SIGKILL),
        "TERM" | "SIGTERM" => Some(process::SIGTERM),
        "HUP" | "SIGHUP" => Some(1),
        n => n.parse().ok(),
    }
}

impl Shell {
    fn prompt(&self) -> String {
        let cwd = process::cwd();
        let shown = match cwd.strip_prefix(self.home.as_str()) {
            Some(rest) if !self.home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
                format!("~{}", rest)
            }
            _ => cwd,
        };
        let sign = if process::uid() == 0 { '#' } else { '$' };
        format!(
            "\x1b[92m{}@banna\x1b[0m:\x1b[94m{}\x1b[0m{} ",
            process::username(),
            shown,
            sign
        )
    }

    fn expand(&self, word: &str) -> String {
        if word == "~" {
            return self.home.clone();
        }
        if let Some(rest) = word.strip_prefix("~/") {
            return format!("{}/{}", self.home, rest);
        }
        if word == "$?" {
            return format!("{}", self.status);
        }
        String::from(word)
    }

    fn report_jobs(&mut self) {
        let mut finished = Vec::new();
        for (i, job) in self.jobs.iter().enumerate() {
            let codes: Vec<Option<i64>> = job.pids.iter().map(|&p| process::try_wait(p)).collect();
            if codes.iter().all(|c| c.is_some()) {
                let code = codes.last().copied().flatten().unwrap_or(0);
                finished.push((i, code));
            }
        }
        for (i, code) in finished.into_iter().rev() {
            let job = self.jobs.remove(i);
            let state = process::describe_exit(code).unwrap_or_else(|| String::from("done"));
            println!("[{}] {}  {}", job.number, state, job.text);
        }
    }

    /// Ham kipte satır düzenleyici. `None` = Ctrl+D (çıkış).
    fn read_line(&mut self) -> Option<String> {
        let prompt = self.prompt();
        print!("{}", prompt);
        let old_mode = tty::set_mode(0, 0).unwrap_or(tty::DEFAULT_MODE);
        let mut line: Vec<char> = Vec::new();
        let mut cursor = 0usize;
        let mut browse = self.history.len();
        let mut draft: Vec<char> = Vec::new();
        let mut pending: Vec<u8> = core::mem::take(&mut self.typeahead);
        let redraw = |line: &[char], cursor: usize| {
            let text: String = line.iter().collect();
            let back = line.len() - cursor;
            if back > 0 {
                print!("\r{}{}\x1b[K\x1b[{}D", prompt, text, back);
            } else {
                print!("\r{}{}\x1b[K", prompt, text);
            }
        };
        let mut first = true;
        let result = loop {
            if !(first && !pending.is_empty()) {
                let mut buf = [0u8; 64];
                let n = io::read_fd(0, &mut buf);
                if n == 0 {
                    break None;
                }
                if n < 0 {
                    continue;
                }
                pending.extend_from_slice(&buf[..n as usize]);
            }
            first = false;
            let mut submit = false;
            let mut quit = false;
            while !pending.is_empty() && !submit && !quit {
                if pending[0] == 0x1b {
                    // ESC [ parametreler son-bayt  ya da  ESC O harf (F1-F4, Home/End).
                    let Some((consumed, params, final_byte)) = escape_sequence(&pending) else {
                        if pending.len() > 16 {
                            pending.drain(..1);
                            continue;
                        }
                        break;
                    };
                    pending.drain(..consumed);
                    let number = |i: usize| -> u32 {
                        params
                            .split(';')
                            .nth(i)
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(0)
                    };
                    let ctrl = number(1) == 5;
                    let key = match (final_byte, number(0)) {
                        (b'~', 1 | 7) => b'H',
                        (b'~', 4 | 8) => b'F',
                        (b'~', 3) => b'~',
                        (b'~', _) => 0,
                        (b'C', _) if ctrl => b'w',
                        (b'D', _) if ctrl => b'b',
                        (k, _) => k,
                    };
                    match key {
                        b'A' | b'B' => {
                            if browse == self.history.len() {
                                draft = line.clone();
                            }
                            browse = if key == b'A' {
                                browse.saturating_sub(1)
                            } else {
                                (browse + 1).min(self.history.len())
                            };
                            line = match self.history.get(browse) {
                                Some(h) => h.chars().collect(),
                                None => draft.clone(),
                            };
                            cursor = line.len();
                        }
                        b'C' => cursor = (cursor + 1).min(line.len()),
                        b'D' => cursor = cursor.saturating_sub(1),
                        b'w' => cursor = word_end(&line, cursor),
                        b'b' => cursor = word_start(&line, cursor),
                        b'H' => cursor = 0,
                        b'F' => cursor = line.len(),
                        b'~' if cursor < line.len() => {
                            line.remove(cursor);
                        }
                        _ => {}
                    }
                    redraw(&line, cursor);
                    continue;
                }
                let text_end = pending
                    .iter()
                    .position(|&b| b < 0x20 || b == 0x7f)
                    .unwrap_or(pending.len());
                if text_end > 0 {
                    let valid = match core::str::from_utf8(&pending[..text_end]) {
                        Ok(_) => text_end,
                        Err(e) if e.error_len().is_none() => e.valid_up_to(),
                        Err(e) => e.valid_up_to() + e.error_len().unwrap_or(1),
                    };
                    if valid == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                    pending.drain(..valid);
                    for c in text.chars() {
                        line.insert(cursor, c);
                        cursor += 1;
                    }
                    redraw(&line, cursor);
                    continue;
                }
                let byte = pending.remove(0);
                match byte {
                    b'\r' | b'\n' => submit = true,
                    0x7f | 0x08 => {
                        if cursor > 0 {
                            cursor -= 1;
                            line.remove(cursor);
                            redraw(&line, cursor);
                        }
                    }
                    0x03 => {
                        print!("^C\n");
                        line.clear();
                        cursor = 0;
                        print!("{}", prompt);
                    }
                    0x04 => {
                        if line.is_empty() {
                            quit = true;
                        } else if cursor < line.len() {
                            line.remove(cursor);
                            redraw(&line, cursor);
                        }
                    }
                    0x01 => {
                        cursor = 0;
                        redraw(&line, cursor);
                    }
                    0x05 => {
                        cursor = line.len();
                        redraw(&line, cursor);
                    }
                    0x15 => {
                        line.drain(..cursor);
                        cursor = 0;
                        redraw(&line, cursor);
                    }
                    0x0b => {
                        line.truncate(cursor);
                        redraw(&line, cursor);
                    }
                    0x0c => {
                        print!("\x1b[2J\x1b[H");
                        redraw(&line, cursor);
                    }
                    b'\t' => self.complete(&mut line, &mut cursor, &prompt),
                    _ => {}
                }
            }
            if quit {
                println!();
                break None;
            }
            if submit {
                println!();
                break Some(line.iter().collect());
            }
        };
        self.typeahead = pending;
        let _ = tty::set_mode(0, old_mode);
        result
    }

    /// Sekme tamamlama: ilk sözcükte komut, sonrakilerde dosya yolu.
    fn complete(&self, line: &mut Vec<char>, cursor: &mut usize, prompt: &str) {
        let before: String = line[..*cursor].iter().collect();
        let start = before.rfind(' ').map_or(0, |i| i + 1);
        let word = &before[start..];
        let mut candidates: Vec<String> = Vec::new();
        if start == 0 && !word.contains('/') {
            for (name, _) in BUILTINS {
                candidates.push(String::from(*name));
            }
            if let Ok(entries) = fs::read_dir("/bin") {
                candidates.extend(entries.into_iter().map(|(n, _)| n));
            }
            candidates.retain(|c| c.starts_with(word));
        } else {
            let (dir, prefix) = match word.rfind('/') {
                Some(i) => (&word[..=i], &word[i + 1..]),
                None => ("", word),
            };
            let base = fs::normalize(&process::cwd(), if dir.is_empty() { "." } else { dir });
            if let Ok(entries) = fs::read_dir(&base) {
                for (name, stat) in entries {
                    if name.starts_with(prefix) {
                        let slash = if stat.kind == fs::KIND_DIRECTORY {
                            "/"
                        } else {
                            ""
                        };
                        candidates.push(format!("{}{}{}", dir, name, slash));
                    }
                }
            }
        }
        candidates.sort();
        candidates.dedup();
        let insert = match candidates.len() {
            0 => return,
            1 => {
                let c = &candidates[0];
                let space = if c.ends_with('/') { "" } else { " " };
                format!("{}{}", &c[word.len()..], space)
            }
            _ => {
                let first = &candidates[0];
                let common = candidates.iter().fold(first.len(), |n, c| {
                    first
                        .chars()
                        .zip(c.chars())
                        .take_while(|(a, b)| a == b)
                        .count()
                        .min(n)
                });
                if common > word.len() {
                    String::from(&first[word.len()..common])
                } else {
                    println!();
                    println!("{}", candidates.join("  "));
                    String::new()
                }
            }
        };
        for c in insert.chars() {
            line.insert(*cursor, c);
            *cursor += 1;
        }
        let text: String = line.iter().collect();
        let back = line.len() - *cursor;
        print!("\r{}{}\x1b[K", prompt, text);
        if back > 0 {
            print!("\x1b[{}D", back);
        }
    }

    fn run_line(&mut self, line: &str) -> Option<i32> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        io::log(&format!("shell: {}", line));
        let tokens = match tokenize(line) {
            Ok(t) => t,
            Err(e) => {
                println!("sh: {}", e);
                self.status = 2;
                return None;
            }
        };
        let tokens = tokens
            .into_iter()
            .map(|t| match t {
                Token::Word(w) => Token::Word(self.expand(&w)),
                t => t,
            })
            .collect();
        let pipelines = match parse(tokens, line) {
            Ok(p) => p,
            Err(e) => {
                println!("sh: {}", e);
                self.status = 2;
                return None;
            }
        };
        for (link, pipeline) in pipelines {
            let skip = match link {
                Link::Always => false,
                Link::And => self.status != 0,
                Link::Or => self.status == 0,
            };
            if skip {
                continue;
            }
            if let Some(code) = self.run_pipeline(pipeline) {
                return Some(code);
            }
        }
        None
    }

    /// `Some(code)` yalnızca `exit` için.
    fn run_pipeline(&mut self, pipeline: Pipeline) -> Option<i32> {
        if pipeline.commands.len() == 1 && !pipeline.background {
            let command = &pipeline.commands[0];
            let name = command.args[0].as_str();
            if command.input.is_none() && command.output.is_none() && is_builtin(name) {
                let args: Vec<&str> = command.args.iter().map(|s| s.as_str()).collect();
                if name == "exit" {
                    return Some(args.get(1).and_then(|c| c.parse().ok()).unwrap_or(0));
                }
                self.status = self.builtin(&args) as i64;
                return None;
            }
        }
        let count = pipeline.commands.len();
        let mut pids = Vec::new();
        let mut pgid = 0u64;
        let mut previous_read: Option<u64> = None;
        for (i, command) in pipeline.commands.iter().enumerate() {
            let next = if i + 1 < count {
                match io::pipe() {
                    Ok(p) => Some(p),
                    Err(e) => {
                        println!("sh: pipe: {}", fs::error_text(e));
                        break;
                    }
                }
            } else {
                None
            };
            match process::fork() {
                Ok(0) => {
                    process::setpgid(0, pgid);
                    if let Some(read) = previous_read {
                        let _ = io::dup2(read, 0);
                        io::close(read);
                    }
                    if let Some((read, write)) = next {
                        let _ = io::dup2(write, 1);
                        io::close(write);
                        io::close(read);
                    }
                    process::exit(self.child(command, pipeline.background));
                }
                Ok(pid) => {
                    if pgid == 0 {
                        pgid = pid;
                    }
                    process::setpgid(pid, pgid);
                    pids.push(pid);
                }
                Err(e) => {
                    println!("sh: fork: {}", fs::error_text(e));
                    break;
                }
            }
            if let Some(read) = previous_read.take() {
                io::close(read);
            }
            if let Some((read, write)) = next {
                io::close(write);
                previous_read = Some(read);
            }
        }
        if let Some(read) = previous_read {
            io::close(read);
        }
        if pids.is_empty() {
            self.status = 1;
            return None;
        }
        if pipeline.background {
            let number = self.next_job;
            self.next_job += 1;
            println!("[{}] {}", number, pids.last().copied().unwrap_or(0));
            self.jobs.push(Job {
                number,
                pgid,
                pids,
                text: pipeline.text,
            });
            self.status = 0;
            return None;
        }
        if self.interactive {
            tty::set_foreground(0, pgid);
        }
        let mut code = 0;
        for &pid in &pids {
            code = process::wait(pid);
        }
        if self.interactive {
            tty::set_foreground(0, process::pgid());
        }
        if code < 0
            && code != -(SIGINT as i64)
            && let Some(text) = process::describe_exit(code)
        {
            println!("[{}]", text);
        }
        self.status = code;
        None
    }

    /// Çocuk işlemde: yönlendirmeleri kurar ve komutu çalıştırır.
    fn child(&mut self, command: &Command, background: bool) -> i32 {
        process::ignore_signals(if background {
            (1 << SIGINT) | (1 << SIGQUIT)
        } else {
            0
        });
        if let Some(path) = &command.input {
            match File::open(path, fs::READ) {
                Ok(file) => {
                    let _ = io::dup2(file.fd(), 0);
                }
                Err(e) => {
                    banna::eprintln!("sh: {}: {}", path, fs::error_text(e));
                    return 1;
                }
            }
        }
        if let Some((path, append)) = &command.output {
            let flags = fs::WRITE | fs::CREATE | if *append { fs::APPEND } else { fs::TRUNCATE };
            match File::open(path, flags) {
                Ok(file) => {
                    let _ = io::dup2(file.fd(), 1);
                }
                Err(e) => {
                    banna::eprintln!("sh: {}: {}", path, fs::error_text(e));
                    return 1;
                }
            }
        }
        let args: Vec<&str> = command.args.iter().map(|s| s.as_str()).collect();
        let name = args[0];
        if is_builtin(name) {
            return self.builtin(&args);
        }
        if let Some(path) = resolve(name) {
            let code = process::exec(&path, &args);
            banna::eprintln!("sh: {}: {}", name, fs::error_text(code));
            return 126;
        }
        if !name.contains('/') && process::kcmd(&args.join(" ")).is_ok() {
            return 0;
        }
        banna::eprintln!("sh: {}: command not found", name);
        127
    }

    fn builtin(&mut self, args: &[&str]) -> i32 {
        match args[0] {
            "cd" => {
                let target = args.get(1).copied().unwrap_or(self.home.as_str());
                let target = if target.is_empty() { "/" } else { target };
                match process::chdir(target) {
                    Ok(()) => 0,
                    Err(e) => {
                        println!("cd: {}: {}", target, fs::error_text(e));
                        1
                    }
                }
            }
            "pwd" => {
                println!("{}", process::cwd());
                0
            }
            "echo" => {
                let (newline, words) = match args.get(1) {
                    Some(&"-n") => (false, &args[2..]),
                    _ => (true, &args[1..]),
                };
                print!("{}{}", words.join(" "), if newline { "\n" } else { "" });
                0
            }
            "clear" => {
                print!("\x1b[2J\x1b[3J\x1b[H");
                0
            }
            "history" => {
                for (i, line) in self.history.iter().enumerate() {
                    println!("{:>4}  {}", i + 1, line);
                }
                0
            }
            "jobs" => {
                self.report_jobs();
                for job in &self.jobs {
                    println!("[{}] running  {}", job.number, job.text);
                }
                0
            }
            "kill" => self.kill(&args[1..]),
            "help" => {
                println!("Built-in commands:");
                for (name, help) in BUILTINS {
                    println!("  {:<10} {}", name, help);
                }
                if let Ok(entries) = fs::read_dir("/bin") {
                    let names: Vec<String> = entries.into_iter().map(|(n, _)| n).collect();
                    println!("Programs in /bin:");
                    println!("  {}", names.join(" "));
                }
                let _ = process::kcmd("help");
                println!("Syntax: a | b, a > file, a >> file, a < file, a && b, a || b, a; b, a &");
                0
            }
            _ => 127,
        }
    }

    fn kill(&mut self, args: &[&str]) -> i32 {
        let (signal, targets) = match args.first() {
            Some(s) if s.starts_with('-') && args.len() > 1 => match signal_number(&s[1..]) {
                Some(n) => (n, &args[1..]),
                None => {
                    println!("kill: unknown signal {}", s);
                    return 1;
                }
            },
            _ => (process::SIGTERM, args),
        };
        if targets.is_empty() {
            println!("usage: kill [-SIGNAL] <pid|%job>...");
            return 1;
        }
        let mut status = 0;
        for target in targets {
            let id = if let Some(job) = target.strip_prefix('%') {
                match job
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| self.jobs.iter().find(|j| j.number == n))
                {
                    Some(job) => -(job.pgid as i64),
                    None => {
                        println!("kill: no job {}", target);
                        status = 1;
                        continue;
                    }
                }
            } else {
                match target.parse::<i64>() {
                    Ok(pid) => pid,
                    Err(_) => {
                        println!("kill: invalid pid {}", target);
                        status = 1;
                        continue;
                    }
                }
            };
            match process::signal(id, signal) {
                Ok(()) => {}
                Err(process::EPERM) => {
                    println!("kill: {}: permission denied", target);
                    status = 1;
                }
                Err(_) => {
                    println!("kill: {}: no such process", target);
                    status = 1;
                }
            }
        }
        status
    }

    fn remember(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || self.history.last().is_some_and(|l| l == line) {
            return;
        }
        if self.history.len() == HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(String::from(line));
    }
}

/// Etkileşimsiz okuma: stdin'den satır satır (dosya, boru ya da kanonik tty).
fn read_lines(fd: u64) -> Vec<String> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        let n = io::read_fd(fd, &mut buf);
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    String::from_utf8_lossy(&data)
        .lines()
        .map(String::from)
        .collect()
}

fn main(args: &[&str]) -> i32 {
    let user = process::username();
    let home = format!("/home/{}", user);
    let home = if fs::stat(&home).is_ok() {
        home
    } else {
        String::from("/")
    };
    let mut shell = Shell {
        history: Vec::new(),
        jobs: Vec::new(),
        next_job: 1,
        status: 0,
        interactive: false,
        typeahead: Vec::new(),
        home,
    };

    match args.get(1).copied() {
        Some("-c") => {
            let line = args[2..].join(" ");
            shell.run_line(&line);
            return shell.status as i32;
        }
        Some(path) => {
            let fd = match File::open(path, fs::READ) {
                Ok(file) => file,
                Err(e) => {
                    println!("sh: {}: {}", path, fs::error_text(e));
                    return 127;
                }
            };
            for line in read_lines(fd.fd()) {
                if let Some(code) = shell.run_line(&line) {
                    return code;
                }
            }
            return shell.status as i32;
        }
        None => {}
    }

    shell.interactive = tty::is_tty(0);
    if !shell.interactive {
        for line in read_lines(0) {
            if let Some(code) = shell.run_line(&line) {
                return code;
            }
        }
        return shell.status as i32;
    }

    process::ignore_signals((1 << SIGINT) | (1 << SIGQUIT));
    tty::set_foreground(0, process::pgid());
    if shell.home != "/" {
        let _ = process::chdir(&shell.home);
    }
    io::log("sh: ready");
    println!("\x1b[96mBanna OS\x1b[0m - type 'help' for commands.");
    loop {
        shell.report_jobs();
        let Some(line) = shell.read_line() else {
            return 0;
        };
        shell.remember(&line);
        if let Some(code) = shell.run_line(&line) {
            return code;
        }
    }
}

/// Tam bir kaçış dizisi: (bayt sayısı, parametreler, son bayt); eksikse `None`.
fn escape_sequence(bytes: &[u8]) -> Option<(usize, String, u8)> {
    match bytes.get(1)? {
        b'[' => {
            let end = bytes[2..].iter().position(|b| (0x40..=0x7e).contains(b))? + 2;
            let params = String::from_utf8_lossy(&bytes[2..end]).into_owned();
            Some((end + 1, params, bytes[end]))
        }
        b'O' => Some((3, String::new(), *bytes.get(2)?)),
        _ => Some((2, String::new(), 0)),
    }
}

fn word_end(line: &[char], mut cursor: usize) -> usize {
    while cursor < line.len() && !line[cursor].is_alphanumeric() {
        cursor += 1;
    }
    while cursor < line.len() && line[cursor].is_alphanumeric() {
        cursor += 1;
    }
    cursor
}

fn word_start(line: &[char], mut cursor: usize) -> usize {
    while cursor > 0 && !line[cursor - 1].is_alphanumeric() {
        cursor -= 1;
    }
    while cursor > 0 && line[cursor - 1].is_alphanumeric() {
        cursor -= 1;
    }
    cursor
}
