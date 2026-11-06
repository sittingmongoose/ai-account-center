//! Crash-cut and corruption suites for one log (docs/store.md 2.6, 9.3).

mod common;

use aac_store::event::Event;
use aac_store::frame;
use aac_store::log::Log;
use aac_store::msgpack::Value;
use common::{copy_dir, find, temp_dir};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const T0: u64 = 1_793_000_000_000; // a fixed day, so no test crosses midnight by accident

fn note(i: u64) -> Event {
    Event::new(
        "test.note",
        T0 + i,
        Value::map(vec![
            ("i".into(), Value::UInt(i)),
            ("pad".into(), Value::Str("x".repeat((i % 40) as usize))),
        ])
        .unwrap(),
    )
}

/// Writes `batches` batches of 3 events; returns the sequences written.
fn write_batches(dir: &Path, batches: u64) -> Vec<u64> {
    let (mut log, findings) = Log::open(dir, "readings", "r").unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    let mut seqs = Vec::new();
    for b in 0..batches {
        let mut events: Vec<Event> = (0..3).map(|j| note(b * 3 + j)).collect();
        for e in events.iter_mut() {
            log.assign(e).unwrap();
            seqs.push(e.sequence_id);
        }
        log.write(&events).unwrap();
    }
    seqs
}

fn read_all(log: &Log) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    log.read_after(None, |e| {
        let i = e.payload.get("i").and_then(|v| v.as_u64()).unwrap();
        out.push((e.sequence_id, i));
        Ok(())
    })
    .unwrap();
    out
}

#[test]
fn torn_tail_at_every_offset_keeps_every_acknowledged_frame() {
    let base = temp_dir("torn");
    let written = write_batches(&base, 5);
    let active = find(&base, ".active").pop().unwrap();
    let mut extra = note(99);
    extra.sequence_id = written.last().unwrap() + 1;
    extra.event_id = "r-extra".into();
    let partial = frame::encode(
        1,
        extra.sequence_id,
        &extra.header_bytes().unwrap(),
        &extra.payload_bytes().unwrap(),
    );
    for k in 1..partial.len() {
        let dir = temp_dir("torn-cut");
        copy_dir(&base, &dir);
        let active_copy = dir.join(active.file_name().unwrap());
        OpenOptions::new()
            .append(true)
            .open(&active_copy)
            .unwrap()
            .write_all(&partial[..k])
            .unwrap();
        let (mut log, findings) = Log::open(&dir, "readings", "r").unwrap();
        assert!(
            findings.iter().any(|f| f.kind == "torn_tail_truncated"),
            "cut {k}: {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.kind.contains("degraded")),
            "cut {k}: {findings:?}"
        );
        let seen: Vec<u64> = read_all(&log).into_iter().map(|(s, _)| s).collect();
        assert_eq!(seen, written, "cut {k}");
        let mut next = note(100);
        log.assign(&mut next).unwrap();
        assert!(next.sequence_id > extra.sequence_id, "a sequence was reused at cut {k}");
        log.write(&[next]).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn cuts_below_the_watermark_are_disclosed_as_loss() {
    let base = temp_dir("loss");
    let written = write_batches(&base, 2);
    let active = find(&base, ".active").pop().unwrap();
    let full = fs::read(&active).unwrap();
    // Frame boundaries, to know which frames survive a cut.
    let scan = aac_store::log::scan(&full);
    for cut in 0..full.len() {
        let dir = temp_dir("loss-cut");
        copy_dir(&base, &dir);
        let path = dir.join(active.file_name().unwrap());
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(cut as u64)
            .unwrap();
        let (mut log, findings) = Log::open(&dir, "readings", "r").unwrap();
        assert!(
            findings.iter().any(|f| f.kind == "active_degraded"),
            "cut {cut}: {findings:?}"
        );
        let survivors: Vec<u64> = scan
            .frames
            .iter()
            .filter(|f| f.offset + f.len <= cut)
            .map(|f| f.seq)
            .collect();
        let seen: Vec<u64> = read_all(&log).into_iter().map(|(s, _)| s).collect();
        assert_eq!(seen, survivors, "cut {cut}");
        let mut next = note(200);
        log.assign(&mut next).unwrap();
        assert!(
            next.sequence_id > *written.last().unwrap(),
            "a sequence was reused at cut {cut}"
        );
        log.write(&[next.clone()]).unwrap();
        drop(log);
        // The degraded segment stays disclosed and untouched after another open.
        let (log, _) = Log::open(&dir, "readings", "r").unwrap();
        assert!(log.manifest().segments.iter().any(|s| s.state == "degraded"));
        assert_eq!(read_all(&log).last().unwrap().0, next.sequence_id);
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn bit_flips_in_a_sealed_segment_lose_only_the_hit_frame() {
    let base = temp_dir("flip");
    let written = write_batches(&base, 4);
    {
        let (mut log, _) = Log::open(&base, "readings", "r").unwrap();
        log.seal().unwrap();
    }
    let sealed_name = find(&base, ".seglog").pop().unwrap().file_name().unwrap().to_owned();
    let original = fs::read(base.join(&sealed_name)).unwrap();
    let scan = aac_store::log::scan(&original);
    let target = &scan.frames[5];
    for at in [target.offset + 3, target.offset + 30, target.offset + target.len - 1] {
        let dir = temp_dir("flip-at");
        copy_dir(&base, &dir);
        let sealed = dir.join(&sealed_name);
        let mut bytes = original.clone();
        bytes[at] ^= 0x40;
        fs::write(&sealed, &bytes).unwrap();
        let (mut log, _) = Log::open(&dir, "readings", "r").unwrap();
        let findings = log.verify_sealed().unwrap();
        assert!(findings.iter().any(|f| f.kind == "segment_degraded"), "flip at {at}");
        let seen: Vec<u64> = read_all(&log).into_iter().map(|(s, _)| s).collect();
        let expected: Vec<u64> = written.iter().copied().filter(|s| *s != target.seq).collect();
        assert_eq!(seen, expected, "flip at {at}");
        // A degraded sealed segment is never rewritten, and stays disclosed.
        assert_eq!(fs::read(&sealed).unwrap(), bytes);
        drop(log);
        let (log, _) = Log::open(&dir, "readings", "r").unwrap();
        assert_eq!(log.manifest().segments[0].state, "degraded");
        drop(log);
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn compression_round_trips_and_survives_crashes_between_steps() {
    let dir = temp_dir("zst");
    let written = write_batches(&dir, 6);
    let (mut log, _) = Log::open(&dir, "readings", "r").unwrap();
    log.seal().unwrap();
    let (start, raw, sha) = log.compress_candidates().pop().unwrap();
    let (temp, bytes) = Log::compress_file(&raw, &sha).unwrap();
    // Crash before publishing: the temporary file is removed and the raw file kept.
    drop(log);
    let (log, findings) = Log::open(&dir, "readings", "r").unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    assert!(!temp.exists());
    assert_eq!(read_all(&log).iter().map(|x| x.0).collect::<Vec<_>>(), written);
    drop(log);
    // Publish, then crash before the raw file is removed: the next open finishes the job.
    let (log, _) = Log::open(&dir, "readings", "r").unwrap();
    let (temp, bytes2) = Log::compress_file(&raw, &sha).unwrap();
    assert_eq!(bytes, bytes2);
    let zst = raw.with_file_name(format!("{}.zst", raw.file_name().unwrap().to_string_lossy()));
    fs::copy(&temp, &zst).unwrap();
    drop(log);
    let _ = fs::remove_file(&temp);
    let (log, findings) = Log::open(&dir, "readings", "r").unwrap();
    // The manifest still says raw; the raw file is present, the stray zst is dropped.
    assert!(findings.is_empty(), "{findings:?}");
    assert!(!zst.exists());
    assert_eq!(read_all(&log).iter().map(|x| x.0).collect::<Vec<_>>(), written);
    drop(log);
    let (mut log, _) = Log::open(&dir, "readings", "r").unwrap();
    let (start2, raw2, sha2) = log.compress_candidates().pop().unwrap();
    assert_eq!(start, start2);
    let (temp, bytes) = Log::compress_file(&raw2, &sha2).unwrap();
    log.finish_compress(start2, &temp, bytes).unwrap();
    assert!(!raw2.exists());
    assert!(log.manifest().segments[0].compressed);
    assert!(log.manifest().segments[0].zst_bytes < log.manifest().segments[0].raw_bytes);
    assert_eq!(read_all(&log).iter().map(|x| x.0).collect::<Vec<_>>(), written);
    assert!(log.verify_sealed().unwrap().is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_seal_interrupted_after_the_rename_is_adopted() {
    let dir = temp_dir("seal");
    let written = write_batches(&dir, 2);
    let active = find(&dir, ".active").pop().unwrap();
    let start: u64 = written[0];
    let end: u64 = *written.last().unwrap();
    fs::rename(&active, dir.join(format!("seg-000001-{start:020}-{end:020}.seglog"))).unwrap();
    let (log, findings) = Log::open(&dir, "readings", "r").unwrap();
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(log.manifest().segments.len(), 1);
    assert!(log.manifest().active.is_none());
    assert_eq!(read_all(&log).iter().map(|x| x.0).collect::<Vec<_>>(), written);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_manifest_is_rebuilt_from_the_frames() {
    let dir = temp_dir("manifest");
    let written = write_batches(&dir, 3);
    {
        let (mut log, _) = Log::open(&dir, "readings", "r").unwrap();
        log.seal().unwrap();
        let mut e = note(50);
        log.assign(&mut e).unwrap();
        log.write(&[e]).unwrap();
    }
    fs::write(dir.join("manifest.v1.msgpack"), b"garbage").unwrap();
    let (log, findings) = Log::open(&dir, "readings", "r").unwrap();
    assert!(findings.iter().any(|f| f.kind == "manifest_rebuilt"), "{findings:?}");
    let seen = read_all(&log);
    assert_eq!(seen.len(), written.len() + 1);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn sequences_are_never_reused_across_opens() {
    let dir = temp_dir("lease");
    let mut last = 0;
    for round in 0..5 {
        let (mut log, _) = Log::open(&dir, "readings", "r").unwrap();
        let mut e = note(round);
        log.assign(&mut e).unwrap();
        assert!(e.sequence_id > last);
        last = e.sequence_id;
        // Every other round "crashes" without writing the assigned event.
        if round % 2 == 0 {
            log.write(&[e]).unwrap();
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}
