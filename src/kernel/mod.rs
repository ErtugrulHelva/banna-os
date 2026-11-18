//! Çekirdek çekirdeği: başlatma, syscall'lar, kullanıcı kipi ve kabuk.

pub mod crash;
pub mod elf;
pub mod initcall;
pub mod module;
pub mod pipe;
pub mod pty;
pub mod recovery;
pub mod rescue;
pub mod services;
pub mod shell;
pub mod syscall;
pub mod update;
pub mod usermode;
pub mod users;
pub mod watchdog;
