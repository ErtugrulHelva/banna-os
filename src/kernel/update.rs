//! `hatch upgrade` ile hazırlanan sistem/sürücü güncellemesi açılışta, oturum ve
//! servisler başlamadan `banna-update apply` ile kurulur. Çekirdek ya da yükleyici
//! değiştiyse yeni sürümle açılmak için hemen yeniden başlatılır.

use alloc::string::String;

const PENDING: &str = "/var/hatch/system/pending";
const TOOL: &str = "/bin/banna-update";
/// `banna::sysupdate::EXIT_REBOOT`
const EXIT_REBOOT: i64 = 3;

pub fn apply_pending() {
    if crate::fs::stat(PENDING).is_err() || crate::fs::stat(TOOL).is_err() {
        return;
    }
    crate::gfx::splash::set_step("Güncellemeler kuruluyor");
    crate::log!("update: applying the pending system update");
    let args = [String::from("banna-update"), String::from("apply")];
    let code = match crate::kernel::elf::spawn_with_uid(TOOL, &args, Some(0)) {
        Ok((_, done)) => done.wait(),
        Err(e) => {
            crate::log!("update: banna-update failed to start: {}", e.message());
            return;
        }
    };
    match code {
        0 => crate::log!("update: installed"),
        EXIT_REBOOT => {
            crate::log!("update: installed, restarting into the new kernel");
            crate::gfx::splash::set_step("Yeniden başlatılıyor");
            if crate::fs::stat("/boot/boot").is_ok() {
                crate::log::save_boot_log("/boot/boot/kernel.log");
            }
            crate::drivers::power::reboot();
        }
        code => crate::log!(
            "update: failed (exit {}), see /var/hatch/system/last-error",
            code
        ),
    }
}
