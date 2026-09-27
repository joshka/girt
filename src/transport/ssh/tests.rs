use rstest::rstest;

use super::*;
use crate::remote::{ProtocolEnvironment, Remote};

fn configured(config: &[u8], environment: SshEnvironment) -> Result<SshRemote, SshError> {
    let config = Config::parse(config).unwrap();
    let destination = Remote::find(&config, b"r")
        .unwrap()
        .unwrap()
        .fetch_destination(&config, &ProtocolEnvironment::default())
        .unwrap();
    SshRemote::configured(&config, &destination, environment)
}

fn default_environment() -> SshEnvironment {
    SshEnvironment {
        default_executable: "/usr/bin/ssh".into(),
        config_file: "/dev/null".into(),
        default_user: "default".into(),
        ..SshEnvironment::default()
    }
}

#[test]
fn configured_endpoint_and_command_precedence() {
    let mut environment = default_environment();
    environment.git_ssh = Some(b"/agent/ssh".to_vec());
    environment.git_ssh_command = Some(b"/agent/ssh -i /agent/key".to_vec());
    environment.approved_command = Some(ApprovedSshCommand {
        configured: b"/agent/ssh -i /agent/key".to_vec(),
        executable: "/agent/ssh".into(),
        arguments: vec!["-i".into(), "/agent/key".into()],
    });
    let remote = configured(
        b"[remote \"r\"]\nurl = ssh://user@host.test:2222/repo\n[core]\nsshCommand = /other/ssh\n",
        environment,
    )
    .unwrap();
    let command = remote.command("git-upload-pack");
    assert_eq!(command.get_program(), "/agent/ssh");
    let args: Vec<_> = command.get_args().collect();
    assert!(args.windows(2).any(|pair| pair == ["-i", "/agent/key"]));
    assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(args.windows(2).any(|pair| pair == ["-l", "user"]));
    assert_eq!(*args.last().unwrap(), "git-upload-pack '/repo'");
    assert!(!format!("{remote:?}").contains("/agent/key"));
}

#[test]
fn unapproved_command_and_variant_are_refused_without_leaking_values() {
    let mut environment = default_environment();
    environment.git_ssh_command = Some(b"secret -oUnsafe=yes".to_vec());
    let error = configured(b"[remote \"r\"]\nurl = host:repo\n", environment).unwrap_err();
    assert!(matches!(error, SshError::Configuration(_)));
    assert!(!format!("{error:?} {error}").contains("secret"));

    let mut environment = default_environment();
    environment.git_ssh_variant = Some(b"plink".to_vec());
    let error = configured(b"[remote \"r\"]\nurl = host:repo\n", environment).unwrap_err();
    assert!(matches!(error, SshError::Configuration("SSH variant")));
}

#[test]
fn config_command_requires_exact_approval() {
    let source =
        b"[remote \"r\"]\nurl = host:repo\n[core]\nsshCommand = /approved/ssh -i /approved/key\n";
    let error = configured(source, default_environment()).unwrap_err();
    assert!(matches!(error, SshError::Configuration(_)));
    let mut environment = default_environment();
    environment.approved_command = Some(ApprovedSshCommand {
        configured: b"/approved/ssh -i /other/key".to_vec(),
        executable: "/approved/ssh".into(),
        arguments: Vec::new(),
    });
    let error = configured(source, environment).unwrap_err();
    assert!(matches!(
        error,
        SshError::Configuration("SSH command approval mismatch")
    ));

    let mut environment = default_environment();
    environment.approved_command = Some(ApprovedSshCommand {
        configured: b"/approved/ssh -i /approved/key".to_vec(),
        executable: "/approved/ssh".into(),
        arguments: vec!["-i".into(), "/approved/key".into()],
    });
    let remote = configured(source, environment).unwrap();
    assert_eq!(
        remote.command("git-upload-pack").get_program(),
        "/approved/ssh"
    );
}

#[rstest]
#[case::percent(b"[remote \"r\"]\nurl = ssh://host/repo%20secret\n")]
#[case::userinfo(b"[remote \"r\"]\nurl = ssh://u:p@host/repo\n")]
#[case::port(b"[remote \"r\"]\nurl = ssh://host:0/repo\n")]
#[case::query(b"[remote \"r\"]\nurl = ssh://host/repo?secret\n")]
fn configured_refuses_unsupported_endpoint_shapes(#[case] source: &[u8]) {
    let error = configured(source, default_environment()).unwrap_err();
    assert!(matches!(error, SshError::Configuration(_)));
    assert!(!format!("{error:?} {error}").contains("secret"));
}

#[test]
fn agent_and_askpass_are_explicit_and_redacted() {
    let mut environment = default_environment();
    environment.agent_socket = Some("/secret/socket".into());
    environment.askpass = Some("/secret/askpass".into());
    let remote = configured(b"[remote \"r\"]\nurl = host:repo\n", environment).unwrap();
    let command = remote.command("git-upload-pack");
    let env: Vec<_> = command.get_envs().collect();
    assert!(env.iter().any(|(name, value)| {
        *name == "SSH_AUTH_SOCK" && *value == Some(std::ffi::OsStr::new("/secret/socket"))
    }));
    assert!(env.iter().any(|(name, value)| {
        *name == "SSH_ASKPASS" && *value == Some(std::ffi::OsStr::new("/secret/askpass"))
    }));
    let args: Vec<_> = command.get_args().collect();
    assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=no"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-o", "IdentityAgent=SSH_AUTH_SOCK"])
    );
    assert!(!format!("{remote:?}").contains("secret"));
}

#[test]
fn configured_input_debug_hides_secrets() {
    let mut environment = default_environment();
    environment.git_ssh_command = Some(b"secret-command".to_vec());
    environment.agent_socket = Some("/secret/socket".into());
    environment.approved_command = Some(ApprovedSshCommand {
        configured: b"secret-command".to_vec(),
        executable: "/secret/ssh".into(),
        arguments: vec!["secret-argument".into()],
    });
    assert!(!format!("{environment:?}").contains("secret"));
    assert!(!format!("{:?}", environment.approved_command.as_ref().unwrap()).contains("secret"));
}

#[rstest]
#[case::option("-oProxyCommand=secret", "user", "repo")]
#[case::url("ssh://host", "user", "repo")]
#[case::scp("user@host:repo", "user", "repo")]
#[case::user("host", "-secret", "repo")]
#[case::path_option("host", "user", "--secret")]
#[case::tilde("host", "user", "~/secret")]
#[case::newline("host", "user", "repo\nsecret")]
#[case::empty("", "user", "repo")]
fn rejects_unsafe_components(#[case] host: &str, #[case] user: &str, #[case] path: &str) {
    let error = SshRemote::new(
        host,
        user,
        22,
        path,
        Path::new("/usr/bin/ssh"),
        Path::new("/dev/null"),
    )
    .unwrap_err();
    assert!(matches!(error, SshError::Configuration(_)));
    assert!(!format!("{error:?} {error}").contains("secret"));
}
#[test]
fn quotes_path_and_separates_options() {
    let remote = SshRemote::new(
        "::1",
        "user",
        22,
        "repo ' ; $(secret)",
        Path::new("/usr/bin/ssh"),
        Path::new("/dev/null"),
    )
    .unwrap();
    let command = remote.command("git-upload-pack");
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(
        *args.last().unwrap(),
        "git-upload-pack 'repo '\\'' ; $(secret)'"
    );
    assert_eq!(args[args.len() - 3], "--");
    assert!(!format!("{remote:?}").contains("secret"));
}
#[rstest]
#[case::zero_port(0, "/usr/bin/ssh", "/dev/null")]
#[case::relative_executable(22, "ssh", "/dev/null")]
#[case::relative_config(22, "/usr/bin/ssh", "config")]
fn rejects_unsupported_configuration(#[case] port: u16, #[case] exe: &str, #[case] config: &str) {
    assert!(
        SshRemote::new(
            "host",
            "user",
            port,
            "repo",
            Path::new(exe),
            Path::new(config)
        )
        .is_err()
    );
}

#[rstest]
#[case::config_over_legacy(None, Some(b"legacy".as_slice()), "configured")]
#[case::command_over_config(Some(b"command".as_slice()), Some(b"legacy".as_slice()), "command")]
#[case::config_without_legacy(None, None, "configured")]
fn selects_highest_priority_command(
    #[case] command: Option<&[u8]>,
    #[case] legacy: Option<&[u8]>,
    #[case] selected: &str,
) {
    let environment = SshEnvironment {
        git_ssh_command: command.map(Vec::from),
        git_ssh: legacy.map(Vec::from),
        approved_command: Some(ApprovedSshCommand {
            configured: selected.as_bytes().to_vec(),
            executable: PathBuf::from(format!("/approved/{selected}")),
            arguments: Vec::new(),
        }),
        ..default_environment()
    };
    let remote = configured(
        b"[remote \"r\"]\nurl = host:repo\n[core]\nsshCommand = old\nsshCommand = configured\n",
        environment,
    )
    .unwrap();
    assert_eq!(
        remote.command("git-upload-pack").get_program(),
        std::ffi::OsStr::new(&format!("/approved/{selected}"))
    );
}

#[rstest]
#[case::command(b"[core]\nsshCommand\n")]
#[case::variant(b"[ssh]\nvariant\n")]
fn rejects_selected_valueless_configuration(#[case] setting: &[u8]) {
    let source = [b"[remote \"r\"]\nurl = host:repo\n".as_slice(), setting].concat();
    let error = configured(&source, default_environment()).unwrap_err();
    assert!(matches!(
        error,
        SshError::Configuration("SSH configuration requires a value")
    ));
}

#[test]
fn environment_overrides_valueless_configuration() {
    let environment = SshEnvironment {
        git_ssh_command: Some(b"approved".to_vec()),
        git_ssh_variant: Some(b"SSH".to_vec()),
        approved_command: Some(ApprovedSshCommand {
            configured: b"approved".to_vec(),
            executable: "/approved/ssh".into(),
            arguments: Vec::new(),
        }),
        ..default_environment()
    };
    let remote = configured(
        b"[remote \"r\"]\nurl = host:repo\n[core]\nsshCommand\n[ssh]\nvariant\n",
        environment,
    )
    .unwrap();
    assert_eq!(
        remote.command("git-upload-pack").get_program(),
        "/approved/ssh"
    );
}

#[rstest]
#[case::environment_command(Some(Vec::new()), Some(b"legacy".to_vec()), "")]
#[case::config_command(None, Some(b"legacy".to_vec()), "[core]\nsshCommand =\n")]
#[case::legacy_command(None, Some(Vec::new()), "")]
fn empty_command_does_not_fall_through(
    #[case] command: Option<Vec<u8>>,
    #[case] legacy: Option<Vec<u8>>,
    #[case] setting: &str,
) {
    let environment = SshEnvironment {
        git_ssh_command: command,
        git_ssh: legacy,
        ..default_environment()
    };
    let source = format!("[remote \"r\"]\nurl = host:repo\n{setting}");
    let error = configured(source.as_bytes(), environment).unwrap_err();
    assert!(matches!(
        error,
        SshError::Configuration("empty SSH command")
    ));
}

#[rstest]
#[case::environment_wins(Some(b"ssh".as_slice()), "plink", true)]
#[case::environment_refused(Some(b"plink".as_slice()), "ssh", false)]
#[case::case_insensitive(None, "SSH", true)]
#[case::auto_is_unsupported(None, "auto", false)]
#[case::simple_is_unsupported(None, "simple", false)]
#[case::unknown_is_unsupported(None, "unknown", false)]
#[case::empty_is_unsupported(Some(b"".as_slice()), "ssh", false)]
fn variant_selection_is_explicit(
    #[case] variant: Option<&[u8]>,
    #[case] setting: &str,
    #[case] supported: bool,
) {
    let environment = SshEnvironment {
        git_ssh_variant: variant.map(Vec::from),
        ..default_environment()
    };
    let source = format!("[remote \"r\"]\nurl = host:repo\n[ssh]\nvariant = {setting}\n");
    let result = configured(source.as_bytes(), environment);
    assert_eq!(result.is_ok(), supported);
}

#[test]
fn lower_priority_approval_cannot_authorize_selected_command() {
    let environment = SshEnvironment {
        git_ssh: Some(b"legacy".to_vec()),
        approved_command: Some(ApprovedSshCommand {
            configured: b"legacy".to_vec(),
            executable: "/approved/ssh".into(),
            arguments: Vec::new(),
        }),
        ..default_environment()
    };
    let error = configured(
        b"[remote \"r\"]\nurl = host:repo\n[core]\nsshCommand = selected\n",
        environment,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SshError::Configuration("SSH command approval mismatch")
    ));
}
