use std::{collections::BTreeSet, fmt};

use crate::{Lab, Model, Provider, ProviderModel, Reasoning, Slug, Spec};

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Issue {
    DuplicateLab {
        id: Slug,
    },
    DuplicateProvider {
        id: Slug,
    },
    DuplicateModel {
        id: Slug,
    },
    DuplicateProviderModel {
        provider: Slug,
        upstream_slug: String,
    },
    UnknownLab {
        model: Slug,
        lab: Slug,
    },
    UnknownProvider {
        provider: Slug,
        upstream_slug: String,
    },
    UnknownModel {
        provider: Slug,
        upstream_slug: String,
        model: Slug,
    },
    EmptyEffortLevels {
        record: SpecRecord,
    },
    DuplicateEffortLevel {
        record: SpecRecord,
        level: String,
    },
    UnknownDefaultEffortLevel {
        record: SpecRecord,
        level: String,
    },
    InvertedReasoningBudget {
        record: SpecRecord,
        min: u64,
        max: u64,
    },
}

/// The record whose spec an issue refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecRecord {
    Model {
        id: Slug,
    },
    ProviderModel {
        provider: Slug,
        upstream_slug: String,
    },
}

impl fmt::Display for SpecRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model { id } => write!(formatter, "model {id}"),
            Self::ProviderModel {
                provider,
                upstream_slug,
            } => write!(formatter, "provider model ({provider}, {upstream_slug})"),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateLab { id } => write!(formatter, "duplicate lab {id}"),
            Self::DuplicateProvider { id } => write!(formatter, "duplicate provider {id}"),
            Self::DuplicateModel { id } => write!(formatter, "duplicate model {id}"),
            Self::DuplicateProviderModel {
                provider,
                upstream_slug,
            } => write!(
                formatter,
                "duplicate provider model ({provider}, {upstream_slug})"
            ),
            Self::UnknownLab { model, lab } => {
                write!(formatter, "model {model} references unknown lab {lab}")
            }
            Self::UnknownProvider {
                provider,
                upstream_slug,
            } => write!(
                formatter,
                "provider model ({provider}, {upstream_slug}) references unknown provider"
            ),
            Self::UnknownModel {
                provider,
                upstream_slug,
                model,
            } => write!(
                formatter,
                "provider model ({provider}, {upstream_slug}) references unknown model {model}"
            ),
            Self::EmptyEffortLevels { record } => {
                write!(formatter, "{record} lists no reasoning effort levels")
            }
            Self::DuplicateEffortLevel { record, level } => {
                write!(formatter, "{record} repeats reasoning effort level {level}")
            }
            Self::UnknownDefaultEffortLevel { record, level } => write!(
                formatter,
                "{record} has default reasoning effort level {level} outside its levels"
            ),
            Self::InvertedReasoningBudget { record, min, max } => write!(
                formatter,
                "{record} has reasoning budget min {min} above max {max}"
            ),
        }
    }
}

/// Reports all issues in list and input order; per record, duplicates come first, then
/// references, then spec issues.
pub fn validate(
    labs: &[Lab],
    providers: &[Provider],
    models: &[Model],
    provider_models: &[ProviderModel],
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut lab_ids = BTreeSet::new();
    for lab in labs {
        if !lab_ids.insert(lab.id.as_str()) {
            issues.push(Issue::DuplicateLab { id: lab.id.clone() });
        }
    }

    let mut provider_ids = BTreeSet::new();
    for provider in providers {
        if !provider_ids.insert(provider.id.as_str()) {
            issues.push(Issue::DuplicateProvider {
                id: provider.id.clone(),
            });
        }
    }

    let mut model_ids = BTreeSet::new();
    for model in models {
        if !model_ids.insert(model.id.as_str()) {
            issues.push(Issue::DuplicateModel {
                id: model.id.clone(),
            });
        }
        if !lab_ids.contains(model.lab.as_str()) {
            issues.push(Issue::UnknownLab {
                model: model.id.clone(),
                lab: model.lab.clone(),
            });
        }
        if let Some(spec) = &model.spec {
            validate_spec(
                spec,
                || SpecRecord::Model {
                    id: model.id.clone(),
                },
                &mut issues,
            );
        }
    }

    let mut provider_model_keys = BTreeSet::new();
    for provider_model in provider_models {
        if !provider_model_keys.insert((
            provider_model.provider.as_str(),
            provider_model.upstream_slug.as_str(),
        )) {
            issues.push(Issue::DuplicateProviderModel {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
            });
        }
        if !provider_ids.contains(provider_model.provider.as_str()) {
            issues.push(Issue::UnknownProvider {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
            });
        }
        if !model_ids.contains(provider_model.model.as_str()) {
            issues.push(Issue::UnknownModel {
                provider: provider_model.provider.clone(),
                upstream_slug: provider_model.upstream_slug.clone(),
                model: provider_model.model.clone(),
            });
        }
        if let Some(spec) = &provider_model.spec {
            validate_spec(
                spec,
                || SpecRecord::ProviderModel {
                    provider: provider_model.provider.clone(),
                    upstream_slug: provider_model.upstream_slug.clone(),
                },
                &mut issues,
            );
        }
    }
    issues
}

fn validate_spec(spec: &Spec, record: impl Fn() -> SpecRecord, issues: &mut Vec<Issue>) {
    let Some(Reasoning::Supported(reasoning)) = &spec.reasoning else {
        return;
    };

    if let Some(effort) = &reasoning.effort {
        if effort.levels.is_empty() {
            issues.push(Issue::EmptyEffortLevels { record: record() });
        }
        let mut levels = BTreeSet::new();
        for level in &effort.levels {
            if !levels.insert(level.as_str()) {
                issues.push(Issue::DuplicateEffortLevel {
                    record: record(),
                    level: level.clone(),
                });
            }
        }
        if let Some(level) = &effort.default_level
            && !levels.contains(level.as_str())
        {
            issues.push(Issue::UnknownDefaultEffortLevel {
                record: record(),
                level: level.clone(),
            });
        }
    }

    if let Some(budget) = reasoning.budget
        && budget.min > budget.max
    {
        issues.push(Issue::InvertedReasoningBudget {
            record: record(),
            min: budget.min,
            max: budget.max,
        });
    }
}
