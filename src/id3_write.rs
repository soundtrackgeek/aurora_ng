//! Compatibility for rust-id3's Latin-1 fields that its writer encodes as UTF-8.
use id3::frame::{Content, Unknown};
use id3::{Frame, Tag, Version};
use std::path::Path;

pub fn write_tag_preserving_frames(tag: &Tag, path: &Path, version: Version) -> Result<(), String> {
    let frames = tag
        .frames()
        .map(|frame| preserve_latin1(frame, version))
        .collect::<Result<Vec<_>, _>>()?;
    // Collect directly: add_frame can deduplicate distinct opaque frames.
    let encoded: Tag = frames.into_iter().collect();
    encoded
        .write_to_path(path, version)
        .map_err(|e| e.to_string())
}

fn with_content(source: &Frame, content: Content) -> Frame {
    let mut frame = Frame::with_content(source.id(), content).set_encoding(source.encoding());
    frame.set_tag_alter_preservation(source.tag_alter_preservation());
    frame.set_file_alter_preservation(source.file_alter_preservation());
    frame
}

fn preserve_latin1(frame: &Frame, version: Version) -> Result<Frame, String> {
    // Chapter containers can themselves contain comments, lyrics and links.
    match frame.content() {
        Content::Chapter(value) => {
            let mut value = value.clone();
            value.frames = value
                .frames
                .iter()
                .map(|f| preserve_latin1(f, version))
                .collect::<Result<_, _>>()?;
            return Ok(with_content(frame, Content::Chapter(value)));
        }
        Content::TableOfContents(value) => {
            let mut value = value.clone();
            value.frames = value
                .frames
                .iter()
                .map(|f| preserve_latin1(f, version))
                .collect::<Result<_, _>>()?;
            return Ok(with_content(frame, Content::TableOfContents(value)));
        }
        _ => {}
    }

    // Offset and length of the incorrectly encoded field in a frame payload.
    // WXXX's URL is the final field; its preceding description has its own encoding.
    let (value, offset, length) = match frame.content() {
        Content::Private(v) => (&v.owner_identifier, Some(0), v.owner_identifier.len()),
        Content::UniqueFileIdentifier(v) => {
            (&v.owner_identifier, Some(0), v.owner_identifier.len())
        }
        Content::Comment(v) => (&v.lang, Some(1), 3),
        Content::Lyrics(v) => (&v.lang, Some(1), 3),
        Content::SynchronisedLyrics(v) => (&v.lang, Some(1), 3),
        Content::ExtendedLink(v) => (&v.link, None, v.link.len()),
        Content::EncapsulatedObject(v) => (&v.mime_type, Some(1), v.mime_type.len()),
        Content::Picture(v) if version != Version::Id3v22 => {
            (&v.mime_type, Some(1), v.mime_type.len())
        }
        _ => return Ok(frame.clone()),
    };
    if value.is_ascii() {
        return Ok(frame.clone());
    }
    let latin1 = value
        .chars()
        .map(|c| {
            u8::try_from(u32::from(c)).map_err(|_| {
                format!(
                    "The {} frame contains a non-Latin-1 value in a Latin-1 field.",
                    frame.id()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(
        frame.content(),
        Content::Comment(_) | Content::Lyrics(_) | Content::SynchronisedLyrics(_)
    ) && latin1.len() != 3
    {
        return Err(format!(
            "The {} frame language must contain exactly three bytes.",
            frame.id()
        ));
    }

    // Let rust-id3 encode the text, binary data and version-specific details,
    // then replace only the known faulty field. No changes to verification.
    let single: Tag = std::iter::once(frame.clone()).collect();
    let mut serialized = Vec::new();
    single
        .write_to(&mut serialized, version)
        .map_err(|e| e.to_string())?;
    let header_size = if version == Version::Id3v22 { 16 } else { 20 };
    let mut data = serialized
        .get(header_size..)
        .ok_or_else(|| format!("Could not preserve the {} frame payload.", frame.id()))?
        .to_vec();
    let start = offset.unwrap_or_else(|| data.len().saturating_sub(length));
    if start.checked_add(length).is_none_or(|end| end > data.len()) {
        return Err(format!(
            "Could not locate the {} frame's Latin-1 field.",
            frame.id()
        ));
    }
    data.splice(start..start + length, latin1);
    Ok(with_content(
        frame,
        Content::Unknown(Unknown { data, version }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn raw_frame(id: &str, data: &[u8], version: Version) -> Frame {
        Frame::with_content(
            id,
            Content::Unknown(Unknown {
                data: data.to_vec(),
                version,
            }),
        )
    }

    #[test]
    fn latin1_fields_roundtrip_without_changing_text_or_binary_data() {
        for version in [Version::Id3v22, Version::Id3v23, Version::Id3v24] {
            // Build fixtures from raw bytes, bypassing the defective encoder.
            let mut raw: Tag = [
                raw_frame("COMM", b"\x00\xff\xfe4note\x00comment \xe9", version),
                raw_frame("COMM", b"\x00engnormal\x00ASCII language", version),
                raw_frame("USLT", b"\x00\x80\xe9\xfflyrics\x00words \xe9", version),
                raw_frame(
                    "SYLT",
                    b"\x00\xff\xfe4\x02\x01sync\x00word \xe9\x00\x00\x00\x01\x23",
                    version,
                ),
                raw_frame("UFID", b"owner\xe9\x00\x00\x80\xff", version),
                raw_frame("WXXX", b"\x00site\x00https://example.test/\xe9", version),
                raw_frame(
                    "GEOB",
                    b"\x00application/\xe9\x00name\x00object\x00\x00\x80\xff",
                    version,
                ),
            ]
            .into_iter()
            .collect();
            if version != Version::Id3v22 {
                raw.extend([
                    raw_frame("PRIV", b"\x01\xff\xfe\x00", version),
                    raw_frame("PRIV", b"other\xe9\x00\x00\x80\xff", version),
                    raw_frame(
                        "APIC",
                        b"\x00image/\xe9\x00\x03cover\x00\x00\x80\xff",
                        version,
                    ),
                ]);
            }
            let mut bytes = Vec::new();
            raw.write_to(&mut bytes, version).unwrap();
            let expected = Tag::read_from2(Cursor::new(bytes)).unwrap();
            let mut broken = Vec::new();
            expected.write_to(&mut broken, version).unwrap();
            assert_ne!(Tag::read_from2(Cursor::new(broken)).unwrap(), expected);

            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("latin1.mp3");
            std::fs::write(&path, b"audio bytes").unwrap();
            write_tag_preserving_frames(&expected, &path, version).unwrap();
            let actual = Tag::read_from_path(&path).unwrap();
            assert_eq!(actual, expected, "{version:?}");
            // A second edit must not progressively corrupt the preserved fields.
            write_tag_preserving_frames(&actual, &path, version).unwrap();
            assert_eq!(Tag::read_from_path(&path).unwrap(), expected);
        }
    }

    #[test]
    fn nested_comments_preserve_language_bytes() {
        use id3::frame::{Chapter, Comment, TableOfContents};
        for version in [Version::Id3v23, Version::Id3v24] {
            let comment = Frame::from(Comment {
                lang: "ÿþ4".into(),
                description: "note".into(),
                text: "preserved".into(),
            });
            let expected: Tag = [
                Frame::from(Chapter {
                    element_id: "chapter".into(),
                    start_time: 0,
                    end_time: 123,
                    start_offset: u32::MAX,
                    end_offset: u32::MAX,
                    frames: vec![comment.clone()],
                }),
                Frame::from(TableOfContents {
                    element_id: "contents".into(),
                    top_level: true,
                    ordered: true,
                    elements: vec!["chapter".into()],
                    frames: vec![comment],
                }),
            ]
            .into_iter()
            .collect();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("nested.mp3");
            std::fs::write(&path, b"audio bytes").unwrap();
            write_tag_preserving_frames(&expected, &path, version).unwrap();
            assert_eq!(Tag::read_from_path(&path).unwrap(), expected);
        }
    }
}
