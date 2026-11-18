//! Fare imleci çizimi (12x19 ok).

use super::Rect;

pub const WIDTH: i32 = 12;
pub const HEIGHT: i32 = 19;

const SHAPE: [&[u8; 12]; 19] = [
    b"X...........",
    b"XX..........",
    b"XOX.........",
    b"XOOX........",
    b"XOOOX.......",
    b"XOOOOX......",
    b"XOOOOOX.....",
    b"XOOOOOOX....",
    b"XOOOOOOOX...",
    b"XOOOOOOOOX..",
    b"XOOOOOOOOOX.",
    b"XOOOOOOXXXXX",
    b"XOOOXOOX....",
    b"XOOXXOOX....",
    b"XOX..XOOX...",
    b"XX...XOOX...",
    b"X.....XOOX..",
    b"......XOOX..",
    b".......XX...",
];

pub fn bounds(x: i32, y: i32) -> Rect {
    Rect::new(x, y, WIDTH, HEIGHT)
}

/// İmlecin (col, row) pikselinin rengi; saydamsa `None`.
pub fn pixel(col: usize, row: usize) -> Option<u32> {
    match SHAPE.get(row)?.get(col)? {
        b'X' => Some(0x00_00_00),
        b'O' => Some(0xff_ff_ff),
        _ => None,
    }
}

/// 64x64 ARGB donanım imleci görüntüsü (virtio-gpu için).
pub fn argb_image() -> [u32; 64 * 64] {
    let mut image = [0u32; 64 * 64];
    for row in 0..HEIGHT as usize {
        for col in 0..WIDTH as usize {
            if let Some(color) = pixel(col, row) {
                image[row * 64 + col] = 0xff00_0000 | color;
            }
        }
    }
    image
}
