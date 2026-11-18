//! Ağ kartı arayüzü: ağ yığını kartın türünü bilmez (Intel e1000, Realtek, virtio-net, USB Ethernet).

use alloc::boxed::Box;
use alloc::vec::Vec;

pub trait Nic: Send {
    fn mac(&self) -> [u8; 6];
    fn model(&self) -> &'static str;
    fn irq(&self) -> u8 {
        0
    }
    fn send(&mut self, frame: &[u8]) -> bool;
    fn receive(&mut self) -> Option<Vec<u8>>;
    /// (bağlı, hız Mbit/s, tam çift yönlü)
    fn link(&self) -> (bool, u32, bool);
    /// Kesmeden beri: (bağlantı değişti, alım oldu).
    fn take_cause(&mut self) -> (bool, bool);
    /// (CRC hataları, kaçırılan çerçeveler)
    fn hardware_errors(&self) -> (u32, u32) {
        (0, 0)
    }
    fn rx_errors(&self) -> u64 {
        0
    }
    /// Sorun giderme satırı (DHCP yanıt alamazken günlüğe yazılır).
    fn diagnostics(&mut self) -> Option<alloc::string::String> {
        None
    }
}

/// Ağ yığını başlamadan önce modüllerin kaydettiği kartlar.
static REGISTERED: crate::sync::SpinLock<Vec<Box<dyn Nic>>> =
    crate::sync::SpinLock::new(Vec::new());

/// Modül sürücüsünün kartı: ağ yığını çalışıyorsa hemen bağlanır, yoksa açılışta alınır.
pub fn register(nic: Box<dyn Nic>) -> bool {
    if crate::net::started() {
        return crate::net::attach(nic);
    }
    REGISTERED.lock().push(nic);
    true
}

/// PCI ağ kartlarını sırayla dener.
pub fn probe() -> Option<Box<dyn Nic>> {
    if let Some((nic, _)) = super::e1000::probe() {
        return Some(Box::new(nic));
    }
    if let Some(nic) = super::virtio_net::probe() {
        return Some(Box::new(nic));
    }
    if let Some(nic) = super::rtl8169::probe() {
        return Some(Box::new(nic));
    }
    REGISTERED.lock().pop()
}
