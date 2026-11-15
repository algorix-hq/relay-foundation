use relay_catalog::{Issue, Lab, Model, Provider, ProviderModel, Slug, validate};
use serde_json::json;

fn slug(value: &str) -> Slug {
    Slug::new(value).expect("valid test slug")
}

#[test]
fn all_issues_are_reported_in_input_order_including_each_repeated_occurrence() {
    let labs: Vec<Lab> = serde_json::from_value(json!([
        {"id": "shared", "name": "Lab"},
        {"id": "shared", "name": "Second lab"},
        {"id": "shared", "name": "Third lab"}
    ]))
    .expect("valid labs");
    let providers: Vec<Provider> = serde_json::from_value(json!([
        {"id": "shared", "name": "Provider"},
        {"id": "shared", "name": "Second provider"},
        {"id": "shared", "name": "Third provider"}
    ]))
    .expect("valid providers");
    let models: Vec<Model> = serde_json::from_value(json!([
        {"id": "model", "lab": "missing-lab", "name": "Model"},
        {"id": "model", "lab": "shared", "name": "Second model"},
        {"id": "model", "lab": "missing-lab", "name": "Third model"}
    ]))
    .expect("valid model records");
    let provider_models: Vec<ProviderModel> = serde_json::from_value(json!([
        {"provider": "shared", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "shared", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "shared", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "missing-provider", "upstream_slug": "Missing/Model", "model": "missing-model"},
        {"provider": "missing-provider", "upstream_slug": "Missing/Model", "model": "missing-model"}
    ]))
    .expect("valid provider model records");

    assert_eq!(
        validate(&labs, &providers, &models, &provider_models),
        vec![
            Issue::DuplicateLab { id: slug("shared") },
            Issue::DuplicateLab { id: slug("shared") },
            Issue::DuplicateProvider { id: slug("shared") },
            Issue::DuplicateProvider { id: slug("shared") },
            Issue::UnknownLab {
                model: slug("model"),
                lab: slug("missing-lab"),
            },
            Issue::DuplicateModel { id: slug("model") },
            Issue::DuplicateModel { id: slug("model") },
            Issue::UnknownLab {
                model: slug("model"),
                lab: slug("missing-lab"),
            },
            Issue::DuplicateProviderModel {
                provider: slug("shared"),
                upstream_slug: "Model:0".into(),
            },
            Issue::DuplicateProviderModel {
                provider: slug("shared"),
                upstream_slug: "Model:0".into(),
            },
            Issue::UnknownProvider {
                provider: slug("missing-provider"),
                upstream_slug: "Missing/Model".into(),
            },
            Issue::UnknownModel {
                provider: slug("missing-provider"),
                upstream_slug: "Missing/Model".into(),
                model: slug("missing-model"),
            },
            Issue::DuplicateProviderModel {
                provider: slug("missing-provider"),
                upstream_slug: "Missing/Model".into(),
            },
            Issue::UnknownProvider {
                provider: slug("missing-provider"),
                upstream_slug: "Missing/Model".into(),
            },
            Issue::UnknownModel {
                provider: slug("missing-provider"),
                upstream_slug: "Missing/Model".into(),
                model: slug("missing-model"),
            },
        ]
    );
}

#[test]
fn lab_and_provider_namespaces_and_both_provider_model_key_parts_are_independent() {
    let labs = vec![Lab {
        id: slug("shared"),
        name: "Lab".into(),
        symbol: None,
    }];
    let providers = vec![
        Provider {
            id: slug("shared"),
            name: "Provider".into(),
            symbol: None,
        },
        Provider {
            id: slug("other"),
            name: "Other provider".into(),
            symbol: None,
        },
    ];
    let models = vec![Model {
        id: slug("model"),
        lab: slug("shared"),
        name: "Model".into(),
        spec: None,
    }];
    let provider_models: Vec<ProviderModel> = serde_json::from_value(json!([
        {"provider": "shared", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "other", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "shared", "upstream_slug": "Model:1", "model": "model"}
    ]))
    .expect("valid provider model records");
    assert!(validate(&labs, &providers, &models, &provider_models).is_empty());
}
