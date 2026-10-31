//! Running a command to completion with its output captured, a deadline and an output cap
//! (architecture §5.6 broker `exec` contract).

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::process::{OwnedProcess, ProcessExit, ProcessSpec, command_for};

/// The OS confinement a captured run gets.
#[derive(Debug)]
#[non_exhaustive]
pub enum Confinement<'policy> {
    /// No OS confinement; the caller must label the run as reduced.
    Unconfined,
    /// A Seatbelt profile (macOS).
    #[cfg(target_os = "macos")]
    Seatbelt(&'policy crate::seatbelt::SeatbeltPolicy),
    /// A Landlock ruleset with its UDP filter (Linux).
    #[cfg(target_os = "linux")]
    Landlock(&'policy crate::landlock::LandlockRuleset),
    /// An AppContainer profile (Windows): the process runs inside the container with only its
    /// standard handles inherited.
    #[cfg(windows)]
    AppContainer(&'policy crate::appcontainer::AppContainerProfile),
    /// Keeps the lifetime on platforms with no confinement yet; never constructed.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    #[doc(hidden)]
    Pending(std::marker::PhantomData<&'policy ()>),
}

/// How a captured run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRun {
    /// The exit, or `None` if the deadline passed and the tree was ended.
    pub exit: Option<ProcessExit>,
    /// Standard output and error, interleaved, up to the cap.
    pub output: Vec<u8>,
    /// True if output beyond the cap was discarded.
    pub truncated: bool,
}

/// Runs `spec` under `confinement` with `input` on its standard input (then closed), waiting at
/// most `deadline` and keeping at most `cap` bytes of output. Output beyond the cap is read and dropped, so a chatty process cannot stall on a full
/// pipe. A run past its deadline is ended with its whole process tree.
///
/// # Errors
///
/// Returns the operating system's error if the program cannot be started or waited for.
pub fn run_captured(
    spec: &ProcessSpec,
    confinement: &Confinement<'_>,
    input: &[u8],
    deadline: Duration,
    cap: usize,
) -> io::Result<CapturedRun> {
    run_captured_reporting(spec, confinement, input, deadline, cap, &mut |_| Ok(()))
}

/// [`run_captured`], telling `started` the process's identity as soon as it exists, before any
/// input is sent, so a crash of the caller can later be recovered from (T16.6b).
///
/// # Errors
///
/// Returns the operating system's error if the program cannot be started or waited for, or
/// the error `started` returned; in that case the process is ended first.
pub fn run_captured_reporting(
    spec: &ProcessSpec,
    confinement: &Confinement<'_>,
    input: &[u8],
    deadline: Duration,
    cap: usize,
    started: &mut dyn FnMut(Option<crate::ProcessIdentity>) -> io::Result<()>,
) -> io::Result<CapturedRun> {
    let (reader, writer) = io::pipe()?;
    let (mut process, stdin): (OwnedProcess, Option<Box<dyn io::Write + Send>>) =
        spawn_captured(spec, confinement, !input.is_empty(), writer)?;
    let collector = std::thread::spawn(move || collect(reader, cap));
    let identity: Option<crate::ProcessIdentity> =
        crate::ProcessIdentity::of(process.id()).unwrap_or(None);
    if let Err(error) = started(identity) {
        let _ = process.kill_tree();
        let _ = process.wait();
        return Err(error);
    }
    if let Some(mut stdin) = stdin {
        let input: Vec<u8> = input.to_vec();
        // Written on a thread so a process that does not read cannot block the deadline.
        std::thread::spawn(move || {
            let _ = std::io::Write::write_all(&mut stdin, &input);
        });
    }
    let exit: Option<ProcessExit> = process.wait_timeout(deadline)?;
    // Ends the tree on timeout, and any leftovers that would keep the pipe open otherwise.
    process.kill_tree()?;
    if exit.is_none() {
        process.wait()?;
    }
    let (output, truncated) = collector
        .join()
        .map_err(|_| io::Error::other("the output collector panicked"))??;
    Ok(CapturedRun {
        exit,
        output,
        truncated,
    })
}

/// Starts `spec` with `output` as its stdout and stderr, and a stdin pipe if `piped`.
fn spawn_captured(
    spec: &ProcessSpec,
    confinement: &Confinement<'_>,
    piped: bool,
    output: io::PipeWriter,
) -> io::Result<(OwnedProcess, Option<Box<dyn io::Write + Send>>)> {
    #[cfg(windows)]
    if let Confinement::AppContainer(profile) = confinement {
        use std::os::windows::io::AsRawHandle;
        // A pipe even without input: dropping its writer gives the process an immediate EOF.
        let (child_stdin, our_stdin) = io::pipe()?;
        let created = crate::appcontainer::create_suspended_contained(
            profile,
            spec,
            child_stdin.as_raw_handle(),
            output.as_raw_handle(),
            output.as_raw_handle(),
        )?;
        drop((child_stdin, output));
        let process: OwnedProcess = OwnedProcess::adopt_created(created)?;
        let stdin: Option<Box<dyn io::Write + Send>> =
            piped.then(|| Box::new(our_stdin) as Box<dyn io::Write + Send>);
        return Ok((process, stdin));
    }
    let mut command: Command = confined_command(spec, confinement);
    command
        .stdin(if piped { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output));
    let mut process: OwnedProcess = OwnedProcess::spawn_owned(command)?;
    let stdin: Option<Box<dyn io::Write + Send>> = process
        .take_stdin()
        .map(|stdin| Box::new(stdin) as Box<dyn io::Write + Send>);
    Ok((process, stdin))
}

fn collect(mut reader: io::PipeReader, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut output: Vec<u8> = Vec::new();
    let mut truncated: bool = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read: usize = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, truncated));
        }
        let room: usize = cap.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(room)]);
        truncated |= read > room;
    }
}

fn confined_command(spec: &ProcessSpec, confinement: &Confinement<'_>) -> Command {
    match confinement {
        Confinement::Unconfined => command_for(spec),
        #[cfg(target_os = "macos")]
        Confinement::Seatbelt(policy) => {
            command_for(&crate::seatbelt::sandboxed_spec(spec, policy))
        }
        #[cfg(target_os = "linux")]
        Confinement::Landlock(ruleset) => {
            use std::os::unix::process::CommandExt;

            let mut command: Command = command_for(spec);
            let fd = ruleset.raw_fd();
            let udp_filter: Option<(usize, u16)> = ruleset.udp_filter_parts();
            #[allow(unsafe_code)]
            // SAFETY: the closure runs in the forked child before `exec` and only calls `prctl`
            // and the `landlock_restrict_self` syscall, which are async-signal-safe; the filter
            // lives in `ruleset`, which outlives the spawn.
            unsafe {
                command.pre_exec(move || crate::landlock::restrict_self(fd, udp_filter));
            }
            command
        }
        // Contained processes are created by `spawn_captured`, never through a `Command`.
        #[cfg(windows)]
        Confinement::AppContainer(_) => command_for(spec),
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        Confinement::Pending(_) => command_for(spec),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CapturedRun, Confinement, run_captured};
    use crate::process::ProcessExit;
    use crate::process::test_support::{a_long_running_process, shell};

    /// Plenty for a shell that prints one line.
    const DEADLINE: Duration = Duration::from_secs(30);
    /// Far shorter than a long-running test process lives.
    const A_MOMENT: Duration = Duration::from_millis(200);
    /// A small cap, so a modest output overflows it.
    const SMALL_CAP: usize = 16;
    /// What the command prints.
    const GREETING: &str = "hello-from-exec";
    /// Where an agent's HTTPS traffic is sent: its egress broker.
    const EGRESS_PROXY: &str = "http://127.0.0.1:41817";
    /// A run with nothing on standard input.
    const NO_INPUT: &[u8] = b"";
    /// A first message, as an agent receives its context pack.
    const FIRST_MESSAGE: &str = "pack-for-attempt-41a";

    #[cfg(unix)]
    #[test]
    fn captured_and_owned_children_receive_only_base_and_explicit_environment() {
        const SUCCESS: ProcessExit = ProcessExit::Code(0);
        const TEST: &str = "capture::tests::captured_and_owned_children_receive_only_base_and_explicit_environment";
        const DUMMY_SECRET: &str = "KAISA_TEST_UNRELATED_SECRET";
        const EXPLICIT: &str = "KAISA_TEST_EXPLICIT_VARIABLE";
        const FIXTURE: &str = "KAISA_TEST_ENVIRONMENT_FIXTURE";
        const MARKER: &str = "dummy-secret-marker";
        const PROXIES: [&str; 8] = [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ];
        const USER_PROXY: &str = "user-proxy-marker";
        const BROKER_PROXY: &str = "broker-proxy-marker";
        if std::env::var(FIXTURE).is_err() {
            let output =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", TEST, "--nocapture"])
                    .env(FIXTURE, "yes")
                    .env(DUMMY_SECRET, MARKER)
                    .envs(PROXIES.map(|name| (name, USER_PROXY)))
                    .output()
                    .expect("isolated parent starts");
            assert!(
                output.status.success(),
                "environment regression in isolated parent: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert_eq!(
            std::env::var(DUMMY_SECRET).as_deref(),
            Ok(MARKER),
            "the launcher's parent really contains the dummy secret"
        );
        let command = format!(
            "test -z \"${{{DUMMY_SECRET}}}\" && test -n \"$PATH\" && test -n \"$HOME\" && test \"${{{EXPLICIT}}}\" = allowed"
        );
        let mut spec = shell(&command);
        spec.env.push((EXPLICIT.to_owned(), "allowed".to_owned()));
        for (confined, proxy_override) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut spec = spec.clone();
            let expected_proxy = if proxy_override {
                BROKER_PROXY
            } else {
                USER_PROXY
            };
            let proxy_checks = PROXIES
                .iter()
                .map(|name| format!(" && test \"${{{name}}}\" = {expected_proxy}"))
                .collect::<String>();
            let mut checked = shell(&format!("{command}{proxy_checks}"));
            checked.env.clone_from(&spec.env);
            spec = checked;
            if proxy_override {
                spec.env
                    .extend(PROXIES.map(|name| (name.to_owned(), BROKER_PROXY.to_owned())));
            }
            #[cfg(target_os = "macos")]
            let policy = crate::SeatbeltPolicy::default();
            #[cfg(target_os = "macos")]
            let confinement = if confined {
                Confinement::Seatbelt(&policy)
            } else {
                Confinement::Unconfined
            };
            #[cfg(not(target_os = "macos"))]
            let confinement = {
                let _ = confined;
                Confinement::Unconfined
            };
            let run = run_captured(&spec, &confinement, NO_INPUT, DEADLINE, SMALL_CAP)
                .expect("check runs");
            assert_eq!(
                run.exit,
                Some(SUCCESS),
                "trusted checks and confined captured agents must exclude unrelated secrets"
            );
            #[cfg(target_os = "macos")]
            let owned_spec = if confined {
                crate::sandboxed_spec(&spec, &policy)
            } else {
                spec.clone()
            };
            #[cfg(not(target_os = "macos"))]
            let owned_spec = spec.clone();
            let mut owned = crate::OwnedProcess::spawn(&owned_spec).expect("owned agent starts");
            assert_eq!(
                owned.wait().expect("agent exits"),
                SUCCESS,
                "owned agents, including Seatbelt-confined agents, use the environment allow-list"
            );
        }
    }

    #[test]
    fn an_exec_reports_its_exit_and_output() {
        let run: CapturedRun = run_captured(
            &shell(&format!("echo {GREETING}")),
            &Confinement::Unconfined,
            NO_INPUT,
            DEADLINE,
            usize::MAX,
        )
        .expect("the shell runs");

        assert_eq!(
            (run.exit, String::from_utf8_lossy(&run.output).trim()),
            (Some(ProcessExit::Code(0)), GREETING),
            "the agent sees what its command printed and how it ended"
        );
    }

    #[test]
    fn an_exec_past_its_deadline_is_ended() {
        let run: CapturedRun = run_captured(
            &a_long_running_process(),
            &Confinement::Unconfined,
            NO_INPUT,
            A_MOMENT,
            usize::MAX,
        )
        .expect("the process starts");

        assert_eq!(
            run.exit, None,
            "a command that hangs must not hang the agent's turn"
        );
    }

    #[test]
    fn exec_output_is_cut_at_its_cap() {
        let run: CapturedRun = run_captured(
            &shell(&format!("echo {GREETING}{GREETING}{GREETING}")),
            &Confinement::Unconfined,
            NO_INPUT,
            DEADLINE,
            SMALL_CAP,
        )
        .expect("the shell runs");

        assert_eq!(
            (run.output.len(), run.truncated),
            (SMALL_CAP, true),
            "a huge log must not flood the agent's context, and the cut is visible"
        );
    }

    #[test]
    fn an_exec_reads_the_input_it_was_given() {
        let echo_stdin = if cfg!(windows) {
            shell("more")
        } else {
            shell("cat")
        };

        let run: CapturedRun = run_captured(
            &echo_stdin,
            &Confinement::Unconfined,
            FIRST_MESSAGE.as_bytes(),
            DEADLINE,
            usize::MAX,
        )
        .expect("the shell runs");

        assert_eq!(
            String::from_utf8_lossy(&run.output).trim(),
            FIRST_MESSAGE,
            "an agent receives its context pack on standard input"
        );
    }

    #[test]
    fn an_exec_sees_the_variables_it_was_given() {
        let mut print_proxy = if cfg!(windows) {
            shell("echo %HTTPS_PROXY%")
        } else {
            shell("echo $HTTPS_PROXY")
        };
        print_proxy.env = vec![("HTTPS_PROXY".to_owned(), EGRESS_PROXY.to_owned())];

        let run: CapturedRun = run_captured(
            &print_proxy,
            &Confinement::Unconfined,
            NO_INPUT,
            DEADLINE,
            usize::MAX,
        )
        .expect("the shell runs");

        assert_eq!(
            String::from_utf8_lossy(&run.output).trim(),
            EGRESS_PROXY,
            "an agent is pointed at its egress broker through its environment"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_exec_inside_an_appcontainer_is_captured_like_any_other() {
        let profile = crate::AppContainerProfile::ensure(&format!(
            "kaisa.test.capture.{}",
            std::process::id()
        ))
        .expect("a non-admin user can create a profile");
        let mut echo_stdin = shell("more");
        echo_stdin.working_directory = std::path::PathBuf::from(r"C:\Windows");

        let run = run_captured(
            &echo_stdin,
            &Confinement::AppContainer(&profile),
            FIRST_MESSAGE.as_bytes(),
            DEADLINE,
            usize::MAX,
        );
        let _ = profile.delete();
        let run: CapturedRun = run.expect("the contained shell runs");

        assert_eq!(
            (run.exit, String::from_utf8_lossy(&run.output).trim()),
            (Some(ProcessExit::Code(0)), FIRST_MESSAGE),
            "a contained agent gets its pack on stdin and its transcript is captured"
        );
    }
}
