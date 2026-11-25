//! End-to-end integration tests for all five `pipe_wrench_cli` subcommands:
//! `scan`, `send`, `listen` (plaintext TCP and TLS 1.2/1.3 with mTLS), `report`, and `convert`.

use std::path::{Path, PathBuf};

use clap::Parser;
use hl7_net::{run_listener_server, ListenerCommand, ListenerEvent};
use pipe_wrench_cli::convert::execute_convert_command;
use pipe_wrench_cli::filter::execute_filter_command;
use pipe_wrench_cli::listen::build_listener_profile;
use pipe_wrench_cli::report::execute_report_command;
use pipe_wrench_cli::scan::execute_scan_command;
use pipe_wrench_cli::search::execute_search_command;
use pipe_wrench_cli::send::execute_send_command;
use pipe_wrench_cli::{Cli, Commands};
use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};
use tokio::sync::mpsc;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn cli_scan_subcommand_end_to_end() {
    let scan_dir = fixtures_dir().join("scan");
    let temp = tempfile::tempdir().unwrap();
    let out_csv = temp.path().join("scan_out.csv");

    let cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "scan",
        scan_dir.to_str().unwrap(),
        "--ext",
        "txt,dat,hl7",
        "--group-by",
        "type,app",
        "--check-batches",
        "--check-duplicate-control-ids",
        "--format",
        "csv",
        "--out",
        out_csv.to_str().unwrap(),
        "--threads",
        "2",
    ])
    .unwrap();

    let Commands::Scan(args) = cli.command else {
        panic!("expected Scan command");
    };
    let (bytes, code) = execute_scan_command(&args).unwrap();
    assert_eq!(code, 0);
    assert!(out_csv.is_file());
    let csv_text = String::from_utf8(bytes).unwrap();
    assert!(csv_text.contains("a.txt"));
    assert!(csv_text.contains("mismatch.txt"));
}

#[tokio::test]
async fn cli_send_and_listen_plaintext_tcp() {
    let temp = tempfile::tempdir().unwrap();
    let recv_log = temp.path().join("received_tcp.hl7");
    let history_csv = temp.path().join("send_history_tcp.csv");
    let input_file = fixtures_dir().join("batch_fhs_bhs.hl7");

    // Parse `pipe_wrench_cli listen` args
    let listen_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "listen",
        "--bind",
        "127.0.0.1",
        "--port",
        "19891",
        "--ack",
        "generated",
        "--ack-code",
        "AA",
        "--out",
        recv_log.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Listen(listen_args) = listen_cli.command else {
        panic!("expected Listen command");
    };
    let listener_profile = build_listener_profile(&listen_args).unwrap();

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let server_handle = tokio::spawn(run_listener_server(listener_profile, ev_tx, cmd_rx));

    // Wait for listener to start
    let started = ev_rx.recv().await.unwrap();
    assert!(matches!(started, ListenerEvent::Started { port: 19891 }));

    // Parse and execute `pipe_wrench_cli send` for records 3..5 (the 3 MSH records inside batch_fhs_bhs.hl7)
    let send_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "send",
        input_file.to_str().unwrap(),
        "--host",
        "127.0.0.1",
        "--port",
        "19891",
        "--scope",
        "3..5",
        "--connect-timeout",
        "5",
        "--delay-connect",
        "0.01",
        "--delay-between",
        "0.01",
        "--ack-timeout",
        "5",
        "--ack-mode",
        "hl7",
        "--nak-action",
        "resend",
        "--max-tries",
        "2",
        "--max-tries-action",
        "stop",
        "--rewrite-control-id",
        "--refresh-timestamp",
        "--history-out",
        history_csv.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Send(send_args) = send_cli.command else {
        panic!("expected Send command");
    };

    let code = execute_send_command(&send_args).await.unwrap();
    assert_eq!(code, 0);

    let _ = cmd_tx.send(ListenerCommand::Stop);
    let _ = server_handle.await.unwrap();

    // Verify history CSV and received log file
    let history_text = std::fs::read_to_string(&history_csv).unwrap();
    assert_eq!(history_text.lines().count(), 4); // header + 3 sent records
    assert!(history_text.contains(",ACK,"));

    let received_text = std::fs::read_to_string(&recv_log).unwrap();
    assert_eq!(received_text.matches("MSH|").count(), 3);
}

#[tokio::test]
async fn cli_send_and_listen_tls_and_mtls() {
    let temp = tempfile::tempdir().unwrap();

    // 1. Generate ephemeral CA certificate
    let mut ca_params = CertificateParams::new(vec!["PipeWrench Test CA".to_string()]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // 2. Generate Server certificate signed by CA for localhost / 127.0.0.1
    let mut server_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    server_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    let server_key = KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    // 3. Generate Client certificate signed by CA for mTLS
    let mut client_params = CertificateParams::new(vec!["pipe_wrench_client".to_string()]).unwrap();
    client_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    let client_key = KeyPair::generate().unwrap();
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    let ca_pem_path = temp.path().join("ca.pem");
    let server_cert_path = temp.path().join("server.crt");
    let server_key_path = temp.path().join("server.key");
    let client_cert_path = temp.path().join("client.crt");
    let client_key_path = temp.path().join("client.key");
    let recv_log = temp.path().join("received_mtls.hl7");

    std::fs::write(&ca_pem_path, ca_cert.pem()).unwrap();
    std::fs::write(&server_cert_path, server_cert.pem()).unwrap();
    std::fs::write(&server_key_path, server_key.serialize_pem()).unwrap();
    std::fs::write(&client_cert_path, client_cert.pem()).unwrap();
    std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();

    // Configure mTLS Listener via CLI flags
    let listen_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "listen",
        "--bind",
        "127.0.0.1",
        "--port",
        "19892",
        "--ack",
        "generated",
        "--ack-code",
        "AA",
        "--out",
        recv_log.to_str().unwrap(),
        "--tls",
        "--tls-cert",
        server_cert_path.to_str().unwrap(),
        "--tls-key",
        server_key_path.to_str().unwrap(),
        "--tls-client-ca",
        ca_pem_path.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Listen(listen_args) = listen_cli.command else {
        panic!("expected Listen command");
    };
    let listener_profile = build_listener_profile(&listen_args).unwrap();

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let server_handle = tokio::spawn(run_listener_server(listener_profile, ev_tx, cmd_rx));

    let started = ev_rx.recv().await.unwrap();
    assert!(matches!(started, ListenerEvent::Started { port: 19892 }));

    // Configure mTLS Sender via CLI flags
    let input_file = fixtures_dir().join("adt_a01_basic.hl7");
    let send_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "send",
        input_file.to_str().unwrap(),
        "--host",
        "127.0.0.1",
        "--port",
        "19892",
        "--tls",
        "--tls-sni",
        "localhost",
        "--tls-ca",
        ca_pem_path.to_str().unwrap(),
        "--tls-client-cert",
        client_cert_path.to_str().unwrap(),
        "--tls-client-key",
        client_key_path.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Send(send_args) = send_cli.command else {
        panic!("expected Send command");
    };

    let code = execute_send_command(&send_args).await.unwrap();
    assert_eq!(code, 0);

    let _ = cmd_tx.send(ListenerCommand::Stop);
    let _ = server_handle.await.unwrap();

    let received_text = std::fs::read_to_string(&recv_log).unwrap();
    assert_eq!(received_text.matches("MSH|").count(), 1);
}

#[test]
fn cli_report_subcommand_csv_and_json() {
    let temp = tempfile::tempdir().unwrap();
    let csv_out = temp.path().join("report.csv");
    let json_out = temp.path().join("report.json");
    let input_file = fixtures_dir().join("batch_fhs_bhs.hl7");

    // 1. CSV report with typed fields and sorting
    let csv_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "report",
        input_file.to_str().unwrap(),
        "--fields",
        "MSH-10:string,MSH-7:date,PID-3.1:string",
        "--grain",
        "record",
        "--sort-by",
        "MSH-10:desc",
        "--format",
        "csv",
        "--encoding",
        "utf8",
        "--out",
        csv_out.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Report(csv_args) = csv_cli.command else {
        panic!("expected Report command");
    };
    let csv_bytes = execute_report_command(&csv_args).unwrap();
    let csv_str = String::from_utf8(csv_bytes).unwrap();
    assert!(csv_str.starts_with("MSH-10,MSH-7,PID-3.1\r\n"));
    assert!(csv_out.is_file());

    // 2. JSON report with per-segment grain
    let escapes_file = fixtures_dir().join("escapes.hl7");
    let json_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "report",
        escapes_file.to_str().unwrap(),
        "--fields",
        "MSH-10,OBX-1:number,OBX-5",
        "--grain",
        "segment:OBX",
        "--sort-by",
        "OBX-1:asc",
        "--format",
        "json",
        "--out",
        json_out.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Report(json_args) = json_cli.command else {
        panic!("expected Report command");
    };
    let json_bytes = execute_report_command(&json_args).unwrap();
    let json_str = String::from_utf8(json_bytes).unwrap();
    assert!(json_str.contains("\"OBX-5\""));
    assert!(json_out.is_file());
}

#[test]
fn cli_convert_subcommand_terminators_envelope_sort_and_delims() {
    let temp = tempfile::tempdir().unwrap();
    let converted_out = temp.path().join("converted.hl7");
    let stripped_out = temp.path().join("stripped.hl7");
    let input_file = fixtures_dir().join("batch_fhs_bhs.hl7");

    // 1. Convert terminators to CRLF, wrap in MLLP envelope, sort by MSH-7, and change field delimiter to ':'
    let conv_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "convert",
        input_file.to_str().unwrap(),
        "--out",
        converted_out.to_str().unwrap(),
        "--term",
        "crlf",
        "--envelope",
        "--sort-msh7",
        "--delims",
        ":^~\\&",
    ])
    .unwrap();
    let Commands::Convert(conv_args) = conv_cli.command else {
        panic!("expected Convert command");
    };
    let count = execute_convert_command(&conv_args).unwrap();
    assert_eq!(count, 7);

    let raw = std::fs::read(&converted_out).unwrap();
    assert_eq!(raw.first().copied(), Some(0x0B));
    assert!(raw.ends_with(&[0x1C, 0x0D]));
    assert!(String::from_utf8_lossy(&raw).contains("MSH:^~\\&:"));

    // 2. Strip MLLP envelope and convert back to standard CR and standard delimiters
    let strip_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "convert",
        converted_out.to_str().unwrap(),
        "--out",
        stripped_out.to_str().unwrap(),
        "--term",
        "cr",
        "--strip-envelope",
        "--delims",
        "|^~\\&",
    ])
    .unwrap();
    let Commands::Convert(strip_args) = strip_cli.command else {
        panic!("expected Convert command");
    };
    let count2 = execute_convert_command(&strip_args).unwrap();
    assert_eq!(count2, 7);

    let raw2 = std::fs::read(&stripped_out).unwrap();
    assert!(!raw2.contains(&0x0B));
    assert!(!raw2.contains(&0x1C));
    assert!(String::from_utf8_lossy(&raw2).contains("MSH|^~\\&|"));
}

#[test]
fn cli_filter_and_filter_field_flags_across_report_convert_and_scan() {
    let temp = tempfile::tempdir().unwrap();
    let filtered_csv = temp.path().join("filtered_report.csv");
    let filtered_hl7 = temp.path().join("filtered_convert.hl7");
    let input_file = fixtures_dir().join("batch_fhs_bhs.hl7");

    // 1. Report with --filter-field MSH-10=BMSG2
    let rep_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "report",
        input_file.to_str().unwrap(),
        "--fields",
        "MSH-10,PID-5.1",
        "--filter-field",
        "MSH-10=BMSG2",
        "--format",
        "csv",
        "--out",
        filtered_csv.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Report(rep_args) = rep_cli.command else {
        panic!("expected Report command");
    };
    let rep_bytes = execute_report_command(&rep_args).unwrap();
    let rep_text = String::from_utf8(rep_bytes).unwrap();
    assert_eq!(rep_text.lines().count(), 2); // header + 1 matching row
    assert!(rep_text.contains("BMSG2"));

    // 2. Convert with --filter-field "PID-5.1 contains PATIENT"
    let conv_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "convert",
        input_file.to_str().unwrap(),
        "--filter-field",
        "MSH-9.1=ADT",
        "--out",
        filtered_hl7.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Convert(conv_args) = conv_cli.command else {
        panic!("expected Convert command");
    };
    let count = execute_convert_command(&conv_args).unwrap();
    assert_eq!(count, 3); // 3 MSH records matched, FHS/BHS/BTS/FTS filtered out

    // 3. Scan with --filter-field MSH-9.1=ADT
    let scan_dir = fixtures_dir().join("scan");
    let scan_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "scan",
        scan_dir.to_str().unwrap(),
        "--filter-field",
        "MSH-9.1=ADT",
        "--format",
        "json",
    ])
    .unwrap();
    let Commands::Scan(scan_args) = scan_cli.command else {
        panic!("expected Scan command");
    };
    let (scan_bytes, code) = execute_scan_command(&scan_args).unwrap();
    assert_eq!(code, 0);
    let scan_json = String::from_utf8(scan_bytes).unwrap();
    assert!(scan_json.contains("\"msh_count\":"));
}

#[test]
fn cli_filter_and_search_subcommands_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let sample_path = temp.path().join("lab_batch.hl7");
    let filtered_out = temp.path().join("high_glucose.hl7");

    let sample_hl7 = concat!(
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20260115120000||ORU^R01|MSG101|P|2.5.1\r",
        "PID|1||PAT101^^^HOSP^MR||DOE^JOHN\r",
        "OBX|1|NM|GLU||95|mg/dL\r",
        "MSH|^~\\&|LAB|FAC|EMR|FAC|20261120120000||ORU^R01|MSG102|P|2.5.1\r",
        "PID|1||PAT102^^^HOSP^MR||SMITH^ALICE\r",
        "OBX|1|NM|GLU||240|mg/dL\r",
        "NTE|1||Alert GLU critical\r",
    );
    std::fs::write(&sample_path, sample_hl7).unwrap();

    // 1. `pipe_wrench_cli filter` with numeric field comparison `OBX-5>200` and `--out`
    let f_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "filter",
        sample_path.to_str().unwrap(),
        "--filter-field",
        "OBX-5>200",
        "--out",
        filtered_out.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Filter(f_args) = f_cli.command else {
        panic!("expected Filter command");
    };
    let (f_bytes, f_code) = execute_filter_command(&f_args).unwrap();
    assert_eq!(f_code, 0);
    assert!(filtered_out.is_file());
    let f_text = String::from_utf8(f_bytes).unwrap();
    assert!(f_text.contains("MSG102"));
    assert!(!f_text.contains("MSG101"));

    // 2. `pipe_wrench_cli filter` with `--regex`, `--invert-match` (-v), and `--count`
    let f_inv_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "filter",
        sample_path.to_str().unwrap(),
        "--filter",
        "SMITH\\^ALICE",
        "--regex",
        "--invert-match",
        "--count",
    ])
    .unwrap();
    let Commands::Filter(f_inv_args) = f_inv_cli.command else {
        panic!("expected Filter command");
    };
    let (count_bytes, count_code) = execute_filter_command(&f_inv_args).unwrap();
    assert_eq!(count_code, 0);
    assert_eq!(String::from_utf8(count_bytes).unwrap().trim(), "1");

    // 3. `pipe_wrench_cli filter` returning exit code 2 when 0 records match
    let f_none_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "filter",
        sample_path.to_str().unwrap(),
        "--filter-field",
        "OBX-5>9999",
    ])
    .unwrap();
    let Commands::Filter(f_none_args) = f_none_cli.command else {
        panic!("expected Filter command");
    };
    let (_, none_code) = execute_filter_command(&f_none_args).unwrap();
    assert_eq!(none_code, 2);

    // 4. `pipe_wrench_cli search` with `--query`, `--with`, `--summary`, `--print-segment`, and `--format table|json|csv`
    let s_table_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "search",
        sample_path.to_str().unwrap(),
        "--query",
        "GLU",
        "--with",
        "MSH-10,MSH-9,PID-3.1",
        "--summary",
        "--print-segment",
        "--format",
        "table",
    ])
    .unwrap();
    let Commands::Search(s_table_args) = s_table_cli.command else {
        panic!("expected Search command");
    };
    let (table_bytes, s_code) = execute_search_command(&s_table_args).unwrap();
    assert_eq!(s_code, 0);
    let table_str = String::from_utf8(table_bytes).unwrap();
    assert!(table_str.contains("Matches found: 3"));
    assert!(table_str.contains("Hit Summary by Field Path:"));
    assert!(table_str.contains("OBX-3"));
    assert!(table_str.contains("NTE-3"));
    assert!(table_str.contains("Segment: OBX|1|NM|GLU||240|mg/dL"));

    // 5. `pipe_wrench_cli search` with `--field OBX-5>200`, `--first`, and `--format json`
    let s_json_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "search",
        sample_path.to_str().unwrap(),
        "--field",
        "OBX-5>200",
        "--first",
        "--format",
        "json",
    ])
    .unwrap();
    let Commands::Search(s_json_args) = s_json_cli.command else {
        panic!("expected Search command");
    };
    let (json_bytes, j_code) = execute_search_command(&s_json_args).unwrap();
    assert_eq!(j_code, 0);
    let json_str = String::from_utf8(json_bytes).unwrap();
    assert!(json_str.contains("\"total_matches\": 1"));
    assert!(json_str.contains("\"value\": \"240\""));

    // 6. `pipe_wrench_cli search` with `--format csv`
    let s_csv_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "search",
        sample_path.to_str().unwrap(),
        "--query",
        "SMITH",
        "--field",
        "PID-5.1",
        "--format",
        "csv",
    ])
    .unwrap();
    let Commands::Search(s_csv_args) = s_csv_cli.command else {
        panic!("expected Search command");
    };
    let (csv_bytes, c_code) = execute_search_command(&s_csv_args).unwrap();
    assert_eq!(c_code, 0);
    let csv_str = String::from_utf8(csv_bytes).unwrap();
    assert!(csv_str.contains("PID-5.1,SMITH,MSG102,ORU^R01,PAT102"));
}

#[test]
fn cli_replace_subcommand_granular_field_regex_dry_run_and_out() {
    use pipe_wrench_cli::replace::execute_replace_command;

    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("replace_input.hl7");
    let out_path = temp.path().join("replace_output.hl7");

    let sample_hl7 = concat!(
        "MSH|^~\\&|SND|OLD|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||OLD&S2&S3&OLD_123^C2^C3^AUTH||DOE^JOHN\r",
        "OBX|1|ST|NOTE||OLD\r",
    );
    std::fs::write(&input_path, sample_hl7).unwrap();

    // 1. Dry-run targeting strictly PID-3.1.4 with regex capture group ($1)
    let dry_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "replace",
        input_path.to_str().unwrap(),
        "--find",
        "OLD_(\\d+)",
        "--replace",
        "NEW_$1",
        "--field",
        "PID-3.1.4",
        "--regex",
        "--dry-run",
    ])
    .unwrap();
    let Commands::Replace(dry_args) = dry_cli.command else {
        panic!("expected Replace command");
    };
    let (dry_bytes, dry_summary, dry_code) = execute_replace_command(&dry_args).unwrap();
    assert_eq!(dry_code, 0);
    assert_eq!(dry_summary.occurrences, 1);
    assert_eq!(dry_summary.records_modified, 1);
    let dry_text = String::from_utf8(dry_bytes).unwrap();
    assert!(dry_text.contains("Dry run: would replace 1 occurrence(s) across 1 record(s)"));
    assert!(!out_path.exists());

    // 2. Actual write to --out: verify only PID-3.1.4 changed while MSH-4, PID-3.1.1, and OBX-5 remain "OLD"
    let run_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "replace",
        input_path.to_str().unwrap(),
        "--find",
        "OLD_(\\d+)",
        "--replace",
        "NEW_$1",
        "--field",
        "PID-3.1.4",
        "--regex",
        "--out",
        out_path.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Replace(run_args) = run_cli.command else {
        panic!("expected Replace command");
    };
    let (_, run_summary, run_code) = execute_replace_command(&run_args).unwrap();
    assert_eq!(run_code, 0);
    assert_eq!(run_summary.occurrences, 1);
    assert!(out_path.is_file());

    let written = std::fs::read_to_string(&out_path).unwrap();
    assert!(written.contains("MSH|^~\\&|SND|OLD|"));
    assert!(written.contains("PID|1||OLD&S2&S3&NEW_123^C2^C3^AUTH||DOE^JOHN"));
    assert!(written.contains("OBX|1|ST|NOTE||OLD"));
}

#[test]
fn cli_filter_search_and_replace_empty_and_null_fields() {
    use pipe_wrench_cli::filter::execute_filter_command;
    use pipe_wrench_cli::replace::execute_replace_command;
    use pipe_wrench_cli::search::execute_search_command;

    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("empty_fields.hl7");
    let out_path = temp.path().join("empty_replaced.hl7");

    let sample_hl7 = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||12345||DOE^JOHN\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120100||ADT^A08|MSG2|P|2.5.1\r",
        "PID|1||||SMITH\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120200||ACK|MSG3|P|2.5.1\r",
        "MSA|AA|MSG1\r",
    );
    std::fs::write(&input_path, sample_hl7).unwrap();

    // 1. `pipe_wrench_cli filter --query 'PID-3=""' --count` matches MSG2 (empty PID-3) + MSG3 (missing PID) = 2
    for q in ["PID-3=\"\"", "PID-3="] {
        let cli = Cli::try_parse_from([
            "pipe_wrench_cli",
            "filter",
            input_path.to_str().unwrap(),
            "--query",
            q,
            "--count",
        ])
        .unwrap();
        let Commands::Filter(args) = cli.command else {
            panic!("expected Filter command");
        };
        let (bytes, code) = execute_filter_command(&args).unwrap();
        assert_eq!(code, 0);
        assert_eq!(String::from_utf8(bytes).unwrap().trim(), "2");
    }

    // 2. `pipe_wrench_cli filter --query 'PID-3!=""' --count` matches only MSG1 = 1
    let pop_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "filter",
        input_path.to_str().unwrap(),
        "--query",
        "PID-3!=\"\"",
        "--count",
    ])
    .unwrap();
    let Commands::Filter(pop_args) = pop_cli.command else {
        panic!("expected Filter command");
    };
    let (pop_bytes, pop_code) = execute_filter_command(&pop_args).unwrap();
    assert_eq!(pop_code, 0);
    assert_eq!(String::from_utf8(pop_bytes).unwrap().trim(), "1");

    // 3. `pipe_wrench_cli search --query 'PID-3=""' --format json` finds 2 matches
    let search_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "search",
        input_path.to_str().unwrap(),
        "--query",
        "PID-3=\"\"",
        "--format",
        "json",
    ])
    .unwrap();
    let Commands::Search(search_args) = search_cli.command else {
        panic!("expected Search command");
    };
    let (s_bytes, s_code) = execute_search_command(&search_args).unwrap();
    assert_eq!(s_code, 0);
    assert!(String::from_utf8(s_bytes)
        .unwrap()
        .contains("\"total_matches\": 2"));

    // 4. `pipe_wrench_cli replace --field PID-3.4 --find "" --replace HOSP --whole-value --out ...`
    let rep_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "replace",
        input_path.to_str().unwrap(),
        "--field",
        "PID-3.4",
        "--find",
        "",
        "--replace",
        "HOSP",
        "--whole-value",
        "--out",
        out_path.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Replace(rep_args) = rep_cli.command else {
        panic!("expected Replace command");
    };
    let (_, rep_summary, rep_code) = execute_replace_command(&rep_args).unwrap();
    assert_eq!(rep_code, 0);
    assert_eq!(rep_summary.occurrences, 2);
    let written = std::fs::read_to_string(&out_path).unwrap();
    assert!(written.contains("PID|1||12345^^^HOSP||DOE^JOHN"));
    assert!(written.contains("PID|1||^^^HOSP||SMITH"));
}

#[test]
fn cli_wildcard_repetition_operators_and_rol_gt1_report() {
    let temp = tempfile::tempdir().unwrap();
    let input_path = temp.path().join("wildcard_rol_gt1.hl7");
    let raw = concat!(
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||AAA^^^HOSP^MR~123^^^CLINIC^PI||DOE^JOHN\r",
        "ROL|1|UP|AT|9999^HOUSE^GREGORY\r",
        "GT1|1||DOE^JOHN||||||||||||||ACME^CORP|100 MAIN ST^^BOSTON^MA^02101\r",
        "OBX|1|NM|GLU||95|mg/dL\r",
        "MSH|^~\\&|SND|FAC|RCV|FAC|20260417120000||ORU^R01|MSG2|P|2.5.1\r",
        "PID|1||999^^^HOSP^MR||SMITH^JANE\r",
        "OBX|1|NM|GLU||240|mg/dL\r",
    );
    std::fs::write(&input_path, raw).unwrap();

    let count_for = |flag: &str, expr: &str| -> usize {
        let cli = Cli::try_parse_from([
            "pipe_wrench_cli",
            "filter",
            input_path.to_str().unwrap(),
            flag,
            expr,
            "--count",
        ])
        .unwrap();
        let Commands::Filter(args) = cli.command else {
            panic!("expected Filter command");
        };
        let (bytes, code) = execute_filter_command(&args).unwrap();
        assert_eq!(code, 0, "expected match for {flag} {expr}");
        String::from_utf8(bytes).unwrap().trim().parse().unwrap()
    };

    // Wildcard repetition, contains (~=), regex (=~), exists, and relational operators
    assert_eq!(count_for("--filter", "PID-3(*).1=123"), 1);
    assert_eq!(count_for("--filter-field", "PID-3(*).1 ~= 23"), 1);
    assert_eq!(count_for("--filter", "PID-5.1 =~ ^SM.*"), 1);
    assert_eq!(count_for("--filter-field", "ROL-4 exists"), 1);
    assert_eq!(count_for("--filter", "PID-3(*).1 exists"), 2);
    assert_eq!(count_for("--filter", "OBX-5 >= 200"), 1);
    assert_eq!(count_for("--filter", "MSH-7 <= 20260115120000"), 1);
    assert_eq!(count_for("--filter", "PID-5.1 != SMITH"), 1);

    // Search with wildcard repetition predicate
    let search_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "search",
        input_path.to_str().unwrap(),
        "--field",
        "PID-3(*).1=123",
        "--format",
        "json",
    ])
    .unwrap();
    let Commands::Search(search_args) = search_cli.command else {
        panic!("expected Search command");
    };
    let (s_bytes, s_code) = execute_search_command(&search_args).unwrap();
    assert_eq!(s_code, 0);
    let s_json = String::from_utf8(s_bytes).unwrap();
    assert!(s_json.contains("\"total_matches\": 1"));
    assert!(s_json.contains("\"value\": \"123\""));

    // Report extracting ROL-4.2, GT1-17.1, GT1-18.3 filtered by `ROL-4 exists`
    let rep_cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "report",
        input_path.to_str().unwrap(),
        "--fields",
        "ROL-4.2,GT1-17.1,GT1-18.3",
        "--filter",
        "ROL-4 exists",
        "--format",
        "csv",
    ])
    .unwrap();
    let Commands::Report(rep_args) = rep_cli.command else {
        panic!("expected Report command");
    };
    let rep_bytes = execute_report_command(&rep_args).unwrap();
    let rep_csv = String::from_utf8(rep_bytes).unwrap();
    assert_eq!(rep_csv.lines().count(), 2);
    assert!(rep_csv.contains("HOUSE,ACME,BOSTON"));
}

#[test]
fn cli_scrub_subcommand_deterministic_seed_and_directory_traversal() {
    use pipe_wrench_cli::scrub::execute_scrub_command;

    let dir = tempfile::tempdir().unwrap();
    let in_dir = dir.path().join("incoming");
    std::fs::create_dir_all(&in_dir).unwrap();
    let adt_path = in_dir.join("01_adt.hl7");
    let oru_path = in_dir.join("02_oru.hl7");

    std::fs::write(
        &adt_path,
        concat!(
            "MSH|^~\\&|EHR|MAIN|LAB|MAIN|20261123080000||ADT^A01|CLI1|P|2.5.1\r",
            "PID|1||MRN551234^^^HOSP^MR||KOWALSKI^MAREK^T||19791103|M|||210 OAK ST^^CHICAGO^IL^60601||(312)555-1940|||M||ACC112233|987-65-4321\r",
            "PV1|1|O|CLINIC^1^1||||||||||||||||ENC889900\r",
            "NTE|1|L|Follow-up note for MAREK KOWALSKI (MRN551234): stable hypertension\r",
        ),
    )
    .unwrap();
    std::fs::write(
        &oru_path,
        concat!(
            "MSH|^~\\&|LAB|MAIN|EHR|MAIN|20261123090000||ORU^R01|CLI2|P|2.5.1\r",
            "PID|1||MRN551234^^^HOSP^MR||KOWALSKI^MAREK^T||19791103|M|||210 OAK ST^^CHICAGO^IL^60601||(312)555-1940|||M||ACC112233|987-65-4321\r",
            "PV1|1|O|CLINIC^1^1||||||||||||||||ENC889900\r",
            "OBX|1|TX|NOTE||Lab panel for KOWALSKI, MAREK (ENC889900): potassium within normal limits\r",
        ),
    )
    .unwrap();

    let out_a1 = dir.path().join("out_a1.hl7");
    let out_a2 = dir.path().join("out_a2.hl7");
    let out_b = dir.path().join("out_b.hl7");

    let run_scrub = |out: &std::path::Path, seed: &str| {
        let cli = Cli::try_parse_from([
            "pipe_wrench_cli",
            "scrub",
            in_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--seed",
            seed,
        ])
        .unwrap();
        let Commands::Scrub(args) = cli.command else {
            panic!("expected Scrub command");
        };
        execute_scrub_command(&args).unwrap()
    };

    let (count_a1, seed_a1) = run_scrub(&out_a1, "seed-alpha");
    let (count_a2, seed_a2) = run_scrub(&out_a2, "seed-alpha");
    let (count_b, seed_b) = run_scrub(&out_b, "seed-beta");

    assert_eq!(count_a1, 2);
    assert_eq!(count_a2, 2);
    assert_eq!(count_b, 2);
    assert_eq!(seed_a1, "seed-alpha");
    assert_eq!(seed_a2, "seed-alpha");
    assert_eq!(seed_b, "seed-beta");

    let bytes_a1 = std::fs::read(&out_a1).unwrap();
    let bytes_a2 = std::fs::read(&out_a2).unwrap();
    let bytes_b = std::fs::read(&out_b).unwrap();

    // Same seed -> byte-identical output; different seed -> different synthetic identities
    assert_eq!(bytes_a1, bytes_a2);
    assert_ne!(bytes_a1, bytes_b);

    let text_a1 = String::from_utf8(bytes_a1).unwrap();
    assert!(!text_a1.contains("KOWALSKI"));
    assert!(!text_a1.contains("MAREK"));
    assert!(!text_a1.contains("MRN551234"));
    assert!(!text_a1.contains("ENC889900"));
    assert!(!text_a1.contains("987-65-4321"));
    assert!(text_a1.contains("stable hypertension"));
    assert!(text_a1.contains("potassium within normal limits"));
}

#[test]
fn cli_extract_subcommand_decodes_ed_and_standalone_attachments() {
    use pipe_wrench_cli::extract::execute_extract_command;

    let dir = tempfile::tempdir().unwrap();
    let input_path = dir.path().join("attachments.hl7");
    let out_dir = dir.path().join("extracted");

    // "%PDF-1.4\nSynthetic Report\n%%EOF"
    let pdf_b64 = "JVBERi0xLjQKU3ludGhldGljIFJlcG9ydAolJUVPRg==";
    // "{\rtf1\ansi Synthetic clinical summary for testing base64 extraction}"
    let rtf_b64 = "e1xydGYxXGFuc2kgU3ludGhldGljIGNsaW5pY2FsIHN1bW1hcnkgZm9yIHRlc3RpbmcgYmFzZTY0IGV4dHJhY3Rpb259";

    let raw = format!(
        "MSH|^~\\&|LAB|MAIN|EHR|MAIN|20261123||ORU^R01|MSG0001|P|2.5.1\r\
         OBX|1|ED|PDF||^Application^PDF^Base64^{pdf_b64}\r\
         OBX|2|TX|NOTE||{rtf_b64}\r"
    );
    std::fs::write(&input_path, raw.as_bytes()).unwrap();

    let cli = Cli::try_parse_from([
        "pipe_wrench_cli",
        "extract",
        input_path.to_str().unwrap(),
        "--out",
        out_dir.to_str().unwrap(),
    ])
    .unwrap();
    let Commands::Extract(args) = cli.command else {
        panic!("expected Extract command");
    };
    let written = execute_extract_command(&args).unwrap();
    assert_eq!(written.len(), 2);

    let pdf_out = out_dir.join("MSG0001_OBX-5.pdf");
    let rtf_out = out_dir.join("MSG0001_OBX-5.rtf");
    assert_eq!(
        std::fs::read(&pdf_out).unwrap(),
        b"%PDF-1.4\nSynthetic Report\n%%EOF"
    );
    assert_eq!(
        std::fs::read(&rtf_out).unwrap(),
        b"{\\rtf1\\ansi Synthetic clinical summary for testing base64 extraction}"
    );
}

#[test]
fn cli_scrub_help_includes_mandatory_best_effort_phi_disclaimer() {
    use clap::CommandFactory;

    let mut cmd = Cli::command();
    let scrub_cmd = cmd
        .find_subcommand_mut("scrub")
        .expect("scrub subcommand must exist");
    let help = scrub_cmd.render_long_help().to_string();
    assert!(
        help.contains("Best-Effort De-Identification Only"),
        "scrub --help must contain disclaimer heading:\n{help}"
    );
    assert!(
        help.contains("CANNOT be 100% guaranteed"),
        "scrub --help must contain disclaimer body:\n{help}"
    );
}
