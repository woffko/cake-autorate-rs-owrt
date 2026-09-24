// Stage the owner boundary independently until controller lifecycle is wired.
#[path = "../src/probe_owner.rs"]
mod probe_owner;

use probe_owner::{ProbeGroupLease, ProbeGroupPool, PROBE_GROUP_SLOTS};

fn groups() -> String {
    let mut text = String::from("root:x:0:\ncake-speedtest:x:1000:\n");
    for slot in 0..PROBE_GROUP_SLOTS {
        text.push_str(&format!("cake-probe-{slot:02}:x:{}:\n", 40000 + slot));
    }
    text
}

const USERS: &str = "root:x:0:0:root:/root:/bin/sh\ncake-speedtest:x:1000:1000::/var:/bin/false\n";

#[test]
#[ignore = "requires private user namespace with GID 40000 mapped"]
fn r6_probe_allocator_rejects_live_task_group() {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    assert_ne!(
        fs::read_link("/proc/self/ns/user")
            .unwrap()
            .to_str()
            .unwrap(),
        std::env::var("CAKE_R6_PARENT_USERNS").unwrap()
    );
    let root = std::env::temp_dir().join(format!("cake-probe-live-group-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(fs::metadata(&root).unwrap().gid(), 40000);
    let pool = ProbeGroupPool::from_account_text(&groups(), USERS).unwrap();
    let lease = ProbeGroupLease::acquire(&pool, &root, 0, &"a".repeat(64)).unwrap();
    assert_eq!(
        lease.gid(),
        40001,
        "must not allocate the live process group"
    );
    assert!(fs::read(root.join("group-40000.lease")).unwrap().is_empty());
    lease.retire(|| Ok(())).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn r6_probe_lease_abrupt_child() {
    use std::os::unix::fs::MetadataExt;
    let Some(path) = std::env::var_os("CAKE_PROBE_LEASE_CHILD_ROOT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let uid = std::fs::metadata(&path).unwrap().uid();
    let pool = ProbeGroupPool::from_account_text(&groups(), USERS).unwrap();
    let lease = ProbeGroupLease::acquire(&pool, &path, uid, &"a".repeat(64)).unwrap();
    assert_eq!(lease.gid(), 40000);
    // Terminate without Rust destructors; the OS closes the live lock.
    std::process::exit(0);
}

#[test]
fn r6_probe_lease_survives_abrupt_process_exit() {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = std::env::temp_dir().join(format!("cake-probe-child-exit-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "r6_probe_lease_abrupt_child"])
        .env("CAKE_PROBE_LEASE_CHILD_ROOT", &root)
        .output()
        .unwrap();
    assert!(result.status.success());
    let uid = fs::metadata(&root).unwrap().uid();
    let pool = ProbeGroupPool::from_account_text(&groups(), USERS).unwrap();
    let lease = ProbeGroupLease::acquire(&pool, &root, uid, &"b".repeat(64)).unwrap();
    assert_eq!(lease.gid(), 40001);
    lease.retire(|| Ok(())).unwrap();
    assert!(!fs::read(root.join("group-40000.lease")).unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn r6_probe_leases_are_exclusive_and_not_recycled_on_drop_or_failed_cleanup() {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = std::env::temp_dir().join(format!("cake-probe-leases-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::metadata(&root).unwrap().uid();
    let pool = ProbeGroupPool::from_account_text(&groups(), USERS).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let spawn = || {
            let lease = ProbeGroupLease::acquire(&pool, &root, uid, &"a".repeat(64)).unwrap();
            barrier.wait();
            lease
        };
        let one = scope.spawn(spawn);
        let two = scope.spawn(spawn);
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_ne!(a.gid(), b.gid());
    let retired_gid = b.gid();
    drop(a); // Emulate a process disappearing without completing cleanup.
    b.retire(|| Ok(())).unwrap();
    let reused = ProbeGroupLease::acquire(&pool, &root, uid, &"b".repeat(64)).unwrap();
    assert_eq!(reused.gid(), retired_gid);
    assert_eq!(
        reused.retire(|| Err("cleanup-failed")),
        Err("cleanup-failed")
    );
    let mut held = Vec::new();
    for _ in 2..PROBE_GROUP_SLOTS {
        held.push(ProbeGroupLease::acquire(&pool, &root, uid, &"c".repeat(64)).unwrap());
    }
    assert!(matches!(
        ProbeGroupLease::acquire(&pool, &root, uid, &"d".repeat(64)),
        Err("probe-owner-pool-exhausted-or-unrecovered")
    ));
    drop(held);
    assert!(ProbeGroupLease::acquire(&pool, &root, uid, &"d".repeat(64)).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn r6_probe_lease_refuses_replacement_and_preserves_foreign_file() {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root =
        std::env::temp_dir().join(format!("cake-probe-lease-replace-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::metadata(&root).unwrap().uid();
    let pool = ProbeGroupPool::from_account_text(&groups(), USERS).unwrap();
    let lease = ProbeGroupLease::acquire(&pool, &root, uid, &"a".repeat(64)).unwrap();
    let path = root.join(format!("group-{}.lease", lease.gid()));
    fs::rename(&path, root.join("old")).unwrap();
    fs::write(&path, "foreign-owner").unwrap();
    assert!(lease
        .retire(|| panic!("must not clean up an unverified lease"))
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "foreign-owner");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn r6_probe_account_files_reject_links_writable_files_and_wrong_owner() {
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    let root = std::env::temp_dir().join(format!("cake-probe-accounts-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::metadata(&root).unwrap().uid();
    fs::write(root.join("group"), groups()).unwrap();
    fs::write(root.join("passwd"), USERS).unwrap();
    fs::set_permissions(root.join("group"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(root.join("passwd"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        ProbeGroupPool::read_at(&root, uid).unwrap().gid(0),
        Some(40000)
    );
    assert!(ProbeGroupPool::read_at(&root, uid.wrapping_add(1)).is_err());
    fs::set_permissions(root.join("group"), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(ProbeGroupPool::read_at(&root, uid).is_err());
    fs::set_permissions(root.join("group"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(root.join("group"), root.join("alias")).unwrap();
    assert!(ProbeGroupPool::read_at(&root, uid).is_err());
    fs::remove_file(root.join("alias")).unwrap();
    fs::rename(root.join("group"), root.join("original")).unwrap();
    symlink("original", root.join("group")).unwrap();
    assert!(ProbeGroupPool::read_at(&root, uid).is_err());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn r6_probe_group_inventory_is_complete_distinct_and_order_independent() {
    let text = groups();
    let pool = ProbeGroupPool::from_account_text(&text, USERS).unwrap();
    assert_eq!(pool.gid(0), Some(40000));
    assert_eq!(pool.gid(63), Some(40063));
    assert_eq!(pool.gid(64), None);
    let reversed = text.lines().rev().collect::<Vec<_>>().join("\n");
    assert_eq!(
        ProbeGroupPool::from_account_text(&reversed, USERS).unwrap(),
        pool
    );
}

#[test]
fn r6_probe_group_inventory_rejects_missing_aliased_or_shared_reservations() {
    let text = groups();
    for changed in [
        text.replace("cake-probe-00:x:40000:\n", ""),
        text.replace("cake-probe-00:x:40000:", "cake-probe-00:x:40000:foreign"),
        text.replace("cake-probe-01:x:40001:", "cake-probe-01:x:40000:"),
        text.replace("cake-probe-00:x:40000:", "cake-probe-00:x:0:"),
        text.replace("cake-probe-00:x:40000:", "cake-probe-00:x:4294967295:"),
        text.replace("cake-probe-00:x:40000:", "cake-probe-00:x:+40000:"),
        format!("{text}foreign:x:40000:\n"),
        format!("{text}cake-probe-00:x:50000:\n"),
        format!("{text}cake-probe-64:x:50000:\n"),
        format!("{text}broken\n"),
    ] {
        assert!(ProbeGroupPool::from_account_text(&changed, USERS).is_err());
    }
    let primary = format!("{USERS}foreign:x:1001:40000::/:/bin/false\n");
    assert!(ProbeGroupPool::from_account_text(&text, &primary).is_err());
    assert!(ProbeGroupPool::from_account_text(&text, "broken").is_err());
    assert!(ProbeGroupPool::from_account_text(&"x".repeat(256 * 1024 + 1), USERS).is_err());
}
