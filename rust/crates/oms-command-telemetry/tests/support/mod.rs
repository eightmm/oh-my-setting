#![allow(dead_code)]
use oms_context::Json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "oms-native-telemetry-{}-{stamp}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".oms/artifacts")).unwrap();
        Self(root)
    }
    pub fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let target = self.0.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
    pub fn index(&self, bytes: impl AsRef<[u8]>) {
        self.write(".oms/artifacts/index.jsonl", bytes);
    }
    pub fn call(&self, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        call(&self.0, args)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
pub fn call(root: &Path, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
    let argv: Vec<_> = std::iter::once(OsString::from("telemetry"))
        .chain(args.iter().map(OsString::from))
        .collect();
    oms_command_telemetry::run(&argv, root, &AtomicBool::new(false))
}
pub fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    if let Json::Object(m) = v {
        m.iter()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v)
            .unwrap()
    } else {
        panic!("expected object")
    }
}
pub fn text(v: &Json) -> &str {
    if let Json::String(s) = v {
        std::str::from_utf8(s).unwrap()
    } else {
        panic!("expected text")
    }
}
pub fn array(v: &Json) -> &[Json] {
    if let Json::Array(a) = v {
        a
    } else {
        panic!("expected array")
    }
}
