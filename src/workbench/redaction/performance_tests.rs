use super::*;

#[test]
fn prefix_lookalikes_do_not_enter_the_slow_path_per_character() {
    // The history budget/window fixtures contain long runs of `x`. Although
    // `x` can start `xoxb-`, `xx` cannot; scanning every byte through drain()
    // makes a small cold index depend on the runner's debug-build CPU speed.
    let policy = RedactionPolicy::new(vec![]).unwrap();
    let text = "x".repeat(4096);
    let mut stream = TextRedactor::new(policy);
    let mut output = stream.push(&text).0;
    output.push_str(stream.finish().as_str());
    assert_eq!(output, text);
    assert!(
        stream.slow_path_steps <= 8,
        "prefix lookalikes took {} slow-path steps",
        stream.slow_path_steps
    );
}

#[test]
fn pair_filter_preserves_case_collisions_short_secrets_and_chunk_state() {
    let empty = RedactionPolicy::new(vec![]).unwrap();
    for (prefix, _) in PREFIXES {
        for case in 0..4 {
            let mut prefix = prefix.as_bytes().to_vec();
            for (index, byte) in prefix.iter_mut().take(2).enumerate() {
                if case & (1 << index) != 0 {
                    byte.make_ascii_uppercase();
                }
            }
            let prefix = String::from_utf8(prefix).unwrap();
            let input = format!("xx中文{prefix}synthetic-value! END");
            for split in input
                .char_indices()
                .map(|(index, _)| index)
                .chain([input.len()])
            {
                for tool in [false, true] {
                    compare(&empty, tool, &[&input[..split], "", &input[split..]]);
                }
            }
        }
    }
    // A real generic prefix after many lookalikes must still be masked.
    let input = format!("{}xoxb-synthetic!", "x".repeat(4096));
    assert_eq!(
        empty.scrub(&input).as_str(),
        format!("{}{OMITTED}!", "x".repeat(4096))
    );
    for split in [0, 1, 4096, 4097, 4098, 4100, input.len()] {
        compare(&empty, false, &[&input[..split], &input[split..]]);
    }
    let single = RedactionPolicy::new(vec!["x".into(), "xy".into()]).unwrap();
    assert_eq!(single.possible_second[b'x' as usize], u64::MAX);
    let collision = RedactionPolicy::new(vec!["qa".into()]).unwrap();
    assert_eq!(b'a' & 63, b'!' & 63);
    assert_eq!(collision.scrub("q! qa").as_str(), format!("q! {OMITTED}"));
    let unicode = RedactionPolicy::new(vec!["中文秘密".into(), "𐀀secret".into()]).unwrap();
    for (policy, input) in [
        (&single, "x xy xx xoxb-synthetic! 中文"),
        (&collision, "q! qa qA q 中文"),
        (&unicode, "中仿文秘密 中文秘密 🔑𐀀secret xoxb-synthetic! 中"),
    ] {
        for split in input
            .char_indices()
            .map(|(index, _)| index)
            .chain([input.len()])
        {
            for tool in [false, true] {
                compare(policy, tool, &[&input[..split], "", &input[split..]]);
                compare(policy, tool, &[&input[..split]]);
            }
        }
    }
}

#[test]
#[ignore = "synthetic CPU benchmark; run explicitly with --nocapture"]
fn synthetic_large_text_benchmark() {
    let policy = RedactionPolicy::new(vec!["synthetic-private-value".into()]).unwrap();
    for (label, unit) in [
        ("chinese", "这是普通中文正文，用于测量脱敏文本处理性能。\n"),
        (
            "english",
            "Ordinary English prose describes local files and useful results.\n",
        ),
    ] {
        let text = unit.repeat((1024 * 1024 / unit.len()) + 1);
        for reference in [true, false] {
            let start = std::time::Instant::now();
            for _ in 0..3 {
                let mut stream = TextRedactor::new(policy.clone());
                stream.reference = reference;
                let mut output = stream.push(std::hint::black_box(&text)).0;
                output.push_str(stream.finish().as_str());
                assert_eq!(output, text);
                std::hint::black_box(output);
            }
            eprintln!(
                "{label} reference={reference}: {} bytes x3: {:?}",
                text.len(),
                start.elapsed()
            );
        }
    }
}

fn compare(policy: &Arc<RedactionPolicy>, tool: bool, chunks: &[&str]) {
    let mut actual = if tool {
        TextRedactor::tool(policy.clone())
    } else {
        TextRedactor::new(policy.clone())
    };
    let mut reference = if tool {
        TextRedactor::tool(policy.clone())
    } else {
        TextRedactor::new(policy.clone())
    };
    reference.reference = true;
    for chunk in chunks {
        assert_eq!(actual.push(chunk).as_str(), reference.push(chunk).as_str());
        assert_eq!(actual.pending, reference.pending);
        let mask = |value: Option<Mask>| {
            value.map(|value| match value {
                Mask::Credential => 1,
                Mask::DataUri => 2,
                Mask::Rest => 3,
            })
        };
        assert_eq!(mask(actual.masked), mask(reference.masked));
    }
    assert_eq!(actual.finish().as_str(), reference.finish().as_str());
    assert_eq!(actual.pending, reference.pending);
    assert!(actual.masked.is_none());
    assert!(reference.masked.is_none());
    assert_eq!(
        actual.push(" after finish 中文").as_str(),
        reference.push(" after finish 中文").as_str()
    );
    assert_eq!(actual.finish().as_str(), reference.finish().as_str());
    assert_eq!(actual.pending, reference.pending);
    assert!(actual.masked.is_none());
    assert!(reference.masked.is_none());
}

#[test]
fn fast_path_matches_original_at_every_utf8_boundary_and_random_chunks() {
    let policies = [
        RedactionPolicy::new(vec![]).unwrap(),
        RedactionPolicy::new(vec![
            "中文秘密".into(),
            "中文秘密扩展".into(),
            "🔑synthetic".into(),
            "bearer synthetic-long-secret".into(),
            "xy".into(),
            "abcdefghijk".into(),
            "abc".into(),
            "sk-fixture-long".into(),
        ])
        .unwrap(),
    ];
    let mut inputs: Vec<String> = vec![
        "普通中文🙂 English xy 中文秘密扩展 🔑synthetic 结束",
        "bearer synthetic-long-X token=abc&ok=1 abcdefgh",
        "SK-fixture-long! BeArEr value\nBASIC c3ludGhldGlj== next",
        "{\"password\":\"synthetic\"} {encrypted_content: 'synthetic'}",
        r#"{"t\u006fken":"synthetic"} API_KEY = synthetic"#,
        "data:image/png;base64,AAAA) data:audio/wav;base64,AAAA\nend",
        "-----BEGIN PRIVATE KEY-----\nsynthetic",
        "b ba bas basi basic basicx g gh ghi github_pat_X 中文秘 🔑synthe",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for (prefix, _) in PREFIXES {
        inputs.push(format!("普通 {prefix}synthetic-value! END"));
        inputs.push(format!(
            "普通 {}synthetic-value! END",
            prefix.to_ascii_uppercase()
        ));
    }
    let atoms = [
        "a",
        "b",
        "中",
        "文",
        "秘密",
        "🔑",
        "synthetic",
        " ",
        "\n",
        "bearer ",
        "sk-",
        "token=",
        "data:image/",
        "xy",
        "abc",
        "defgh",
        "\"password\":",
        "\\u0061",
        "!",
        "ghp_",
    ];
    let mut seed = 0x92ab_1773_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        seed
    };
    for _ in 0..80 {
        let mut text = String::new();
        for _ in 0..40 {
            text.push_str(atoms[next() as usize % atoms.len()]);
        }
        inputs.push(text);
    }
    for policy in policies {
        for tool in [false, true] {
            for input in &inputs {
                let boundaries: Vec<_> = input
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain([input.len()])
                    .collect();
                for &split in &boundaries {
                    compare(
                        &policy,
                        tool,
                        &["", &input[..split], "", &input[split..], ""],
                    );
                    // Finishing at every prefix also checks interrupted secrets.
                    compare(&policy, tool, &[&input[..split]]);
                }
                let characters: Vec<_> = boundaries
                    .windows(2)
                    .map(|pair| &input[pair[0]..pair[1]])
                    .collect();
                compare(&policy, tool, &characters);
                let mut chunks = Vec::new();
                let mut offset = 0;
                while offset + 1 < boundaries.len() {
                    let end = (offset + 1 + next() as usize % 11).min(boundaries.len() - 1);
                    chunks.push(&input[boundaries[offset]..boundaries[end]]);
                    offset = end;
                }
                compare(&policy, tool, &chunks);
            }
        }
    }
}

#[test]
fn fast_path_matches_original_at_secret_budget_boundaries() {
    let singles = RedactionPolicy::new(vec!["q".into(), "中".into(), "🔑".into()]).unwrap();
    let many_secrets: Vec<_> = (0..32)
        .map(|index| format!("合成秘密-{index:02}-value"))
        .collect();
    let many_input = many_secrets.join(" ordinary / ");
    let many = RedactionPolicy::new(many_secrets).unwrap();
    for (policy, input) in [
        (&singles, "q普通中英文🔑 q中🔑 remainder"),
        (&many, many_input.as_str()),
    ] {
        for tool in [false, true] {
            for split in input
                .char_indices()
                .map(|(index, _)| index)
                .chain([input.len()])
            {
                compare(policy, tool, &[&input[..split], &input[split..]]);
                compare(policy, tool, &[&input[..split]]);
            }
        }
    }
    // Exactly the configured byte budget, with a multi-byte first character.
    let longest_secret = format!("🔑{}", "q".repeat(8188));
    assert_eq!(longest_secret.len(), 8192);
    let longest = RedactionPolicy::new(vec![longest_secret.clone()]).unwrap();
    let input = format!("{longest_secret} ordinary 中文");
    for tool in [false, true] {
        for split in [0, 4, 5, 7, 8, 8188, 8191, 8192, 8193, input.len()] {
            compare(&longest, tool, &[&input[..split], &input[split..]]);
            compare(&longest, tool, &[&input[..split]]);
        }
    }
}
