use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

pub struct TempDirGuard(PathBuf);

impl TempDirGuard {
    pub fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub fn from_host(prefix: &str, scenario: &str) -> Self {
        let guard = Self::new(prefix);
        copy_host_scenario(scenario, guard.path());
        guard
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn copy_host_scenario(scenario: &str, dest: &Path) {
    let hosts_root = std::env::var("VERULE_ENGINE_TEST_HOSTS_DIR").expect(
        "VERULE_ENGINE_TEST_HOSTS_DIR must be set (run via `pnpm --filter @verule/engine test`)",
    );
    let src = Path::new(&hosts_root).join(scenario);
    assert!(
        src.is_dir(),
        "checked engine host scenario '{scenario}' does not exist at {}",
        src.display()
    );
    copy_dir_recursive(&src, dest);
}

pub fn copy_dir_recursive(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create target dir");
    for entry in fs::read_dir(src).expect("read source dir") {
        let entry = entry.expect("read dir entry");
        let name = entry.file_name();
        if name == ".git" || name == "node_modules" || name == "dist" {
            continue;
        }
        let src_path = entry.path();
        let dest_path = dest.join(name);
        let file_type = entry.file_type().expect("read entry file type");
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dest_path);
        } else if file_type.is_file() {
            fs::copy(&src_path, &dest_path).expect("copy host file");
        }
    }
}

pub fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn spawn_engine_stdio() -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_verule-engine"))
        .args(["serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn verule-engine serve --stdio");
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    (child, stdin, BufReader::new(stdout))
}

pub fn rpc_call(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    request: &Value,
) -> Value {
    let line = serde_json::to_string(request).expect("serialize request");
    writeln!(stdin, "{line}").expect("write rpc line");
    stdin.flush().expect("flush rpc line");
    let mut resp = String::new();
    let bytes = reader.read_line(&mut resp).expect("read rpc response");
    assert!(bytes > 0, "expected RPC response line, got EOF");
    serde_json::from_str(&resp).expect("valid JSON response")
}

pub fn finish_engine(child: Child, stdin: ChildStdin) {
    drop(stdin);
    let output = child.wait_with_output().expect("wait for engine exit");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
