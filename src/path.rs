use anyhow::{Result, bail};

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
}
