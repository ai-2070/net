// SPDX-License-Identifier: MIT OR Apache-2.0
// Minimal ordered MCP startup fixture, compiled by wrap_startup.rs without
// external dependencies. The control socket also witnesses process lifetime.
use std::io::{BufRead, Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut control = std::net::TcpStream::connect(&args[1]).unwrap();
    control.write_all(b"ready\n").unwrap();
    if args[2] != "stall" {
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap();
            if line.contains("\"initialize\"") {
                println!(
                    r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"startup-fixture","version":"1"}}}}}}"#
                );
            } else if line.contains("\"tools/list\"") {
                println!(
                    r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"echo","description":"test tool","inputSchema":{{"type":"object"}}}}]}}}}"#
                );
                std::io::stdout().flush().unwrap();
                break;
            }
            std::io::stdout().flush().unwrap();
        }
    }
    let mut stop = [0];
    let _ = control.read(&mut stop);
}
