use super::*;

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
