use tssae_adapter_api::{
    ADAPTER_ABI_VERSION, Adapter, AdapterError, AdapterManifest, Context, FactContribution,
    FactRequest,
};

mod extractor;

#[derive(Default)]
struct ZodAdapter;

impl Adapter for ZodAdapter {
    fn manifest(&self) -> AdapterManifest {
        AdapterManifest {
            abi_version: ADAPTER_ABI_VERSION,
            id: "zod".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            fact_type_ids: vec![
                "verule.validator.input-fields".to_string(),
                "verule.validator.output-fields".to_string(),
                "verule.validator.field".to_string(),
            ],
        }
    }

    fn extract<'a>(
        &self,
        context: &'a Context<'_>,
        request: &FactRequest<'a>,
    ) -> Result<Vec<FactContribution<'a>>, AdapterError> {
        extractor::extract(context, request)
    }
}

tssae_adapter_api::export_adapter!(ZodAdapter);
