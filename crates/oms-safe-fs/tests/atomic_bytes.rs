use oms_safe_fs::{
    ByteError, ByteReadOptions, MutationError, MutationOptions, MutationPhase, Root,
};
use std::cell::Cell;
use std::path::Path;

#[test]
fn atomic_public_boundary_refuses_malformed_paths_before_callback() {
    let root = Root::open(std::env::temp_dir()).unwrap();
    let called = Cell::new(false);
    assert_eq!(
        root.atomic_mutate(Path::new("x\0y"), MutationOptions::default(), |_| {
            called.set(true);
            Ok::<_, ()>(Vec::new())
        }),
        Err(MutationError::Storage {
            phase: MutationPhase::BeforePublication,
            error: ByteError::InvalidPath,
        })
    );
    assert!(!called.get());
    assert_eq!(
        root.read_no_follow(Path::new("x\0y"), ByteReadOptions::default()),
        Err(ByteError::InvalidPath)
    );
}
