use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CANCELLED, HANDLE, HWND, MAX_PATH, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Startup, IShellLinkW, SHGetKnownFolderPath, ShellExecuteExW, ShellLink,
    KNOWN_FOLDER_FLAG, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

pub const SHORTCUT_FILENAME: &str = "ZoneSnap.lnk";
pub const ADMIN_TASK_NAME: &str = "ZoneSnap";
pub const ARG_ADMIN_ENABLE: &str = "--startup-admin-enable";
pub const ARG_ADMIN_DISABLE: &str = "--startup-admin-disable";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Off,
    Standard,
    Administrator,
}

#[derive(Debug, Clone)]
pub struct StartupStatus {
    pub mode: StartupMode,
    pub registered_path: Option<PathBuf>,
    pub current_exe: Option<PathBuf>,
    pub path_mismatch: bool,
    pub running_as_admin: bool,
    pub startup_folder: Option<PathBuf>,
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Normalize a Windows path string for case-insensitive comparison.
pub fn normalize_windows_path(p: &Path) -> String {
    let mut s = p.to_string_lossy().trim().trim_matches('"').to_string();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    let mut s = s.replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s.to_ascii_lowercase()
}

pub fn paths_match(a: &Path, b: &Path) -> bool {
    let na = normalize_windows_path(a);
    let nb = normalize_windows_path(b);
    !na.is_empty() && na == nb
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub fn quote_windows_arg(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == '"') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in s.chars() {
        if ch == '\\' {
            backslashes += 1;
        } else if ch == '"' {
            for _ in 0..(backslashes * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            backslashes = 0;
            out.push(ch);
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Build Task Scheduler 1.2 XML for an interactive elevated logon task.
pub fn build_admin_task_xml(exe_path: &Path, user_id: &str) -> String {
    let exe_str = exe_path.to_string_lossy();
    let work_dir = exe_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let esc_user = xml_escape(user_id);
    let esc_cmd = xml_escape(&exe_str);
    let esc_dir = xml_escape(&work_dir);

    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Start ZoneSnap with Administrator privileges at logon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{esc_user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{esc_user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>false</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{esc_cmd}</Command>
      <WorkingDirectory>{esc_dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#
    )
}

pub fn decode_schtasks_output(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 4 && bytes.len() % 2 == 0 && bytes[1] == 0 && bytes[3] == 0 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn parse_task_xml_command(xml: &str) -> Option<PathBuf> {
    let start_tag = "<Command>";
    let end_tag = "</Command>";
    let start = xml.find(start_tag)? + start_tag.len();
    let rest = &xml[start..];
    let end = rest.find(end_tag)?;
    let raw = xml_unescape(rest[..end].trim());
    let cleaned = raw.trim().trim_matches('"').trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(PathBuf::from(cleaned))
    }
}

pub fn current_user_id() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    if domain.is_empty() || domain.eq_ignore_ascii_case(&user) {
        user
    } else {
        format!("{domain}\\{user}")
    }
}

pub fn is_running_as_admin() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

pub fn startup_folder_path() -> Option<PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Startup, KNOWN_FOLDER_FLAG(0), HANDLE::default())
            .ok()?;
        let mut len = 0usize;
        while *raw.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(raw.0, len);
        let path_str = String::from_utf16_lossy(slice);
        CoTaskMemFree(Some(raw.0 as *const c_void));
        if path_str.is_empty() {
            None
        } else {
            Some(PathBuf::from(path_str))
        }
    }
}

fn shortcut_file_path() -> Option<PathBuf> {
    startup_folder_path().map(|dir| dir.join(SHORTCUT_FILENAME))
}

struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn new() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr.0 as u32 == 0x8001_0106 {
            // RPC_E_CHANGED_MODE: thread already initialized (e.g., MTA from eframe/winit)
            Ok(Self {
                should_uninit: false,
            })
        } else {
            Err(format!("COM initialization failed (0x{:08X})", hr.0 as u32))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

pub fn write_shortcut(lnk_path: &Path, target_exe: &Path) -> Result<(), String> {
    let _com = ComGuard::new()?;
    let target_str = target_exe.to_string_lossy();
    let work_dir_str = target_exe
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lnk_str = lnk_path.to_string_lossy();

    let w_target = to_wide_null(&target_str);
    let w_dir = to_wide_null(&work_dir_str);
    let w_desc = to_wide_null("ZoneSnap — Portable Window Manager");
    let w_lnk = to_wide_null(&lnk_str);

    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("Failed to create ShellLink COM object: {e}"))?;
        link.SetPath(PCWSTR(w_target.as_ptr()))
            .map_err(|e| format!("Failed to set shortcut target path: {e}"))?;
        if !work_dir_str.is_empty() {
            link.SetWorkingDirectory(PCWSTR(w_dir.as_ptr()))
                .map_err(|e| format!("Failed to set shortcut working directory: {e}"))?;
        }
        link.SetDescription(PCWSTR(w_desc.as_ptr()))
            .map_err(|e| format!("Failed to set shortcut description: {e}"))?;
        link.SetIconLocation(PCWSTR(w_target.as_ptr()), 0)
            .map_err(|e| format!("Failed to set shortcut icon: {e}"))?;

        let persist: IPersistFile = link
            .cast()
            .map_err(|e| format!("Failed to acquire IPersistFile: {e}"))?;
        persist
            .Save(PCWSTR(w_lnk.as_ptr()), true)
            .map_err(|e| format!("Failed to save shortcut {}: {e}", lnk_path.display()))?;
    }
    Ok(())
}

pub fn read_shortcut_target(lnk_path: &Path) -> Option<PathBuf> {
    if !lnk_path.exists() {
        return None;
    }
    let _com = ComGuard::new().ok()?;
    let w_lnk = to_wide_null(&lnk_path.to_string_lossy());
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist.Load(PCWSTR(w_lnk.as_ptr()), STGM_READ).ok()?;
        let mut buf = vec![0u16; MAX_PATH as usize * 4];
        let mut find_data = WIN32_FIND_DATAW::default();
        link.GetPath(&mut buf, &mut find_data, 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        }
    }
}

pub fn create_standard_shortcut(exe_path: &Path) -> Result<(), String> {
    let lnk = shortcut_file_path()
        .ok_or_else(|| "Could not locate Windows Startup folder (shell:startup).".to_string())?;
    if let Some(parent) = lnk.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_shortcut(&lnk, exe_path)
}

pub fn remove_standard_shortcut() -> Result<(), String> {
    let Some(lnk) = shortcut_file_path() else {
        return Ok(());
    };
    if lnk.exists() {
        std::fs::remove_file(&lnk)
            .map_err(|e| format!("Failed to remove {}: {e}", lnk.display()))?;
    }
    Ok(())
}

pub fn open_startup_folder() -> Result<(), String> {
    let dir = startup_folder_path()
        .ok_or_else(|| "Could not locate Windows Startup folder (shell:startup).".to_string())?;
    Command::new("explorer.exe")
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("Failed to open Explorer: {e}"))?;
    Ok(())
}

pub fn query_admin_task_target() -> Option<PathBuf> {
    let out = Command::new("schtasks.exe")
        .args(["/Query", "/TN", ADMIN_TASK_NAME, "/XML"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = decode_schtasks_output(&out.stdout);
    parse_task_xml_command(&text)
}

pub fn install_admin_task_direct(exe_path: &Path, user_id: &str) -> Result<(), String> {
    let xml = build_admin_task_xml(exe_path, user_id);
    let temp_xml = std::env::temp_dir().join(format!("zonesnap_task_{}.xml", std::process::id()));

    let mut bytes = Vec::with_capacity(2 + xml.len() * 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&temp_xml, &bytes)
        .map_err(|e| format!("Failed to write temporary task XML: {e}"))?;

    let temp_str = temp_xml.to_string_lossy().into_owned();
    let result = Command::new("schtasks.exe")
        .args(["/Create", "/TN", ADMIN_TASK_NAME, "/XML", &temp_str, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let _ = std::fs::remove_file(&temp_xml);

    let out = result.map_err(|e| format!("Failed to launch schtasks.exe: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = decode_schtasks_output(&out.stderr);
        let stdout = decode_schtasks_output(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(if detail.is_empty() {
            "Task Scheduler failed to create the ZoneSnap task.".to_string()
        } else {
            detail
        })
    }
}

pub fn remove_admin_task_direct() -> Result<(), String> {
    let out = Command::new("schtasks.exe")
        .args(["/Delete", "/TN", ADMIN_TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Failed to launch schtasks.exe: {e}"))?;

    if out.status.success() || query_admin_task_target().is_none() {
        Ok(())
    } else {
        let stderr = decode_schtasks_output(&out.stderr);
        let stdout = decode_schtasks_output(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(if detail.is_empty() {
            "Task Scheduler failed to delete the ZoneSnap task.".to_string()
        } else {
            detail
        })
    }
}

fn run_elevated_self(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not determine current executable path: {e}"))?;
    let exe_str = exe.to_string_lossy().into_owned();
    let params = args
        .iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ");

    let w_exe = to_wide_null(&exe_str);
    let w_params = to_wide_null(&params);

    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: HWND::default(),
            lpVerb: w!("runas"),
            lpFile: PCWSTR(w_exe.as_ptr()),
            lpParameters: PCWSTR(w_params.as_ptr()),
            lpDirectory: PCWSTR::null(),
            nShow: SW_HIDE.0,
            ..Default::default()
        };

        if let Err(err) = ShellExecuteExW(&mut sei) {
            let last = GetLastError();
            if last == ERROR_CANCELLED || err.code().0 as u32 == 0x8007_04C7 {
                return Err(
                    "Administrator permission was cancelled — startup setting unchanged."
                        .to_string(),
                );
            }
            return Err(format!("Failed to request Administrator permission: {err}"));
        }

        if sei.hProcess.is_invalid() || sei.hProcess.0.is_null() {
            return Err("Elevated helper process did not return a valid handle.".to_string());
        }

        let wait = WaitForSingleObject(sei.hProcess, 30_000);
        let mut exit_code = 1u32;
        let _ = GetExitCodeProcess(sei.hProcess, &mut exit_code);
        let _ = CloseHandle(sei.hProcess);

        if wait != WAIT_OBJECT_0 {
            return Err("Timed out waiting for Task Scheduler update.".to_string());
        }
        if exit_code != 0 {
            return Err(format!(
                "Task Scheduler update exited with error code {exit_code}."
            ));
        }
    }
    Ok(())
}

fn install_admin_task(exe_path: &Path) -> Result<(), String> {
    let user_id = current_user_id();
    if is_running_as_admin() {
        install_admin_task_direct(exe_path, &user_id)
    } else {
        run_elevated_self(&[
            ARG_ADMIN_ENABLE.to_string(),
            exe_path.to_string_lossy().into_owned(),
            user_id,
        ])
    }
}

fn remove_admin_task() -> Result<(), String> {
    if query_admin_task_target().is_none() {
        return Ok(());
    }
    if is_running_as_admin() {
        remove_admin_task_direct()
    } else {
        run_elevated_self(&[ARG_ADMIN_DISABLE.to_string()])
    }
}

pub fn query_startup_status() -> StartupStatus {
    let current_exe = std::env::current_exe().ok();
    let startup_folder = startup_folder_path();
    let running_as_admin = is_running_as_admin();

    let admin_target = query_admin_task_target();
    let lnk_target = shortcut_file_path().and_then(|p| read_shortcut_target(&p));

    let (mode, registered_path) = if let Some(target) = admin_target {
        (StartupMode::Administrator, Some(target))
    } else if let Some(target) = lnk_target {
        (StartupMode::Standard, Some(target))
    } else if shortcut_file_path().is_some_and(|p| p.exists()) {
        (StartupMode::Standard, None)
    } else {
        (StartupMode::Off, None)
    };

    let path_mismatch = registered_path
        .as_ref()
        .zip(current_exe.as_ref())
        .is_some_and(|(reg, cur)| !paths_match(reg, cur));

    StartupStatus {
        mode,
        registered_path,
        current_exe,
        path_mismatch,
        running_as_admin,
        startup_folder,
    }
}

pub fn apply_startup_mode(target: StartupMode) -> Result<(StartupStatus, String), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Could not determine current executable path: {e}"))?;

    let message = match target {
        StartupMode::Off => {
            remove_admin_task()?;
            remove_standard_shortcut()?;
            "Start with Windows disabled (all startup entries removed).".to_string()
        }
        StartupMode::Standard => {
            remove_admin_task()?;
            create_standard_shortcut(&current_exe)?;
            "Enabled Standard startup via shell:startup shortcut.".to_string()
        }
        StartupMode::Administrator => {
            install_admin_task(&current_exe)?;
            remove_standard_shortcut()?;
            "Enabled Administrator startup via Windows Task Scheduler.".to_string()
        }
    };

    Ok((query_startup_status(), message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_normalization_ignores_case_slashes_and_verbatim_prefix() {
        let a = Path::new(r#"\\?\C:/Tools/ZoneSnap/zonesnap.exe"#);
        let b = Path::new(r#"c:\tools\zonesnap\zonesnap.exe"#);
        let c = Path::new(r#"C:\Other\zonesnap.exe"#);
        assert!(paths_match(a, b));
        assert!(!paths_match(a, c));
    }

    #[test]
    fn xml_escape_and_unescape_round_trip() {
        let raw = r#"C:\Apps\Tom & Jerry's "ZoneSnap"\zonesnap.exe"#;
        let escaped = xml_escape(raw);
        assert!(!escaped.contains('&') || escaped.contains("&amp;"));
        assert_eq!(xml_unescape(&escaped), raw);
    }

    #[test]
    fn admin_task_xml_disables_battery_and_time_limits() {
        let exe = Path::new(r#"C:\Apps\Tom & Bob\zonesnap.exe"#);
        let xml = build_admin_task_xml(exe, r#"PC\Tom"#);
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains("<Priority>5</Priority>"));
        assert!(xml.contains("<Command>C:\\Apps\\Tom &amp; Bob\\zonesnap.exe</Command>"));
        assert!(xml.contains("<WorkingDirectory>C:\\Apps\\Tom &amp; Bob</WorkingDirectory>"));
        assert_eq!(parse_task_xml_command(&xml).as_deref(), Some(exe));
    }

    #[test]
    fn decode_schtasks_output_handles_utf16le_bom_and_utf8() {
        let xml = r#"<Task><Actions><Exec><Command>"C:\My Tools\zonesnap.exe"</Command></Exec></Actions></Task>"#;
        let mut utf16 = vec![0xFFu8, 0xFEu8];
        for u in xml.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let decoded = decode_schtasks_output(&utf16);
        assert_eq!(
            parse_task_xml_command(&decoded).as_deref(),
            Some(Path::new(r#"C:\My Tools\zonesnap.exe"#))
        );
        assert_eq!(
            parse_task_xml_command(xml).as_deref(),
            Some(Path::new(r#"C:\My Tools\zonesnap.exe"#))
        );
    }

    #[test]
    fn com_shortcut_round_trip_in_temp_dir() {
        let exe = std::env::current_exe().expect("current_exe");
        let tmp_lnk =
            std::env::temp_dir().join(format!("zonesnap_test_{}.lnk", std::process::id()));
        write_shortcut(&tmp_lnk, &exe).expect("write_shortcut");
        let target = read_shortcut_target(&tmp_lnk).expect("read_shortcut_target");
        let _ = std::fs::remove_file(&tmp_lnk);
        assert!(paths_match(&target, &exe));
    }

    #[test]
    fn startup_folder_and_status_query_succeed() {
        assert!(startup_folder_path().is_some());
        let st = query_startup_status();
        assert!(st.current_exe.is_some());
    }
}
