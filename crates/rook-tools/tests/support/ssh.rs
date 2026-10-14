use rook_tools::{
    ToolContext,
    environments::{Context, Pool, Profile, Settings},
};
use std::{collections::BTreeMap, path::PathBuf, process::Command, sync::Arc, time::Duration};
pub struct Server {
    pub root: tempfile::TempDir,
    pub id: String,
    pub program: PathBuf,
    pub config: PathBuf,
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn checked(command: &mut Command) -> std::process::Output {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output
}
impl Server {
    pub fn start() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::env::var_os("ROOK_ENV_TEST_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let root = tempfile::Builder::new().prefix("rook-ssh-native-").tempdir_in(base).unwrap();
        for name in ["host_key", "client_key"] {
            checked(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(root.path().join(name)),
            );
        }
        std::fs::copy(root.path().join("client_key.pub"), root.path().join("authorized_keys")).unwrap();
        std::fs::write(root.path().join("sshd_config"),"Port 22\nListenAddress 0.0.0.0\nHostKey /fixture/host_key\nAuthorizedKeysFile /fixture/authorized_keys\nPermitRootLogin prohibit-password\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nStrictModes no\nLogLevel ERROR\nSubsystem sftp internal-sftp\n").unwrap();
        let output = checked(
            Command::new("docker")
                .args(["run", "--pull", "never", "--detach", "--publish", "127.0.0.1::22", "--mount"])
                .arg(format!("type=bind,source={},target=/fixture,readonly", root.path().display()))
                .args(["--label", "rook.fixture=ssh-20261014", "rook-ssh-fixture:20261014"]),
        );
        let id = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let mut server = Self { root, id, program: PathBuf::new(), config: PathBuf::new() };
        let output = checked(Command::new("docker").args(["port", &server.id, "22/tcp"]));
        let endpoint = String::from_utf8(output.stdout).unwrap();
        let endpoint = endpoint.trim();
        let port = endpoint.rsplit(':').next().unwrap();
        let pubkey = std::fs::read_to_string(server.root.path().join("host_key.pub")).unwrap();
        std::fs::write(server.root.path().join("known_hosts"), format!("[127.0.0.1]:{port} {pubkey}"))
            .unwrap();
        server.config = server.root.path().join("ssh_config");
        std::fs::write(&server.config,format!("Host rook-native\n HostName 127.0.0.1\n Port {port}\n User root\n IdentityFile {}\n IdentitiesOnly yes\n UserKnownHostsFile {}\n",server.root.path().join("client_key").display(),server.root.path().join("known_hosts").display())).unwrap();
        server.program = server.root.path().join("ssh-wrapper");
        std::fs::write(
            &server.program,
            format!("#!/bin/sh\nexec ssh -F {} \"$@\"\n", quote(server.config.to_str().unwrap())),
        )
        .unwrap();
        std::fs::set_permissions(&server.program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if std::net::TcpStream::connect(endpoint).is_ok() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "SSH fixture never accepted TCP");
            std::thread::sleep(Duration::from_millis(20));
        }
        server
    }
    #[allow(dead_code)] // Shared by engine/frontend fixtures that only need the server.
    pub fn context(&self) -> ToolContext {
        let workspace = self.root.path().join("workspace");
        let state = self.root.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let mut ctx = ToolContext::new(workspace);
        ctx.execution = Some(Arc::new(Context {
            settings: Settings {
                selected: "lab".into(),
                profiles: BTreeMap::from([(
                    "lab".into(),
                    Profile::Ssh {
                        host: "rook-native".into(),
                        program: self.program.display().to_string(),
                        remote_workspace: "/tmp/rook-workspaces".into(),
                    },
                )]),
                ..Default::default()
            },
            pool: Arc::new(Pool::default()),
            state_root: state,
        }));
        ctx
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "--force", &self.id]).output();
    }
}
