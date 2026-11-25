use verule_adapter_api::{ADAPTER_ABI_VERSION, AdapterError, AdapterManifest, NodeOperation};

#[test]
fn resolves_the_appended_declaration_node_operation() {
    assert_eq!(
        NodeOperation::try_from(17),
        Ok(NodeOperation::DeclarationNode)
    );
}

#[test]
fn accepts_a_valid_native_adapter_manifest() {
    let manifest = AdapterManifest {
        abi_version: ADAPTER_ABI_VERSION,
        id: "example-db".to_string(),
        version: "1.2.0".to_string(),
        fact_type_ids: vec!["example.query.projection".to_string()],
    };

    assert_eq!(manifest.validate(), Ok(()));
}

#[test]
fn rejects_an_unsupported_abi_before_loading_an_adapter() {
    let manifest = AdapterManifest {
        abi_version: ADAPTER_ABI_VERSION + 1,
        id: "example-db".to_string(),
        version: "1.2.0".to_string(),
        fact_type_ids: vec!["example.query.projection".to_string()],
    };

    assert!(matches!(
        manifest.validate(),
        Err(AdapterError::UnsupportedAbi { found, expected })
            if found == ADAPTER_ABI_VERSION + 1 && expected == ADAPTER_ABI_VERSION
    ));
}

#[test]
fn rejects_duplicate_fact_types_in_a_manifest() {
    let manifest = AdapterManifest {
        abi_version: ADAPTER_ABI_VERSION,
        id: "example-db".to_string(),
        version: "1.2.0".to_string(),
        fact_type_ids: vec![
            "example.query.projection".to_string(),
            "example.query.projection".to_string(),
        ],
    };

    assert!(matches!(
        manifest.validate(),
        Err(AdapterError::DuplicateFactType { .. })
    ));
}
