//! Sistem servisleri: açılıştan sonra root olarak başlayan kullanıcı alanı programları.
//!
//! - `banna-agent`: `/etc/banna-agent/config` varsa (Banna Panel'den eşleştirilmiş cihaz)
//!   Banna ekosistemine sistem durumunu ve paket geçmişini bildirir, uzaktan kurulumları uygular.

use alloc::string::String;

const AGENT: &str = "/bin/banna-agent";
const AGENT_CONFIG: &str = "/etc/banna-agent/config";

pub fn start() {
    crate::sched::spawn_detached("services", || {
        if crate::fs::stat(AGENT_CONFIG).is_ok() && crate::fs::stat(AGENT).is_ok() {
            let args = [String::from("banna-agent"), String::from("run")];
            match crate::kernel::elf::spawn_with_uid(AGENT, &args, Some(0)) {
                Ok(_) => crate::log!("services: banna-agent started"),
                Err(e) => crate::log!("services: banna-agent failed: {}", e.message()),
            }
        }
        0
    });
}
