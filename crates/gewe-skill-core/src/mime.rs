//! Detect attachment types from bytes rather than unreliable upstream headers.

#[must_use]
pub fn detect_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if bytes.get(4..8) == Some(b"ftyp")
        && bytes.get(8..12).is_some_and(|brand| {
            [
                b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"avc1",
                b"dash", b"M4V ", b"MSNV",
            ]
            .iter()
            .any(|candidate| brand == *candidate)
        })
    {
        Some("video/mp4")
    } else if bytes.starts_with(b"#!SILK_V3") || bytes.starts_with(b"\x02#!SILK_V3") {
        Some("audio/silk")
    } else if bytes.starts_with(b"#!AMR-WB\n") {
        Some("audio/amr-wb")
    } else if bytes.starts_with(b"#!AMR\n") {
        Some("audio/amr")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"]
        .iter()
        .any(|signature| bytes.starts_with(*signature))
    {
        Some("application/zip")
    } else {
        None
    }
}

#[must_use]
pub fn corrected_mime(bytes: &[u8], upstream: Option<&str>) -> String {
    if let Some(mime) = detect_mime(bytes) {
        return mime.to_string();
    }
    let base = upstream
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    if base.is_empty() || base.eq_ignore_ascii_case("application/octst-stream") {
        "application/octet-stream".to_string()
    } else {
        base.to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_headers_and_overrides_wrong_upstream_values() {
        let fixtures: &[(&[u8], &str)] = &[
            (b"\xff\xd8\xff\xe0", "image/jpeg"),
            (b"\x89PNG\r\n\x1a\n", "image/png"),
            (b"GIF89a", "image/gif"),
            (b"GIF87a", "image/gif"),
            (b"RIFF\0\0\0\0WEBP", "image/webp"),
            (b"\0\0\0\x18ftypmp42", "video/mp4"),
            (b"#!SILK_V3", "audio/silk"),
            (b"\x02#!SILK_V3", "audio/silk"),
            (b"#!AMR\n", "audio/amr"),
            (b"#!AMR-WB\n", "audio/amr-wb"),
            (b"%PDF-1.7", "application/pdf"),
            (b"PK\x03\x04", "application/zip"),
            (b"PK\x05\x06", "application/zip"),
        ];
        for (bytes, mime) in fixtures {
            assert_eq!(detect_mime(bytes), Some(*mime));
            assert_eq!(corrected_mime(bytes, None), *mime);
            assert_eq!(
                corrected_mime(bytes, Some("application/octst-stream")),
                *mime
            );
            assert_eq!(corrected_mime(bytes, Some("text/html")), *mime);
        }
        assert_eq!(detect_mime(b"RIFF"), None);
        assert_eq!(detect_mime(b"\0\0\0\x18ftypheic"), None);
        assert_eq!(detect_mime(b"\0\0\0\x18ftypavif"), None);
        assert_eq!(detect_mime(b""), None);
        assert_eq!(
            corrected_mime(b"unknown", Some("application/octst-stream")),
            "application/octet-stream"
        );
    }
}
