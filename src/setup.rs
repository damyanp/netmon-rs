//! Framework-dependent deployment needs the Windows App Runtime installed on the
//! machine. If `bootstrap()` fails because it is missing, offer to install it
//! (best effort, via winget) and open the official download page as a fallback.

use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, Result, w};

const DOWNLOAD_PAGE: &str =
    "https://learn.microsoft.com/windows/apps/windows-app-sdk/downloads";

/// winget package IDs to try, most specific first. IDs occasionally change
/// across releases, so we try a small list and fall back to the download page.
const WINGET_IDS: &[&str] = &[
    "Microsoft.WindowsAppRuntime.2",
    "Microsoft.WindowsAppRuntime.1.7",
    "Microsoft.WindowsAppRuntime.1.6",
];

/// Called when `bootstrap()` fails. Returns `Ok(())` if the runtime was
/// (probably) installed and a retry is worth attempting; `Err` otherwise.
pub fn handle_missing_runtime(err: &Error) -> Result<()> {
    let choice = unsafe {
        MessageBoxW(
            None,
            w!(
                "The Windows App Runtime is required to run Network Monitor but was not found.\n\n\
                 Install it now? (This runs 'winget install' for the runtime.)"
            ),
            w!("Network Monitor - runtime required"),
            MB_YESNO | MB_ICONQUESTION,
        )
    };

    if choice != IDYES {
        open_download_page();
        return Err(err.clone());
    }

    if install_via_winget() {
        return Ok(());
    }

    // winget failed or is unavailable: send the user to the download page.
    unsafe {
        MessageBoxW(
            None,
            w!(
                "Automatic install did not complete. Opening the Windows App SDK \
                 download page - install the Runtime, then relaunch Network Monitor."
            ),
            w!("Network Monitor"),
            MB_OK | MB_ICONWARNING,
        );
    }
    open_download_page();
    Err(err.clone())
}

fn install_via_winget() -> bool {
    for id in WINGET_IDS {
        let ok = std::process::Command::new("winget")
            .args([
                "install",
                "--id",
                id,
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    false
}

fn open_download_page() {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", DOWNLOAD_PAGE])
        .status();
}
