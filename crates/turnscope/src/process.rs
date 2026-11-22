//! Running another program: the Keychain's `security`, an agent's own command.

use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Run `command` to its end, with nothing on its standard input, waiting no
/// longer than `wait`: a locked Keychain waits for a password no one may
/// type, and an agent's command may wait for a prompt.
pub fn output_within(command: &mut Command, wait: Duration) -> std::io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read as written, so a child that writes more than a pipe holds
    // doesn't wait on us. Multiplex both pipes onto one channel so a
    // lingering background process waits one `QUIET` window, not two, and
    // stdout can't starve stderr near the deadline.
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
        is_stdout: bool,
        sent: mpsc::Sender<(bool, Vec<u8>)>,
    ) {
        let Some(mut pipe) = pipe else { return };
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(count @ 1..) = pipe.read(&mut bytes) {
                if sent.send((is_stdout, bytes[..count].to_vec())).is_err() {
                    return;
                }
            }
        });
    }
    let (sent, read) = mpsc::channel();
    drain(child.stdout.take(), true, sent.clone());
    drain(child.stderr.take(), false, sent);
    let deadline = Instant::now() + wait;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let message = format!("no answer within {} s", wait.as_secs());
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, message));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A child can leave a process holding its pipes open, as a login
    // shell's background job does, so each pipe is read until it closes or
    // is quiet for `QUIET`: until the deadline, or `QUIET` past it for a
    // child that ended just before. Chosen over reading to the deadline,
    // which waited all of it: 10 s for a login shell's `PATH`. The cost:
    // output is lost if its reader waits longer than `QUIET` for a
    // processor. Measured 2026-11-14 at a load of 7 on 8 cores: `sleep 4 &
    // echo said` came back in 0.22 s, not 4 s, and 300 children writing
    // 1 MB per pipe lost nothing.
    const QUIET: Duration = Duration::from_millis(100);
    let until = deadline.max(Instant::now() + QUIET);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    while let Ok((is_stdout, more)) =
        read.recv_timeout(QUIET.min(until.saturating_duration_since(Instant::now())))
    {
        if is_stdout {
            stdout.extend(more);
        } else {
            stderr.extend(more);
        }
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_waited_on_to_its_end_not_for_what_it_leaves_running() {
        let began = Instant::now();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 4 & echo said"]);
        let output = output_within(&mut command, Duration::from_secs(4)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"said\n");
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_command_is_waited_on_no_longer_than_asked_whatever_it_leaves_running() {
        let began = Instant::now();
        let mut command = Command::new("/bin/sh");
        // What it leaves running goes on writing, for 3 s or more.
        let writing = "n=0; while [ $n -lt 150 ]; do echo more; sleep 0.02; n=$((n + 1)); done";
        command.args(["-c", &format!("({writing}) & echo said")]);
        let output = output_within(&mut command, Duration::from_secs(1)).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("said\n"));
        assert!(
            began.elapsed() < Duration::from_millis(2500),
            "{:?}",
            began.elapsed()
        );
    }

    /// As a locked Keychain's `security` waits for a password no one types.
    #[test]
    fn a_command_that_doesnt_end_in_time_is_stopped() {
        let began = Instant::now();
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        let error = output_within(&mut command, Duration::from_millis(200)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        // 0.21 to 0.23 s here; a second leaves room for a slower machine,
        // and is still far short of the 5 s it would run.
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_command_that_writes_more_than_a_pipe_holds_gives_all_of_it() {
        let mut command = Command::new("/bin/sh");
        let megabyte = "/usr/bin/head -c 1048576 /dev/zero";
        command.args(["-c", &format!("{megabyte}; {megabyte} >&2")]);
        let output = output_within(&mut command, Duration::from_secs(30)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1 << 20);
        assert_eq!(output.stderr.len(), 1 << 20);
    }
}
