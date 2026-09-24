//! Owner-triggered native folder selection; never a browser upload or shell API.
use std::{path::PathBuf, sync::Arc};
#[cfg(any(target_os = "macos", test))]
use std::{process::Stdio, time::Duration};
use tokio::sync::Semaphore;
#[cfg(any(target_os = "macos", test))]
use tokio::{io::AsyncReadExt, process::Command};

pub(crate) struct FolderPicker {
    slot: Arc<Semaphore>,
    #[cfg(test)]
    pub test_program: std::sync::Mutex<Option<PathBuf>>,
}
impl FolderPicker {
    pub fn new() -> Self {
        Self {
            slot: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            test_program: std::sync::Mutex::new(None),
        }
    }
    pub fn supported() -> bool {
        cfg!(target_os = "macos")
    }
    pub async fn select(&self) -> Result<Option<PathBuf>, &'static str> {
        let _slot = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| "picker_busy")?;
        #[cfg(test)]
        let fake = self.test_program.lock().unwrap().clone();
        #[cfg(test)]
        if let Some(program) = fake {
            return run(Command::new(program), Duration::from_secs(5)).await;
        }
        #[cfg(target_os = "macos")]
        {
            let (directory, executable) = tokio::task::spawn_blocking(prepare_native_picker)
                .await
                .map_err(|_| "picker_unavailable")?
                .map_err(|_| "picker_unavailable")?;
            let result = run(Command::new(executable), Duration::from_secs(180)).await;
            drop(directory);
            result
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("picker_unsupported")
        }
    }
}

// Embedded, build-time compiled helper with its own language metadata. Never
// interpret browser input, modify OS preferences, or depend on runtime compilers.
#[cfg(target_os = "macos")]
fn prepare_native_picker() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix("codex-folder-picker-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    // A Mach-O info section is sufficient for in-process bundle lookups, but
    // the system panel service also needs a real application bundle on disk.
    let contents = directory.path().join("Codex Folder Picker.app/Contents");
    let executable_directory = contents.join("MacOS");
    std::fs::create_dir_all(&executable_directory)?;
    std::fs::write(
        contents.join("Info.plist"),
        include_bytes!("native_picker.plist"),
    )?;
    let executable = executable_directory.join("codex-folder-picker");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&executable)?;
    file.write_all(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/codex-folder-picker"
    )))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    drop(file);
    Ok((directory, executable))
}

#[cfg(any(target_os = "macos", test))]
async fn run(mut command: Command, timeout: Duration) -> Result<Option<PathBuf>, &'static str> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "picker_unavailable")?;
    let stdout = child.stdout.take().ok_or("picker_unavailable")?;
    tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        stdout
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "picker_unavailable")?;
        if bytes.len() > 16 * 1024 {
            return Err("picker_invalid_result");
        }
        let status = child.wait().await.map_err(|_| "picker_unavailable")?;
        if !status.success() {
            return Err("picker_unavailable");
        }
        decode(&bytes)
    })
    .await
    .map_err(|_| "picker_timeout")?
}
#[cfg(any(target_os = "macos", test))]
fn decode(bytes: &[u8]) -> Result<Option<PathBuf>, &'static str> {
    let path: Option<String> =
        serde_json::from_slice(bytes).map_err(|_| "picker_invalid_result")?;
    path.map(|path| {
        if path.len() > 4096
            || path.chars().any(char::is_control)
            || !std::path::Path::new(&path).is_absolute()
        {
            return Err("picker_invalid_result");
        }
        Ok(PathBuf::from(path))
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires a macOS graphical login (WindowServer); no window or activation"]
    async fn native_picker_starts_and_stops_the_application_event_loop() {
        let (directory, executable) = prepare_native_picker().unwrap();
        let mut command = Command::new(executable);
        command
            .arg("--event-loop-probe")
            .env("HOME", directory.path())
            .env("USERPROFILE", directory.path())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        // No panel or activation: exercise the same launch and asynchronous
        // completion path without presenting a panel or requesting activation.
        let output = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .expect("native application must stop without another input event")
            .unwrap();
        assert!(output.status.success(), "native event loop probe failed");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["running"], true);
        assert_eq!(value["finishedLaunching"], true);
        assert_eq!(value["mainThread"], true);
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn embedded_picker_follows_language_preferences_in_a_private_temporary_directory() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, executable) = prepare_native_picker().unwrap();
        let bundle = directory.path().join("Codex Folder Picker.app");
        assert_eq!(
            executable,
            bundle.join("Contents/MacOS/codex-folder-picker")
        );
        assert_eq!(
            std::fs::read(bundle.join("Contents/Info.plist")).unwrap(),
            include_bytes!("native_picker.plist")
        );
        assert_eq!(
            std::fs::metadata(directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&executable).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for language in ["zh-Hans", "en", "fr"] {
            let mut command = Command::new(&executable);
            command
                .args([
                    "--localization-probe",
                    "-AppleLanguages",
                    &format!("({language})"),
                ])
                .env("HOME", directory.path())
                .env("USERPROFILE", directory.path())
                .stdin(Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(5), command.output())
                .await
                .unwrap()
                .unwrap();
            assert!(output.status.success(), "picker localization probe failed");
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["bundlePath"], bundle.to_str().unwrap());
            assert_eq!(value["main"][0], language);
            assert!(
                value["appkit"][0]
                    .as_str()
                    .unwrap()
                    .starts_with(language.split('-').next().unwrap())
            );
        }
        let path = directory.path().to_owned();
        drop(directory);
        assert!(
            !path.exists(),
            "picker executable must not persist after use"
        );
    }
    #[test]
    fn native_result_is_json_with_exact_path_or_cancellation() {
        assert_eq!(decode(b"null\n"), Ok(None));
        let path = "/tmp/项目 with \"quotes\" and ' apostrophes/";
        assert_eq!(
            decode(&serde_json::to_vec(path).unwrap()).unwrap(),
            Some(PathBuf::from(path))
        );
        for bad in [
            b"\"relative\"".as_slice(),
            b"\"/tmp/a\\nnext\"",
            b"{}",
            b"garbage",
        ] {
            assert_eq!(decode(bad), Err("picker_invalid_result"));
        }
    }
    #[tokio::test]
    async fn native_child_deadline_output_and_single_slot_are_bounded() {
        let picker = FolderPicker::new();
        let permit = picker.slot.clone().try_acquire_owned().unwrap();
        assert_eq!(picker.select().await, Err("picker_busy"));
        drop(permit);
        let mut command = Command::new("/bin/cat");
        command.arg("/dev/zero");
        assert_eq!(
            run(command, Duration::from_secs(1)).await,
            Err("picker_invalid_result")
        );
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        assert_eq!(
            run(command, Duration::from_millis(20)).await,
            Err("picker_timeout")
        );
        assert!(picker.slot.clone().try_acquire_owned().is_ok());
    }
}
