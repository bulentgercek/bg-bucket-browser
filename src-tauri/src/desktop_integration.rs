//! "Open in BG Bucket Browser": the entry a file manager offers on a folder.
//!
//! Three platforms, three unrelated mechanisms — a `.desktop` file on Linux, a
//! registry shell verb on Windows, a Services workflow on macOS — behind the
//! same two commands, so the frontend only ever sees a toggle.

use std::sync::Mutex;

/// Where the folder macOS hands over waits until the frontend asks for it.
///
/// Linux and Windows pass the folder as a command line argument; macOS sends an
/// Apple Event instead, which arrives after startup and needs somewhere to sit.
#[derive(Default)]
pub struct OpenHereState(pub Mutex<Option<String>>);

/// The folder this launch was asked to open, if it was asked at all.
///
/// Anything that is not a directory is ignored, so a stray argument cannot send
/// the pane somewhere meaningless.
#[tauri::command]
pub fn take_startup_path(state: tauri::State<OpenHereState>) -> Option<String> {
    let candidate = std::env::args()
        .nth(1)
        .or_else(|| state.0.lock().unwrap().take());
    candidate.filter(|p| std::path::Path::new(p).is_dir())
}

#[tauri::command]
pub fn is_open_here_registered() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::is_registered()
    }
    #[cfg(target_os = "windows")]
    {
        windows::is_registered()
    }
    #[cfg(target_os = "macos")]
    {
        macos::is_registered()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

/// Creates the registration or removes it.
#[tauri::command]
pub fn register_open_here(enable: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::register(enable)
    }
    #[cfg(target_os = "windows")]
    {
        windows::register(enable)
    }
    #[cfg(target_os = "macos")]
    {
        macos::register(enable)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = enable;
        Err("not supported on this platform yet".to_string())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! A user-level `.desktop` file that adds the app to a folder's "Open With"
    //! list.
    //!
    //! Tauri's own file-association support is not used: the `.desktop` file it
    //! generates never puts `%f` on the `Exec=` line, so the clicked folder's
    //! path would never reach the app. Writing the file at runtime, from the
    //! running executable's real path, also covers both ways this app is
    //! installed — a fixed path from the `.deb`, and wherever the user keeps
    //! the AppImage.
    //!
    //! Nothing system-wide changes: no default handler is set, and no root is
    //! needed.

    use std::fs;
    use std::path::PathBuf;

    const DESKTOP_FILE_NAME: &str = "com.bulentgercek.bgbucketbrowser-openhere.desktop";

    fn desktop_file_path() -> Result<PathBuf, String> {
        let home = std::env::home_dir()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "home directory not found".to_string())?;
        Ok(home.join(".local/share/applications").join(DESKTOP_FILE_NAME))
    }

    pub fn is_registered() -> bool {
        desktop_file_path().map(|p| p.exists()).unwrap_or(false)
    }

    /// Writes the file or removes it, then nudges the desktop database.
    ///
    /// The refresh is best effort: file managers find the file on their own scan
    /// anyway.
    pub fn register(enable: bool) -> Result<(), String> {
        let path = desktop_file_path()?;
        if enable {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_str = exe.to_string_lossy();
            let content = format!(
                "[Desktop Entry]\n\
                 Type=Application\n\
                 Name=BG Bucket Browser\n\
                 Comment=Open this folder in BG Bucket Browser\n\
                 Exec=\"{exe_str}\" %f\n\
                 Icon=bg-bucket-browser\n\
                 Categories=Utility;\n\
                 MimeType=inode/directory;\n\
                 NoDisplay=true\n\
                 Terminal=false\n"
            );
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::write(&path, content).map_err(|e| e.to_string())?;
        } else {
            let _ = fs::remove_file(&path);
        }
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("update-desktop-database")
                .arg(dir)
                .status();
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod windows {
    //! A shell verb on `Directory`, the practical Windows counterpart of the
    //! Linux entry: Explorer's "Open With" dialog is meant for files, and
    //! folders are customised through the right-click verb instead.
    //!
    //! It goes under `HKEY_CURRENT_USER`, so it stays a per-user setting and
    //! never asks for admin rights.

    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const KEY_PATH: &str = r"Software\Classes\Directory\shell\BGBucketBrowserOpenHere";

    pub fn is_registered() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(KEY_PATH)
            .is_ok()
    }

    pub fn register(enable: bool) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if enable {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_str = exe.to_string_lossy();
            let (key, _) = hkcu.create_subkey(KEY_PATH).map_err(|e| e.to_string())?;
            key.set_value("", &"Open in BG Bucket Browser")
                .map_err(|e| e.to_string())?;
            key.set_value("Icon", &format!("{exe_str},0"))
                .map_err(|e| e.to_string())?;
            let (cmd, _) = key.create_subkey("command").map_err(|e| e.to_string())?;
            cmd.set_value("", &format!("\"{exe_str}\" \"%V\""))
                .map_err(|e| e.to_string())?;
        } else {
            // Deleting a key that is not there gives the same outcome, so the result is ignored.
            let _ = hkcu.delete_subkey_all(KEY_PATH);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! A Services entry under `~/Library/Services/`, which is what actually puts
    //! the app in Finder's right-click menu for a folder.
    //!
    //! The workflow runs a single shell action, `open -a "<bundle>" "$1"`, with
    //! the folder arriving as the first argument. The bundle path is derived
    //! from the running executable rather than from the app's name, so a renamed
    //! or moved app still works.
    //!
    //! Like the other two platforms this is user-level and needs no admin, and
    //! the toggle in Settings writes and removes it at runtime.

    use std::fs;
    use std::path::PathBuf;

    const WORKFLOW_NAME: &str = "Open in BG Bucket Browser.workflow";

    fn workflow_dir() -> Result<PathBuf, String> {
        let home = std::env::home_dir()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "home directory not found".to_string())?;
        Ok(home.join("Library/Services").join(WORKFLOW_NAME))
    }

    /// The `.app` bundle around the running binary, three levels up.
    ///
    /// Only meaningful in a packaged build: during development the binary sits
    /// in the target directory and is inside no bundle at all, so this feature
    /// can only be tested from a real build.
    fn app_bundle_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        exe.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .ok_or_else(|| "could not resolve .app bundle path".to_string())
    }

    /// Escapes the path for XML text content.
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    pub fn is_registered() -> bool {
        workflow_dir().map(|p| p.exists()).unwrap_or(false)
    }

    pub fn register(enable: bool) -> Result<(), String> {
        let dir = workflow_dir()?;
        if !enable {
            // Removing what is not there gives the same outcome, so the result is ignored.
            let _ = fs::remove_dir_all(&dir);
            return Ok(());
        }

        let app_path = app_bundle_path()?;
        let command = format!(
            "open -a \"{}\" \"$1\"",
            xml_escape(&app_path.to_string_lossy())
        );

        // Automator's own export also contains an empty `Contents/QuickLook/`
        // directory; it only feeds Finder's preview and is left out.
        let contents_dir = dir.join("Contents");
        fs::create_dir_all(&contents_dir).map_err(|e| e.to_string())?;

        fs::write(contents_dir.join("Info.plist"), INFO_PLIST).map_err(|e| e.to_string())?;
        fs::write(
            contents_dir.join("document.wflow"),
            document_wflow(&command),
        )
        .map_err(|e| e.to_string())?;

        // Best effort refresh of the services database, the way Automator does
        // after saving; Finder would find the workflow on its own regardless.
        let _ = std::process::Command::new("/System/Library/CoreServices/pbs")
            .arg("-flush")
            .status();

        Ok(())
    }

    /// The exported `Info.plist`, copied as it was; nothing here is templated.
    const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSBackgroundColorName</key>
			<string>background</string>
			<key>NSIconName</key>
			<string>NSActionTemplate</string>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>Open in BG Bucket Browser</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
				<string>public.folder</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
"#;

    /// The workflow document, with only the shell command substituted.
    ///
    /// The UUIDs are Automator's internal references, kept exactly as exported;
    /// they mean nothing at runtime.
    fn document_wflow(command: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>512.1</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{command}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/zsh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Category</key>
				<array>
					<string>AMCategoryUtilities</string>
				</array>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>CBE2420D-E90B-492E-B96E-A5E6457EEE3B</string>
				<key>Keywords</key>
				<array>
					<string>Shell</string>
					<string>Script</string>
					<string>Command</string>
					<string>Run</string>
					<string>Unix</string>
				</array>
				<key>OutputUUID</key>
				<string>00AB5EE6-1735-4859-AC99-AE7CF185B3BC</string>
				<key>UUID</key>
				<string>325B5525-1BD9-4EB6-9663-17AA4813E52D</string>
				<key>UnlocalizedApplications</key>
				<array>
					<string>Automator</string>
				</array>
				<key>arguments</key>
				<dict>
					<key>0</key>
					<dict>
						<key>default value</key>
						<integer>0</integer>
						<key>name</key>
						<string>inputMethod</string>
						<key>required</key>
						<string>0</string>
						<key>type</key>
						<string>0</string>
						<key>uuid</key>
						<string>0</string>
					</dict>
					<key>1</key>
					<dict>
						<key>default value</key>
						<false/>
						<key>name</key>
						<string>CheckedForUserDefaultShell</string>
						<key>required</key>
						<string>0</string>
						<key>type</key>
						<string>0</string>
						<key>uuid</key>
						<string>1</string>
					</dict>
					<key>2</key>
					<dict>
						<key>default value</key>
						<string></string>
						<key>name</key>
						<string>source</string>
						<key>required</key>
						<string>0</string>
						<key>type</key>
						<string>0</string>
						<key>uuid</key>
						<string>2</string>
					</dict>
					<key>3</key>
					<dict>
						<key>default value</key>
						<string></string>
						<key>name</key>
						<string>COMMAND_STRING</string>
						<key>required</key>
						<string>0</string>
						<key>type</key>
						<string>0</string>
						<key>uuid</key>
						<string>3</string>
					</dict>
					<key>4</key>
					<dict>
						<key>default value</key>
						<string>/bin/sh</string>
						<key>name</key>
						<string>shell</string>
						<key>required</key>
						<string>0</string>
						<key>type</key>
						<string>0</string>
						<key>uuid</key>
						<string>4</string>
					</dict>
				</dict>
				<key>conversionLabel</key>
				<integer>0</integer>
				<key>isViewVisible</key>
				<integer>1</integer>
				<key>location</key>
				<string>318.000000:305.000000</string>
				<key>nibPath</key>
				<string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
			</dict>
			<key>isViewVisible</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>applicationBundleID</key>
		<string>com.apple.finder</string>
		<key>applicationBundleIDsByPath</key>
		<dict>
			<key>/System/Library/CoreServices/Finder.app</key>
			<string>com.apple.finder</string>
		</dict>
		<key>applicationPath</key>
		<string>/System/Library/CoreServices/Finder.app</string>
		<key>applicationPaths</key>
		<array>
			<string>/System/Library/CoreServices/Finder.app</string>
		</array>
		<key>inputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject.folder</string>
		<key>outputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>presentationMode</key>
		<integer>15</integer>
		<key>processesInput</key>
		<integer>0</integer>
		<key>serviceApplicationBundleID</key>
		<string>com.apple.finder</string>
		<key>serviceApplicationPath</key>
		<string>/System/Library/CoreServices/Finder.app</string>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject.folder</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<integer>0</integer>
		<key>systemImageName</key>
		<string>NSActionTemplate</string>
		<key>useAutomaticInputType</key>
		<integer>0</integer>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#
        )
    }
}
