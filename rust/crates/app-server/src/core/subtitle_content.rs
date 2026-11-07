use std::io::{Cursor, Read};

use serde_json::Value;
use zip::{CompressionMethod, ZipArchive};

pub const MAX_SUBTITLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 64;
const EXTENSIONS: &[&str] = &[".srt", ".ass", ".ssa", ".vtt"];

#[derive(Debug)]
pub struct SubtitleContentError(pub &'static str);

#[derive(Debug)]
pub struct SubtitleContent {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

fn file_extension(value: &str) -> &str {
    value.rsplit_once('.').map_or("", |(_, extension)| {
        // The extension slice is recovered from the original string below.
        let _ = extension;
        value.rfind('.').map(|index| &value[index..]).unwrap_or("")
    })
}

pub fn safe_subtitle_name(value: &str) -> Result<String, SubtitleContentError> {
    let name = value.trim();
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(|character| character.is_control())
        || !EXTENSIONS.contains(&file_extension(name).to_ascii_lowercase().as_str())
    {
        return Err(SubtitleContentError("INVALID_SUBTITLE_NAME"));
    }
    Ok(name.to_owned())
}

fn safe_archive_path(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let segments = name.split('/').collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        // ZIP directory names may have one trailing slash.
        if !name.ends_with('/')
            || segments[..segments.len().saturating_sub(1)]
                .iter()
                .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
        {
            return false;
        }
    }
    let first = segments.first().copied().unwrap_or_default().as_bytes();
    !(first.len() >= 2 && first[0].is_ascii_alphabetic() && first[1] == b':')
}

fn digits_at(bytes: &[u8], offset: usize, maximum: usize) -> (Option<i64>, usize) {
    let mut end = offset;
    while end < bytes.len() && bytes[end].is_ascii_digit() && end - offset < maximum {
        end += 1;
    }
    if end == offset {
        return (None, offset);
    }
    let number = std::str::from_utf8(&bytes[offset..end])
        .ok()
        .and_then(|digits| digits.parse::<i64>().ok());
    (number, end)
}

fn parsed_episode(text: &str) -> Option<(Option<i64>, i64)> {
    let upper = text.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] == b'S' {
            let (season, mut cursor) = digits_at(bytes, index + 1, 2);
            if season.is_none() || bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                continue;
            }
            while cursor < bytes.len()
                && (bytes[cursor] == b' ' || bytes[cursor] == b'.' || bytes[cursor] == b'_')
            {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'E') {
                continue;
            }
            cursor += 1;
            if bytes.get(cursor) == Some(&b'P') {
                cursor += 1;
            }
            let (episode, episode_end) = digits_at(bytes, cursor, 3);
            if let (Some(season), Some(episode)) = (
                season,
                episode.filter(|episode| {
                    *episode > 0 && !bytes.get(episode_end).is_some_and(u8::is_ascii_digit)
                }),
            ) {
                return Some((Some(season), episode));
            }
        }
    }

    for (marker, word_boundary) in [
        (b"EPISODE".as_slice(), true),
        (b"EP".as_slice(), true),
        (b"E".as_slice(), true),
    ] {
        for (index, window) in bytes.windows(marker.len()).enumerate() {
            if window != marker
                || (word_boundary && index > 0 && bytes[index - 1].is_ascii_alphabetic())
            {
                continue;
            }
            let mut cursor = index + marker.len();
            while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'0') {
                cursor += 1;
            }
            let (episode, end) = digits_at(bytes, cursor, 3);
            if let Some(episode) = episode
                .filter(|episode| *episode > 0 && !bytes.get(end).is_some_and(u8::is_ascii_digit))
            {
                return Some((None, episode));
            }
        }
    }
    if let Some(position) = upper.find('第') {
        let mut digits_start = position + '第'.len_utf8();
        while bytes.get(digits_start).is_some_and(u8::is_ascii_whitespace) {
            digits_start += 1;
        }
        let (episode, end) = digits_at(bytes, digits_start, 3);
        let mut suffix = end;
        while bytes.get(suffix).is_some_and(u8::is_ascii_whitespace) {
            suffix += 1;
        }
        if upper
            .as_bytes()
            .get(suffix..)
            .is_some_and(|rest| rest.starts_with("集".as_bytes()))
            && !bytes.get(end).is_some_and(u8::is_ascii_digit)
        {
            if let Some(episode) = episode.filter(|episode| *episode > 0) {
                return Some((None, episode));
            }
        }
    }
    None
}

fn numeric_field(candidate: &Value, name: &str) -> Option<i64> {
    candidate.get(name).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
    })
}

pub fn candidate_matches(candidate: &Value, season: i64, episode: i64) -> bool {
    let explicit_season = numeric_field(candidate, "seasonNumber");
    let explicit_episode = numeric_field(candidate, "episodeNumber");
    let text = ["fileName", "release", "title"]
        .iter()
        .filter_map(|name| candidate.get(*name).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let parsed = parsed_episode(&text);
    let selected_episode = explicit_episode.or_else(|| parsed.map(|(_, episode)| episode));
    let selected_season = explicit_season.or_else(|| parsed.and_then(|(season, _)| season));
    selected_episode == Some(episode)
        && selected_season.is_none_or(|candidate_season| candidate_season == season)
        && (season == 1 || selected_season == Some(season))
}

fn subtitle_episode_file(file_name: &str, season: i64, episode: i64) -> bool {
    let Some((found_season, found_episode)) = parsed_episode(file_name) else {
        return false;
    };
    found_episode == episode && found_season.is_none_or(|found_season| found_season == season)
}

fn select_file(
    mut files: Vec<SubtitleContent>,
    season: i64,
    episode: i64,
) -> Result<SubtitleContent, SubtitleContentError> {
    if files.is_empty() {
        return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
    }
    let matching = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            subtitle_episode_file(&file.file_name, season, episode).then_some(index)
        })
        .collect::<Vec<_>>();
    let selected = if matching.len() == 1 {
        matching[0]
    } else if matching.is_empty() && files.len() == 1 {
        0
    } else {
        return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
    };
    let file = files.swap_remove(selected);
    if file.bytes.is_empty()
        || (file.bytes.contains(&0)
            && !file.bytes.starts_with(&[0xff, 0xfe])
            && !file.bytes.starts_with(&[0xfe, 0xff]))
    {
        return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
    }
    Ok(file)
}

fn read_zip(
    source: &[u8],
    season: i64,
    episode: i64,
) -> Result<SubtitleContent, SubtitleContentError> {
    let mut archive = ZipArchive::new(Cursor::new(source))
        .map_err(|_| SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
    }
    let mut files = Vec::new();
    let mut total_output = 0_usize;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"))?;
        let name = file.name().to_owned();
        if !safe_archive_path(&name)
            || file.encrypted()
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
            || file.size() > MAX_SUBTITLE_BYTES as u64
        {
            return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
        }
        if file.is_dir() {
            continue;
        }
        let basename = name.rsplit('/').next().unwrap_or_default();
        let extension = file_extension(basename).to_ascii_lowercase();
        if !EXTENSIONS.contains(&extension.as_str()) {
            continue;
        }
        let expected_size = usize::try_from(file.size())
            .map_err(|_| SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"))?;
        total_output = total_output
            .checked_add(expected_size)
            .filter(|total| *total <= MAX_SUBTITLE_BYTES)
            .ok_or(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"))?;
        let mut bytes = Vec::with_capacity(expected_size);
        file.take((MAX_SUBTITLE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"))?;
        if bytes.len() != expected_size {
            return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
        }
        files.push(SubtitleContent {
            file_name: basename.to_owned(),
            bytes,
        });
    }
    select_file(files, season, episode)
}

pub fn normalize_download(
    file_name: &str,
    bytes: Vec<u8>,
    season: i64,
    episode: i64,
) -> Result<SubtitleContent, SubtitleContentError> {
    if bytes.is_empty() || bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(SubtitleContentError("INVALID_SUBTITLE_ARCHIVE"));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return read_zip(&bytes, season, episode);
    }
    let file_name = safe_subtitle_name(file_name)?;
    select_file(vec![SubtitleContent { file_name, bytes }], season, episode)
}

pub fn sidecar_name(
    video_name: &str,
    extension_source: &str,
) -> Result<String, SubtitleContentError> {
    if video_name.is_empty()
        || video_name.contains('/')
        || video_name.contains('\\')
        || video_name.chars().any(|character| character.is_control())
    {
        return Err(SubtitleContentError("INVALID_VIDEO_NAME"));
    }
    let base = video_name
        .rfind('.')
        .map(|index| &video_name[..index])
        .filter(|base| !base.is_empty())
        .ok_or(SubtitleContentError("INVALID_VIDEO_NAME"))?;
    let extension = file_extension(extension_source).to_ascii_lowercase();
    safe_subtitle_name(&format!("{base}{extension}"))
}
