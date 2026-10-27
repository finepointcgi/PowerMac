//! Clipboard text format (Core/Clipboard.cs, Helper/FormHelper.cs).
//!
//! Text clipboard content is serialized as
//! `"TXT" + text + SEP [+ "RTF" + rtf + SEP] [+ "HTM" + html + SEP]`, where
//! `SEP = "{4CFF57F7-BEDD-43d5-AE8F-27A61E886F2F}"`, encoded as UTF-16LE and
//! compressed with raw DEFLATE (.NET `DeflateStream`). If the compressed size is
//! at most 1 MB it is pushed over the main message socket as a run of 64-byte
//! `ClipboardText` packages each carrying 48 bytes at offset 16, terminated by a
//! `ClipboardDataEnd` package.

use std::io::{Read, Write};

use crate::protocol::{ptype, Data, ID_ALL, PACKAGE_SIZE_EX};

pub const TEXT_TYPE_SEP: &str = "{4CFF57F7-BEDD-43d5-AE8F-27A61E886F2F}";
/// `DATA_SIZE`: payload bytes per clipboard package.
pub const DATA_SIZE: usize = 48;
pub const DATA_START: usize = PACKAGE_SIZE_EX - DATA_SIZE;
/// `MAX_CLIPBOARD_DATA_SIZE_CAN_BE_SENT_INSTANTLY_TCP`.
pub const MAX_INSTANT_SIZE: usize = 1024 * 1024;

/// Build the compressed payload for plain text.
pub fn encode_text(text: &str) -> Vec<u8> {
    let s = format!("TXT{text}{TEXT_TYPE_SEP}");
    let utf16: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&utf16).unwrap();
    enc.finish().unwrap()
}

/// Decoded clipboard text formats.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClipboardTexts {
    pub text: Option<String>,
    pub rtf: Option<String>,
    pub html: Option<String>,
}

/// `SetClipboardData`: inflate, decode UTF-16LE and split on the separator.
pub fn decode_text(data: &[u8]) -> anyhow::Result<ClipboardTexts> {
    let mut dec = flate2::read::DeflateDecoder::new(data);
    let mut raw = Vec::new();
    dec.read_to_end(&mut raw)?;
    let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let st = String::from_utf16_lossy(&units);
    let mut out = ClipboardTexts::default();
    let mut count = 0;
    for txt in st.split(TEXT_TYPE_SEP).filter(|s| !s.is_empty()) {
        if txt.trim_matches('\0').is_empty() {
            continue;
        }
        let head: String = txt.chars().take(3).collect();
        let tail: String = txt.chars().skip(3).collect();
        match head.to_ascii_uppercase().as_str() {
            "RTF" => out.rtf = Some(tail),
            "HTM" => out.html = Some(tail),
            "TXT" => out.text = Some(tail),
            _ => {
                if count == 0 {
                    out.text = Some(txt.to_string());
                }
            }
        }
        count += 1;
    }
    Ok(out)
}

/// Split a payload into `ClipboardText` packages + `ClipboardDataEnd`
/// (`SendClipboardDataUsingTCP`). Ids/Src are filled in by the sender.
pub fn chunk_packages(payload: &[u8], image: bool) -> Vec<Data> {
    let mut out = Vec::new();
    let mut index = 0;
    let t = if image { ptype::ClipboardImage } else { ptype::ClipboardText };
    loop {
        let mut d = Data::default();
        let len = DATA_SIZE.min(payload.len().saturating_sub(index));
        d.buf[DATA_START..DATA_START + len].copy_from_slice(&payload[index..index + len]);
        d.set_type(t);
        d.set_des(ID_ALL);
        out.push(d);
        index += DATA_SIZE;
        if index >= payload.len() {
            break;
        }
    }
    let mut end = Data::new(ptype::ClipboardDataEnd);
    end.set_des(ID_ALL);
    out.push(end);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = encode_text("hello wörld ✓");
        let mut padded = p.clone();
        padded.extend([0u8; 40]); // trailing zeros like MemoryStream.GetBuffer()
        let t = decode_text(&padded).unwrap();
        assert_eq!(t.text.as_deref(), Some("hello wörld ✓"));
        let pk = chunk_packages(&p, false);
        assert_eq!(pk.len(), p.len().div_ceil(DATA_SIZE) + 1);
        let mut joined = Vec::new();
        for d in &pk[..pk.len() - 1] {
            joined.extend_from_slice(&d.buf[DATA_START..]);
        }
        assert_eq!(decode_text(&joined).unwrap().text.as_deref(), Some("hello wörld ✓"));
    }
}
