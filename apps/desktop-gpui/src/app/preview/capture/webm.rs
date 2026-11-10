//! A `WebM` file for one AV1 video track, written whole once a recording stops (Matroska's
//! `V_AV1` mapping). Clusters start at keyframes, so the cues seek to each of them.

/// One encoded frame: its time in milliseconds from the start and its AV1 temporal unit.
pub(super) struct Block {
    pub at_ms: u64,
    pub key: bool,
    pub data: Vec<u8>,
}

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;

/// The `WebM` bytes. `codec_private` is the track's `av1C` record with its sequence header.
pub(super) fn mux(width: u32, height: u32, codec_private: &[u8], blocks: &[Block], duration_ms: u64) -> Vec<u8> {
    let header = element(
        EBML,
        &[
            uint(0x4286, 1),
            uint(0x42F7, 1),
            uint(0x42F2, 4),
            uint(0x42F3, 8),
            element(0x4282, b"webm"),
            uint(0x4287, 4),
            uint(0x4285, 2),
        ]
        .concat(),
    );
    let info = element(
        INFO,
        &[
            uint(0x2A_D7B1, 1_000_000),
            element(0x4D80, b"T3 Code"),
            element(0x5741, b"T3 Code"),
            element(0x4489, &(duration_ms as f64).to_be_bytes()),
        ]
        .concat(),
    );
    let video = element(0xE0, &[uint(0xB0, width.into()), uint(0xBA, height.into())].concat());
    let track = element(
        0xAE,
        &[
            uint(0xD7, 1),
            uint(0x73C5, 1),
            uint(0x83, 1),
            uint(0x9C, 0),
            element(0x86, b"V_AV1"),
            element(0x63A2, codec_private),
            video,
        ]
        .concat(),
    );
    let tracks = element(TRACKS, &track);
    let (clusters, starts) = clusters(blocks);

    // The seek head has a fixed size (8-byte positions), so every offset is known before it is written.
    let seek_head_len = seek_head([0; 3]).len();
    let clusters_at = (seek_head_len + info.len() + tracks.len()) as u64;
    let cues = element(
        CUES,
        &starts
            .iter()
            .map(|(at_ms, offset)| {
                let position = element(0xB7, &[uint(0xF7, 1), uint(0xF1, clusters_at + offset)].concat());
                element(0xBB, &[uint(0xB3, *at_ms), position].concat())
            })
            .collect::<Vec<_>>()
            .concat(),
    );
    let info_at = seek_head_len as u64;
    let seek_head = seek_head([info_at, info_at + info.len() as u64, clusters_at + clusters.len() as u64]);
    let segment = element(SEGMENT, &[seek_head, info, tracks, clusters, cues].concat());
    [header, segment].concat()
}

/// Seek entries for the info, tracks and cues, at these offsets into the segment.
fn seek_head([info, tracks, cues]: [u64; 3]) -> Vec<u8> {
    let seek = |id: u32, position: u64| {
        element(SEEK, &[element(SEEK_ID, &id_bytes(id)), element(SEEK_POSITION, &position.to_be_bytes())].concat())
    };
    element(SEEK_HEAD, &[seek(INFO, info), seek(TRACKS, tracks), seek(CUES, cues)].concat())
}

/// The clusters' bytes, and each keyframe cluster's time and offset into them. A cluster starts
/// at every keyframe and whenever a block's time no longer fits its 16-bit offset.
fn clusters(blocks: &[Block]) -> (Vec<u8>, Vec<(u64, u64)>) {
    let mut out = Vec::new();
    let mut starts = Vec::new();
    let mut index = 0;
    while let Some(first) = blocks.get(index) {
        let start = first.at_ms;
        let mut body = uint(0xE7, start);
        let mut end = index;
        while let Some(block) = blocks.get(end) {
            let Ok(offset) = i16::try_from(block.at_ms.saturating_sub(start)) else { break };
            if end > index && block.key {
                break;
            }
            let mut simple = vec![0x81];
            simple.extend_from_slice(&offset.to_be_bytes());
            simple.push(if block.key { 0x80 } else { 0 });
            simple.extend_from_slice(&block.data);
            body.extend(element(0xA3, &simple));
            end += 1;
        }
        if first.key {
            starts.push((start, out.len() as u64));
        }
        out.extend(element(CLUSTER, &body));
        index = end;
    }
    (out, starts)
}

fn id_bytes(id: u32) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|byte| **byte == 0).count().min(3);
    bytes[skip..].to_vec()
}

/// An EBML element: its id, its size as the shortest variable-length integer, then its body.
fn element(id: u32, body: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend(size(body.len() as u64));
    out.extend_from_slice(body);
    out
}

fn uint(id: u32, value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take_while(|byte| **byte == 0).count().min(7);
    element(id, &bytes[skip..])
}

/// A size takes the fewest bytes whose value bits aren't all ones (that pattern means "unknown").
fn size(value: u64) -> Vec<u8> {
    let length = (1..=8u32).find(|length| value < (1u64 << (7 * length)) - 1).unwrap_or(8);
    let marked = value | (1u64 << (7 * length));
    marked.to_be_bytes()[8 - length as usize..].to_vec()
}

/// AV1 OBUs in a temporal unit: each one's type and its bytes, header included. `None` for a
/// malformed unit.
pub(super) fn obus(data: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let header = data[at];
        let kind = (header >> 3) & 0xF;
        let mut cursor = at + 1 + usize::from(header & 0b100 != 0);
        let end = if header & 0b10 == 0 {
            data.len()
        } else {
            let (length, read) = leb128(data.get(cursor..)?)?;
            cursor += read;
            cursor.checked_add(usize::try_from(length).ok()?)?
        };
        out.push((kind, data.get(at..end)?));
        at = end;
    }
    Some(out)
}

fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (index, byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

pub(super) const OBU_SEQUENCE_HEADER: u8 = 1;
pub(super) const OBU_TEMPORAL_DELIMITER: u8 = 2;

/// A temporal unit as Matroska stores it: without temporal delimiters.
pub(super) fn strip_temporal_delimiters(data: Vec<u8>) -> Vec<u8> {
    match obus(&data) {
        Some(units) if units.iter().any(|(kind, _)| *kind == OBU_TEMPORAL_DELIMITER) => units
            .into_iter()
            .filter(|(kind, _)| *kind != OBU_TEMPORAL_DELIMITER)
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect(),
        _ => data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(at_ms: u64, key: bool) -> Block {
        Block { at_ms, key, data: vec![0xAB] }
    }

    #[test]
    fn sizes_use_the_shortest_form_but_never_the_unknown_pattern() {
        assert_eq!(size(0), [0x80]);
        assert_eq!(size(126), [0xFE]);
        assert_eq!(size(127), [0x40, 0x7F], "0xFF would read as an unknown size");
        assert_eq!(size(300), [0x41, 0x2C]);
        assert_eq!(uint(0xD7, 0), [0xD7, 0x81, 0x00]);
        assert_eq!(uint(0x2A_D7B1, 1_000_000), [0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40]);
    }

    #[test]
    fn clusters_start_at_keyframes_and_before_offsets_overflow() {
        let blocks = [block(0, true), block(33, false), block(66, true), block(40_000, false)];
        let (bytes, starts) = clusters(&blocks);
        let count = bytes.windows(4).filter(|w| *w == CLUSTER.to_be_bytes()).count();
        assert_eq!(count, 3, "a keyframe and a 16-bit overflow each start a cluster");
        assert_eq!(starts.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [0, 66], "only keyframe clusters are cues");
        assert_eq!(starts[0].1, 0);
    }

    #[test]
    fn the_file_is_webm_with_its_seek_head_pointing_at_each_part() {
        let blocks = [block(0, true), block(33, false)];
        let file = mux(640, 360, &[0x81, 0, 0, 0], &blocks, 66);
        assert!(file.starts_with(&EBML.to_be_bytes()));
        let find = |needle: &[u8]| file.windows(needle.len()).position(|w| w == needle);
        assert!(find(b"V_AV1").is_some());
        let segment = find(&SEGMENT.to_be_bytes()).expect("a segment");
        let size_len = (file[segment + 4].leading_zeros() + 1) as usize;
        let data_at = segment + 4 + size_len;
        for id in [INFO, TRACKS, CUES] {
            let entry = find(&[&[0x53, 0xAB, 0x84][..], &id.to_be_bytes()].concat()).expect("a seek entry");
            let position_at = entry + 7 + 3;
            let position = u64::from_be_bytes(file[position_at..position_at + 8].try_into().expect("8 bytes"));
            let target = usize::try_from(position).expect("an offset") + data_at;
            assert_eq!(file[target..target + 4], id.to_be_bytes(), "seek entry for {id:x}");
        }
    }

    #[test]
    fn obus_are_split_and_temporal_delimiters_dropped() {
        // A temporal delimiter (type 2, empty), a sequence header (type 1, 2 bytes) and a frame
        // (type 6) without a size field, which runs to the end.
        let unit = vec![0x12, 0x00, 0x0A, 0x02, 0xAA, 0xBB, 0x30, 0x01, 0x02];
        let parsed = obus(&unit).expect("well formed");
        assert_eq!(parsed.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(), [2, 1, 6]);
        assert_eq!(parsed[1].1, [0x0A, 0x02, 0xAA, 0xBB]);
        assert_eq!(strip_temporal_delimiters(unit), [0x0A, 0x02, 0xAA, 0xBB, 0x30, 0x01, 0x02]);
        assert_eq!(obus(&[0x0A, 0x05, 0x00]), None, "a size past the end");
    }
}
