//! Full-width glyph source replacing the PC-98 kanji ROM.
//!
//! `UK2.EXE` reads 16x16 JIS X 0208 glyphs from the kanji ROM (`int 18h`
//! AH=14h / ports A1h-A5h).  A dump of a real ROM can be supplied as
//! `KANJI16.ROM` (94x94 JIS cells, 32 bytes each, row-major) in the game
//! directory or through `UK2_KANJI_ROM`; otherwise the embedded public-domain
//! Shinonome 16-dot font is used.  Half-width glyphs come from the game's own
//! `kana.pdt1`, exactly as in the original.

use std::path::Path;

use anyhow::{Context, Result};

static SHINONOME: &[u8] = include_bytes!("../../assets/shinonome_jis16.bin");
const CELL_COUNT: usize = 94 * 94;

pub struct KanjiRom {
    data: Vec<u8>,
}

impl KanjiRom {
    pub fn load(game_root: &Path) -> Result<Self> {
        let candidate = std::env::var_os("UK2_KANJI_ROM")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let path = game_root.join("KANJI16.ROM");
                path.is_file().then_some(path)
            });
        if let Some(path) = candidate {
            let data = std::fs::read(&path)
                .with_context(|| format!("uk2: failed to read kanji ROM {}", path.display()))?;
            if data.len() >= CELL_COUNT * 32 {
                return Ok(Self { data });
            }
            log::warn!(
                "uk2: {} is not a 94x94x32 kanji dump; using the embedded font",
                path.display()
            );
        }
        Ok(Self::embedded())
    }

    pub fn embedded() -> Self {
        Self {
            data: SHINONOME.to_vec(),
        }
    }

    /// 16x16 glyph (two bytes per row) for a JIS code `0xHHLL`.
    pub fn jis_glyph(&self, jis: u16) -> [u8; 32] {
        let row = (jis >> 8) as usize;
        let col = (jis & 0xff) as usize;
        let mut out = [0u8; 32];
        if (0x21..=0x7e).contains(&row) && (0x21..=0x7e).contains(&col) {
            let at = ((row - 0x21) * 94 + (col - 0x21)) * 32;
            out.copy_from_slice(&self.data[at..at + 32]);
        }
        out
    }

    /// Converts a Shift-JIS double-byte code (lead, trail) to JIS.
    pub fn sjis_to_jis(lead: u8, trail: u8) -> u16 {
        let mut lead = u16::from(lead);
        let trail = u16::from(trail);
        if lead >= 0xe0 {
            lead -= 0x40;
        }
        let mut row = (lead - 0x81) * 2 + 0x21;
        let col = if trail >= 0x9f {
            row += 1;
            trail - 0x7e
        } else {
            trail - 0x1f - u16::from(trail >= 0x80)
        };
        (row << 8) | col
    }

    pub fn sjis_glyph(&self, lead: u8, trail: u8) -> [u8; 32] {
        self.jis_glyph(Self::sjis_to_jis(lead, trail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_shift_jis_to_jis() {
        assert_eq!(KanjiRom::sjis_to_jis(0x88, 0x9f), 0x3021); // 亜
        assert_eq!(KanjiRom::sjis_to_jis(0x82, 0xa0), 0x2422); // あ
        assert_eq!(KanjiRom::sjis_to_jis(0x81, 0x40), 0x2121);
        let rom = KanjiRom::embedded();
        assert_ne!(rom.sjis_glyph(0x88, 0x9f), [0; 32]);
    }
}
