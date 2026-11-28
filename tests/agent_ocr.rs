#![cfg(feature = "ocr")]
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Stdio, time::Duration};
use taskloom::runtime::{ocr::*, sandbox::DockerSandboxConfig, ToolContext};
use tokio::io::AsyncWriteExt;
fn tiny_png() -> Vec<u8> {
    let mut result = b"\x89PNG\r\n\x1a\n".to_vec();
    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        let checksum = crc(&out[start..]);
        out.extend_from_slice(&checksum.to_be_bytes());
    }
    chunk(
        &mut result,
        b"IHDR",
        &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0],
    );
    // Valid zlib stream containing one uncompressed transparent RGBA pixel.
    chunk(
        &mut result,
        b"IDAT",
        &[0x78, 0x01, 1, 5, 0, 0xfa, 0xff, 0, 0, 0, 0, 0, 0, 5, 0, 1],
    );
    chunk(&mut result, b"IEND", &[]);
    result
}
fn owned_fixture(name: &str) -> Vec<u8> {
    let root = PathBuf::from(
        std::env::var("TASKLOOM_TEST_OCR_FIXTURES")
            .expect("explicit owned generated fixture directory"),
    );
    assert!(root.is_absolute());
    std::fs::read(root.join(format!("ocr-{name}.png"))).unwrap()
}
fn config() -> DockerOcrConfig {
    DockerOcrConfig {
        execution: DockerSandboxConfig {
            executable: std::env::current_exe().unwrap(),
            endpoint: if cfg!(windows) {
                "npipe:////./pipe/dockerDesktopLinuxEngine".into()
            } else {
                "unix:///var/run/docker.sock".into()
            },
            image: format!("sha256:{}", "a".repeat(64)),
            platform: "linux/amd64".into(),
            memory_mib: 512,
            cpu_millis: 1000,
            timeout_ms: 30000,
            max_output_bytes: 65536,
        },
        language: OcrLanguage::SimplifiedChineseEnglish,
        max_image_bytes: 4 * 1024 * 1024,
        max_pixels: 16 * 1024 * 1024,
        max_text_bytes: 32768,
    }
}
fn context(name: &str) -> ToolContext {
    ToolContext {
        run_id: "ocr-owned-fixture".into(),
        call_id: name.into(),
    }
}
fn crc(data: &[u8]) -> u32 {
    let mut v = !0u32;
    for b in data {
        v ^= u32::from(*b);
        for _ in 0..8 {
            v = (v >> 1) ^ (0xedb88320u32.wrapping_mul(v & 1));
        }
    }
    !v
}
#[test]
fn ocr_fixed_worker_contract_preflights_images_and_randomizes_only_container_name() {
    let english = tiny_png();
    let worker = DockerOcr::new(config()).unwrap();
    let first = worker
        .command_spec(english.as_slice(), &context("english"))
        .unwrap();
    let second = worker
        .command_spec(english.as_slice(), &context("english"))
        .unwrap();
    assert_ne!(first.container_name, second.container_name);
    assert_eq!(
        worker.identity(),
        DockerOcr::new(config()).unwrap().identity()
    );
    for flag in [
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--pids-limit=32",
        "--pull=never",
        "--security-opt=no-new-privileges",
        "--ipc=none",
        "--cgroupns=private",
        "--log-driver=none",
        "--entrypoint=/usr/bin/python3",
    ] {
        assert!(first.arguments.iter().any(|v| v == flag), "{flag}");
    }
    assert!(first.arguments.iter().all(|v| !v.contains("mount")
        && !v.contains("privileged")
        && !v.contains("/host")
        && !v.contains("base64")));
    assert!(first
        .arguments
        .iter()
        .any(|v| v == "/opt/taskloom/ocr-supervisor.py"));
    assert!(first.arguments.iter().any(|v| v == "--png-sha256"));
    let mut too_small = config();
    too_small.max_image_bytes = english.as_slice().len() - 1;
    assert!(DockerOcr::new(too_small)
        .unwrap()
        .command_spec(english.as_slice(), &context("large"))
        .is_err());
    let mut damaged = english.as_slice().to_vec();
    damaged[40] ^= 1;
    assert!(worker.command_spec(&damaged, &context("crc")).is_err());
    let mut bomb = english.as_slice().to_vec();
    bomb[16..20].copy_from_slice(&16000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&16000u32.to_be_bytes());
    let checksum = crc(&bomb[12..29]);
    bomb[29..33].copy_from_slice(&checksum.to_be_bytes());
    assert!(worker.command_spec(&bomb, &context("pixel-bomb")).is_err());
    assert!(worker
        .command_spec(
            &english.as_slice()[..english.as_slice().len() - 3],
            &context("truncated")
        )
        .is_err());
    assert!(worker.command_spec(b"not a PNG", &context("bad")).is_err());
    let mut unsafe_config = config();
    unsafe_config.execution.image = "debian:latest".into();
    assert!(DockerOcr::new(unsafe_config).is_err());
}
fn owned_config() -> DockerOcrConfig {
    let mut cfg = config();
    cfg.execution.executable =
        PathBuf::from(std::env::var("TASKLOOM_TEST_DOCKER").expect("explicit owned Docker CLI"));
    cfg.execution.endpoint =
        std::env::var("TASKLOOM_TEST_DOCKER_ENDPOINT").expect("explicit local engine");
    cfg.execution.image =
        std::env::var("TASKLOOM_TEST_OCR_IMAGE").expect("explicit pinned owned OCR image");
    cfg
}
fn docker(cfg: &DockerOcrConfig) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&cfg.execution.executable);
    command
        .args(["--host", &cfg.execution.endpoint])
        .env_clear()
        .env("LANG", "C");
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
    }
    command
}
#[tokio::test]
#[ignore = "requires explicitly owned local Linux engine and pinned OCR worker image"]
async fn owned_ocr_english_chinese_provenance_output_timeout_and_corruption() {
    let english = owned_fixture("english");
    let chinese = owned_fixture("chinese");
    let long = owned_fixture("long");
    let cfg = owned_config();
    let worker = DockerOcr::new(cfg.clone()).unwrap();
    for (bytes, name, expected) in [
        (english.as_slice(), "english", "TASKLOOM"),
        (chinese.as_slice(), "chinese", "任务测试"),
    ] {
        let outcome = worker.extract(bytes, &context(name)).await.unwrap();
        assert_eq!(outcome.exit_code, 0, "{outcome:?}");
        assert!(outcome.cleanup_confirmed, "{outcome:?}");
        let normalized = outcome
            .text
            .as_ref()
            .unwrap()
            .split_whitespace()
            .collect::<String>();
        assert!(normalized.contains(expected), "{normalized}");
        let provenance = outcome.provenance.as_ref().unwrap();
        assert_eq!(
            provenance.source_image_sha256,
            hex::encode(Sha256::digest(bytes))
        );
        assert_eq!(
            provenance.text_sha256,
            hex::encode(Sha256::digest(outcome.text.as_ref().unwrap().as_bytes()))
        );
        assert_eq!(provenance.traineddata_sha256.len(), 2);
        println!(
            "OCR_PROVENANCE {name} {}",
            serde_json::to_string(provenance).unwrap()
        );
        let document = outcome.into_document(name, name).unwrap();
        assert!(document.source.starts_with("ocr:taskloom-png-v1:"));
        assert_eq!(document.metadata["format"], "ocr-png-text-snapshot-v1");
        println!("{name}: {}", document.text.trim());
    }
    let mut bounded = cfg.clone();
    bounded.max_text_bytes = 8;
    let outcome = DockerOcr::new(bounded)
        .unwrap()
        .extract(english.as_slice(), &context("output-limit"))
        .await
        .unwrap();
    assert!(
        outcome.output_limited && outcome.text.is_none() && outcome.cleanup_confirmed,
        "{outcome:?}"
    );
    assert!(outcome.into_document("partial", "partial").is_err());
    let mut deadline = cfg.clone();
    deadline.execution.timeout_ms = 1;
    let outcome = DockerOcr::new(deadline)
        .unwrap()
        .extract(long.as_slice(), &context("timeout"))
        .await
        .unwrap();
    assert!(
        outcome.timed_out && outcome.text.is_none() && outcome.cleanup_confirmed,
        "{outcome:?}"
    );
    // Valid frame/CRC with deliberately invalid zlib image data: rejection must
    // occur in the isolated native decoder, never in an unisolated host tool.
    let mut bad = english.as_slice().to_vec();
    let mut at = 8;
    loop {
        let length = u32::from_be_bytes(bad[at..at + 4].try_into().unwrap()) as usize;
        if &bad[at + 4..at + 8] == b"IDAT" {
            bad[at + 8] ^= 0xff;
            let checksum = crc(&bad[at + 4..at + 8 + length]);
            bad[at + 8 + length..at + 12 + length].copy_from_slice(&checksum.to_be_bytes());
            break;
        }
        at += 12 + length;
    }
    let outcome = worker
        .extract(&bad, &context("damaged-decoder-input"))
        .await
        .unwrap();
    assert_ne!(outcome.exit_code, 0);
    assert!(outcome.cleanup_confirmed);
    assert!(outcome.text.is_none());
}
#[tokio::test]
#[ignore = "requires explicitly owned local Linux engine and pinned OCR worker image"]
async fn owned_ocr_incomplete_ingress_deadline_and_cancel_remove_only_named_container() {
    let english = owned_fixture("english");
    let long = owned_fixture("long");
    let cfg = owned_config();
    let worker = DockerOcr::new(cfg.clone()).unwrap();
    for case in [
        "empty-open",
        "partial-open",
        "truncated-closed",
        "pixel-bomb-closed",
    ] {
        let mut ingress = cfg.clone();
        ingress.execution.timeout_ms = 200;
        let mut spec = DockerOcr::new(ingress)
            .unwrap()
            .command_spec(english.as_slice(), &context("incomplete-ingress"))
            .unwrap();
        let mut payload = english.clone();
        if case == "pixel-bomb-closed" {
            payload[16..20].copy_from_slice(&16000u32.to_be_bytes());
            payload[20..24].copy_from_slice(&16000u32.to_be_bytes());
            let checksum = crc(&payload[12..29]);
            payload[29..33].copy_from_slice(&checksum.to_be_bytes());
            let at = spec
                .arguments
                .iter()
                .position(|v| v == "--png-sha256")
                .unwrap();
            // Deliberate low-level bypass of SDK preflight, testing the second
            // pure header/CRC/pixel boundary inside the audited PID 1 monitor.
            spec.arguments[at + 1] = hex::encode(Sha256::digest(&payload));
        }
        let dir = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.arguments)
            .env_clear()
            .env("DOCKER_CONFIG", dir.path())
            .env("HOME", dir.path())
            .env("LANG", "C")
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x08000000);
            if let Some(root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", root);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut opened = child.stdin.take();
        if case == "partial-open" || case == "truncated-closed" {
            opened
                .as_mut()
                .unwrap()
                .write_all(&payload[..16])
                .await
                .unwrap();
        } else if case == "pixel-bomb-closed" {
            opened.as_mut().unwrap().write_all(&payload).await.unwrap();
        }
        if case.ends_with("closed") {
            drop(opened.take());
        }
        let output = tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        drop(opened);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["timed_out"], case.ends_with("open"), "{case}");
        assert_eq!(
            response["exit_code"],
            if case.ends_with("open") { 124 } else { 126 },
            "{case}"
        );
        let inspect = docker(&cfg)
            .args(["inspect", "--type=container", &spec.container_name])
            .output()
            .await
            .unwrap();
        assert!(!inspect.status.success());
    }
    let context = context("actual-cancel");
    let spec = worker.command_spec(long.as_slice(), &context).unwrap();
    let label = spec
        .arguments
        .iter()
        .find_map(|v| v.strip_prefix("--label=org.taskloom.ocr.request="))
        .unwrap()
        .to_owned();
    let cloned = worker.clone();
    let task = tokio::spawn(async move { cloned.extract(long.as_slice(), &context).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let name = loop {
        let output = docker(&cfg)
            .args([
                "ps",
                "--filter",
                &format!("label=org.taskloom.ocr.request={label}"),
                "--format",
                "{{.Names}}",
            ])
            .output()
            .await
            .unwrap();
        let name = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        if !name.is_empty() {
            break name;
        }
        assert!(
            !task.is_finished(),
            "OCR completed before actual cancellation could be observed"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned OCR container did not appear"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    };
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        let output = docker(&cfg)
            .args(["inspect", "--type=container", &name])
            .output()
            .await
            .unwrap();
        if !output.status.success() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cancelled named container retained: {name}"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}
