#![forbid(unsafe_code)]

use oms_command_plan::completion::gc_completion_captures;
use oms_command_plan::json;
use std::fs::{self, FileTimes, OpenOptions, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "oms-plan-gc-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mkdir_private(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let mut cur = path.to_path_buf();
    while cur.components().count() > 1 {
        let _ = fs::set_permissions(&cur, Permissions::from_mode(0o750));
        if !cur.pop() || cur.file_name().is_none_or(|n| n == ".oms") {
            let _ = fs::set_permissions(&cur, Permissions::from_mode(0o750));
            break;
        }
    }
}

fn write_aged(root: &Path, rel: &str, bytes: &[u8], mtime_secs: u64) {
    let target = root.join(rel);
    mkdir_private(target.parent().unwrap());
    fs::write(&target, bytes).unwrap();
    fs::set_permissions(&target, Permissions::from_mode(0o640)).unwrap();
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs);
    let file = OpenOptions::new().write(true).open(&target).unwrap();
    file.set_times(FileTimes::new().set_modified(ts)).unwrap();
}

fn write_hashed(root: &Path, dir: &str, ext: &str, bytes: &[u8], mtime_secs: u64) -> String {
    let sha = json::sha(bytes);
    let rel = format!(".oms/plan/{dir}/{sha}.{ext}");
    write_aged(root, &rel, bytes, mtime_secs);
    rel
}

#[test]
fn gc_completion_captures_collects_and_respects_roots_and_veto() {
    let tmp = TempDir::new();
    let root = tmp.path();
    fs::set_permissions(root, Permissions::from_mode(0o750)).unwrap();
    mkdir_private(&root.join(".oms/plan"));

    assert_eq!(
        gc_completion_captures(root, 2000, false, None).unwrap(),
        vec![]
    );

    let plan_cur = format!("plan_{}", "a".repeat(32));
    let plan_old = format!("plan_{}", "b".repeat(32));
    let lease_active = format!("lease_{}", "1".repeat(32));
    let lease_stale = format!("lease_{}", "2".repeat(32));

    let seg_kept = write_hashed(root, "completion-segments", "bin", b"seg-kept", 500);
    let seg_sha = json::sha(b"seg-kept");
    let seg_stale = write_hashed(root, "completion-segments", "bin", b"seg-stale", 500);

    let prefix_kept = write_hashed(
        root,
        "completion-prefixes",
        "json",
        format!(
            r#"{{"schema":1,"stream_sha256":"{}","segments":[{{"path":"{}","sha256":"{}"}}]}}"#,
            "c".repeat(64),
            seg_kept,
            seg_sha
        )
        .as_bytes(),
        500,
    );
    let prefix_stale = write_hashed(root, "completion-prefixes", "jsonl", b"{\"a\":1}\n", 500);

    let capture_stale = write_hashed(
        root,
        "completion-captures",
        "json",
        br#"{"schema":1,"repository_sha256":"abc","room_id":"r1","room":{},"lifecycle":{}}"#,
        500,
    );
    let capture_bearer = write_hashed(
        root,
        "completion-captures",
        "json",
        br#"{"schema":1,"repository_sha256":"abc","room_id":"r1","room":{},"lifecycle":{},"bearer":true}"#,
        500,
    );
    let receipt_retired = write_hashed(
        root,
        "completions",
        "json",
        format!(r#"{{"bundle":{{"plan_id":"{plan_old}"}}}}"#).as_bytes(),
        500,
    );
    let receipt_current = write_hashed(
        root,
        "completions",
        "json",
        format!(r#"{{"bundle":{{"plan_id":"{plan_cur}"}}}}"#).as_bytes(),
        500,
    );
    let recent_stale_kind = write_hashed(
        root,
        "completion-captures",
        "json",
        br#"{"schema":1,"repository_sha256":"abc","room_id":"r2","room":{},"lifecycle":{}}"#,
        2500,
    );

    let pin_id_active = "3".repeat(32);
    let pin_active_rel = format!(".oms/plan/completion-pins/{pin_id_active}.json");
    write_aged(
        root,
        &pin_active_rel,
        format!(
            r#"{{"schema":1,"lease_id":"{lease_active}","objects":[{{"path":"{prefix_kept}","sha256":"{}"}}]}}"#,
            json::sha(
                fs::read(root.join(&prefix_kept)).unwrap()
            )
        )
        .as_bytes(),
        500,
    );
    let pin_stale_rel = format!(".oms/plan/completion-pins/{}.json", "4".repeat(32));
    write_aged(
        root,
        &pin_stale_rel,
        format!(r#"{{"schema":1,"lease_id":"{lease_stale}","objects":[]}}"#).as_bytes(),
        500,
    );
    let pin_tmp_rel = format!(
        ".oms/plan/completion-pins/.{}.json.tmp.99.abc",
        "5".repeat(32)
    );
    write_aged(root, &pin_tmp_rel, b"{", 500);

    let lease_active_rel = format!(".oms/plan/completion-pin-leases/{lease_active}.json");
    write_aged(
        root,
        &lease_active_rel,
        format!(r#"{{"schema":1,"pins":["{pin_id_active}"]}}"#).as_bytes(),
        500,
    );
    let lease_stale_rel = format!(".oms/plan/completion-pin-leases/{lease_stale}.json");
    write_aged(
        root,
        &lease_stale_rel,
        format!(r#"{{"schema":1,"pins":["{}"]}}"#, "4".repeat(32)).as_bytes(),
        500,
    );

    // First write tasks.json without a matching plan_id while active delegation marker vetoes GC.
    write_aged(
        root,
        ".oms/plan/tasks.json",
        format!(
            r#"{{"plan_id":"{plan_cur}","tasks":{{"t1":{{"state":"running","lease_id":"{lease_active}"}}}}}}"#
        )
        .as_bytes(),
        2500,
    );
    let delegation_rel = ".oms/delegations/w1.json";
    write_aged(
        root,
        delegation_rel,
        format!(r#"{{"lease_id":"{lease_active}"}}"#).as_bytes(),
        2500,
    );
    assert_eq!(
        gc_completion_captures(root, 2000, true, None).unwrap(),
        vec![]
    );

    // Removing the legacy delegation marker allows GC.
    fs::remove_file(root.join(delegation_rel)).unwrap();

    let dry = gc_completion_captures(root, 2000, false, None).unwrap();
    let actual_paths: Vec<String> = dry.iter().map(|(_, p)| p.clone()).collect();
    let mut want = vec![
        capture_stale.clone(),
        lease_stale_rel.clone(),
        pin_tmp_rel.clone(),
        pin_stale_rel.clone(),
        prefix_stale.clone(),
        seg_stale.clone(),
        receipt_retired.clone(),
    ];
    want.sort();
    assert_eq!(actual_paths, want);
    for (_, p) in &dry {
        assert!(root.join(p).exists(), "dry run must not unlink {p}");
    }

    let applied = gc_completion_captures(root, 2000, true, None).unwrap();
    assert_eq!(applied, dry);
    for (_, p) in &applied {
        assert!(!root.join(p).exists(), "apply must unlink {p}");
    }

    for keep in [
        &seg_kept,
        &prefix_kept,
        &capture_bearer,
        &receipt_current,
        &recent_stale_kind,
        &pin_active_rel,
        &lease_active_rel,
    ] {
        assert!(root.join(keep).exists(), "must keep {keep}");
    }
}
