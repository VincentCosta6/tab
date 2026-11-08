use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tab-sandbox-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    real(&d)
}

#[test]
fn secrets_are_recognised_by_whole_words() {
    for s in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_PROFILE",
        "STRIPE_SECRET_KEY",
        "SENTRY_DSN",
        "NPM_TOKEN",
        "DATABASE_PASSWORD",
        "SLACK_WEBHOOK_URL",
        "ANTHROPIC_API_KEY",
    ] {
        assert!(env::is_secret(s), "{s} should count as secret");
    }
    for s in [
        "PATH",
        "HOME",
        "PWD",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "LANG",
        "KEYBOARD_LAYOUT",
        "PATHEXT",
        "TERM",
    ] {
        assert!(!env::is_secret(s), "{s} isn't secret");
    }
}

#[test]
fn trust_decides_which_secrets_survive() {
    use env::keeps;
    assert!(keeps("ANTHROPIC_API_KEY", Trust::Person));
    assert!(keeps("ANTHROPIC_API_KEY", Trust::Agent));
    assert!(!keeps("ANTHROPIC_API_KEY", Trust::Untrusted));
    assert!(keeps("GH_TOKEN", Trust::Person));
    assert!(!keeps("GH_TOKEN", Trust::Agent));
    assert!(!keeps("AWS_SECRET_ACCESS_KEY", Trust::Person));
    assert!(keeps("SSH_AUTH_SOCK", Trust::Person));
    assert!(!keeps("SSH_AUTH_SOCK", Trust::Agent));
    assert!(keeps("PATH", Trust::Untrusted));
}

#[test]
fn network_lists() {
    assert_eq!(Network::from_list(&[]), Network::None);
    assert_eq!(Network::from_list(&[" ".into()]), Network::None);
    assert_eq!(Network::from_list(&["*".into()]), Network::Any);
    assert_eq!(
        Network::from_list(&["Crates.io".into()]),
        Network::Domains(vec!["crates.io".into()])
    );
}

#[test]
fn proxy_matches_hosts() {
    let d = vec![
        "crates.io".to_string(),
        "*.githubusercontent.com".to_string(),
    ];
    assert!(proxy::allowed(&d, "crates.io"));
    assert!(proxy::allowed(&d, "CRATES.IO."));
    assert!(!proxy::allowed(&d, "static.crates.io"));
    assert!(proxy::allowed(&d, "raw.githubusercontent.com"));
    assert!(!proxy::allowed(&d, "githubusercontent.com"));
    assert!(!proxy::allowed(&d, "evilcrates.io"));
    assert!(!proxy::allowed(&d, "crates.io.evil.com"));
    assert!(proxy::allowed(&["*".into()], "anything.example"));
}

/// A tiny HTTP server answering "ok" once per connection.
async fn origin() -> u16 {
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    port
}

async fn through(proxy_port: u16, req: String) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).to_string()
}

#[tokio::test]
async fn proxy_tunnels_allowed_hosts_and_refuses_others() {
    let port = origin().await;
    let h = proxy::start(vec!["localhost".into()]).await.unwrap();

    let ok = through(
        h.port,
        format!("CONNECT localhost:{port} HTTP/1.1\r\nHost: localhost\r\n\r\nGET / HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
    .await;
    assert!(
        ok.starts_with("HTTP/1.1 200 Connection Established"),
        "{ok}"
    );
    assert!(ok.ends_with("ok"), "{ok}");

    let plain = through(
        h.port,
        format!("GET http://localhost:{port}/x HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
    .await;
    assert!(plain.starts_with("HTTP/1.1 200 OK"), "{plain}");

    let no = through(h.port, format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert!(no.starts_with("HTTP/1.1 403"), "{no}");
    assert!(no.contains("127.0.0.1"), "{no}");
}

fn policy(
    protect: Vec<PathBuf>,
    writable: Vec<PathBuf>,
    hide: Vec<PathBuf>,
    network: Network,
) -> Policy {
    Policy {
        protect,
        writable,
        hide,
        reveal: Vec::new(),
        block_exec: Vec::new(),
        drop_env: Vec::new(),
        brokered: false,
        network,
        trust: Trust::Untrusted,
    }
}

#[test]
fn seatbelt_profile_orders_rules() {
    let p = policy(
        vec!["/".into()],
        vec!["/work".into()],
        vec!["/home/me/.aws".into()],
        Network::Domains(vec!["crates.io".into()]),
    );
    let s = seatbelt::profile(&p, Some(4242), true);
    let deny = s.find("(deny file-write* (subpath \"/\"))").unwrap();
    let allow = s
        .find("(allow file-write* (literal \"/work\") (subpath \"/work\"))")
        .unwrap();
    let hide = s
        .find("(deny file-read* file-write* (literal \"/home/me/.aws\")")
        .unwrap();
    assert!(deny < allow && allow < hide, "{s}");
    assert!(s.contains("(deny network-outbound (remote ip))"));
    assert!(s.contains("(allow network-outbound (remote ip \"localhost:4242\"))"));
    // Quotes in paths can't break out of the string.
    let q = seatbelt::profile(
        &policy(vec!["/a\"b".into()], vec![], vec![], Network::Any),
        None,
        false,
    );
    assert!(q.contains(r#""/a\"b""#), "{q}");
    assert!(s.contains(seatbelt::PROCARGS_RULE), "{s}");
    assert!(!q.contains("kern.procargs"), "{q}");
    // A program tab runs instead can't be started or copied.
    let mut b = policy(vec![], vec![], vec![], Network::Any);
    b.block_exec.push("/opt/homebrew/bin/gh".into());
    let g = seatbelt::profile(&b, None, false);
    assert!(
        g.contains("(deny process-exec file-read* (literal \"/opt/homebrew/bin/gh\"))"),
        "{g}"
    );
}

#[test]
fn claude_rules_cover_protected_and_hidden_paths() {
    let dir = tmp("claude-rules");
    let file = dir.join("config.json");
    std::fs::write(&file, "{}").unwrap();
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    let p = policy(vec![dir.clone()], vec![], vec![file.clone()], Network::Any);
    let v = s.claude_settings(&p);
    let deny: Vec<String> = serde_json::from_value(v["permissions"]["deny"].clone()).unwrap();
    let d = claude_path(&dir);
    assert!(d.starts_with("//") && !d.contains('\\'), "{d}");
    assert!(deny.contains(&format!("Edit({d}/**)")), "{deny:?}");
    assert!(!deny.iter().any(|r| r.starts_with("Write(")), "{deny:?}");
    assert!(
        deny.contains(&format!("Read({})", claude_path(&file))),
        "{deny:?}"
    );
    assert_eq!(
        claude_path(std::path::Path::new(r"C:\Users\me\x")),
        "//c/Users/me/x"
    );
    assert_eq!(
        claude_path(std::path::Path::new("/home/me/x")),
        "//home/me/x"
    );
    // Nothing that would move Claude Code out of the person's permission
    // mode (CLAUDE_CODE_SUBPROCESS_ENV_SCRUB turns auto into default).
    assert!(v.get("env").is_none(), "{v}");
    // Commands run without asking where the sandbox holds; elsewhere only
    // gh and git, which tab checks itself.
    let allow = |s: &Sandbox, brokered: bool| {
        let mut p = p.clone();
        p.brokered = brokered;
        s.claude_settings(&p)["permissions"]["allow"].clone()
    };
    if s.enforced() {
        assert_eq!(allow(&s, false), serde_json::json!(["Bash"]));
    } else {
        assert_eq!(
            allow(&s, true),
            serde_json::json!(["Bash(gh:*)", "Bash(git:*)"])
        );
        assert_eq!(allow(&s, false), serde_json::json!([]));
    }

    // Off: no rules, but the scrub stays.
    let off = Sandbox::new(
        SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
    );
    assert_eq!(
        off.claude_settings(&p)["permissions"]["deny"],
        serde_json::json!([])
    );
}

#[test]
fn policies_protect_tab_and_hide_credentials() {
    let data = tmp("data");
    let config = tmp("config");
    let s = Sandbox::new(
        SandboxConfig::default(),
        Some(data.clone()),
        Some(config.clone()),
    );
    let chat = s.chat_policy();
    assert!(chat.protect.contains(&data));
    assert!(chat.hide.contains(&config.join("config.json")));
    assert!(chat.hide.contains(&data.join("tab.db")));
    // The tab cloud sign-in, the local hub's records and its TLS key are
    // hidden under every policy, not only read-only.
    let review = s.review_policy(&data.join("pr-1"), &tmp("code"));
    let iso = s.isolated_policy(&[data.join("run")], Network::None);
    for p in [&chat, &review, &iso] {
        for f in [
            config.join("account.json"),
            data.join("hub.db"),
            data.join("hub.db-wal"),
            data.join("hub.db-shm"),
            data.join("hub-tls"),
        ] {
            assert!(p.hide.contains(&f), "{} isn't hidden", f.display());
        }
    }
    assert_eq!(chat.network, Network::Any);
    if let Some(h) = home() {
        let h = real(&h);
        assert!(chat.hide.contains(&h.join(".aws")));
        assert!(
            !chat.hide.contains(&h.join(".ssh")),
            "a person's chat can still push"
        );
        assert!(chat.protect.contains(&h.join(".bashrc")));
        let review = s.review_policy(&data.join("pr-1"), &tmp("code"));
        assert!(review.hide.contains(&h.join(".ssh")));
        assert!(review.hide.contains(&h.join(".config/gh")));
    }
    let iso = s.isolated_policy(&[data.join("run")], Network::None);
    assert_eq!(iso.protect, vec![PathBuf::from("/")]);
    assert!(iso.writable.contains(&data.join("run")));
}

#[tokio::test]
async fn a_missing_start_folder_is_named() {
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    let gone = std::env::temp_dir().join(format!("tab-gone-{}", uuid::Uuid::new_v4()));
    let p = s.chat_policy();
    let exe = std::env::current_exe().unwrap();
    let Err(e) = s.command(&exe, &[], &gone, &p).await.unwrap().spawn() else {
        panic!("spawned in a missing folder");
    };
    assert!(e.to_string().contains(&gone.display().to_string()), "{e}");
}

#[test]
fn a_dev_build_guards_the_installed_app_too() {
    let (data, config, prod) = (tmp("data"), tmp("config"), tmp("prod"));
    let s =
        Sandbox::new(SandboxConfig::default(), Some(data), Some(config)).guarding([prod.clone()]);
    let chat = s.chat_policy();
    assert!(chat.protect.contains(&prod));
    assert!(chat.hide.contains(&prod.join("config.json")));
    assert!(chat.hide.contains(&prod.join("tab.db")));
    assert!(chat.hide.contains(&prod.join("account.json")));
    assert!(chat.hide.contains(&prod.join("hub.db")));
}

#[cfg(unix)]
async fn sh(
    s: &Sandbox,
    p: &Policy,
    cwd: &std::path::Path,
    shell: &str,
    script: &str,
) -> (bool, String) {
    let mut prepared = s
        .command(
            std::path::Path::new(shell),
            &["-c".into(), script.into()],
            cwd,
            p,
        )
        .await
        .unwrap();
    prepared.cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = prepared.spawn().unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .await
        .unwrap();
    let ok = child.wait().await.unwrap().success();
    (ok, out)
}

/// Runs the real sandbox where one works (Linux with bubblewrap, macOS).
#[cfg(unix)]
#[tokio::test]
async fn enforces_files_and_network_where_available() {
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    if !s.backend().enforces() {
        eprintln!(
            "no sandbox tab can start here ({:?}); skipping",
            s.backend()
        );
        return;
    }
    let root = tmp("enforce");
    let (work, locked, secret) = (root.join("work"), root.join("locked"), root.join("secret"));
    for d in [&work, &locked, &secret] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(secret.join("key"), "hunter2").unwrap();
    let w = |d: &std::path::Path| format!("echo hi > '{}/a'", d.display());

    let p = policy(
        vec![locked.clone()],
        vec![],
        vec![secret.clone()],
        Network::Any,
    );
    assert!(
        sh(&s, &p, &root, "/bin/sh", &w(&work)).await.0,
        "unprotected folders stay writable"
    );
    assert!(
        !sh(&s, &p, &root, "/bin/sh", &w(&locked)).await.0,
        "protected folders are read-only"
    );
    let (_, read) = sh(
        &s,
        &p,
        &root,
        "/bin/sh",
        &format!("cat '{}/key'", secret.display()),
    )
    .await;
    assert!(!read.contains("hunter2"), "hidden folders can't be read");

    // Read-only everywhere except the run folder.
    let iso = policy(vec!["/".into()], vec![work.clone()], vec![], Network::None);
    assert!(sh(&s, &iso, &work, "/bin/sh", &w(&work)).await.0);
    assert!(!sh(&s, &iso, &work, "/bin/sh", &w(&root)).await.0);

    // No network: a listener on the host can't be reached.
    let port = origin().await;
    let probe = format!("exec 3<>/dev/tcp/127.0.0.1/{port} && echo reached");
    if std::path::Path::new("/bin/bash").exists() {
        let open = policy(vec![], vec![], vec![], Network::Any);
        assert!(
            sh(&s, &open, &root, "/bin/bash", &probe)
                .await
                .1
                .contains("reached")
        );
        assert!(
            !sh(&s, &iso, &work, "/bin/bash", &probe)
                .await
                .1
                .contains("reached"),
            "network is cut off"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_ends_the_whole_tree() {
    let s = Sandbox::new(
        SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
    );
    let p = s.chat_policy();
    let mut prepared = s
        .command(
            std::path::Path::new("/bin/sh"),
            &["-c".into(), "sleep 300 & echo $!; wait".into()],
            &std::env::temp_dir(),
            &p,
        )
        .await
        .unwrap();
    prepared.cmd.stdout(Stdio::piped());
    let mut child = prepared.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 32];
    let n = stdout.read(&mut buf).await.unwrap();
    let grandchild: i32 = String::from_utf8_lossy(&buf[..n]).trim().parse().unwrap();
    // SAFETY: signal 0 only checks the process exists.
    assert_eq!(unsafe { libc::kill(grandchild, 0) }, 0);
    drop(child);
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Gone, or a zombie waiting for init: either way not running `sleep`.
    let alive = std::fs::read_to_string(format!("/proc/{grandchild}/stat"))
        .map(|s| !s.contains(") Z"))
        .unwrap_or(false);
    let alive = if cfg!(target_os = "linux") {
        alive
    } else {
        unsafe { libc::kill(grandchild, 0) == 0 }
    };
    assert!(!alive, "the grandchild was left running");
}

#[tokio::test]
async fn untrusted_processes_get_no_secrets() {
    // SAFETY: tests in this module don't read this variable concurrently.
    unsafe { std::env::set_var("TAB_TEST_API_TOKEN", "sekrit") };
    let s = Sandbox::new(
        SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
    );
    let p = s.isolated_policy(&[], Network::None);
    let sh = if cfg!(windows) { "cmd" } else { "/bin/sh" };
    if cfg!(windows) {
        return;
    }
    let mut prepared = s
        .command(
            std::path::Path::new(sh),
            &["-c".into(), "echo \"[$TAB_TEST_API_TOKEN]\"".into()],
            &std::env::temp_dir(),
            &p,
        )
        .await
        .unwrap();
    prepared.cmd.stdout(Stdio::piped());
    let out = prepared.spawn().unwrap();
    let mut child = out;
    let mut s = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut s)
        .await
        .unwrap();
    assert_eq!(s.trim(), "[]");
}

/// The agent CLIs start and answer inside a person's chat policy. Ignored
/// by default: it needs `claude` installed.
#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn claude_starts_in_the_chat_sandbox() {
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    let exe = which::which("claude").expect("claude on PATH");
    let (ok, out) = sh(
        &s,
        &s.chat_policy(),
        &std::env::temp_dir(),
        "/bin/sh",
        &format!("'{}' --version", exe.display()),
    )
    .await;
    assert!(ok && out.contains("Claude Code"), "{out}");
}

/// A program tab runs for the agent (the real `gh`) can't be started from
/// inside the sandbox, while everything else still runs.
#[cfg(unix)]
#[tokio::test]
async fn blocked_programs_cannot_start() {
    use std::os::unix::fs::PermissionsExt;
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    if !s.enforced() {
        return;
    }
    let dir = tmp("block-exec");
    let prog = dir.join("fake-gh");
    std::fs::write(&prog, "#!/bin/sh\necho ran\n").unwrap();
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut p = s.chat_policy();
    let script = format!("{} ; echo after", prog.display());
    let (_, out) = sh(&s, &p, &dir, "/bin/sh", &script).await;
    assert!(out.contains("ran"), "{out}");
    p.block_exec.push(real(&prog));
    let (_, out) = sh(&s, &p, &dir, "/bin/sh", &script).await;
    assert!(!out.contains("ran") && out.contains("after"), "{out}");
}

/// tab's own secrets can't be read by anything it starts, under every
/// policy, where the sandbox is enforced: the cloud sign-in, the local
/// hub's copy of every record and its TLS key, and a credential file that
/// didn't exist yet when the process started.
#[cfg(unix)]
#[tokio::test]
async fn tabs_own_secrets_are_unreadable_under_every_policy() {
    let (data, config) = (tmp("own-data"), tmp("own-config"));
    let s = Sandbox::new(
        SandboxConfig::default(),
        Some(data.clone()),
        Some(config.clone()),
    );
    if !s.enforced() {
        eprintln!("no sandbox tab can start here; skipping");
        return;
    }
    std::fs::write(config.join("account.json"), "refresh-token-xyz").unwrap();
    std::fs::write(data.join("hub.db"), "every-record-xyz").unwrap();
    std::fs::create_dir_all(data.join("hub-tls")).unwrap();
    std::fs::write(data.join("hub-tls/key.der"), "hub-key-xyz").unwrap();
    let work = tmp("own-work");
    let policies = [
        s.chat_policy(),
        s.review_policy(&work, &tmp("own-code")),
        s.isolated_policy(std::slice::from_ref(&work), Network::None),
    ];
    for p in &policies {
        for f in [
            config.join("account.json"),
            data.join("hub.db"),
            data.join("hub-tls/key.der"),
        ] {
            let (_, out) = sh(&s, p, &work, "/bin/sh", &format!("cat '{}'", f.display())).await;
            assert!(!out.contains("xyz"), "{} was readable: {out}", f.display());
        }
    }
    // Missing when the sandbox starts, written later: still hidden.
    std::fs::remove_file(config.join("account.json")).unwrap();
    let p = &policies[0];
    let mut child = s
        .command(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "sleep 1; cat \"$0\"".into(),
                config.join("account.json").into(),
            ],
            &work,
            p,
        )
        .await
        .unwrap();
    child.cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = child.spawn().unwrap();
    std::fs::write(config.join("account.json"), "late-token-xyz").unwrap();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .await
        .unwrap();
    assert!(
        !out.contains("xyz"),
        "a sign-in written mid-chat was readable"
    );
}

/// Under bubblewrap a process can't read tab's environment through
/// `/proc/<tab's pid>/environ`: it has its own PID namespace and `/proc`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn tabs_own_environment_is_out_of_reach() {
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    if !s.enforced() || !bwrap_pid() {
        eprintln!("no PID namespace here; skipping");
        return;
    }
    // SAFETY: nothing in these tests reads this variable concurrently.
    unsafe { std::env::set_var("TAB_TEST_PARENT_TOKEN", "parent-only-xyz") };
    let me = std::process::id();
    let tmpd = std::env::temp_dir();
    let p = s.isolated_policy(&[], Network::None);
    let (_, out) = sh(
        &s,
        &p,
        &tmpd,
        "/bin/sh",
        &format!("cat /proc/{me}/environ; echo; echo [$TAB_TEST_PARENT_TOKEN]"),
    )
    .await;
    assert!(out.contains("[]"), "{out}");
    assert!(
        !out.contains("parent-only-xyz"),
        "tab's environment showed through /proc"
    );
}

/// What a runner keeps in memory reaches what it starts only as far as each
/// one's trust allows, and never through tab's own environment.
#[cfg(unix)]
#[tokio::test]
async fn spawn_env_goes_through_trust() {
    let s = Sandbox::new(
        SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
    );
    s.set_spawn_env(
        [
            ("ANTHROPIC_API_KEY".to_string(), "sk-model".to_string()),
            ("GH_TOKEN".to_string(), "ghp-person".to_string()),
        ]
        .into(),
    );
    assert!(std::env::var_os("ANTHROPIC_API_KEY").is_none_or(|v| v != "sk-model"));
    let script = "echo \"[$ANTHROPIC_API_KEY][$GH_TOKEN]\"";
    let tmpd = std::env::temp_dir();
    let (_, chat) = sh(&s, &s.chat_policy(), &tmpd, "/bin/sh", script).await;
    assert_eq!(chat.trim(), "[sk-model][ghp-person]");
    let (_, review) = sh(&s, &s.review_policy(&tmpd, &tmpd), &tmpd, "/bin/sh", script).await;
    assert_eq!(review.trim(), "[sk-model][]");
    let (_, iso) = sh(
        &s,
        &s.isolated_policy(&[], Network::None),
        &tmpd,
        "/bin/sh",
        script,
    )
    .await;
    assert_eq!(iso.trim(), "[][]");
}

/// A runner that requires the sandbox refuses to start anything without it.
#[tokio::test]
async fn strict_sandboxes_refuse_to_run_unenforced() {
    let s = Sandbox::new(
        SandboxConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
    );
    s.require_enforcement(true);
    let exe = std::env::current_exe().unwrap();
    let r = s
        .command(&exe, &[], &std::env::temp_dir(), &s.chat_policy())
        .await;
    assert!(r.is_err(), "started without the sandbox");
}

/// Under bubblewrap a domain list is enforced: with no bridge the process
/// has no network at all, never all of it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn domain_lists_never_mean_open_network_on_linux() {
    let s = Sandbox::new(SandboxConfig::default(), None, None);
    if !s.enforced() || bridge::helper().is_some() {
        return;
    }
    let port = origin().await;
    let p = policy(
        vec![],
        vec![],
        vec![],
        Network::Domains(vec!["example.com".into()]),
    );
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let probe = format!("exec 3<>/dev/tcp/127.0.0.1/{port} && echo reached");
    let (_, out) = sh(&s, &p, &std::env::temp_dir(), "/bin/bash", &probe).await;
    assert!(!out.contains("reached"), "the host was reachable");
    // And Claude Code isn't told commands may run without asking.
    assert_eq!(
        s.claude_settings(&p)["permissions"]["allow"],
        serde_json::json!([])
    );
}

/// The proxy also answers on its Unix socket, which is what a bridged
/// sandbox reaches.
#[cfg(unix)]
#[tokio::test]
async fn proxy_answers_on_its_unix_socket() {
    let port = origin().await;
    let h = proxy::start_on(vec!["localhost".into()], true)
        .await
        .unwrap();
    let sock = h.socket.clone().unwrap();
    let mut s = tokio::net::UnixStream::connect(&sock).await.unwrap();
    s.write_all(
        format!("GET http://localhost:{port}/x HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 200 OK"));
    let dir = sock.parent().unwrap().to_path_buf();
    drop(h);
    assert!(!dir.exists(), "the socket's folder outlived the proxy");
}
