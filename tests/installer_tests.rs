//! End-to-end tests for the standalone release installers.
//!
//! The fixtures replace network and signature tooling, but execute the real
//! installer script. This protects installer behavior rather than its source
//! layout or wording.

#[cfg(unix)]
mod unix {
    use flate2::{write::GzEncoder, Compression};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn create_release_archive(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let body = b"#!/bin/sh\nprintf '%s\\n' 'xv v-test'\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, "xv", &body[..]).unwrap();
        archive.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn unix_installer_does_not_invoke_azure_cli() {
        let temp = tempfile::tempdir().unwrap();
        let fake_bin = temp.path().join("bin");
        let fixture_dir = temp.path().join("fixtures");
        let install_dir = temp.path().join("install");
        let home_dir = temp.path().join("home");
        fs::create_dir_all(&fake_bin).unwrap();
        fs::create_dir_all(&fixture_dir).unwrap();
        fs::create_dir_all(&home_dir).unwrap();

        let archive_path = fixture_dir.join("xv.tar.gz");
        create_release_archive(&archive_path);
        let archive = fs::read(&archive_path).unwrap();
        let checksum = hex::encode(Sha256::digest(&archive));
        let checksum_path = fixture_dir.join("xv.tar.gz.sha256");
        fs::write(&checksum_path, format!("{checksum}  xv.tar.gz\n")).unwrap();
        let signature_path = fixture_dir.join("xv.tar.gz.minisig");
        fs::write(
            &signature_path,
            "untrusted comment: test fixture\nplaceholder\ntrusted comment: crosstache v-test\n",
        )
        .unwrap();

        let az_marker = temp.path().join("az-invoked");
        write_executable(
            &fake_bin.join("az"),
            "#!/bin/sh\n: > \"$AZ_MARKER\"\nprintf '%s\\n' '{\"azure-cli\":\"9.9.9\"}'\n",
        );
        write_executable(&fake_bin.join("minisign"), "#!/bin/sh\nexit 0\n");
        write_executable(
            &fake_bin.join("curl"),
            r#"#!/bin/sh
out=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            out=$2
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done
case "$url" in
    *.sha256) cp -f "$FIXTURE_CHECKSUM" "$out" ;;
    *.minisig) cp -f "$FIXTURE_SIGNATURE" "$out" ;;
    *.tar.gz) cp -f "$FIXTURE_ARCHIVE" "$out" ;;
    *) exit 64 ;;
esac
"#,
        );

        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&inherited_path)),
        )
        .unwrap();
        let installer = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
        let output = Command::new("bash")
            .arg(installer)
            .arg("v-test")
            .env("PATH", path)
            .env("HOME", &home_dir)
            .env("SHELL", "/bin/bash")
            .env("XDG_BIN_HOME", &install_dir)
            .env("AZ_MARKER", &az_marker)
            .env("FIXTURE_ARCHIVE", &archive_path)
            .env("FIXTURE_CHECKSUM", &checksum_path)
            .env("FIXTURE_SIGNATURE", &signature_path)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(install_dir.join("xv").is_file(), "xv was not installed");
        assert!(
            !az_marker.exists(),
            "the generic installer must not invoke Azure CLI"
        );
    }
}
