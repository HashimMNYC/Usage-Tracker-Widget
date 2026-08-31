use crate::{AutoLaunch, Result};
use winreg::enums::RegType::REG_BINARY;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::{RegKey, RegValue};

static AL_REGKEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
static TASK_MANAGER_OVERRIDE_REGKEY: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
static TASK_MANAGER_OVERRIDE_ENABLED_VALUE: [u8; 12] = [
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn windows_command(app_path: &str, args: &[impl AsRef<str>]) -> Result<String> {
    if app_path.contains('"') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "application paths containing quotes are unsupported",
        )
        .into());
    }
    let mut command = format!("\"{app_path}\"");
    for arg in args {
        command.push(' ');
        command.push_str(&quote_windows_arg(arg.as_ref()));
    }
    Ok(command)
}

fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return arg.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for character in arg.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.extend(std::iter::repeat_n('\\', backslashes.saturating_mul(2) + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.extend(std::iter::repeat_n('\\', backslashes));
            quoted.push(character);
            backslashes = 0;
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes.saturating_mul(2)));
    quoted.push('"');
    quoted
}

/// Windows implement
impl AutoLaunch {
    /// Create a new AutoLaunch instance
    /// - `app_name`: application name
    /// - `app_path`: application path
    /// - `args`: startup args passed to the binary
    ///
    /// ## Notes
    ///
    /// The parameters of `AutoLaunch::new` are different on each platform.
    pub fn new(app_name: &str, app_path: &str, args: &[impl AsRef<str>]) -> AutoLaunch {
        AutoLaunch {
            app_name: app_name.into(),
            app_path: app_path.into(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
        }
    }

    /// Enable the AutoLaunch setting
    ///
    /// ## Errors
    ///
    /// - failed to open the registry key
    /// - failed to set value
    pub fn enable(&self) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.open_subkey_with_flags(AL_REGKEY, KEY_SET_VALUE)?
            .set_value::<_, _>(
                &self.app_name,
                &windows_command(&self.app_path, &self.args)?,
            )?;

        // this key maybe not found
        if let Ok(reg) = hkcu.open_subkey_with_flags(TASK_MANAGER_OVERRIDE_REGKEY, KEY_SET_VALUE) {
            reg.set_raw_value(
                &self.app_name,
                &RegValue {
                    vtype: REG_BINARY,
                    bytes: TASK_MANAGER_OVERRIDE_ENABLED_VALUE.to_vec(),
                },
            )?;
        }

        Ok(())
    }

    /// Disable the AutoLaunch setting
    ///
    /// ## Errors
    ///
    /// - failed to open the registry key
    /// - failed to delete value
    pub fn disable(&self) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.open_subkey_with_flags(AL_REGKEY, KEY_SET_VALUE)?
            .delete_value(&self.app_name)?;
        Ok(())
    }

    /// Check whether the AutoLaunch setting is enabled
    pub fn is_enabled(&self) -> Result<bool> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        let al_enabled = hkcu
            .open_subkey_with_flags(AL_REGKEY, KEY_READ)?
            .get_value::<String, _>(&self.app_name)
            .is_ok();
        let task_manager_enabled = self.task_manager_enabled(hkcu);

        Ok(al_enabled && task_manager_enabled.unwrap_or(true))
    }

    fn task_manager_enabled(&self, hkcu: RegKey) -> Option<bool> {
        let task_manager_override_raw_value = hkcu
            .open_subkey_with_flags(TASK_MANAGER_OVERRIDE_REGKEY, KEY_READ)
            .ok()?
            .get_raw_value(&self.app_name)
            .ok()?;
        Some(last_eight_bytes_all_zeros(
            &task_manager_override_raw_value.bytes,
        )?)
    }
}

fn last_eight_bytes_all_zeros(bytes: &[u8]) -> Option<bool> {
    if bytes.len() < 8 {
        return None;
    }
    Some(bytes.iter().rev().take(8).all(|v| *v == 0u8))
}

#[cfg(test)]
mod command_tests {
    use super::windows_command;

    #[test]
    fn executable_paths_with_spaces_are_quoted() {
        assert_eq!(
            windows_command(
                r"C:\Program Files\Usage Widget\usage-widget.exe",
                &[] as &[&str],
            )
            .unwrap(),
            r#""C:\Program Files\Usage Widget\usage-widget.exe""#
        );
    }

    #[test]
    fn unicode_executable_paths_are_preserved_inside_quotes() {
        assert_eq!(
            windows_command(r"C:\应用\Usage Widget\usage-widget.exe", &["--hidden"]).unwrap(),
            r#""C:\应用\Usage Widget\usage-widget.exe" --hidden"#
        );
    }

    #[test]
    fn executable_paths_with_embedded_quotes_are_rejected() {
        assert!(windows_command(r#"C:\Apps\bad"name\usage-widget.exe"#, &[] as &[&str]).is_err());
    }
}
