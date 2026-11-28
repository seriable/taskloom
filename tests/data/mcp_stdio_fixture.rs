// Dependency-free owned subprocess fixture compiled by agent_mcp process tests.
use std::io::{BufRead, Write};
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let input = std::io::stdin();
    for line in input.lock().lines() {
        let line = line.unwrap();
        let Some(at) = line.find("\"id\":") else {
            continue;
        };
        let id: String = line[at + 5..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let result = if line.contains("\"initialize\"") {
            r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{},"resources":{}}}"#
                .to_owned()
        } else if line.contains("\"tools/list\"") {
            r#"{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}"#.to_owned()
        } else if line.contains("\"resources/list\"") {
            r#"{"resources":[{"uri":"fixture://one","name":"one"}]}"#.to_owned()
        } else if line.contains("\"resources/read\"") {
            r#"{"contents":[{"uri":"fixture://one","text":"resource evidence"}]}"#.to_owned()
        } else {
            if mode == "effectlost" {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(std::env::args().nth(2).unwrap())
                    .unwrap();
                writeln!(file, "effect").unwrap();
                file.sync_all().unwrap();
                return;
            }
            if mode == "slow" {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            if mode == "oversize" {
                println!("{}", "x".repeat(8192));
                std::io::stdout().flush().unwrap();
                continue;
            }
            if mode == "eof" {
                return;
            }
            if mode == "badid" {
                println!(r#"{{"jsonrpc":"2.0","id":999999,"result":{{}}}}"#);
                std::io::stdout().flush().unwrap();
                continue;
            }
            if mode == "notifications" {
                for _ in 0..1000 {
                    println!(r#"{{"jsonrpc":"2.0","method":"notifications/progress"}}"#);
                }
                std::io::stdout().flush().unwrap();
                continue;
            }
            if mode == "environment" {
                let inherited = std::env::var_os("TASKLOOM_MCP_FORBIDDEN_SECRET").is_some();
                let allowed =
                    std::env::var("TASKLOOM_MCP_ALLOWED_TEST").unwrap_or_default() == "allowed";
                format!(
                    r#"{{"content":[],"structuredContent":{{"inherited":{inherited},"allowed":{allowed}}}}}"#
                )
            } else {
                r#"{"content":[{"type":"text","text":"done"}]}"#.to_owned()
            }
        };
        println!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#);
        std::io::stdout().flush().unwrap();
    }
}
