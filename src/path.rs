use anyhow::{Result, bail};

pub const CONFIG_SHARE_GLOB_ERROR: &str = "Glob patterns are not supported in .ai-workspace.json share entries. Use \"docs\" to share the directory.";

pub fn normalize_portable_rel_path(input: &str) -> Result<String> {
    let value = input.trim().replace('\\', "/");
    let value = value.trim_end_matches('/');

    if value.is_empty() {
        bail!("Path must name a file or directory inside project directory");
    }
    if value.starts_with('/') || value.contains(":/") {
        bail!("Path must be relative to project directory: {}", input);
    }

    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" => bail!("Path contains empty component: {}", input),
            "." => {}
            ".." => bail!("Path is outside project directory: {}", input),
            _ => parts.push(part),
        }
    }

    if parts.is_empty() {
        bail!("Path must name a file or directory inside project directory");
    }

    Ok(parts.join("/"))
}

pub fn validate_config_share_path(input: &str) -> Result<String> {
    let normalized = normalize_portable_rel_path(input)?;

    if normalized
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        bail!(CONFIG_SHARE_GLOB_ERROR);
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_portable_rel_path_accepts_common_aliases() {
        let cases = [
            ("examples", "examples"),
            ("examples/", "examples"),
            (r"examples\", "examples"),
            (r".\examples", "examples"),
            (r"docs\README.md", "docs/README.md"),
            ("docs/./guide.md", "docs/guide.md"),
        ];

        for (input, expected) in cases {
            assert_eq!(normalize_portable_rel_path(input).unwrap(), expected);
        }
    }

    #[test]
    fn normalize_portable_rel_path_rejects_unsafe_or_empty_paths() {
        for input in [
            "",
            " ",
            ".",
            "/abs",
            r"C:\abs",
            "../x",
            "docs/../x",
            "docs//x",
        ] {
            assert!(
                normalize_portable_rel_path(input).is_err(),
                "expected {input:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_config_share_path_accepts_concrete_directory_aliases() {
        for input in ["docs", "docs/", r"docs\"] {
            assert_eq!(validate_config_share_path(input).unwrap(), "docs");
        }
    }

    #[test]
    fn validate_config_share_path_rejects_glob_patterns() {
        let expected = "Glob patterns are not supported in .ai-workspace.json share entries. Use \"docs\" to share the directory.";

        for input in [
            "docs/**",
            r"docs\**",
            "docs/guide?.md",
            "docs/[draft].md",
            "docs/{api,web}.md",
        ] {
            let err = validate_config_share_path(input).unwrap_err();
            assert_eq!(err.to_string(), expected);
        }
    }
}
