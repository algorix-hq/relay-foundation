use relay_catalog::{
    Auth, Issue, Lab, Model, Provider, ProviderModel, ProviderModelAlias, Slug, SpecRecord,
    validate,
};
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
        {"id": "shared", "name": "Provider", "auth": "byok", "api_formats": ["messages"]},
        {"id": "shared", "name": "Second provider", "auth": "byok", "api_formats": []},
        {"id": "shared", "name": "Third provider", "auth": "byok",
         "api_formats": ["messages", "responses", "messages", "responses", "messages"]}
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
        validate(&labs, &providers, &models, &provider_models, &[]),
        vec![
            Issue::DuplicateLab { id: slug("shared") },
            Issue::DuplicateLab { id: slug("shared") },
            Issue::DuplicateProvider { id: slug("shared") },
            Issue::EmptyApiFormats {
                provider: slug("shared"),
            },
            Issue::DuplicateProvider { id: slug("shared") },
            Issue::DuplicateApiFormat {
                provider: slug("shared"),
                api_format: "messages".into(),
            },
            Issue::DuplicateApiFormat {
                provider: slug("shared"),
                api_format: "responses".into(),
            },
            Issue::DuplicateApiFormat {
                provider: slug("shared"),
                api_format: "messages".into(),
            },
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
            auth: Auth::Byok,
            api_formats: vec!["messages".into()],
        },
        Provider {
            id: slug("other"),
            name: "Other provider".into(),
            symbol: None,
            auth: Auth::Subscription,
            api_formats: vec!["messages".into()],
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
    assert!(validate(&labs, &providers, &models, &provider_models, &[]).is_empty());
}

#[test]
fn service_tier_lists_and_defaults_validate_independently_of_reasoning() {
    let labs = [serde_json::from_value(json!({"id":"lab","name":"Lab"})).unwrap()];
    let record = SpecRecord::Model { id: slug("model") };
    for (spec, issues) in [
        (
            json!({"service_tiers":["standard","custom-tier"],"default_service_tier":"custom-tier"}),
            vec![],
        ),
        (json!({"service_tiers":["standard"]}), vec![]),
        (
            json!({"service_tiers":[]}),
            vec![Issue::EmptyServiceTiers {
                record: record.clone(),
            }],
        ),
        (
            json!({"service_tiers":["","standard","standard"],"default_service_tier":"missing"}),
            vec![
                Issue::EmptyServiceTier {
                    record: record.clone(),
                },
                Issue::DuplicateServiceTier {
                    record: record.clone(),
                    tier: "standard".into(),
                },
                Issue::UnknownDefaultServiceTier {
                    record: record.clone(),
                    tier: "missing".into(),
                },
            ],
        ),
        (
            json!({"default_service_tier":"standard"}),
            vec![Issue::UnknownDefaultServiceTier {
                record: record.clone(),
                tier: "standard".into(),
            }],
        ),
    ] {
        let model: Model =
            serde_json::from_value(json!({"id":"model","name":"Model","lab":"lab","spec":spec}))
                .unwrap();
        assert_eq!(serde_json::to_value(&model.spec).unwrap(), spec);
        assert_eq!(validate(&labs, &[], &[model], &[], &[]), issues);
    }
}

#[test]
fn reasoning_spec_issues_name_the_record_after_its_reference_issues() {
    let labs: Vec<Lab> =
        serde_json::from_value(json!([{"id": "lab", "name": "Lab"}])).expect("valid labs");
    let providers: Vec<Provider> = serde_json::from_value(json!([{
        "id": "provider", "name": "Provider", "auth": "byok", "api_formats": ["messages"]
    }]))
    .expect("valid providers");
    let models: Vec<Model> = serde_json::from_value(json!([
        {"id": "model", "lab": "missing-lab", "name": "Model",
         "spec": {"reasoning": {"effort": {"levels": []}}}}
    ]))
    .expect("valid model records");
    let provider_models: Vec<ProviderModel> = serde_json::from_value(json!([
        {"provider": "provider", "upstream_slug": "Model:0", "model": "model",
         "spec": {"reasoning": {
             "effort": {"levels": ["low", "high", "low"], "default_level": "medium"},
             "budget": {"min": 2048, "max": 1024}
         }}},
        {"provider": "provider", "upstream_slug": "Model:1", "model": "model",
         "spec": {"reasoning": {
             "effort": {"levels": ["low"], "default_level": "low"},
             "budget": {"min": 1024, "max": 1024}
         }}}
    ]))
    .expect("valid provider model records");
    let provider_model = SpecRecord::ProviderModel {
        provider: slug("provider"),
        upstream_slug: "Model:0".into(),
    };

    assert_eq!(
        validate(&labs, &providers, &models, &provider_models, &[]),
        vec![
            Issue::UnknownLab {
                model: slug("model"),
                lab: slug("missing-lab"),
            },
            Issue::EmptyEffortLevels {
                record: SpecRecord::Model { id: slug("model") },
            },
            Issue::DuplicateEffortLevel {
                record: provider_model.clone(),
                level: "low".into(),
            },
            Issue::UnknownDefaultEffortLevel {
                record: provider_model.clone(),
                level: "medium".into(),
            },
            Issue::InvertedReasoningBudget {
                record: provider_model,
                min: 2048,
                max: 1024,
            },
        ]
    );
}

#[test]
fn aliases_share_upstream_keys_and_report_references_before_specs_in_input_order() {
    let labs: Vec<Lab> =
        serde_json::from_value(json!([{"id": "lab", "name": "Lab"}])).expect("valid labs");
    let providers: Vec<Provider> = serde_json::from_value(json!([{
        "id": "provider", "name": "Provider", "auth": "byok", "api_formats": ["responses"]
    }]))
    .expect("valid providers");
    let models: Vec<Model> = serde_json::from_value(json!([
        {"id": "model", "lab": "lab", "name": "Model"}
    ]))
    .expect("valid models");
    let provider_models: Vec<ProviderModel> = serde_json::from_value(json!([
        {"provider": "provider", "upstream_slug": "Model:0", "model": "model"},
        {"provider": "provider", "upstream_slug": "Model:1", "model": "missing"}
    ]))
    .expect("valid provider model records");
    let aliases: Vec<ProviderModelAlias> = serde_json::from_value(json!([
        {
            "provider": "provider", "upstream_slug": "Model:0", "name": "Auto",
            "models": ["model", "model", "missing", "missing"],
            "spec": {"reasoning": {"effort": {"levels": []}}}
        },
        {
            "provider": "missing-provider", "upstream_slug": "auto", "name": "Auto",
            "models": ["missing"],
            "spec": {"reasoning": {"budget": {"min": 2, "max": 1}}}
        },
        {"provider": "missing-provider", "upstream_slug": "auto", "name": "Second auto"}
    ]))
    .expect("valid provider model alias records");

    assert_eq!(
        validate(&labs, &providers, &models, &provider_models, &aliases),
        vec![
            Issue::UnknownModel {
                provider: slug("provider"),
                upstream_slug: "Model:1".into(),
                model: slug("missing"),
            },
            Issue::DuplicateProviderModel {
                provider: slug("provider"),
                upstream_slug: "Model:0".into(),
            },
            Issue::DuplicateAliasModel {
                provider: slug("provider"),
                upstream_slug: "Model:0".into(),
                model: slug("model"),
            },
            Issue::UnknownAliasModel {
                provider: slug("provider"),
                upstream_slug: "Model:0".into(),
                model: slug("missing"),
            },
            Issue::DuplicateAliasModel {
                provider: slug("provider"),
                upstream_slug: "Model:0".into(),
                model: slug("missing"),
            },
            Issue::EmptyEffortLevels {
                record: SpecRecord::ProviderModelAlias {
                    provider: slug("provider"),
                    upstream_slug: "Model:0".into(),
                },
            },
            Issue::UnknownProvider {
                provider: slug("missing-provider"),
                upstream_slug: "auto".into(),
            },
            Issue::UnknownAliasModel {
                provider: slug("missing-provider"),
                upstream_slug: "auto".into(),
                model: slug("missing"),
            },
            Issue::InvertedReasoningBudget {
                record: SpecRecord::ProviderModelAlias {
                    provider: slug("missing-provider"),
                    upstream_slug: "auto".into(),
                },
                min: 2,
                max: 1,
            },
            Issue::DuplicateProviderModel {
                provider: slug("missing-provider"),
                upstream_slug: "auto".into(),
            },
            Issue::UnknownProvider {
                provider: slug("missing-provider"),
                upstream_slug: "auto".into(),
            },
        ]
    );
}
