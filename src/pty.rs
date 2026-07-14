//! `pixmux run`: spawn a command inside a PTY and translate its output.
//!
//! Layout:
//!   user terminal <-> pixmux (translate) <-> PTY master/slave <-> child
//!
//! stdin is forwarded to the child verbatim on a helper thread; child output
//! flows through the [`Transformer`] before reaching stdout. For the zellij
//! target, kitty support queries (`a=q`) are answered by pixmux itself so
//! probing applications enable their kitty backend.
//!
//! Window resizes are propagated: a SIGWINCH handler sets an atomic flag, the
//! output loop (poll-based, so it wakes up regardless of which thread received
//! the signal) re-reads the outer terminal size with TIOCGWINSZ, applies it to
//! the PTY master with TIOCSWINSZ, and forwards SIGWINCH to the child's
//! process group so full-screen programs redraw at the new dimensions.

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use nix::pty::{forkpty, ForkptyResult, Winsize};
use nix::sys::signal::{killpg, sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use nix::sys::wait::{waitpid, WaitStatus};

use crate::parser::{Event, StreamParser};
use crate::protocol::ok_response;
use crate::transform::{Target, Transformer};

/// Restore the caller's termios on drop (raw-mode guard).
struct TermiosGuard {
    saved: Option<Termios>,
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, &saved);
        }
    }
}

/// Set when SIGWINCH arrives; consumed by the output loop in [`run`].
static SIGWINCH_PENDING: AtomicBool = AtomicBool::new(false);

/// How often the output loop wakes up to check for a pending resize (ms).
const WINCH_POLL_MS: u16 = 200;

extern "C" fn on_sigwinch(_signo: nix::libc::c_int) {
    // Only an atomic store here: it is async-signal-safe.
    SIGWINCH_PENDING.store(true, Ordering::SeqCst);
}

fn current_winsize() -> Option<Winsize> {
    // TIOCGWINSZ on stdout; fall back to a sane default when not a tty.
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        nix::libc::ioctl(
            std::io::stdout().as_raw_fd(),
            nix::libc::TIOCGWINSZ,
            &mut ws as *mut Winsize,
        )
    };
    if rc == 0 && ws.ws_row > 0 {
        Some(ws)
    } else {
        None
    }
}

/// Re-read the outer terminal size and apply it to the PTY master, then
/// forward SIGWINCH to the child's process group. The kernel also signals the
/// PTY's foreground process group when TIOCSWINSZ actually changes the size;
/// the explicit killpg covers emulators that report a resize with no delta
/// and children that moved out of the foreground group.
fn propagate_winsize(master: &OwnedFd, child: nix::unistd::Pid) {
    if let Some(ws) = current_winsize() {
        let rc = unsafe {
            nix::libc::ioctl(
                master.as_raw_fd(),
                nix::libc::TIOCSWINSZ,
                &ws as *const Winsize,
            )
        };
        if rc == 0 {
            let _ = killpg(child, Signal::SIGWINCH);
        }
    }
}

/// Run `argv` under a PTY, translating output for `target`.
/// Returns the child's exit code.
pub fn run(
    argv: &[String],
    target: Target,
    verbose: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    if argv.is_empty() {
        return Err("no command given: usage `pixmux run -- <command> [args...]`".into());
    }
    let c_args: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_str()))
        .collect::<Result<_, _>>()
        .map_err(|_| "command arguments must not contain NUL bytes")?;

    let winsize = current_winsize().unwrap_or(Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });

    // SAFETY: forkpty is async-signal-unsafe after fork in threaded programs;
    // we fork before spawning any helper thread and the child only calls
    // execvp/_exit, which is the documented safe pattern.
    let fork = unsafe { forkpty(Some(&winsize), None) }?;
    let (child, master): (nix::unistd::Pid, OwnedFd) = match fork {
        ForkptyResult::Child => {
            let err = nix::unistd::execvp(&c_args[0], &c_args).unwrap_err();
            // Write directly to fd 2; we are post-fork.
            let msg = format!("pixmux: cannot exec {}: {err}\n", argv[0]);
            let _ = nix::unistd::write(std::io::stderr(), msg.as_bytes());
            unsafe { nix::libc::_exit(127) };
        }
        ForkptyResult::Parent { child, master } => (child, master),
    };

    // Forward outer-terminal resizes for the lifetime of the child (parent
    // side only; registered after fork so the child keeps the default
    // disposition).
    let winch_action = SigAction::new(
        SigHandler::Handler(on_sigwinch),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: the handler only performs an atomic store (async-signal-safe).
    unsafe { sigaction(Signal::SIGWINCH, &winch_action) }?;

    // Put the controlling terminal in raw mode so keystrokes reach the child
    // unmangled (only when stdin actually is a terminal).
    let stdin_is_tty = nix::unistd::isatty(std::io::stdin().as_raw_fd()).unwrap_or(false);
    let _guard = TermiosGuard {
        saved: if stdin_is_tty {
            let saved = termios::tcgetattr(std::io::stdin().as_fd())?;
            let mut raw = saved.clone();
            termios::cfmakeraw(&mut raw);
            termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, &raw)?;
            Some(saved)
        } else {
            None
        },
    };

    let master_resize = master.try_clone()?;
    let master_read = File::from(master.try_clone()?);
    let mut master_write_input = File::from(master.try_clone()?);
    let mut master_write_resp = File::from(master);

    // stdin -> child (helper thread; exits when stdin closes or child dies).
    let (drop_tx, drop_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if master_write_input.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
            if drop_rx.try_recv().is_ok() {
                break;
            }
        }
    });

    // child output -> transform -> stdout.
    let mut transformer = Transformer::new(target);
    let answer_queries = target == Target::Zellij;
    let mut query_parser = StreamParser::new();
    let mut reader = master_read;
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 65536];
    loop {
        if SIGWINCH_PENDING.swap(false, Ordering::SeqCst) {
            propagate_winsize(&master_resize, child);
        }
        // Poll with a timeout so a pending resize is handled promptly even
        // when the child produces no output (e.g. a paused full-screen app).
        let ready = {
            let mut fds = [PollFd::new(reader.as_fd(), PollFlags::POLLIN)];
            poll(&mut fds, PollTimeout::from(WINCH_POLL_MS))
        };
        match ready {
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e.into()),
            Ok(0) => continue,
            Ok(_) => {}
        }
        match reader.read(&mut buf) {
            // 0 = EOF; EIO is the normal "child exited" signal on Linux.
            Ok(0) => break,
            Err(_) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                if answer_queries {
                    for ev in query_parser.feed(chunk) {
                        if let Event::Graphics { cmd, .. } = ev {
                            if cmd.is_query() {
                                let _ = master_write_resp.write_all(&ok_response(&cmd));
                            }
                        }
                    }
                }
                let out = transformer.feed(chunk);
                stdout.write_all(&out)?;
                stdout.flush()?;
            }
        }
    }
    let tail = transformer.finish();
    stdout.write_all(&tail)?;
    stdout.flush()?;
    let _ = drop_tx.send(());

    if verbose {
        let st = transformer.stats();
        eprintln!(
            "pixmux: {} graphics command(s), {} translated, {} untranslated",
            st.graphics_commands, st.translated, st.untranslated
        );
        for note in transformer.notes() {
            eprintln!("pixmux: note: {note}");
        }
    }

    let code = match waitpid(child, None)? {
        WaitStatus::Exited(_, code) => code,
        WaitStatus::Signaled(_, sig, _) => 128 + sig as i32,
        _ => 0,
    };
    Ok(code)
}
