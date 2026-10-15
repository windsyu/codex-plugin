//! Synthetic published catalog; measures new queries, not the old R6 scanner.
use super::*;
#[test]
#[ignore = "R7 10k query p95; run alone to avoid compiler/test contention"]
fn r7_10000_group_and_organization_warm_queries() {
    let (_temp, shared, sources, mut db, _lock) = catalog();
    let tx = db.transaction().unwrap();
    for i in 0..10_000u128 {
        let index = if i < 8000 {
            0
        } else if i < 9000 {
            2
        } else {
            1
        };
        let source = &sources[index];
        let thread = uuid::Uuid::from_u128(if index == 2 { i - 7999 } else { i + 1 }).to_string();
        let key = if index == 0 {
            thread.clone()
        } else {
            format!("record-{i}")
        };
        let mut c = candidate(source, &key, &thread, "/tmp/r7-performance/project");
        if (1000..2000).contains(&i) {
            c.entry.conversation_kind = "subagent".into();
        }
        if index == 1 {
            c.locator = json!({"run":key});
        }
        c.entry.recorded_at = Some(format!(
            "2026-10-08T{:02}:{:02}:{:02}Z",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60
        ));
        tx.execute(
            "INSERT INTO catalog_entries VALUES (?1,?2,?1,?3,?4,?5,?6,'bounded needle search')",
            params![
                source.id(),
                c.entry.entry_id,
                c.entry.project_id,
                c.entry.kind,
                c.entry.recorded_at,
                serde_json::to_string(&c.entry).unwrap()
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO catalog_locators VALUES (?1,?2,?3)",
            params![source.id(), c.entry.entry_id, c.locator.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let original = list(&shared, &sources, GroupQuery::default()).unwrap();
    assert_eq!(
        original["counts"]["primary"],
        json!({"groups":7000,"records":8000})
    );
    assert_eq!(original["counts"]["auxiliary"]["records"], 1000);
    assert_eq!(original["counts"]["runs"]["records"], 1000);
    let id = original["records"][0]["preferredEntryId"]
        .as_str()
        .unwrap()
        .to_string();
    let raw: String = db
        .query_row(
            "SELECT metadata FROM catalog_entries WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    let entry: Entry = serde_json::from_str(&raw).unwrap();
    organization_view::save(
        &shared,
        &sources,
        id.clone(),
        "absent".into(),
        OrganizationWrite {
            instance_id: uuid::Uuid::new_v4(),
            source_identity: entry.source_identity,
            source_revision: entry.source_revision,
            action: "unassigned".into(),
            project_id: None,
        },
    )
    .unwrap();
    for (name, q) in [("groups", None), ("search", Some("needle".into()))] {
        for _ in 0..3 {
            list(
                &shared,
                &sources,
                GroupQuery {
                    q: q.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let mut timings = vec![];
        for _ in 0..25 {
            let started = Instant::now();
            let value = list(
                &shared,
                &sources,
                GroupQuery {
                    q: q.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(value.to_string().len() < 1024 * 1024);
            timings.push(started.elapsed().as_secs_f64() * 1000.);
        }
        timings.sort_by(f64::total_cmp);
        let p95 = timings[23];
        println!(
            "{}",
            json!({"r7Metric":name,"records":10000,"samples":25,"p50Ms":timings[12],"p95Ms":p95,"maxMs":timings[24]})
        );
        assert!(p95 <= 200., "{name} p95 {p95}ms exceeds 200ms");
    }
    let mut timings = vec![];
    for _ in 0..25 {
        let started = Instant::now();
        organization_view::read(&shared, &sources, id.clone()).unwrap();
        timings.push(started.elapsed().as_secs_f64() * 1000.);
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "{}",
        json!({"r7Metric":"organization","samples":25,"p95Ms":timings[23]})
    );
    assert!(timings[23] <= 200.);
}
