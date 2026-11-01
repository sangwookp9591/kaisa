//! Shared-repository worktree metadata regressions: no agents or network involved.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

use kaisa_workspace::{
    CandidateCheckout, IgnoredPolicy, PreservationReceipt, create_worktree, freeze_candidate,
    remove_worktree,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const DEADLINE: Duration = Duration::from_secs(90);

struct Repository(PathBuf);

impl Repository {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "kaisa-metadata-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("repo")).expect("private repository");
        let fixture = Self(root);
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fixture.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        fixture
    }

    fn path(&self) -> PathBuf {
        self.0.join("repo")
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .current_dir(self.path())
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn metadata_lock(&self) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path().join(".git/kaisa-worktree.lock"))
            .expect("lock file");
        file.lock().expect("hold cross-process metadata lock");
        file
    }

    fn assert_clean(&self) {
        let listing = self.git(&["worktree", "list", "--porcelain"]);
        assert_eq!(
            String::from_utf8_lossy(&listing)
                .matches("worktree ")
                .count(),
            1,
            "all temporary worktrees must be unregistered"
        );
        let metadata = self.path().join(".git/worktrees");
        assert!(
            !metadata.exists()
                || std::fs::read_dir(metadata)
                    .expect("metadata")
                    .next()
                    .is_none(),
            "no partial metadata entries remain"
        );
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mixed_operations(repository: &Path, root: &Path, worker: &str, detached: bool, rounds: usize) {
    let candidate = freeze_candidate(repository, "main").expect("candidate");
    let started = Instant::now();
    for round in 0..rounds {
        assert!(
            started.elapsed() < DEADLINE,
            "bounded mixed-worktree stress"
        );
        let name = format!("{worker}-{round}");
        let path = root.join(&name);
        if detached {
            let checkout = CandidateCheckout::create(repository, &candidate, &path)
                .expect("detached add must not race branch add/remove");
            if round == 0 {
                std::fs::write(root.join(format!("{worker}.first-add")), [])
                    .expect("first add marker");
            }
            assert_eq!(
                std::fs::read_to_string(checkout.path().join(".git"))
                    .expect("git pointer")
                    .lines()
                    .count(),
                1
            );
            drop(checkout);
            assert!(!path.exists(), "detached remove completed");
        } else {
            let checkout = create_worktree(repository, "main", &name, &path)
                .expect("branch add must not race detached add/remove");
            if round == 0 {
                std::fs::write(root.join(format!("{worker}.first-add")), [])
                    .expect("first add marker");
            }
            remove_worktree(
                repository,
                &checkout,
                &PreservationReceipt::default(),
                &IgnoredPolicy::with_build_output(&[]),
            )
            .expect("branch remove must not race detached add/remove");
        }
    }
}

#[test]
fn mixed_two_thread_worktree_stress() {
    let repository = Repository::new();
    let held = repository.metadata_lock();
    let barrier = Barrier::new(3);
    let respected_lock = std::thread::scope(|scope| {
        for detached in [true, false] {
            let repository = &repository;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                mixed_operations(
                    &repository.path(),
                    &repository.0,
                    if detached { "check" } else { "attempt" },
                    detached,
                    150,
                );
            });
        }
        barrier.wait();
        // A third metadata owner keeps the first additions out. This makes the regression
        // reliably red without depending on Git hitting its rare commondir/create race.
        std::thread::sleep(Duration::from_millis(200));
        let respected_lock = !repository.0.join("check.first-add").exists()
            && !repository.0.join("attempt.first-add").exists();
        drop(held);
        respected_lock
    });
    assert!(
        respected_lock,
        "mixed worktree operations bypassed an active metadata owner"
    );
    repository.assert_clean();
}

#[test]
fn an_external_metadata_lock_blocks_both_checkout_apis() {
    for detached in [true, false] {
        let repository = Arc::new(Repository::new());
        let candidate = freeze_candidate(&repository.path(), "main").expect("candidate");
        let linked =
            CandidateCheckout::create(&repository.path(), &candidate, &repository.0.join("anchor"))
                .expect("linked repository entry point");
        let entry_point = linked.path().to_owned();
        let held = repository.metadata_lock();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_repository = Arc::clone(&repository);
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).expect("ready");
            mixed_operations(&entry_point, &worker_repository.0, "locked", detached, 1);
            done_tx.send(()).expect("done");
        });
        ready_rx.recv_timeout(DEADLINE).expect("worker started");
        let blocked = done_rx.recv_timeout(Duration::from_millis(200)).is_err();
        // Release before asserting, so even a red regression drains its owned thread.
        drop(held);
        worker.join().expect("worker drained");
        drop(linked);
        assert!(
            blocked,
            "worktree metadata changed while another owner held its advisory lock"
        );
        repository.assert_clean();
    }
}

#[test]
fn a_busy_repository_does_not_block_an_unrelated_repository() {
    let busy = Repository::new();
    let held = busy.metadata_lock();
    let unrelated = Repository::new();
    mixed_operations(&unrelated.path(), &unrelated.0, "independent", true, 1);
    unrelated.assert_clean();
    drop(held);
}

#[test]
fn an_external_metadata_lock_blocks_both_removal_paths() {
    for detached in [true, false] {
        let repository = Arc::new(Repository::new());
        let candidate = freeze_candidate(&repository.path(), "main").expect("candidate");
        let path = repository.0.join("remove");
        let checkout = detached.then(|| {
            CandidateCheckout::create(&repository.path(), &candidate, &path).expect("checkout")
        });
        let attempt = (!detached).then(|| {
            create_worktree(&repository.path(), "main", "remove", &path).expect("attempt")
        });
        let held = repository.metadata_lock();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_repository = Arc::clone(&repository);
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).expect("ready");
            drop(checkout);
            if let Some(attempt) = attempt {
                remove_worktree(
                    &worker_repository.path(),
                    &attempt,
                    &PreservationReceipt::default(),
                    &IgnoredPolicy::with_build_output(&[]),
                )
                .expect("remove after metadata lock released");
            }
            done_tx.send(()).expect("done");
        });
        ready_rx.recv_timeout(DEADLINE).expect("worker started");
        let blocked = done_rx.recv_timeout(Duration::from_millis(200)).is_err();
        drop(held);
        worker.join().expect("worker drained");
        assert!(blocked, "removal bypassed the metadata lock");
        repository.assert_clean();
    }
}

struct Worker(Child);

impl Worker {
    fn start(repository: &Repository, name: &str, detached: bool, rounds: usize) -> Self {
        Self(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "metadata_process_worker", "--nocapture"])
                .env("KAISA_METADATA_TEST_REPOSITORY", repository.path())
                .env("KAISA_METADATA_TEST_ROOT", &repository.0)
                .env("KAISA_METADATA_TEST_NAME", name)
                .env("KAISA_METADATA_TEST_DETACHED", detached.to_string())
                .env("KAISA_METADATA_TEST_ROUNDS", rounds.to_string())
                .spawn()
                .expect("worker process"),
        )
    }

    fn wait(&mut self) {
        let started = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().expect("worker status") {
                assert!(status.success(), "mixed-operation worker failed: {status}");
                return;
            }
            assert!(started.elapsed() < DEADLINE, "worker timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn metadata_process_worker() {
    let Some(repository) = std::env::var_os("KAISA_METADATA_TEST_REPOSITORY") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("KAISA_METADATA_TEST_ROOT").expect("root"));
    let name = std::env::var("KAISA_METADATA_TEST_NAME").expect("name");
    std::fs::write(root.join(format!("{name}.ready")), []).expect("ready marker");
    mixed_operations(
        Path::new(&repository),
        &root,
        &name,
        std::env::var("KAISA_METADATA_TEST_DETACHED").expect("mode") == "true",
        std::env::var("KAISA_METADATA_TEST_ROUNDS")
            .expect("rounds")
            .parse()
            .expect("round count"),
    );
    std::fs::write(root.join(format!("{name}.done")), []).expect("done marker");
}

#[test]
fn mixed_two_process_worktree_stress() {
    let repository = Repository::new();
    let mut detached = Worker::start(&repository, "check", true, 100);
    let mut branch = Worker::start(&repository, "attempt", false, 100);
    detached.wait();
    branch.wait();
    repository.assert_clean();
}

#[test]
fn another_process_waits_for_the_common_directory_lock() {
    let repository = Repository::new();
    let held = repository.metadata_lock();
    let mut worker = Worker::start(&repository, "blocked", true, 1);
    let started = Instant::now();
    while !repository.0.join("blocked.ready").exists() {
        assert!(started.elapsed() < DEADLINE, "child started");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    let blocked = !repository.0.join("blocked.done").exists();
    drop(held);
    worker.wait();
    assert!(blocked, "another process bypassed the metadata lock");
    repository.assert_clean();
}
