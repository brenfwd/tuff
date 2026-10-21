use super::*;

fn header() -> Header {
    Header::current("Steve", 1_700_000_000_000_000_000)
}

fn write(records: &[(i64, Tag, Vec<u8>)]) -> Vec<u8> {
    let mut writer = Writer::new(Vec::new(), &header()).unwrap();
    for (ns, tag, payload) in records {
        writer.push(*ns, *tag, payload).unwrap();
    }
    writer.finish().unwrap()
}

fn prelude() -> Vec<u8> {
    Writer::new(Vec::new(), &header()).unwrap().out
}

fn block(raw: &[u8], count: u32) -> Vec<u8> {
    let mut crc = flate2::Crc::new();
    crc.update(raw);
    block_with(raw, count, crc.sum(), raw.len() as u32)
}

fn block_with(raw: &[u8], count: u32, crc: u32, raw_len: u32) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(raw).unwrap();
    let packed = encoder.finish().unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&raw_len.to_le_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&packed);
    out
}

fn read(bytes: &[u8]) -> Result<Recording, FormatError> {
    read_all(bytes, usize::MAX)
}

fn play_cb() -> Tag {
    Tag::packet(ConnState::Play, Direction::Clientbound)
}

#[tuff_core::watchdog]
#[test]
fn records_round_trip_with_tags_and_out_of_order_times() {
    let records = vec![
        (5, Tag::event(), vec![EVENT_CONNECTION, 1]),
        (
            10,
            Tag::packet(ConnState::Handshake, Direction::Serverbound),
            vec![0, 1, 2],
        ),
        (8, play_cb().chat(), vec![0x72, b'h', b'i']),
        (
            1_000_000_000_000,
            Tag::packet(ConnState::Login, Direction::Serverbound).redacted(),
            vec![1],
        ),
    ];
    let recording = read(&write(&records)).unwrap();
    assert_eq!(recording.header, header());
    assert!(!recording.truncated);
    let got: Vec<_> = recording
        .records
        .iter()
        .map(|r| (r.ns, r.tag, r.payload.to_vec()))
        .collect();
    assert_eq!(got, records);
    let chat = &recording.records[2];
    assert!(chat.tag.is_chat() && !chat.tag.is_redacted());
    assert_eq!(chat.tag.state(), ConnState::Play);
    assert_eq!(chat.tag.direction(), Direction::Clientbound);
    assert_eq!(chat.packet_id(), Some(0x72));
    assert_eq!(&chat.body()[..], b"hi");
    assert!(recording.records[3].tag.is_redacted());
    assert_eq!(recording.records[0].packet_id(), None);
}

#[tuff_core::watchdog]
#[test]
fn many_records_span_blocks_and_compress() {
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 7) as u8).collect();
    let records: Vec<_> = (0..500)
        .map(|i| (i * 50_000_000, play_cb(), payload.clone()))
        .collect();
    let bytes = write(&records);
    assert!(bytes.len() * 4 < 500 * 1000, "{} bytes", bytes.len());
    let recording = read(&bytes).unwrap();
    assert_eq!(recording.records.len(), 500);
    assert_eq!(recording.records[499].ns, 499 * 50_000_000);
    let mut reader = Reader::new(&bytes[..]).unwrap();
    let mut blocks = 0;
    while reader.next_block().unwrap().is_some() {
        blocks += 1;
    }
    assert!(blocks > 1);
}

#[tuff_core::watchdog]
#[test]
fn an_oversized_payload_is_kept_as_its_redacted_id() {
    let mut big = vec![0x80, 0x01];
    big.resize(MAX_RECORD + 10, 0xaa);
    let recording = read(&write(&[(0, play_cb(), big)])).unwrap();
    let record = &recording.records[0];
    assert!(record.tag.is_redacted());
    assert_eq!(&record.payload[..], &[0x80, 0x01]);
    assert_eq!(record.packet_id(), Some(128));
}

#[tuff_core::watchdog]
#[test]
fn a_cut_file_keeps_its_complete_blocks() {
    let records: Vec<_> = (0..300).map(|i| (i, play_cb(), vec![7u8; 1000])).collect();
    let bytes = write(&records);
    let unterminated = &bytes[..bytes.len() - BLOCK_HEADER];
    let recording = read(unterminated).unwrap();
    assert!(recording.truncated);
    assert_eq!(recording.records.len(), 300);

    let mut reader = Reader::new(&bytes[..]).unwrap();
    let first = reader.next_block().unwrap().unwrap().len();
    let recording = read(&bytes[..bytes.len() / 2]).unwrap();
    assert!(recording.truncated);
    assert!(recording.records.len() >= first && recording.records.len() < 300);
}

#[tuff_core::watchdog]
#[test]
fn headers_are_checked() {
    assert_eq!(read(b"").unwrap_err(), FormatError::Magic);
    assert_eq!(read(b"NOTATRACEFILE").unwrap_err(), FormatError::Magic);
    let mut future = prelude();
    future[8] = 2;
    assert_eq!(
        read(&future).unwrap_err(),
        FormatError::Version { found: 2 }
    );
    let mut huge = prelude();
    huge[10..14].copy_from_slice(&(MAX_HEADER as u32 + 1).to_le_bytes());
    assert_eq!(read(&huge).unwrap_err(), FormatError::Header);
    let mut short = prelude();
    short.truncate(short.len() - 3);
    assert_eq!(read(&short).unwrap_err(), FormatError::Header);
    let mut lying = MAGIC.to_vec();
    lying.extend_from_slice(&VERSION.to_le_bytes());
    lying.extend_from_slice(&3u32.to_le_bytes());
    lying.extend_from_slice(&[0x80, 0x80, 0x80]);
    assert_eq!(
        read(&lying).unwrap_err(),
        FormatError::Corrupt("truncated record")
    );
}

#[tuff_core::watchdog]
#[test]
fn block_lengths_are_capped_before_reading() {
    let mut file = prelude();
    file.extend_from_slice(&(MAX_BLOCK_RAW as u32 + 1).to_le_bytes());
    file.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::BlockTooLarge(MAX_BLOCK_RAW + 1)
    );

    let mut file = prelude();
    file.extend_from_slice(&100u32.to_le_bytes());
    file.extend_from_slice(&(100 + PACKED_SLACK as u32 + 1).to_le_bytes());
    file.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::Corrupt("block lengths")
    );
}

#[tuff_core::watchdog]
#[test]
fn an_inflate_bomb_stops_at_the_declared_length() {
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let mut crc = flate2::Crc::new();
    crc.update(&zeros[..16_384]);
    let mut file = prelude();
    let mut bomb = block_with(&zeros, 1, crc.sum(), 16_384);
    let packed = u32::from_le_bytes(bomb[4..8].try_into().unwrap());
    assert!(packed < 16_384, "{packed}");
    file.append(&mut bomb);
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::Corrupt("inflated length")
    );
}

#[tuff_core::watchdog]
#[test]
fn corrupt_blocks_are_rejected() {
    let raw = [play_cb().byte(), 0, 2, 0xaa, 0xbb];
    let good = {
        let mut file = prelude();
        file.extend(block(&raw, 1));
        file.extend_from_slice(&[0; BLOCK_HEADER]);
        file
    };
    assert_eq!(read(&good).unwrap().records.len(), 1);

    let mut file = prelude();
    file.extend(block_with(&raw, 1, 12345, raw.len() as u32));
    assert_eq!(read(&file).unwrap_err(), FormatError::Corrupt("checksum"));

    let mut flipped = good.clone();
    let at = prelude().len() + BLOCK_HEADER + 3;
    flipped[at] ^= 0xff;
    assert!(read(&flipped).is_err());

    let mut file = prelude();
    file.extend(block(&raw, 2));
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::Corrupt("record count")
    );

    let mut file = prelude();
    file.extend(block(&[0xc0, 0, 0], 1));
    assert_eq!(read(&file).unwrap_err(), FormatError::Corrupt("record tag"));

    let mut file = prelude();
    file.extend(block(&[0x88, 0, 1, 0], 1));
    assert_eq!(read(&file).unwrap_err(), FormatError::Corrupt("reference"));

    let mut file = prelude();
    file.extend(block(&[0x07, 0, 0], 1));
    assert_eq!(read(&file).unwrap_err(), FormatError::Corrupt("record tag"));

    let mut oversized = vec![play_cb().byte(), 0];
    put_varint(&mut oversized, MAX_RECORD as u64 + 1);
    let mut file = prelude();
    file.extend(block(&oversized, 1));
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::RecordTooLarge(MAX_RECORD + 1)
    );

    let mut short = vec![play_cb().byte(), 0];
    put_varint(&mut short, 50);
    short.extend_from_slice(&[1, 2, 3]);
    let mut file = prelude();
    file.extend(block(&short, 1));
    assert_eq!(
        read(&file).unwrap_err(),
        FormatError::Corrupt("truncated record")
    );

    let mut file = prelude();
    file.extend(block(
        &[
            play_cb().byte(),
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0xff,
            0x01,
        ],
        1,
    ));
    assert_eq!(read(&file).unwrap_err(), FormatError::Corrupt("varint"));
}

#[tuff_core::watchdog]
#[test]
fn the_total_payload_is_capped() {
    let records: Vec<_> = (0..100).map(|i| (i, play_cb(), vec![1u8; 1000])).collect();
    let bytes = write(&records);
    assert_eq!(
        read_all(&bytes[..], 50_000).unwrap_err(),
        FormatError::TooLarge(50_000)
    );
    assert_eq!(read_all(&bytes[..], 100_000).unwrap().records.len(), 100);
}

#[tuff_core::watchdog]
#[test]
fn zigzag_round_trips_extremes() {
    for value in [0, 1, -1, i64::MAX, i64::MIN, 123_456_789, -987_654_321] {
        assert_eq!(unzigzag(zigzag(value)), value);
    }
}

fn deduped_header() -> Header {
    let mut header = header();
    header.flags |= FLAG_DEDUPE;
    header
}

fn write_deduped(records: &[(i64, Tag, Vec<u8>)]) -> Vec<u8> {
    let mut writer = Writer::new(Vec::new(), &deduped_header()).unwrap();
    for (ns, tag, payload) in records {
        writer.push(*ns, *tag, payload).unwrap();
    }
    writer.finish().unwrap()
}

#[tuff_core::watchdog]
#[test]
fn identical_packets_are_stored_once_and_read_back_in_full() {
    let chunk = vec![7u8; 4000];
    let records: Vec<_> = (0..50)
        .map(|i| (i, play_cb(), chunk.clone()))
        .chain([
            (60, play_cb(), vec![9u8; 40]),
            (61, Tag::event(), vec![1, 2]),
        ])
        .collect();
    let plain = write(&records);
    let deduped = write_deduped(&records);
    assert!(deduped.len() < plain.len());
    let read = read(&deduped).unwrap();
    assert_eq!(read.records.len(), records.len());
    for (got, (ns, tag, payload)) in read.records.iter().zip(&records) {
        assert_eq!(
            (got.ns, got.tag, &got.payload[..]),
            (*ns, *tag, &payload[..])
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn a_reference_keeps_its_own_tag_and_small_packets_stay_inline() {
    let big = vec![3u8; 100];
    let small = vec![4u8; 8];
    let chat = play_cb().chat();
    let records = vec![
        (0, play_cb(), big.clone()),
        (1, chat, big.clone()),
        (2, play_cb(), small.clone()),
        (3, play_cb(), small.clone()),
    ];
    let bytes = write_deduped(&records);
    let read = read(&bytes).unwrap();
    assert_eq!(read.records[1].tag, chat);
    assert_eq!(&read.records[1].payload[..], &big[..]);
    assert_eq!(&read.records[3].payload[..], &small[..]);
}

#[tuff_core::watchdog]
#[test]
fn packets_that_left_the_window_are_written_in_full_again() {
    let first = vec![1u8; 64];
    let mut records = vec![(0, play_cb(), first.clone())];
    for i in 0..WINDOW_ENTRIES as i64 + 2 {
        let mut other = vec![2u8; 64];
        other[..8].copy_from_slice(&(i as u64).to_le_bytes());
        records.push((i + 1, play_cb(), other));
    }
    records.push((1_000_000, play_cb(), first.clone()));
    let read = read(&write_deduped(&records)).unwrap();
    assert_eq!(read.records.len(), records.len());
    assert_eq!(&read.records.last().unwrap().payload[..], &first[..]);
}

#[tuff_core::watchdog]
#[test]
fn a_dangling_or_malformed_reference_is_corrupt() {
    let mut dangling = prelude_with(&deduped_header());
    dangling.extend(block(&[0x88, 0, 1, 5], 1));
    assert_eq!(
        read(&dangling).unwrap_err(),
        FormatError::Corrupt("dangling reference")
    );

    let mut trailing = prelude_with(&deduped_header());
    trailing.extend(block(&[0x88, 0, 2, 0, 0], 1));
    assert_eq!(
        read(&trailing).unwrap_err(),
        FormatError::Corrupt("reference")
    );
}

fn prelude_with(header: &Header) -> Vec<u8> {
    Writer::new(Vec::new(), header).unwrap().out
}
