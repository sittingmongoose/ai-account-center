//! G16: the Rust answer delivery replays what the TS delivery produced
//! (tests/spec/answer-delivery-vectors.json, from tests/spec/answer-delivery-vectors.ts):
//! the same steps in the same words, the same share files byte for byte (signed with the same
//! fixture key), and the same rings.
use aac_status::answer_delivery::*;
use aac_status::answer_signing::load_or_create_answer_key;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../tests/spec/answer-delivery-vectors.json")).unwrap()
}

fn key_from(pem: &str) -> (tempfile::TempDir, aac_status::answer_signing::AnswerKey) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("answer-key.pem");
    std::fs::write(&file, pem).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let key = load_or_create_answer_key(dir.path()).unwrap();
    (dir, key)
}

fn pending() -> Step {
    Step {
        state: "pending".into(),
        attempts: 0,
    }
}

fn entries_before(scenario: &Value) -> Vec<Entry> {
    scenario["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| Entry {
            id: e["id"].as_str().unwrap().into(),
            send_id: e["sendId"].as_str().map(str::to_owned),
            answer: e["answer"].clone(),
            card: e["card"].clone(),
            share: pending(),
            doorbell: pending(),
        })
        .collect()
}

fn sends(scenario: &Value) -> Vec<Send> {
    scenario["sends"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| Send {
            send_id: s["sendId"].as_str().unwrap().into(),
            at: s["at"].as_str().unwrap().into(),
            device: s["device"].as_str().unwrap().into(),
            entry_ids: s["entryIds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().into())
                .collect(),
            summary: pending(),
        })
        .collect()
}

fn files_under(v1: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().into_string().unwrap();
            let rel_name = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &rel_name, out);
            } else {
                out.insert(rel_name, std::fs::read_to_string(entry.path()).unwrap());
            }
        }
    }
    walk(&v1.join("keys"), "keys", &mut out);
    walk(&v1.join("answers"), "answers", &mut out);
    out
}

#[test]
fn every_scenario_replays_byte_for_byte() {
    let v = vectors();
    assert_eq!(v["doorbellSender"], DOORBELL_SENDER);
    assert_eq!(v["noMailDetail"], NO_MAIL_DETAIL);
    assert_eq!(v["doorbellOffDetail"], DOORBELL_OFF_DETAIL);
    let mut replayed = 0;
    for scenario in v["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        // The retried pass is the failed pass plus one ring; its files are the same.
        if name == "ring-retry.retried" {
            continue;
        }
        let (_key_dir, key) = key_from(scenario["key"].as_str().unwrap());
        let reports = tempfile::tempdir().unwrap();
        let v1 = reports.path().join("v1");
        std::fs::create_dir(&v1).unwrap();
        if let Some(newer) = scenario["newerOnShare"].as_str() {
            std::fs::create_dir_all(v1.join("answers")).unwrap();
            std::fs::write(v1.join("answers").join("vm.lead--Q8.json"), newer).unwrap();
        }
        let mut entries = entries_before(scenario);
        let recorded = scenario["entries"].as_array().unwrap();

        // 1. Every signed copy, in journal order.
        for i in 0..entries.len() {
            let id = entries[i].id.clone();
            for step in deliver_share(&v1, &entries, &id, &key, &|| true) {
                let target = if step.step == "share" {
                    &mut entries[i].share
                } else {
                    &mut entries[i].doorbell
                };
                target.state = step.state.into();
                target.attempts += 1;
                let want = &recorded[i][step.step];
                assert_eq!(step.state, want["state"], "{name} {} {}", step.step, id);
                assert_eq!(step.detail, want["detail"].as_str().unwrap(), "{name} {}", step.step);
            }
        }
        // 2. Every send summary.
        for (j, send) in sends(scenario).iter().enumerate() {
            let step = deliver_summary(&v1, &entries, send, &key, &|| true);
            let want = &scenario["sends"][j]["summary"];
            assert_eq!(step.state, want["state"], "{name} summary");
            assert_eq!(step.detail, want["detail"].as_str().unwrap(), "{name} summary");
            // A second write finds this send's summary published and keeps it.
            assert_eq!(deliver_summary(&v1, &entries, send, &key, &|| true).state, "done");
        }
        // 3. The ring plan and the rings.
        let off = name == "doorbell-off";
        let plan = ring_plan(&entries, &HashSet::new(), off);
        for (id, detail) in &plan.skipped {
            let i = entries.iter().position(|e| &e.id == id).unwrap();
            assert_eq!(recorded[i]["doorbell"]["state"], "skipped", "{name}");
            assert_eq!(detail, recorded[i]["doorbell"]["detail"].as_str().unwrap(), "{name}");
        }
        let rings = scenario["rings"].as_array().unwrap();
        let texts: Vec<(String, String, String)> = plan
            .groups
            .iter()
            .map(|group| {
                let (subject, body) = ring_text(group, &entries, &key.key_id);
                (group.to.clone(), subject, body)
            })
            .collect();
        assert_eq!(texts.len(), rings.len(), "{name}: rings");
        for ((to, subject, body), ring) in texts.iter().zip(rings) {
            assert_eq!(to, ring["to"].as_str().unwrap(), "{name}");
            assert_eq!(subject, ring["subject"].as_str().unwrap(), "{name}");
            assert_eq!(body, ring["body"].as_str().unwrap(), "{name}");
        }
        // 4. Every file on the share, byte for byte.
        let want: BTreeMap<String, String> = scenario["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect();
        assert_eq!(files_under(&v1), want, "{name}: share files");
        replayed += 1;
    }
    assert_eq!(replayed, 5);
}

#[test]
fn the_retry_plan_follows_retry_pending() {
    let v = vectors();
    let failed = v["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "ring-retry.failed")
        .unwrap();
    let mut entries = entries_before(failed);
    entries[0].share = Step {
        state: "done".into(),
        attempts: 1,
    };
    entries[0].doorbell = Step {
        state: "failed".into(),
        attempts: 1,
    };
    let mut all = sends(failed);
    all[0].summary = Step {
        state: "done".into(),
        attempts: 1,
    };
    let plan = retry_plan(&entries, &all);
    assert!(plan.shares.is_empty() && plan.summaries.is_empty());
    assert_eq!(plan.rings, vec![entries[0].id.clone()]);
    // The attempt cap ends retries.
    entries[0].doorbell.attempts = MAX_DELIVERY_ATTEMPTS;
    assert!(retry_plan(&entries, &all).rings.is_empty());
    // A share still owed is retried; a send without its summary is retried.
    entries[0].share = Step {
        state: "failed".into(),
        attempts: 3,
    };
    all[0].summary = Step {
        state: "failed".into(),
        attempts: 2,
    };
    let plan = retry_plan(&entries, &all);
    assert_eq!((plan.shares.len(), plan.summaries.len()), (1, 1));
}

#[test]
fn a_dropped_write_never_publishes() {
    let v = vectors();
    let single = &v["scenarios"][0];
    let (_d, key) = key_from(single["key"].as_str().unwrap());
    let reports = tempfile::tempdir().unwrap();
    let v1 = reports.path().join("v1");
    std::fs::create_dir(&v1).unwrap();
    let entries = entries_before(single);
    // `proceed` turns false after a few checks (a deadline or a lost writer mid-write).
    let calls = std::cell::Cell::new(0);
    let steps = deliver_share(&v1, &entries, &entries[0].id, &key, &|| {
        calls.set(calls.get() + 1);
        calls.get() < 6
    });
    assert_eq!(steps[0].state, "failed");
    assert!(!v1.join("answers").join("vm.lead--Q1.json").exists());
    // No temp file is left behind either.
    let leftovers: Vec<_> = std::fs::read_dir(v1.join("keys"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
        .into_iter()
        .collect();
    assert!(leftovers.iter().all(|n| *n == 0));
    // A missing v1 is the unavailable share, nothing created.
    let gone = reports.path().join("missing").join("v1");
    let steps = deliver_share(&gone, &entries, &entries[0].id, &key, &|| true);
    assert_eq!(steps[0].detail, SHARE_UNAVAILABLE_DETAIL);
    assert!(!gone.exists());
}

#[cfg(unix)]
#[test]
fn the_doorbell_runs_pm_mail_with_the_body_on_stdin() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let inbox = dir.path().join("inbox");
    let bin = dir.path().join("pm-mail");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nmode=$(cat '{0}/mode' 2>/dev/null)\nprintf '%s\\n' \"$@\" > '{0}/args'\ncat > '{0}/stdin'\n[ \"$mode\" = sleep ] && sleep 5\n[ \"$mode\" = fail ] && exit 3\nmkdir -p '{1}' && cp '{0}/stdin' '{1}/mail'\nexit 0\n",
            dir.path().display(),
            inbox.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let body = "line one\nline two ✓";
    let ok = pm_mail_ring(
        &bin,
        "lead",
        "2 answers from the owner (deck-7)",
        body,
        std::time::Duration::from_secs(5),
    );
    assert_eq!(
        ok,
        RingResult {
            ok: true,
            detail: "Rang lead through pm-mail.".into()
        }
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("args")).unwrap(),
        "--as\naac-answers\nsend\nlead\n2 answers from the owner (deck-7)\n"
    );
    assert_eq!(std::fs::read_to_string(inbox.join("mail")).unwrap(), body);
    std::fs::write(dir.path().join("mode"), "fail").unwrap();
    assert_eq!(
        pm_mail_ring(&bin, "lead", "s", "b", std::time::Duration::from_secs(5)).detail,
        "pm-mail failed."
    );
    std::fs::write(dir.path().join("mode"), "sleep").unwrap();
    assert_eq!(
        pm_mail_ring(&bin, "lead", "s", "b", std::time::Duration::from_millis(300)).detail,
        "pm-mail did not answer in time."
    );
    assert_eq!(
        pm_mail_ring(
            &dir.path().join("absent"),
            "lead",
            "s",
            "b",
            std::time::Duration::from_secs(1)
        )
        .detail,
        "pm-mail is not installed here."
    );
    // PATH first, then the share's fallback; a bare name when neither holds it.
    assert_eq!(find_pm_mail(Some(dir.path().as_os_str())), bin);
}
