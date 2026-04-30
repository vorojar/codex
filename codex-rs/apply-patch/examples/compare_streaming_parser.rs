use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;

use codex_apply_patch::Hunk;
use codex_apply_patch::StreamingPatchParser;
use codex_apply_patch::parse_patch;
use serde_json::Value;

#[derive(Debug)]
enum ParseOutcome {
    Ok(Vec<Hunk>),
    Err(String),
}

fn normal_parse(patch: &str) -> ParseOutcome {
    parse_patch(patch)
        .map(|args| ParseOutcome::Ok(args.hunks))
        .unwrap_or_else(|err| ParseOutcome::Err(err.to_string()))
}

fn streaming_parse(patch: &str) -> ParseOutcome {
    let mut parser = StreamingPatchParser::default();
    parser
        .push_delta(patch)
        .and_then(|_| parser.finish())
        .map(ParseOutcome::Ok)
        .unwrap_or_else(|err| ParseOutcome::Err(err.to_string()))
}

fn mismatch_kind(normal: &ParseOutcome, streaming: &ParseOutcome) -> Option<&'static str> {
    match (normal, streaming) {
        (ParseOutcome::Ok(normal), ParseOutcome::Ok(streaming)) if normal == streaming => None,
        (ParseOutcome::Err(normal), ParseOutcome::Err(streaming)) if normal == streaming => None,
        (ParseOutcome::Ok(_), ParseOutcome::Err(_)) => Some("Old OK, new error"),
        (ParseOutcome::Err(_), ParseOutcome::Ok(_)) => Some("Old error, new OK"),
        (ParseOutcome::Ok(_), ParseOutcome::Ok(_)) => Some("Both OK, different parse"),
        (ParseOutcome::Err(_), ParseOutcome::Err(_)) => Some("Both error, different error"),
    }
}

fn decode_patch(raw: &str) -> String {
    if raw.contains("\\n") {
        let wrapped = format!("\"{raw}\"");
        if let Ok(decoded) = serde_json::from_str::<String>(&wrapped) {
            return decoded;
        }
    }
    if raw.starts_with('"')
        && let Ok(decoded) = serde_json::from_str::<String>(raw)
    {
        return decoded;
    }
    raw.to_string()
}

fn summarize_patch(patch: &str) -> String {
    let mut out = String::new();
    for (i, line) in patch.lines().take(80).enumerate() {
        out.push_str(&format!("{:>4}: {line}\n", i + 1));
    }
    if patch.lines().count() > 80 {
        out.push_str("... truncated after 80 lines ...\n");
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: compare_streaming_parser <jsonl> [start_line]");
    let start_line = args
        .next()
        .map(|line| line.parse::<usize>().expect("start_line must be a number"))
        .unwrap_or(1);
    let file = File::open(&path).expect("failed to open jsonl");
    let reader = BufReader::new(file);

    let mut total = 0usize;
    let mut both_error = 0usize;
    for line in reader.lines() {
        total += 1;
        if total < start_line {
            continue;
        }
        let line = line.expect("failed to read line");
        let value: Value = serde_json::from_str(&line).expect("invalid jsonl row");
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("<missing request_id>");
        let raw = value
            .get("patch_payload_escaped")
            .and_then(Value::as_str)
            .unwrap_or("");
        let patch = decode_patch(raw);

        let normal = normal_parse(&patch);
        let streaming = streaming_parse(&patch);
        if let Some(kind) = mismatch_kind(&normal, &streaming) {
            println!("first mismatch at row {total}");
            println!("result: {kind}");
            println!("request_id: {request_id}");
            println!("raw payload bytes: {}", raw.len());
            println!("decoded patch bytes: {}", patch.len());
            println!("\nnormal parser:\n{normal:#?}");
            println!("\nstreaming parser:\n{streaming:#?}");
            println!("\npatch preview:\n{}", summarize_patch(&patch));
            return;
        }
        if matches!(normal, ParseOutcome::Err(_)) {
            both_error += 1;
        }
        if total % 10_000 == 0 {
            eprintln!("checked {total} rows ({both_error} rows where both parsers errored)");
        }
    }

    println!("checked all {total} rows; no parser result mismatches");
    if both_error > 0 {
        println!("{both_error} rows produced an error in both parsers");
    }
}
