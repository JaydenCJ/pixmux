//! End-to-end CLI tests driving the compiled `pixmux` binary.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pixmux"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_filter(target: &str, input: &[u8]) -> (Vec<u8>, Vec<u8>, i32) {
    let mut child = bin()
        .args(["filter", "--target", target, "--verbose"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pixmux filter");
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

#[test]
fn version_and_help_agree() {
    let v = bin().arg("--version").output().unwrap();
    assert!(v.status.success());
    let version = String::from_utf8(v.stdout).unwrap();
    assert!(version.contains("pixmux 0.1.0"), "got {version:?}");

    let h = bin().arg("--help").output().unwrap();
    assert!(h.status.success());
    let help = String::from_utf8(h.stdout).unwrap();
    for sub in ["run", "filter", "cat", "doctor"] {
        assert!(help.contains(sub), "--help lacks subcommand {sub}");
    }
}

#[test]
fn filter_tmux_wraps_fixture_stream() {
    let input = std::fs::read(fixture("icat_chunked.bin")).unwrap();
    let golden = std::fs::read(fixture("icat_chunked.tmux.golden")).unwrap();
    let (stdout, stderr, code) = run_filter("tmux", &input);
    assert_eq!(code, 0);
    assert_eq!(stdout, golden);
    let err = String::from_utf8_lossy(&stderr);
    assert!(err.contains("4 graphics command(s)"), "stderr: {err}");
}

#[test]
fn filter_zellij_emits_sixel() {
    let input = std::fs::read(fixture("icat_chunked.bin")).unwrap();
    let (stdout, _stderr, code) = run_filter("zellij", &input);
    assert_eq!(code, 0);
    let s = String::from_utf8_lossy(&stdout);
    assert!(s.contains("\u{1b}P0;1;0q"));
    assert!(!s.contains("\u{1b}_G"));
}

#[test]
fn cat_none_target_emits_kitty_apc() {
    let out = bin()
        .args(["cat", "--target", "none"])
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\u{1b}_Ga=T,f=100,"));
}

#[test]
fn cat_tmux_target_wraps() {
    let out = bin()
        .args(["cat", "--target", "tmux"])
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\u{1b}Ptmux;\u{1b}\u{1b}_G"));
}

#[test]
fn cat_zellij_target_transcodes_to_sixel() {
    let out = bin()
        .args(["cat", "--target", "zellij"])
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("\u{1b}P0;1;0q"));
    assert!(!s.contains("\u{1b}_G"));
}

#[test]
fn cat_missing_file_fails_with_message() {
    let out = bin()
        .args(["cat", "/nonexistent/nope.png"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot read"), "stderr: {err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn cat_rejects_non_png() {
    let dir = std::env::temp_dir().join("pixmux-test-nonpng");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake.png");
    std::fs::write(&path, b"GIF89a not a png").unwrap();
    let out = bin().arg("cat").arg(&path).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a PNG"), "stderr: {err}");
}

#[test]
fn doctor_runs_and_reports() {
    let out = bin()
        .arg("doctor")
        .env_remove("TMUX")
        .env_remove("ZELLIJ")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("multiplexer"));
    assert!(s.contains("none"));
}

#[test]
fn doctor_detects_tmux_env() {
    let out = bin()
        .arg("doctor")
        .env("TMUX", "/tmp/tmux-1000/default,42,0")
        .env_remove("ZELLIJ")
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("tmux"));
    assert!(s.contains("allow-passthrough"));
}

#[cfg(unix)]
#[test]
fn run_translates_child_output_under_pty() {
    // The child prints a kitty APC; with --target tmux the wrapped
    // passthrough must appear on pixmux's stdout.
    let out = bin()
        .args([
            "run",
            "--target",
            "tmux",
            "--",
            "printf",
            "before\\033_Ga=T,f=100;QUJD\\033\\\\after",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "status: {:?}", out.status);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("before"));
    assert!(s.contains("after"));
    assert!(
        s.contains("\u{1b}Ptmux;\u{1b}\u{1b}_Ga=T,f=100;QUJD"),
        "wrapped sequence missing in {s:?}"
    );
}

#[cfg(unix)]
#[test]
fn run_propagates_window_resize_to_child() {
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use nix::libc;

    // The child polls its own tty size (the inner PTY created by pixmux) and
    // prints it before and after it changes, so no signal handling is needed
    // on the child side.
    let script = "import struct, fcntl, termios, time, sys\n\
                  def size():\n\
                  \x20   r, c = struct.unpack('HHHH', fcntl.ioctl(1, termios.TIOCGWINSZ, b'\\0' * 8))[:2]\n\
                  \x20   return (c, r)\n\
                  first = size()\n\
                  sys.stdout.write('S1=%dx%d\\n' % first)\n\
                  sys.stdout.flush()\n\
                  deadline = time.time() + 10\n\
                  cur = first\n\
                  while time.time() < deadline and cur == first:\n\
                  \x20   time.sleep(0.05)\n\
                  \x20   cur = size()\n\
                  sys.stdout.write('S2=%dx%d\\n' % cur)\n\
                  sys.stdout.flush()\n";

    // Give pixmux a PTY as stdout so it can read the "outer" size (80x24)
    // via TIOCGWINSZ, then shrink/grow it and send SIGWINCH like a terminal
    // emulator or multiplexer would.
    let pty = nix::pty::openpty(None, None).expect("openpty");
    let initial = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCSWINSZ, &initial) };
    assert_eq!(rc, 0, "TIOCSWINSZ on test PTY failed");

    let slave_out = File::from(pty.slave.try_clone().expect("clone slave"));
    let slave_err = File::from(pty.slave);
    let mut child = bin()
        .args(["run", "--target", "none", "--", "python3", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::from(slave_err))
        .spawn()
        .expect("spawn pixmux run");

    // Drain the PTY master on a helper thread so reads never block the test.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut master_read = File::from(pty.master.try_clone().expect("clone master"));
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match master_read.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut out: Vec<u8> = Vec::new();
    let wait_for = |out: &mut Vec<u8>, needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !String::from_utf8_lossy(out).contains(needle) {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("timed out waiting for {needle:?}; got {out:?}"));
            match rx.recv_timeout(left) {
                Ok(chunk) => out.extend(chunk),
                Err(_) => panic!("PTY closed or timed out waiting for {needle:?}; got {out:?}"),
            }
        }
    };

    // The child saw the initial size that pixmux copied from our PTY.
    wait_for(&mut out, "S1=80x24");

    // Resize the outer PTY and poke pixmux with SIGWINCH (pixmux has no
    // controlling tty here, so the kernel will not signal it for us).
    let resized = libc::winsize {
        ws_row: 50,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(pty.master.as_raw_fd(), libc::TIOCSWINSZ, &resized) };
    assert_eq!(rc, 0, "TIOCSWINSZ resize failed");
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGWINCH) };
    assert_eq!(rc, 0, "sending SIGWINCH to pixmux failed");

    // pixmux must push the new size into the inner PTY, where the child
    // observes it via TIOCGWINSZ.
    wait_for(&mut out, "S2=100x50");

    let status = child.wait().expect("wait pixmux");
    assert!(status.success(), "pixmux exited with {status:?}");
}

#[cfg(unix)]
#[test]
fn run_propagates_exit_code() {
    let out = bin()
        .args(["run", "--target", "none", "--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}

#[cfg(unix)]
#[test]
fn run_missing_command_fails() {
    let out = bin()
        .args([
            "run",
            "--target",
            "none",
            "--",
            "definitely-not-a-command-xyz",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
}
