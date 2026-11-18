//! DesignWare I2C denetleyicisi (Intel LPSS "Serial IO" ve AMD FCH): yoklamalı ana
//! kip aktarımları. Kesme kullanılmaz; FIFO'lar komut göndererek doldurulup boşaltılır.

use core::ptr::{read_volatile, write_volatile};

use crate::arch::tsc;
use crate::sched::Mutex;

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_DATA_CMD: usize = 0x10;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_INTR_MASK: usize = 0x30;
const IC_RAW_INTR_STAT: usize = 0x34;
const IC_RX_TL: usize = 0x38;
const IC_TX_TL: usize = 0x3c;
const IC_CLR_INTR: usize = 0x40;
const IC_CLR_TX_ABRT: usize = 0x54;
const IC_CLR_STOP_DET: usize = 0x60;
const IC_ENABLE: usize = 0x6c;
const IC_TXFLR: usize = 0x74;
const IC_RXFLR: usize = 0x78;
const IC_SDA_HOLD: usize = 0x7c;
const IC_TX_ABRT_SOURCE: usize = 0x80;
const IC_ENABLE_STATUS: usize = 0x9c;
const IC_COMP_PARAM_1: usize = 0xf4;
const IC_COMP_TYPE: usize = 0xfc;

const COMP_TYPE_DW: u32 = 0x4457_0140;
const CON_MASTER: u32 = 1 << 0;
const CON_RESTART_EN: u32 = 1 << 5;
const CON_SLAVE_DISABLE: u32 = 1 << 6;
const CMD_READ: u32 = 1 << 8;
const CMD_STOP: u32 = 1 << 9;
const CMD_RESTART: u32 = 1 << 10;
const INTR_TX_ABRT: u32 = 1 << 6;
const INTR_STOP_DET: u32 = 1 << 9;

/// SCL yüksek/düşük süre sayaçları ve SDA bekletme (ACPI SSCN/FMCN paketleri).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub hcnt: u16,
    pub lcnt: u16,
    pub sda_hold: Option<u32>,
}

pub struct Controller {
    regs: *mut u8,
    rx_depth: usize,
    tx_depth: usize,
    lock: Mutex<()>,
}

unsafe impl Send for Controller {}
unsafe impl Sync for Controller {}

impl Controller {
    /// `regs`: eşlenmiş yazmaç alanı. `fast` verilirse 400 kHz, yoksa 100 kHz.
    pub fn new(
        regs: *mut u8,
        standard: Timing,
        fast: Option<Timing>,
    ) -> Result<Self, &'static str> {
        let c = Self {
            regs,
            rx_depth: 8,
            tx_depth: 8,
            lock: Mutex::new(()),
        };
        let kind = c.r(IC_COMP_TYPE);
        if kind != COMP_TYPE_DW {
            crate::log!("i2c: component type {:#x}, not DesignWare", kind);
            return Err("not a DesignWare I2C controller");
        }
        if !c.set_enabled(false) {
            return Err("controller did not disable");
        }
        let params = c.r(IC_COMP_PARAM_1);
        let (rx_depth, tx_depth) = if params == 0 || params == u32::MAX {
            (8, 8)
        } else {
            (
                ((params >> 8) & 0xff) as usize + 1,
                ((params >> 16) & 0xff) as usize + 1,
            )
        };
        let speed = match fast {
            Some(t) => {
                c.w(IC_FS_SCL_HCNT, t.hcnt.max(6) as u32);
                c.w(IC_FS_SCL_LCNT, t.lcnt.max(8) as u32);
                if let Some(hold) = t.sda_hold {
                    c.w(IC_SDA_HOLD, hold);
                }
                2
            }
            None => {
                c.w(IC_SS_SCL_HCNT, standard.hcnt.max(6) as u32);
                c.w(IC_SS_SCL_LCNT, standard.lcnt.max(8) as u32);
                if let Some(hold) = standard.sda_hold {
                    c.w(IC_SDA_HOLD, hold);
                }
                1
            }
        };
        c.w(
            IC_CON,
            CON_MASTER | speed << 1 | CON_RESTART_EN | CON_SLAVE_DISABLE,
        );
        c.w(IC_INTR_MASK, 0);
        c.w(IC_RX_TL, 0);
        c.w(IC_TX_TL, 0);
        Ok(Self {
            rx_depth: rx_depth.clamp(1, 256),
            tx_depth: tx_depth.clamp(1, 256),
            ..c
        })
    }

    fn r(&self, offset: usize) -> u32 {
        unsafe { read_volatile(self.regs.add(offset) as *const u32) }
    }

    fn w(&self, offset: usize, value: u32) {
        unsafe { write_volatile(self.regs.add(offset) as *mut u32, value) }
    }

    fn set_enabled(&self, on: bool) -> bool {
        let deadline = tsc::now_ns() + 25_000_000;
        loop {
            self.w(IC_ENABLE, on as u32);
            if (self.r(IC_ENABLE_STATUS) & 1 != 0) == on {
                return true;
            }
            if tsc::now_ns() > deadline {
                return false;
            }
            core::hint::spin_loop();
        }
    }

    /// `write` baytlarını gönderir, ardından (tekrar başlatmayla) `read` kadar okur.
    pub fn transfer(
        &self,
        address: u16,
        ten_bit: bool,
        write: &[u8],
        read: &mut [u8],
    ) -> Result<(), &'static str> {
        let total = write.len() + read.len();
        if total == 0 {
            return Ok(());
        }
        let _guard = self.lock.lock();
        if !self.set_enabled(false) {
            return Err("controller did not disable");
        }
        let con = self.r(IC_CON) & !(1 << 4);
        self.w(IC_CON, con | (ten_bit as u32) << 4);
        self.w(IC_TAR, (address & 0x3ff) as u32 | (ten_bit as u32) << 12);
        if !self.set_enabled(true) {
            return Err("controller did not enable");
        }
        self.r(IC_CLR_INTR);

        let deadline = tsc::now_ns() + 20_000_000 + total as u64 * 200_000;
        let mut sent = 0;
        let mut received = 0;
        loop {
            while sent < total
                && (self.r(IC_TXFLR) as usize) < self.tx_depth
                && (sent < write.len() || sent - write.len() < received + self.rx_depth)
            {
                let mut cmd = if sent < write.len() {
                    write[sent] as u32
                } else {
                    CMD_READ
                };
                if sent == write.len() && !write.is_empty() {
                    cmd |= CMD_RESTART;
                }
                if sent == total - 1 {
                    cmd |= CMD_STOP;
                }
                self.w(IC_DATA_CMD, cmd);
                sent += 1;
            }
            while received < read.len() && self.r(IC_RXFLR) > 0 {
                read[received] = self.r(IC_DATA_CMD) as u8;
                received += 1;
            }
            let raw = self.r(IC_RAW_INTR_STAT);
            if raw & INTR_TX_ABRT != 0 {
                let source = self.r(IC_TX_ABRT_SOURCE);
                self.r(IC_CLR_TX_ABRT);
                self.r(IC_CLR_STOP_DET);
                // Yanıt vermeyen adres (7/10 bit NACK) olağan bir durumdur, sessizce döner.
                return Err(if source & 0xf != 0 {
                    "no acknowledge"
                } else {
                    "transfer aborted"
                });
            }
            if sent == total && received == read.len() && raw & INTR_STOP_DET != 0 {
                self.r(IC_CLR_STOP_DET);
                return Ok(());
            }
            if tsc::now_ns() > deadline {
                self.set_enabled(false);
                return Err("timeout");
            }
            crate::sched::yield_now();
        }
    }
}

/// Giriş saati bilinmediğinde kullanılan sayaçlar: 216 MHz'e göre hesaplanır, daha
/// yavaş saatlerde hat yalnızca yavaşlar (100/400 kHz sınırı aşılmaz).
pub const DEFAULT_STANDARD: Timing = Timing {
    hcnt: 870,
    lcnt: 1020,
    sda_hold: None,
};
pub const DEFAULT_FAST: Timing = Timing {
    hcnt: 180,
    lcnt: 360,
    sda_hold: None,
};
