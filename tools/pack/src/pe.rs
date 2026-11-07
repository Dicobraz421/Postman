//! Версия файла exe (`VS_FIXEDFILEINFO`) — для столбца `File.Version` MSI.

/// Сигнатура `VS_FIXEDFILEINFO` (`0xFEEF04BD`, little-endian).
const SIGNATURE: [u8; 4] = [0xBD, 0x04, 0xEF, 0xFE];

/// `a.b.c.d` из ресурса версии или `None`, если ресурса нет.
///
/// Ищется сигнатура структуры: в образе она одна, за ней — версия структуры и два слова версии файла.
pub fn file_version(image: &[u8]) -> Option<String> {
    let at = image.windows(4).position(|w| w == SIGNATURE)?;
    let word = |i: usize| image.get(at + i..at + i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (ms, ls) = (word(8)?, word(12)?);
    Some(format!("{}.{}.{}.{}", ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fixed_file_info() {
        let mut image = vec![0u8; 64];
        image[20..24].copy_from_slice(&SIGNATURE);
        image[24..28].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        image[28..32].copy_from_slice(&((2u32 << 16) | 3).to_le_bytes());
        image[32..36].copy_from_slice(&(4u32 << 16).to_le_bytes());
        assert_eq!(file_version(&image).as_deref(), Some("2.3.4.0"));
        assert_eq!(file_version(&[0; 16]), None);
    }
}
