//! Run with: cargo run -p vera-adapters --features vera --example typed_pipeline
use std::io;
use vera_adapters::{
    cbor::Cbor,
    json::Json,
    transcode,
    vera::{decode_params, encode_params, Typed},
    yaml::Yaml,
    Adapter,
};
use vera_compiler::{
    analyze, parse_source,
    runtime::eval_function,
    types::ValueType,
    verifier::{verify_all, VerifierConfig},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = r#"
        module PortableBoundary;
        record Entry { sequence:u64; data:bytes<16>; amount:dec64; }
        record Empty {}
        union Envelope { Entry:Entry; Empty:Empty; }
        function identity(input:Envelope)->Envelope
          ensure unchanged:eq(result,input);
          body { yield input; }
    "#;
    let syntax = parse_source(source).map_err(|errors| io::Error::other(format!("{errors:?}")))?;
    let program = analyze(syntax).map_err(|errors| io::Error::other(format!("{errors:?}")))?;

    // Parsing/type checking alone is NOT verification. Refuse execution unless
    // every reported obligation succeeds; unknown/timeouts are not success.
    let reports = verify_all(&program, &VerifierConfig::default());
    if reports.is_empty() || reports.iter().any(|report| !report.ok()) {
        return Err(io::Error::other(
            reports
                .iter()
                .map(|report| report.summary())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .into());
    }
    let binding = Typed::new(
        &program,
        ValueType::Named(
            program
                .type_id("Envelope")
                .ok_or_else(|| io::Error::other("missing schema"))?,
        ),
    )?;
    // Dotted decimal *text* means human units. Numeric decimals instead mean
    // integral scaled coefficients, compatible with VERA's existing host JSON output.
    let input = br#"{"tag":"Entry","payload":{"sequence":18446744073709551615,"data":"0x00ff01","amount":"1.25"}}"#;
    let admitted = binding.decode(&Json, input)?;
    let function_id = program
        .function_id("identity")
        .ok_or_else(|| io::Error::other("missing function"))?;
    let result = eval_function(&program, function_id, std::slice::from_ref(&admitted))?;
    assert_eq!(result, admitted);

    for (name, adapter) in [
        ("JSON", &Json as &dyn Adapter),
        ("YAML", &Yaml),
        ("CBOR", &Cbor),
    ] {
        let encoded = binding.encode(adapter, &result)?;
        assert_eq!(binding.decode(adapter, &encoded)?, admitted);
        println!("{name}: typed roundtrip passed ({} bytes)", encoded.len());
    }
    println!(
        "YAML output:\n{}",
        String::from_utf8(binding.encode(&Yaml, &result)?)?
    );

    // Whole parameter maps use the exact declaration, including unknown/missing
    // parameter rejection. Hosts order eval arguments by the checked signature.
    let signature = &program.functions()[function_id.0].params;
    let input_params = format!(r#"{{"input":{}}}"#, std::str::from_utf8(input)?);
    let params = decode_params(&Json, &program, signature, input_params.as_bytes())?;
    let wire_params = encode_params(&Cbor, &program, signature, &params)?;
    assert_eq!(
        decode_params(&Cbor, &program, signature, &wire_params)?,
        params
    );

    // Transport-only conversion requires neither VERA types nor execution.
    let portable_yaml = transcode(&Json, &Yaml, input)?;
    assert_eq!(Json.decode(input)?, Yaml.decode(&portable_yaml)?);
    println!("Verified pure function and independent transport conversion passed.");
    Ok(())
}
