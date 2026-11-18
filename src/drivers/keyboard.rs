//! PS/2 klavye: scancode set 1 çözümleme, değiştirici tuşlar, LED'ler ve olay kuyruğu.

use crate::arch::idt;
use crate::kernel::initcall::Detail;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

use super::ps2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    /// Windows/Command tuşu.
    Super,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
    /// PS/2 set 1 tarama kodu (düzenden bağımsız kısayollar için; ör. 0x02 = "1" tuşu).
    pub scancode: u8,
}

const QUEUE_SIZE: usize = 128;

/// Basılıp hiçbir tuşla kullanılmadan bu kadar bekleyen değiştirici bayat sayılır.
/// UTM imleç yakalamayı Ctrl+Option ile bırakırken basmaları iletir ama bırakmaları
/// iletmez; bu olmadan Ctrl/Alt sonsuza dek basılı kalır.
const STALE_MODIFIER_NS: u64 = 1_500_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    TurkishQ,
    Us,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::TurkishQ => "tr",
            Layout::Us => "us",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tr" | "trq" => Some(Layout::TurkishQ),
            "us" | "en" => Some(Layout::Us),
            _ => None,
        }
    }

    fn tables(self) -> (&'static str, &'static str) {
        match self {
            Layout::TurkishQ => (TR_NORMAL, TR_SHIFTED),
            Layout::Us => (US_NORMAL, US_SHIFTED),
        }
    }

    /// 102. tuş (sol Shift'in yanı): (normal, shift).
    fn iso_key(self) -> (char, char) {
        match self {
            Layout::TurkishQ => ('<', '>'),
            Layout::Us => ('\\', '|'),
        }
    }

    fn altgr(self, code: u8) -> Option<char> {
        if self != Layout::TurkishQ {
            return None;
        }
        Some(match code {
            0x02 => '>',
            0x03 => '£',
            0x04 => '#',
            0x05 => '$',
            0x06 => '½',
            0x08 => '{',
            0x09 => '[',
            0x0a => ']',
            0x0b => '}',
            0x0c => '\\',
            0x0d => '|',
            0x10 => '@',
            0x12 => '€',
            0x1a => '¨',
            0x1b => '~',
            0x27 => '´',
            0x2b => '`',
            0x56 => '|',
            _ => return None,
        })
    }
}

struct State {
    layout: Layout,
    right_alt: bool,
    /// İşlenen son tarama kodu (olaya eklenir).
    code: u8,
    now_ns: u64,
    modifier_since_ns: u64,
    modifier_used: bool,
    modifiers: Modifiers,
    left_shift: bool,
    right_shift: bool,
    extended: bool,
    pause_skip: u8,
    pending_led: Option<u8>,
    queue: [Option<KeyEvent>; QUEUE_SIZE],
    head: usize,
    tail: usize,
    dropped: u64,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    layout: Layout::TurkishQ,
    right_alt: false,
    code: 0,
    now_ns: 0,
    modifier_since_ns: 0,
    modifier_used: true,
    modifiers: Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
        caps_lock: false,
        num_lock: true,
    },
    left_shift: false,
    right_shift: false,
    extended: false,
    pause_skip: 0,
    pending_led: None,
    queue: [None; QUEUE_SIZE],
    head: 0,
    tail: 0,
    dropped: 0,
});

/// Set 1 tarama kodu 0x00-0x39 sırasıyla karakterler (`\0`: karakter üretmez).
const US_NORMAL: &str = "\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const US_SHIFTED: &str = "\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";
const TR_NORMAL: &str = "\0\x1b1234567890*-\x08\tqwertyuıopğü\n\0asdfghjklşi\"\0,zxcvbnmöç.\0*\0 ";
const TR_SHIFTED: &str = "\0\x1b!'^+%&/()=?_\x08\tQWERTYUIOPĞÜ\n\0ASDFGHJKLŞİé\0;ZXCVBNMÖÇ:\0*\0 ";
const KEYPAD: &[u8; 13] = b"789-456+1230.";

impl State {
    fn push(&mut self, key: Key) {
        if matches!(key, Key::Char('b' | 'B')) {
            B_PRESSED.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        let next = (self.head + 1) % QUEUE_SIZE;
        if next == self.tail {
            self.dropped += 1;
            return;
        }
        self.queue[self.head] = Some(KeyEvent {
            key,
            modifiers: self.modifiers,
            scancode: self.code,
        });
        self.head = next;
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.head == self.tail {
            return None;
        }
        let event = self.queue[self.tail].take();
        self.tail = (self.tail + 1) % QUEUE_SIZE;
        event
    }

    fn leds(&self) -> u8 {
        (self.modifiers.num_lock as u8) << 1 | (self.modifiers.caps_lock as u8) << 2
    }

    fn update_leds(&mut self) {
        self.pending_led = Some(self.leds());
        ps2::write_data(0xed);
    }

    fn handle(&mut self, byte: u8) {
        if byte == 0xfa {
            if let Some(leds) = self.pending_led.take() {
                ps2::write_data(leds);
            }
            return;
        }
        if self.pause_skip > 0 {
            self.pause_skip -= 1;
            return;
        }
        match byte {
            0xe0 => {
                self.extended = true;
                return;
            }
            0xe1 => {
                self.pause_skip = 5;
                return;
            }
            0x00 | 0xee | 0xfe | 0xff if !self.extended => return,
            _ => {}
        }

        let extended = core::mem::take(&mut self.extended);
        let released = byte & 0x80 != 0;
        let code = byte & 0x7f;

        if !released {
            if is_modifier(code) {
                self.modifier_since_ns = self.now_ns;
                self.modifier_used = false;
            } else if !(code == 0x0f && self.modifiers.alt && !self.modifiers.ctrl) {
                // Alt+Tab'da Alt uzun süre basılı tutulabilir; bayat sayılmaz.
                self.drop_stale_modifiers();
            }
        }

        self.code = code;
        if extended {
            self.handle_extended(code, released);
        } else {
            self.handle_normal(code, released);
        }
    }

    fn handle_at(&mut self, byte: u8, now_ns: u64) {
        self.now_ns = now_ns;
        self.handle(byte);
    }

    /// Kullanılmadan bekleyen ya da Ctrl+Alt (UTM yakalama bırakma) olarak kalmış
    /// değiştiricileri sıfırlar; ilk gerçek tuşla birlikte çağrılır.
    fn drop_stale_modifiers(&mut self) {
        let held = self.left_shift
            || self.right_shift
            || self.right_alt
            || self.modifiers.ctrl
            || self.modifiers.alt;
        let expired = self.now_ns.saturating_sub(self.modifier_since_ns) > STALE_MODIFIER_NS;
        let grab_release = self.modifiers.ctrl && self.modifiers.alt;
        if held && !self.modifier_used && (expired || grab_release) {
            self.left_shift = false;
            self.right_shift = false;
            self.right_alt = false;
            self.modifiers.shift = false;
            self.modifiers.ctrl = false;
            self.modifiers.alt = false;
        }
        self.modifier_used = true;
    }

    fn handle_extended(&mut self, code: u8, released: bool) {
        match code {
            0x1d => self.modifiers.ctrl = !released,
            0x38 => self.right_alt = !released,
            0x2a | 0x36 => {}
            _ if released => {}
            0x1c => self.push(Key::Enter),
            0x35 => self.push(Key::Char('/')),
            0x47 => self.push(Key::Home),
            0x48 => self.push(Key::Up),
            0x49 => self.push(Key::PageUp),
            0x4b => self.push(Key::Left),
            0x4d => self.push(Key::Right),
            0x4f => self.push(Key::End),
            0x50 => self.push(Key::Down),
            0x51 => self.push(Key::PageDown),
            0x52 => self.push(Key::Insert),
            0x53 => self.push(Key::Delete),
            0x5b | 0x5c => self.push(Key::Super),
            _ => {}
        }
    }

    fn handle_normal(&mut self, code: u8, released: bool) {
        match code {
            0x2a => self.left_shift = !released,
            0x36 => self.right_shift = !released,
            0x1d => self.modifiers.ctrl = !released,
            0x38 => self.modifiers.alt = !released,
            _ if released => {}
            0x3a => {
                self.modifiers.caps_lock = !self.modifiers.caps_lock;
                self.update_leds();
            }
            0x45 => {
                self.modifiers.num_lock = !self.modifiers.num_lock;
                self.update_leds();
            }
            0x3b..=0x44 => self.push(Key::F(code - 0x3b + 1)),
            0x57 => self.push(Key::F(11)),
            0x58 => self.push(Key::F(12)),
            0x47..=0x53 => self.handle_keypad(code),
            0x01 => self.push(Key::Escape),
            0x0e => self.push(Key::Backspace),
            0x0f => self.push(Key::Tab),
            0x1c => self.push(Key::Enter),
            0x56 => {
                let (normal, shifted) = self.layout.iso_key();
                self.push_char(0x56, normal, shifted);
            }
            0x00..0x3a => {
                let (normal, shifted) = self.layout.tables();
                let at = |t: &str| t.chars().nth(code as usize).unwrap_or('\0');
                self.push_char(code, at(normal), at(shifted));
            }
            _ => {}
        }
        self.modifiers.shift = self.left_shift || self.right_shift;
    }

    fn push_char(&mut self, code: u8, normal: char, shifted: char) {
        self.modifiers.shift = self.left_shift || self.right_shift;
        if self.right_alt
            && let Some(ch) = self.layout.altgr(code)
        {
            self.push(Key::Char(ch));
            return;
        }
        if normal == '\0' {
            return;
        }
        let mut shift = self.modifiers.shift;
        if normal.is_alphabetic() && self.modifiers.caps_lock {
            shift = !shift;
        }
        self.push(Key::Char(if shift { shifted } else { normal }));
    }

    fn handle_keypad(&mut self, code: u8) {
        let byte = KEYPAD[(code - 0x47) as usize];
        if self.modifiers.num_lock || matches!(byte, b'-' | b'+') {
            self.push(Key::Char(byte as char));
            return;
        }
        let key = match code {
            0x47 => Key::Home,
            0x48 => Key::Up,
            0x49 => Key::PageUp,
            0x4b => Key::Left,
            0x4d => Key::Right,
            0x4f => Key::End,
            0x50 => Key::Down,
            0x51 => Key::PageDown,
            0x52 => Key::Insert,
            0x53 => Key::Delete,
            _ => return,
        };
        self.push(key);
    }
}

static INPUT: WaitQueue = WaitQueue::new();
static ROUTE_TO_GUI: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static GUI_KEYS: SpinLock<alloc::collections::VecDeque<KeyEvent>> =
    SpinLock::new(alloc::collections::VecDeque::new());

/// `true`: tuşlar odaklı uygulama penceresine gider; `false`: kabuğa.
/// Açılıştan beri (kuyruk dolsa bile) B tuşuna basıldı mı: kurtarma kipi isteği.
static B_PRESSED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn b_pressed() -> bool {
    B_PRESSED.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn route_to_gui(enabled: bool) {
    ROUTE_TO_GUI.store(enabled, core::sync::atomic::Ordering::Relaxed);
}

pub fn take_gui_key() -> Option<KeyEvent> {
    GUI_KEYS.lock().pop_front()
}

pub(super) fn on_byte(byte: u8) {
    inject(&[byte]);
}

/// Set 1 tarama kodlarını işler (PS/2 kesmesi ya da USB HID çevirisi).
pub fn inject(bytes: &[u8]) {
    let queued = {
        let mut state = STATE.lock();
        let now = crate::arch::tsc::now_ns();
        for &byte in bytes {
            state.handle_at(byte, now);
        }
        if ROUTE_TO_GUI.load(core::sync::atomic::Ordering::Relaxed) {
            let mut keys = GUI_KEYS.lock();
            while let Some(event) = state.pop() {
                if keys.len() < 64 {
                    keys.push_back(event);
                }
            }
            drop(keys);
            crate::gui::wake();
            false
        } else {
            state.head != state.tail
        }
    };
    if queued {
        INPUT.wake_all();
    }
}

/// Sol Alt şu an basılı mı (Alt+Tab değiştiricisinin bırakılmasını izlemek için).
pub fn alt_down() -> bool {
    STATE.lock().modifiers.alt
}

pub fn layout() -> Layout {
    STATE.lock().layout
}

pub fn set_layout(layout: Layout) {
    STATE.lock().layout = layout;
}

pub fn read_event() -> Option<KeyEvent> {
    STATE.lock().pop()
}

pub fn has_event() -> bool {
    let state = STATE.lock();
    state.head != state.tail
}

/// Bir tuş olayı gelene kadar geçerli thread'i uyutur.
pub fn wait_event() -> KeyEvent {
    loop {
        INPUT.wait_until(has_event);
        if let Some(event) = read_event() {
            return event;
        }
    }
}

pub fn read_char() -> Option<char> {
    loop {
        let event = read_event()?;
        let ch = match event.key {
            Key::Char(c) if event.modifiers.ctrl && c.is_ascii_alphabetic() => {
                ((c.to_ascii_lowercase() as u8) - b'a' + 1) as char
            }
            Key::Char(c) => c,
            Key::Enter => '\n',
            Key::Backspace => '\x08',
            Key::Tab => '\t',
            Key::Escape => '\x1b',
            _ => continue,
        };
        return Some(ch);
    }
}

fn is_modifier(code: u8) -> bool {
    matches!(code, 0x1d | 0x2a | 0x36 | 0x38)
}

fn self_test() -> bool {
    let mut state = State {
        layout: Layout::Us,
        right_alt: false,
        code: 0,
        now_ns: 0,
        modifier_since_ns: 0,
        modifier_used: true,
        modifiers: Modifiers::default(),
        left_shift: false,
        right_shift: false,
        extended: false,
        pause_skip: 0,
        pending_led: None,
        queue: [None; QUEUE_SIZE],
        head: 0,
        tail: 0,
        dropped: 0,
    };
    for byte in [
        0x2a, 0x23, 0xa3, 0xaa, 0x17, 0x97, 0xe0, 0x48, 0xe0, 0xc8, 0x1d, 0x2e, 0xae, 0x9d,
    ] {
        state.handle(byte);
    }
    let events: [Option<KeyEvent>; 5] = core::array::from_fn(|_| state.pop());
    let keys = events.map(|e| e.map(|e| e.key));
    keys == [
        Some(Key::Char('H')),
        Some(Key::Char('i')),
        Some(Key::Up),
        Some(Key::Char('c')),
        None,
    ] && events[3].is_some_and(|e| e.modifiers.ctrl)
        && !state.modifiers.shift
        && !state.modifiers.ctrl
        && turkish_self_test(state)
}

fn turkish_self_test(mut state: State) -> bool {
    let sizes_ok = [US_NORMAL, US_SHIFTED, TR_NORMAL, TR_SHIFTED]
        .iter()
        .all(|t| t.chars().count() == 0x3a);
    state.layout = Layout::TurkishQ;
    for byte in [
        0x17, 0x97, 0x2a, 0x28, 0xa8, 0xaa, 0x35, 0xb5, 0xe0, 0x38, 0x10, 0x90, 0xe0, 0xb8, 0x27,
        0x56,
    ] {
        state.handle(byte);
    }
    let keys: [Option<Key>; 7] = core::array::from_fn(|_| state.pop().map(|e| e.key));
    sizes_ok
        && keys
            == [
                Some(Key::Char('ı')),
                Some(Key::Char('İ')),
                Some(Key::Char('.')),
                Some(Key::Char('@')),
                Some(Key::Char('ş')),
                Some(Key::Char('<')),
                None,
            ]
        && !state.right_alt
        && stale_modifier_self_test(state)
}

fn stale_modifier_self_test(mut state: State) -> bool {
    const S: u64 = 1_000_000_000;
    let mut feed = |bytes: &[u8], t: u64| {
        for &b in bytes {
            state.handle_at(b, t);
        }
    };
    // UTM yakalama bırakma: Ctrl+Option basılır, bırakmalar gelmez.
    feed(&[0x1d, 0x38], S);
    feed(&[0x02, 0x82], S + 100_000_000);
    // Shift basılı unutulmuş, 3 sn sonra yazılan tuş.
    feed(&[0x2a], 2 * S);
    feed(&[0x04, 0x84], 5 * S);
    // Gerçek kısayol: Ctrl basılı tutulur, hemen C; ardından Shift+2 (').
    feed(&[0x1d, 0x2e, 0xae, 0x9d], 6 * S);
    feed(&[0x2a, 0x03, 0x83, 0xaa], 7 * S);
    let events: [Option<KeyEvent>; 5] = core::array::from_fn(|_| state.pop());
    let keys = events.map(|e| e.map(|e| e.key));
    keys == [
        Some(Key::Char('1')),
        Some(Key::Char('3')),
        Some(Key::Char('c')),
        Some(Key::Char('\'')),
        None,
    ] && events[0].is_some_and(|e| !e.modifiers.ctrl && !e.modifiers.alt)
        && events[2].is_some_and(|e| e.modifiers.ctrl)
}

fn init(_: &mut Detail) -> Result<(), &'static str> {
    crate::kernel::initcall::self_test("keyboard", self_test())?;
    ps2::init()?;
    idt::register_irq(1, ps2::on_irq);
    Ok(())
}

crate::initcall!(driver, 20, "PS/2 keyboard", init);
