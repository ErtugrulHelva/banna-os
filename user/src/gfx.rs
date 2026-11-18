//! 2B çizim: renkler, yüzeyler, kırpmalı tuval, SSE2 karışım ve Inter yazı tipi.
//! Çekirdeğin compositor'ıyla aynı kod; uygulamalar masaüstüyle birebir aynı çizer.

#[path = "../../src/gfx/canvas.rs"]
mod canvas;
#[path = "../../src/gfx/fast.rs"]
pub mod fast;
#[path = "../../src/gfx/glass.rs"]
pub mod glass;
#[path = "../../src/gfx/png.rs"]
pub mod png;
#[path = "../../src/gfx/text.rs"]
#[allow(dead_code)]
pub mod text;

pub use canvas::*;
pub use glass::Glass;
