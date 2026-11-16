use relay_catalog::{
    Lab, Model, Price, Pricing, Provider, ProviderModel, Reasoning, ReasoningSpec, Slug, Spec,
    validate,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[test]
fn agreed_examples_validate_and_round_trip_without_losing_fields() {
    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Catalog {
        labs: Vec<Lab>,
        providers: Vec<Provider>,
        models: Vec<Model>,
        provider_models: Vec<ProviderModel>,
    }

    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/catalog.json")).expect("valid fixture JSON");
    let catalog: Catalog = serde_json::from_value(fixture.clone()).expect("valid example catalog");
    assert_eq!(catalog.labs.len(), 3);
    assert_eq!(catalog.providers.len(), 5);
    assert_eq!(catalog.models.len(), 3);
    assert_eq!(catalog.provider_models.len(), 5);
    assert!(
        validate(
            &catalog.labs,
            &catalog.providers,
            &catalog.models,
            &catalog.provider_models,
        )
        .is_empty()
    );
    let serialized = serde_json::to_value(&catalog).expect("serialize catalog");
    assert_eq!(serialized, fixture);
    assert_eq!(
        serde_json::from_value::<Catalog>(serialized).expect("deserialize serialized catalog"),
        catalog
    );
}

#[test]
fn unknown_fields_are_ignored_in_every_record_and_nested_object() {
    let lab: Lab = serde_json::from_value(json!({
        "id": "lab", "name": "Lab", "future": {"nested": true}
    }))
    .expect("unknown lab field is accepted");
    assert_eq!(
        serde_json::to_value(lab).expect("serialize lab"),
        json!({"id": "lab", "name": "Lab"})
    );

    let provider: Provider = serde_json::from_value(json!({
        "id": "provider", "name": "Provider", "future": [1, 2]
    }))
    .expect("unknown provider field is accepted");
    assert_eq!(
        serde_json::to_value(provider).expect("serialize provider"),
        json!({"id": "provider", "name": "Provider"})
    );

    let model: Model = serde_json::from_value(json!({
        "id": "model", "lab": "lab", "name": "Model",
        "future": true, "spec": {"reasoning": false, "future": "value"}
    }))
    .expect("unknown model and spec fields are accepted");
    assert_eq!(
        serde_json::to_value(model).expect("serialize model"),
        json!({
            "id": "model", "lab": "lab", "name": "Model",
            "spec": {"reasoning": false}
        })
    );

    let provider_model: ProviderModel = serde_json::from_value(json!({
        "provider": "provider", "upstream_slug": "Model", "model": "model",
        "future": null,
        "spec": {"tool_call": true, "future": {}},
        "pricing": {
            "future": [],
            "input": {"amount": 375, "scale": 2, "currency": "USD", "future": false}
        }
    }))
    .expect("unknown provider model, spec, pricing, and price fields are accepted");
    assert_eq!(
        serde_json::to_value(provider_model).expect("serialize provider model"),
        json!({
            "provider": "provider", "upstream_slug": "Model", "model": "model",
            "spec": {"tool_call": true},
            "pricing": {"input": {"amount": 375, "scale": 2, "currency": "USD"}}
        })
    );
}

fn assert_missing_fields<T: DeserializeOwned + Serialize>(input: Value, paths: &[&str]) {
    let record: T = serde_json::from_value(input.clone()).expect("optional fields can be absent");
    let serialized = serde_json::to_value(record).expect("serialize sparse record");
    assert_eq!(serialized, input);

    let mut with_nulls = input;
    for path in paths {
        with_nulls[*path] = Value::Null;
    }
    let record: T = serde_json::from_value(with_nulls).expect("optional fields can be null");
    assert_eq!(
        serde_json::to_value(record).expect("serialize record with null fields"),
        serialized
    );
}

#[test]
fn missing_or_null_optional_fields_remain_none_and_are_omitted() {
    assert_missing_fields::<Lab>(json!({"id": "lab", "name": "Lab"}), &["symbol"]);
    assert_missing_fields::<Provider>(json!({"id": "provider", "name": "Provider"}), &["symbol"]);
    assert_missing_fields::<Model>(
        json!({"id": "model", "lab": "lab", "name": "Model"}),
        &["spec"],
    );
    assert_missing_fields::<ProviderModel>(
        json!({"provider": "provider", "upstream_slug": "Model", "model": "model"}),
        &["spec", "pricing"],
    );
    assert_missing_fields::<Spec>(
        json!({}),
        &[
            "context_window",
            "max_output_tokens",
            "tool_call",
            "reasoning",
            "input_modalities",
        ],
    );
    assert_missing_fields::<Pricing>(json!({}), &["input", "output", "cache_read", "cache_write"]);

    let spec: Spec = serde_json::from_value(json!({})).expect("empty spec");
    assert_eq!(
        spec,
        Spec {
            context_window: None,
            max_output_tokens: None,
            tool_call: None,
            reasoning: None,
            input_modalities: None,
        }
    );
    let pricing: Pricing = serde_json::from_value(json!({})).expect("empty pricing");
    assert_eq!(
        pricing,
        Pricing {
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
        }
    );
}

#[test]
fn slug_construction_and_deserialization_enforce_the_same_ascii_grammar() {
    for value in ["a", "0", "claude-sonnet-4.5", "9model", "a.-", "a0"] {
        let slug = Slug::new(value).expect("valid slug");
        let from_json: Slug = serde_json::from_value(json!(value)).expect("valid JSON slug");
        assert_eq!(slug, from_json);
        assert_eq!(
            serde_json::to_value(slug).expect("serialize slug"),
            json!(value)
        );
    }
    for value in [
        "Claude Sonnet",
        "-x",
        "",
        "UPPERCASE",
        ".x",
        "é",
        "a_b",
        "a/b",
        "a\n",
    ] {
        let error = Slug::new(value).expect_err("invalid slug");
        assert_eq!(error.value, value);
        assert!(
            serde_json::from_value::<Slug>(json!(value)).is_err(),
            "accepted invalid slug {value:?}"
        );
        assert!(
            serde_json::from_value::<Model>(json!({
                "id": value, "lab": "lab", "name": "Model"
            }))
            .is_err(),
            "accepted invalid model id {value:?}"
        );
    }
}

#[test]
fn upstream_model_identifiers_are_nonempty_but_not_relay_slugs() {
    assert!(
        serde_json::from_value::<ProviderModel>(json!({
            "provider": "provider", "upstream_slug": "", "model": "model"
        }))
        .is_err()
    );
    let value = json!({
        "provider": "provider", "upstream_slug": "Lab.Model/Version:0", "model": "model"
    });
    let provider_model: ProviderModel =
        serde_json::from_value(value.clone()).expect("upstream identifier is not a Relay slug");
    assert_eq!(
        serde_json::to_value(provider_model).expect("serialize upstream identifier"),
        value
    );
}

#[test]
fn present_symbols_must_not_be_empty() {
    let record = json!({"id": "entity", "name": "Entity", "symbol": ""});
    assert!(serde_json::from_value::<Lab>(record.clone()).is_err());
    assert!(serde_json::from_value::<Provider>(record).is_err());
}

#[test]
fn price_currency_must_not_be_empty() {
    assert!(
        serde_json::from_value::<Price>(json!({"amount": 1, "scale": 0, "currency": ""})).is_err()
    );
}

#[test]
fn prices_reject_negative_amounts_and_scales_without_limiting_unsigned_values() {
    for value in [
        json!({"amount": 1, "scale": -1, "currency": "USD"}),
        json!({"amount": -1, "scale": 0, "currency": "USD"}),
    ] {
        assert!(serde_json::from_value::<Price>(value).is_err());
    }
    let value = json!({"amount": u64::MAX, "scale": u32::MAX, "currency": "USD"});
    let price: Price = serde_json::from_value(value.clone()).expect("unsigned bounds are valid");
    assert_eq!(serde_json::to_value(price).expect("serialize price"), value);
}

#[test]
fn reasoning_is_false_when_unsupported_and_an_object_when_supported() {
    for value in [
        json!(false),
        json!({}),
        json!({
            "effort": {"levels": ["low", "high"], "default_level": "low"},
            "budget": {"min": 0, "max": 1024},
            "display": ["full", "summarized", "omitted"]
        }),
        json!({"effort": {"levels": ["low"]}}),
    ] {
        let reasoning: Reasoning =
            serde_json::from_value(value.clone()).expect("valid reasoning value");
        assert_eq!(
            serde_json::to_value(reasoning).expect("serialize reasoning"),
            value
        );
    }
    assert_eq!(
        serde_json::from_value::<Reasoning>(json!(false)).expect("false is unsupported"),
        Reasoning::Unsupported
    );
    assert_eq!(
        serde_json::from_value::<Reasoning>(json!({})).expect("empty object is supported"),
        Reasoning::Supported(ReasoningSpec::default())
    );
}

#[test]
fn reasoning_rejects_true_unknown_display_values_and_effort_without_levels() {
    for value in [
        json!(true),
        json!({"display": ["verbose"]}),
        json!({"effort": {"default_level": "low"}}),
        json!({"budget": {"min": 1}}),
    ] {
        assert!(
            serde_json::from_value::<Reasoning>(value.clone()).is_err(),
            "accepted {value}"
        );
    }
}

#[test]
fn reasoning_ignores_unknown_fields() {
    let reasoning: Reasoning = serde_json::from_value(json!({
        "future": 1,
        "effort": {"levels": ["low"], "future": true},
        "budget": {"min": 1, "max": 2, "future": null}
    }))
    .expect("unknown reasoning fields are accepted");
    assert_eq!(
        serde_json::to_value(reasoning).expect("serialize reasoning"),
        json!({"effort": {"levels": ["low"]}, "budget": {"min": 1, "max": 2}})
    );
}
