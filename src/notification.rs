use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::combaseapi::{CoCreateInstance, CoInitializeEx};
use windows::core::{Error, HRESULT, HSTRING, Interface, PCWSTR, PWSTR, Result, w};
use windows::objbase::COINIT_APARTMENTTHREADED;
use windows::objidl::IPersistFile;
use windows::propidlbase::{
    PROPVAR_PAD1, PROPVAR_PAD2, PROPVAR_PAD3, PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::propkey::PKEY_AppUserModel_ID;
use windows::propsys::IPropertyStore;
use windows::shobjidl_core::{IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink};
use windows::wtypes::{VARTYPE, VT_LPWSTR};
use windows::wtypesbase::CLSCTX_INPROC_SERVER;

// Keep in sync with AppUserModelID in installer/netmon-rs.iss.
pub const APP_USER_MODEL_ID: &str = "DamyanPepper.NetworkMonitor";
const E_FAIL: HRESULT = HRESULT(0x8000_4005u32 as i32);

pub fn initialize_app_identity() -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED as u32) }.ok()?;
    unsafe { SetCurrentProcessExplicitAppUserModelID(w!("DamyanPepper.NetworkMonitor")) }.ok()?;
    ensure_start_menu_shortcut()
}

fn ensure_start_menu_shortcut() -> Result<()> {
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| Error::new(E_FAIL, "APPDATA is not available"))?;
    let shortcut_dir = Path::new(&appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Network Monitor");
    std::fs::create_dir_all(&shortcut_dir).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    let shortcut_path = shortcut_dir.join("Network Monitor.lnk");

    let shell_link: IShellLinkW =
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    let persist: IPersistFile = shell_link.cast()?;
    let shortcut_wide = wide(&shortcut_path);

    if shortcut_path.exists() {
        unsafe { persist.Load(PCWSTR(shortcut_wide.as_ptr()), 2) }.ok()?;
    } else {
        let executable = std::env::current_exe().map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let executable_wide = wide(&executable);
        unsafe { shell_link.SetPath(PCWSTR(executable_wide.as_ptr())) }.ok()?;
    }

    let property_store: IPropertyStore = shell_link.cast()?;
    let mut app_id_wide: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().chain(Some(0)).collect();
    let value = PROPVARIANT {
        Anonymous: windows::propidlbase::PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VARTYPE(VT_LPWSTR as u16),
                wReserved1: PROPVAR_PAD1(0),
                wReserved2: PROPVAR_PAD2(0),
                wReserved3: PROPVAR_PAD3(0),
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: PWSTR(app_id_wide.as_mut_ptr()),
                },
            }),
        },
    };
    unsafe {
        property_store
            .SetValue(&PKEY_AppUserModel_ID, &value)
            .ok()?;
        property_store.Commit().ok()?;
        persist.Save(PCWSTR(shortcut_wide.as_ptr()), true).ok()
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub fn show_packet_loss_alert(targets: &[(String, u32)]) -> Result<()> {
    let details = targets
        .iter()
        .map(|(name, loss)| format!("{name}: {loss}%"))
        .collect::<Vec<_>>()
        .join(", ");
    show("High packet loss detected", &details)
}

pub fn show_test_notification() -> Result<()> {
    show(
        "Network Monitor test",
        "Notifications are configured correctly.",
    )
}

fn show(title: &str, body: &str) -> Result<()> {
    let xml = format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        escape_xml(title),
        escape_xml(body)
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_USER_MODEL_ID))?;
    notifier.Show(&toast)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::escape_xml;

    #[test]
    fn escapes_toast_text() {
        assert_eq!(
            escape_xml("A&B <router> \"down\""),
            "A&amp;B &lt;router&gt; &quot;down&quot;"
        );
    }
}
