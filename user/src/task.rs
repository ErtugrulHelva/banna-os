//! Arka planda çalışan komut: çıktısı bir boruyla yakalanır, arayüz donmaz
//! (ör. Banna Store'un çalıştırdığı `hatch install`).

use crate::io::{self, PollEntry};
use crate::{String, process};

pub struct Task {
    pid: u64,
    fd: u64,
    eof: bool,
    pub output: String,
    pub exit: Option<i64>,
}

impl Task {
    /// `path`'i `args` ile başlatır; stdout ve stderr yakalanır.
    pub fn start(path: &str, args: &[&str]) -> Result<Self, i64> {
        Self::spawn(path, args, None)
    }

    /// Yalnızca çocuk işlem yönetici olur (`password` ile); çağıran işlem yetkisiz kalır.
    /// Parola yanlışsa çocuk 126 koduyla çıkar.
    pub fn start_as_root(path: &str, args: &[&str], password: &str) -> Result<Self, i64> {
        Self::spawn(path, args, Some(password))
    }

    fn spawn(path: &str, args: &[&str], password: Option<&str>) -> Result<Self, i64> {
        let (read, write) = io::pipe()?;
        match process::fork()? {
            0 => {
                let _ = io::dup2(write, 1);
                let _ = io::dup2(write, 2);
                io::close(read);
                io::close(write);
                if let Some(password) = password
                    && process::uid() != 0
                    && let Err(e) = process::elevate(password)
                {
                    crate::eprintln!(
                        "{}",
                        if e == process::EPERM {
                            "Bu hesap yönetici değil."
                        } else {
                            "Parola yanlış."
                        }
                    );
                    process::exit(126);
                }
                process::exec(path, args);
                process::exit(127)
            }
            pid => {
                io::close(write);
                Ok(Self {
                    pid,
                    fd: read,
                    eof: false,
                    output: String::new(),
                    exit: None,
                })
            }
        }
    }

    /// `io::poll` için okuma ucu.
    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Hazır çıktıyı okur; bittiyse çıkış kodunu alır. Yeni bir şey olduysa `true`.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while !self.eof {
            let mut entry = [PollEntry::fd(self.fd)];
            if io::poll(&mut entry, Some(0)) <= 0 || entry[0].ready == 0 {
                break;
            }
            let mut buf = [0u8; 1024];
            let n = io::read_fd(self.fd, &mut buf);
            if n <= 0 {
                self.eof = true;
                io::close(self.fd);
            } else {
                self.output
                    .push_str(&String::from_utf8_lossy(&buf[..n as usize]));
            }
            changed = true;
        }
        if self.eof && self.exit.is_none() {
            self.exit = Some(process::wait(self.pid));
            changed = true;
        }
        changed
    }

    /// Çıktının son boş olmayan satırı (durum çubuğu için).
    pub fn last_line(&self) -> &str {
        self.output
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
    }
}
