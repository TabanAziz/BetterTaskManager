//! The files setup owns. Uninstall only ever deletes these names, never a whole tree.

use std::io;
use std::path::Path;

pub const APP: &str = "BTM.exe";
pub const UNINSTALLER: &str = "uninstall.exe";
const RETIRED: &str = "BTM.old.exe";

pub struct Payload<'a> {
    pub app: &'a [u8],
    pub setup: &'a [u8],
    pub texts: &'a [(&'a str, &'a [u8])],
}

pub fn put(dir: &Path, payload: &Payload) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    replace(dir, APP, payload.app)?;
    // When setup is started from the install folder it already is that file.
    let uninstaller = dir.join(UNINSTALLER);
    if std::fs::read(&uninstaller).ok().as_deref() != Some(payload.setup) {
        std::fs::write(&uninstaller, payload.setup)?;
    }
    for (name, bytes) in payload.texts {
        std::fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

/// A running program cannot be overwritten, but it can be renamed.
fn replace(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let target = dir.join(name);
    let retired = dir.join(RETIRED);
    let _ = std::fs::remove_file(&retired);
    if std::fs::write(&target, bytes).is_ok() {
        return Ok(());
    }
    std::fs::rename(&target, &retired)?;
    if let Err(error) = std::fs::write(&target, bytes) {
        let _ = std::fs::rename(&retired, &target); // put the working version back
        return Err(error);
    }
    Ok(())
}

pub fn remove(dir: &Path, texts: &[&str]) -> io::Result<()> {
    // Without our program in it, this is not an install folder: touch nothing.
    if !dir.join(UNINSTALLER).is_file() && !dir.join(APP).is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "Better Task Manager is not installed there"));
    }
    for name in [APP, RETIRED].iter().chain(texts).chain([UNINSTALLER].iter()) {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let _ = std::fs::remove_dir(dir); // only succeeds when nothing else is inside
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_update_and_remove_keep_foreign_files() {
        let dir = std::env::temp_dir().join(format!("btm-setup-test-{}", std::process::id()));
        let first = Payload { app: b"v1", setup: b"setup", texts: &[("LICENSE.txt", b"licence")] };
        put(&dir, &first).unwrap();
        put(&dir, &Payload { app: b"v2", ..first }).unwrap();
        assert_eq!(std::fs::read(dir.join(APP)).unwrap(), b"v2");

        let foreign = dir.join("notes of the user.txt");
        std::fs::write(&foreign, b"keep").unwrap();
        remove(&dir, &["LICENSE.txt"]).unwrap();
        assert!(!dir.join(APP).exists() && !dir.join(UNINSTALLER).exists() && foreign.exists());
        assert!(remove(&dir, &["LICENSE.txt"]).is_err(), "not an install folder any more");

        std::fs::remove_file(&foreign).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
