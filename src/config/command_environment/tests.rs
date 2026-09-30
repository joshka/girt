use rstest::rstest;

use super::*;
use crate::config::{ConfigInputs, ConfigScope};

fn decode(input: &[u8], pairs: usize, bytes: usize) -> Result<Config, ConfigError> {
    Config::from_command_environment(
        |key| (key == "GIT_CONFIG_PARAMETERS").then(|| input.to_vec()),
        pairs,
        bytes,
    )
}

#[rstest]
#[case::old(b"'fixture.value=a=b'", Some(b"a=b".as_slice()))]
#[case::new(b"'fixture.value'='a=b'", Some(b"a=b".as_slice()))]
#[case::implicit(b"'fixture.value'", None)]
#[case::old_empty(b"'fixture.value='", Some(b"".as_slice()))]
#[case::new_empty(b"'fixture.value'=''", Some(b"".as_slice()))]
#[case::unquoted_empty(b"'fixture.value'=", Some(b"".as_slice()))]
#[case::quote(b"'fixture.value=a'\\''b'", Some(b"a'b".as_slice()))]
#[case::bang(b"'fixture.value'='a'\\!'b'", Some(b"a!b".as_slice()))]
#[case::literal_escape(b"'fixture.value=a\\nb'", Some(b"a\\nb".as_slice()))]
#[case::newline(b"'fixture.value=a\nb'", Some(b"a\nb".as_slice()))]
#[case::bytes(b"'fixture.value=\xff'", Some(b"\xff".as_slice()))]
fn command_environment_values(#[case] input: &[u8], #[case] expected: Option<&[u8]>) {
    let config = decode(input, 1, 4096).unwrap();
    assert_eq!(config.value("fixture", None, "value"), Some(expected));
}

#[rstest]
#[case::bare(b"fixture.value=x")]
#[case::double_quote(b"\"fixture.value=x\"")]
#[case::leading_space(b" 'fixture.value=x'")]
#[case::leading_tab(b"\t'fixture.value=x'")]
#[case::unclosed(b"'fixture.value=x")]
#[case::unquoted_value(b"'fixture.value'=x")]
#[case::adjacent(b"'fixture.value'='a''b'")]
#[case::trailing(b"'fixture.value=x'junk")]
#[case::spaced_equal(b"'fixture.value' = 'x'")]
#[case::no_section(b"'value=x'")]
#[case::no_name(b"'fixture.=x'")]
#[case::numeric_name(b"'fixture.1bad=x'")]
#[case::nul_value(b"'fixture.value=\0'")]
fn command_environment_rejects_invalid(#[case] input: &[u8]) {
    assert!(decode(input, 1, 4096).is_err());
}

#[test]
fn command_environment_preserves_sections_and_runtime_provenance() {
    let environment = Config::from_command_environment(
        |key| match key {
            "GIT_CONFIG_COUNT" => Some(b"1".to_vec()),
            "GIT_CONFIG_KEY_0" => Some(b"fixture.value".to_vec()),
            "GIT_CONFIG_VALUE_0" => Some(b"counted".to_vec()),
            "GIT_CONFIG_PARAMETERS" => {
                Some(b"'fixture.value=legacy'\t'fixture.value' \r\n".to_vec())
            }
            _ => None,
        },
        3,
        4096,
    )
    .unwrap();
    let config = Config::resolve(&ConfigInputs {
        environment: Some(environment),
        command: Some(Config::parse(b"[fixture]\nvalue=cli\n").unwrap()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        config.values("fixture", None, "value").collect::<Vec<_>>(),
        [
            Some(b"counted".as_slice()),
            Some(b"legacy"),
            None,
            Some(b"cli")
        ]
    );
    assert_eq!(
        config
            .section_occurrences()
            .map(|s| s.entry_indices())
            .collect::<Vec<_>>(),
        [&[0][..], &[1], &[2], &[3]]
    );
    assert_eq!(
        config.entries()[2].origin.as_ref().unwrap().scope,
        ConfigScope::Environment
    );
    assert_eq!(
        config.entries()[2].origin.as_ref().unwrap().location.line,
        3
    );
    assert_eq!(
        config.entries()[3].origin.as_ref().unwrap().scope,
        ConfigScope::Command
    );
}

#[test]
fn command_environment_limits_counted_and_legacy_together() {
    let get = |key: &str| match key {
        "GIT_CONFIG_COUNT" => Some(b"1".to_vec()),
        "GIT_CONFIG_KEY_0" => Some(b"a.b".to_vec()),
        "GIT_CONFIG_VALUE_0" => Some(b"c".to_vec()),
        "GIT_CONFIG_PARAMETERS" => Some(b"'a.b=d'".to_vec()),
        _ => None,
    };
    let exact = 1 + 3 + 1 + 7 + 2 * ASSIGNMENT_BYTES;
    assert!(Config::from_command_environment(get, 2, exact).is_ok());
    assert_eq!(
        Config::from_command_environment(get, 2, exact - 1)
            .unwrap_err()
            .reason,
        "environment byte limit"
    );
    assert_eq!(
        Config::from_command_environment(get, 1, exact)
            .unwrap_err()
            .reason,
        "environment pair limit"
    );
    assert_eq!(
        Config::from_command_environment(get, 2, 4)
            .unwrap_err()
            .reason,
        "environment byte limit"
    );
}

#[test]
fn command_environment_checks_limits_before_decoding_next_assignment() {
    assert_eq!(
        decode(b"'a.b=x' 'unterminated", 1, 4096)
            .unwrap_err()
            .reason,
        "environment pair limit"
    );
    assert_eq!(
        decode(b"'unterminated", 1, 1).unwrap_err().reason,
        "environment byte limit"
    );
    assert!(decode(b"", 0, 0).is_ok());
    assert_eq!(
        decode(b"'a.b=x'", 0, 4096).unwrap_err().reason,
        "environment pair limit"
    );
}

#[test]
fn command_environment_preserves_counted_failure_order() {
    let mut legacy_requested = false;
    let error = Config::from_command_environment(
        |key| {
            legacy_requested |= key == "GIT_CONFIG_PARAMETERS";
            (key == "GIT_CONFIG_COUNT").then(|| b"invalid".to_vec())
        },
        10,
        4096,
    )
    .unwrap_err();
    assert_eq!(error.reason, "invalid environment count");
    assert!(!legacy_requested);
}

#[test]
fn counted_environment_still_ignores_legacy() {
    let config = Config::from_environment(
        |key| {
            assert_ne!(key, "GIT_CONFIG_PARAMETERS");
            None
        },
        0,
    )
    .unwrap();
    assert!(config.entries().is_empty());
}

/// Fixtures are original command-environment strings, compared with installed Git's config CLI.
#[rstest]
#[case::both_encodings(b"'fixture.value=old' 'fixture.value'='new'")]
#[case::implicit_and_empty(b"'fixture.value' 'fixture.value=' 'fixture.value'=''")]
#[case::quoted_bytes(b"'fixture.value=a'\\''b' 'fixture.value'='a'\\!'b'")]
#[case::whitespace(b"'fixture.value=a'\t'fixture.value=b'\n'fixture.value=c' \r")]
#[case::subsection(b"'fixture.Sub section.value=x'")]
fn command_environment_matches_git(#[case] input: &[u8]) {
    let temp = tempfile::tempdir().unwrap();
    let empty_config = temp.path().join("empty-config");
    std::fs::write(&empty_config, []).unwrap();
    let input = std::str::from_utf8(input).unwrap();
    let output = std::process::Command::new("git")
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty_config)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "fixture.value")
        .env("GIT_CONFIG_VALUE_0", "counted")
        .env("GIT_CONFIG_PARAMETERS", input)
        .args(["-c", "fixture.value=cli", "config", "--null", "--list"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    let parsed = Config::from_command_environment(
        |key| match key {
            "GIT_CONFIG_COUNT" => Some(b"1".to_vec()),
            "GIT_CONFIG_KEY_0" => Some(b"fixture.value".to_vec()),
            "GIT_CONFIG_VALUE_0" => Some(b"counted".to_vec()),
            "GIT_CONFIG_PARAMETERS" => Some(input.as_bytes().to_vec()),
            _ => None,
        },
        32,
        4096,
    );
    assert!(output.status.success());
    let config = Config::resolve(&ConfigInputs {
        environment: Some(parsed.unwrap()),
        command: Some(Config::parse(b"[fixture]\nvalue=cli\n").unwrap()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(git_list_bytes(&config), output.stdout);
}

fn git_list_bytes(config: &Config) -> Vec<u8> {
    let mut result = Vec::new();
    for entry in config.entries() {
        result.extend(entry.section.iter().map(u8::to_ascii_lowercase));
        result.push(b'.');
        if let Some(subsection) = &entry.subsection {
            result.extend_from_slice(subsection);
            result.push(b'.');
        }
        result.extend(entry.name.iter().map(u8::to_ascii_lowercase));
        if let Some(value) = &entry.value {
            result.push(b'\n');
            result.extend_from_slice(value);
        }
        result.push(0);
    }
    result
}

#[rstest]
#[case::leading(b" 'fixture.value=x'")]
#[case::unquoted(b"fixture.value=x")]
#[case::key(b"'fixture.1bad=x'")]
#[case::quotes(b"'fixture.value'='a''b'")]
fn command_environment_rejection_matches_git(#[case] input: &[u8]) {
    let temp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("git")
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_PARAMETERS", std::str::from_utf8(input).unwrap())
        .args(["config", "--null", "--list"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(decode(input, 1, 4096).is_err());
}
