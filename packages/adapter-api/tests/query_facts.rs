use serde_json::json;
use tssae_adapter_api::{
    DeclarationInfo, Evidence, FactQuery, Gap, HostFactQuery, HostQueriedFact,
    NativeAdapterInputAttribution, NativeAdapterOutput, QueriedFact,
};

#[test]
fn serializes_host_queried_facts_and_adapter_input_attributions() {
    let queried = HostQueriedFact {
        subject_node_id: 42,
        value: json!({ "field": "required" }),
        evidence: vec![Evidence {
            kind: "source".to_string(),
            message: "declared in A".to_string(),
            at: None,
            model_id: None,
            declaration: None,
        }],
        gaps: vec![Gap {
            kind: "dynamic-field".to_string(),
            message: "unknown".to_string(),
            at: None,
        }],
        providers: vec!["provider-a".to_string()],
        explain_trace: None,
    };
    let encoded = serde_json::to_value(&queried).expect("serialize HostQueriedFact");
    assert_eq!(encoded["subjectNodeId"], json!(42));
    assert_eq!(encoded["providers"], json!(["provider-a"]));

    let output = NativeAdapterOutput {
        contributions: Vec::new(),
        additional_inputs: vec!["fixture.inputs.json".to_string()],
        adapter_inputs: vec![NativeAdapterInputAttribution {
            adapter_id: "provider-a".to_string(),
            path: "fixture.inputs.json".to_string(),
        }],
    };
    let roundtrip: NativeAdapterOutput =
        serde_json::from_value(serde_json::to_value(&output).expect("serialize output"))
            .expect("deserialize output");
    assert_eq!(roundtrip, output);

    let _ = std::mem::size_of::<FactQuery<'_>>();
    let _ = std::mem::size_of::<QueriedFact<'_>>();
    let _ = std::mem::size_of::<HostFactQuery>();
}

#[test]
fn serializes_declaration_info_package_provenance() {
    let info = DeclarationInfo {
        name: Some("createSelectSchema".to_string()),
        kind: Some("VariableDeclaration".to_string()),
        location: None,
        syntax: None,
        is_external: true,
        package_name: Some("drizzle-zod".to_string()),
        export_name: Some("createSelectSchema".to_string()),
    };
    let encoded = serde_json::to_value(&info).expect("serialize DeclarationInfo");
    assert_eq!(encoded["packageName"], json!("drizzle-zod"));
    assert_eq!(encoded["exportName"], json!("createSelectSchema"));

    let legacy: DeclarationInfo = serde_json::from_value(json!({
        "name": "localFn",
        "kind": "FunctionDeclaration",
        "location": null,
        "syntax": null,
        "isExternal": false
    }))
    .expect("deserialize legacy DeclarationInfo");
    assert_eq!(legacy.package_name, None);
    assert_eq!(legacy.export_name, None);
}
