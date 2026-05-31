#[cfg(test)]
mod cli_tests {
    use std::process::Command;

    fn get_cli_exe() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_finupdate-cli"))
    }

    struct MockEnv {
        _temp_dir: tempfile::TempDir,
        bin_dir: std::path::PathBuf,
        config_dir: std::path::PathBuf,
    }

    impl MockEnv {
        fn new() -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let bin_dir = temp_dir.path().join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let config_dir = temp_dir.path().join("config");
            std::fs::create_dir_all(&config_dir).unwrap();
            Self {
                _temp_dir: temp_dir,
                bin_dir,
                config_dir,
            }
        }

        fn create_mock_bin(&self, name: &str, stdout: &str, exit_code: i32) {
            let path = self.bin_dir.join(name);
            let content = format!(
                "#!/bin/sh\n\
                 echo -n \"{stdout}\"\n\
                 exit {exit_code}\n",
                stdout = stdout,
                exit_code = exit_code
            );
            std::fs::write(&path, content).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    #[test]
    fn test_cli_help() {
        let exe = get_cli_exe();
        let output = Command::new(&exe).arg("help").output().unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage: finupdate-cli"));
        assert!(stdout.contains("timer [cmd]"));
        assert!(stdout.contains("update"));
    }

    #[test]
    fn test_cli_status_and_family() {
        let env = MockEnv::new();
        let exe = get_cli_exe();

        let output = Command::new(&exe)
            .arg("status")
            .env(
                "FINUPDATE_IMAGE",
                "ghcr.io/projectbluefin/dakota:latest-20260527",
            )
            .env("XDG_CONFIG_HOME", &env.config_dir)
            .output()
            .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("ghcr.io/projectbluefin/dakota:latest-20260527"));

        let output_fam = Command::new(&exe)
            .arg("family")
            .env(
                "FINUPDATE_IMAGE",
                "ghcr.io/projectbluefin/dakota:latest-20260527",
            )
            .env("XDG_CONFIG_HOME", &env.config_dir)
            .output()
            .unwrap();

        assert!(output_fam.status.success());
        let stdout_fam = String::from_utf8_lossy(&output_fam.stdout);
        assert!(stdout_fam.contains("family: Bluefin Dakota"));
        assert!(stdout_fam.contains("base:   dakota"));
    }

    #[test]
    fn test_cli_timer_status() {
        let env = MockEnv::new();
        env.create_mock_bin("which", "/usr/bin/uupd\n", 0);
        env.create_mock_bin("systemctl", "enabled\n", 0);

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", env.bin_dir.display(), original_path);

        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("timer")
            .env("PATH", &new_path)
            .env("XDG_CONFIG_HOME", &env.config_dir)
            .output()
            .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("installed: true"));
        assert!(stdout.contains("timer:     enabled"));
    }

    #[test]
    fn test_cli_update_system_only() {
        let env = MockEnv::new();
        env.create_mock_bin("bootc", "", 0);
        env.create_mock_bin("flatpak", "", 0);
        env.create_mock_bin("su", "", 0);
        env.create_mock_bin("distrobox", "", 0);

        let runner_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("finupdate-runner");

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", env.bin_dir.display(), original_path);

        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("update")
            .arg("--system-only")
            .env("PATH", &new_path)
            .env("FINUPDATE_TEST_MOCK_RUNNER", &runner_path)
            .env("XDG_CONFIG_HOME", &env.config_dir)
            .output()
            .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Starting update sequence..."));
        assert!(stdout.contains("=== MODULE STARTED: System ==="));
        assert!(stdout.contains("=== MODULE FINISHED: System (Success) ==="));
        assert!(stdout.contains("=== MODULE FINISHED: Flatpak (Skipped) ==="));
        assert!(stdout.contains("Update completed successfully!"));
    }
}
